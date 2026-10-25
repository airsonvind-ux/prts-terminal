#![allow(clippy::too_many_arguments)] // Tauri 命令参数面与前端 invoke 契约固定，拆 struct 会改变 IPC 形状

use super::prelude::*;

use crate::attachments::Attachment;
use crate::orchestrator::HistorySeed;
use crate::session::{SessionEvent, SessionLog, TurnEndReason};

// ═══════════════════════════════════════════════════════
// 请求类型
// ═══════════════════════════════════════════════════════

/// 前端 agent_process invoke 的请求体。
///
/// 对应 `types.ts` 的 AgentRequest：
///   input            — 用户原始输入
///   sessionId        — 本次请求的唯一标识，随所有 AgentEvent 回传
///   conversationId   — 对话标识（记忆/目录/前缀快照按此冻结；可选字段，
///                      旧前端不传时后端用 sessionId 兜底）
///
/// rename_all = "camelCase" 将 Rust 的 snake_case 字段名
/// 自动映射为前端传来的 camelCase JSON key，无需手动 rename。
/// 2026-10（C-lite）：conversation_id 与 user_content 是唯一历史入口；
/// input 仍是纯用户输入（不含 reminder 块）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct AgentRequest {
    pub input: String,
    pub session_id: String,
    /// 对话标识（可选：旧前端不传 → 反序列化为空串，命令层兜底为 session_id）
    #[serde(default)]
    pub conversation_id: String,
    /// 本轮 user 消息的完整正文（含 `[prts_reminder]` 块），写入官方 log。
    #[serde(default)]
    pub user_content: String,
    /// 本轮 user 消息附件（primary 模式入 log）。
    #[serde(default)]
    pub user_attachments: Vec<Attachment>,
    /// 首轮建文件用的会话元数据（primary 模式）。
    #[serde(default)]
    pub conversation_title: Option<String>,
    #[serde(default)]
    pub conversation_created_at: Option<u64>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    /// 会话级请求身份（新前端）；缺省时回退全局 LlmConfig。
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking_level: Option<String>,
    /// 环境通知游标（primary 模式随轮写 index）。
    #[serde(default)]
    pub last_notice_seq: Option<u64>,
}

// ═══════════════════════════════════════════════════════
// 活动 turn RAII 收尾
// ═══════════════════════════════════════════════════════

/// 活动 turn 的 RAII 收尾：无论正常返回、错误还是 `?` 提前退出，
/// Drop 都会注销该 session 的 busy 位（防忙位泄漏，批 1 审查修复 🟡1）。
struct TurnGuard {
    registry: Arc<TurnRegistry>,
    session_id: String,
    signal: crate::turn_registry::AbortSignal,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.registry.unregister_if(&self.session_id, &self.signal);
    }
}

/// 插话队列的 RAII 收尾：正常返回、错误与所有 `?` 早退都走同一出口。
/// 未交付条目有 conversation 归属时置保留态，同对话下一轮 adopt 领取。
struct SteerGuard {
    queue: Arc<SteerQueue>,
    session_id: String,
    conversation_id: String,
}

impl Drop for SteerGuard {
    fn drop(&mut self) {
        self.queue
            .finish_turn(&self.session_id, &self.conversation_id);
    }
}

/// 预检失败也按真实原因归档一轮，避免重启恢复把它猜成 interrupted。
///
/// 触发时机：journal 已建立、但 `process_with_log` 尚未开始。此时模型
/// 没有请求过，工具也没有执行过；用户需要看到的是实际失败原因，而不是
/// “应用异常退出”。这里把日志补成完整的 error 轮并提交，随后原错误仍
/// 返回给前端做即时展示。
fn persist_preflight_failure(
    store: &Arc<Mutex<SessionStore>>,
    log: &Arc<Mutex<SessionLog>>,
    request: &AgentRequest,
    llm_config: &LlmConfig,
    user_event: &SessionEvent,
    error: &str,
) -> Result<(), String> {
    // 与 process_with_log 的轮起点同形：turn/start + user 在边界之前，
    // 边界之后的 error / turn/end 才会写入 journal。
    {
        let mut log = log.lock().map_err(|e| format!("会话日志锁获取失败: {e}"))?;
        log.append(SessionEvent::turn_start(&request.session_id));
        log.append(user_event.clone());
        log.mark_turn_boundary();
        log.append(SessionEvent::error(&request.session_id, error, false));
        log.append(SessionEvent::turn_end(
            &request.session_id,
            TurnEndReason::Error,
        ));
    }

    let events = {
        let log = log.lock().map_err(|e| format!("会话日志锁获取失败: {e}"))?;
        log.archive_events(&request.session_id, user_event)
    };
    let events_json =
        serde_json::to_string(&events).map_err(|e| format!("失败轮事件序列化失败：{e}"))?;
    let req = AppendTurnReq {
        conversation_id: request.conversation_id.clone(),
        events_json,
        title: request.conversation_title.clone(),
        created_at: request.conversation_created_at,
        model_name: request
            .model_name
            .clone()
            .or_else(|| Some(llm_config.model.clone())),
        context_length: request.context_length,
        provider: request.provider.clone(),
        model: request.model.clone(),
        thinking_level: request.thinking_level.clone(),
        context_tokens: None,
        last_notice_seq: request.last_notice_seq,
        usage_deltas: None,
    };
    {
        let mut s = store
            .lock()
            .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        s.append_turn(&req)?;
    }

    // 提交成功后关闭并删除 journal；失败时保留 journal，让启动恢复
    // 带着已经写入的 turn/end(error) 重新提交，而不是丢掉真实失败原因。
    if let Ok(mut log) = log.lock() {
        log.close_journal();
    }
    if let Ok(s) = store.lock() {
        s.finish_inflight(&request.session_id);
    }
    Ok(())
}

fn combine_preflight_error(original: String, persisted: Result<(), String>) -> String {
    match persisted {
        Ok(()) => original,
        Err(e) => format!("{original}；失败记录保存失败：{e}"),
    }
}

/// 错误轮也必须把真实原因写进 journal，并补上终止标记。
///
/// 协议选择失败发生在 `process_with_log` 建立 turn 之前，此时 log 还是空的；
/// 这样 journal 里只有 begin_inflight 写的 user，崩溃恢复会把已知错误误判成
/// interrupted。这里补齐 turn/start、user、error、turn/end，保证恢复时仍能
/// 带着原始错误原因落盘。
fn record_error_terminal(
    log: &Arc<Mutex<SessionLog>>,
    session_id: &str,
    user_event: &SessionEvent,
    error: &str,
) -> Result<(), String> {
    let mut log = log.lock().map_err(|e| format!("会话日志锁获取失败: {e}"))?;

    let has_turn_start = log
        .events()
        .iter()
        .any(|e| matches!(e, SessionEvent::TurnStart { .. }));
    if !has_turn_start {
        log.append(SessionEvent::turn_start(session_id));
        log.append(user_event.clone());
        log.mark_turn_boundary();
    }

    log.append(SessionEvent::error(session_id, error, false));

    let has_turn_end = log
        .events()
        .iter()
        .any(|e| matches!(e, SessionEvent::TurnEnd { .. }));
    if !has_turn_end {
        log.append(SessionEvent::turn_end(session_id, TurnEndReason::Error));
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════
// agent_process —— 核心命令
// ═══════════════════════════════════════════════════════

/// 处理一次完整的 Agent 交互。
///
/// 前端调用 `invoke("agent_process", { request })`，
/// 本命令同步返回 Ok(()) 表示"接收成功"，实际处理结果通过
/// `app_handle.emit("agent-event", ...)` 异步推送。
///
/// 异步命令在 Tauri 的多线程运行时上执行，
/// orchestrator::process 的 await 不会阻塞主线程。
/// 前端事件通过 listen("agent-event") 独立通道到达，
/// 不依赖命令返回值。
///
/// 参数管理方式：
///   AppHandle    — 命令函数自动注入，用于 emit 事件
///   State<Arc<T>> — lib.rs 装配时通过 app.manage() 注入的共享状态
///                   Arc 保证异步上下文中数据存活，Mutex 保护可变访问
#[tauri::command]
pub async fn agent_process(
    app_handle: AppHandle,
    request: AgentRequest,
    tools: State<'_, Arc<ToolRegistry>>,
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    db: State<'_, Arc<Database>>,
    model_registry: State<'_, Arc<ModelRegistry>>,
    exec_settings: State<'_, Arc<Mutex<crate::exec_settings::ExecutionSettings>>>,
    approval_registry: State<'_, Arc<crate::approval::ApprovalRegistry>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    settings_write_gate: State<'_, Arc<tokio::sync::Mutex<()>>>,
    steer_queue: State<'_, Arc<SteerQueue>>,
    settings_store: State<'_, Arc<Mutex<SettingsStore>>>,
    credentials: State<'_, Arc<Mutex<crate::credentials::CredentialsStore>>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    snapshot_store: State<'_, Arc<Mutex<SnapshotStore>>>,
    skills_snapshot_store: State<'_, Arc<Mutex<SkillSnapshotStore>>>,
    prefix_snapshot_store: State<'_, Arc<Mutex<PrefixSnapshotStore>>>,
    store: State<'_, Arc<Mutex<SessionStore>>>,
) -> Result<(), String> {
    // ── Clone Arc：从 State 引用中获得可移入 async 上下文的独立所有权 ──
    let tools = Arc::clone(&tools);
    let llm = Arc::clone(&llm);
    let config = Arc::clone(&config);
    let db = Some(Arc::clone(&db));
    let model_registry = Arc::clone(&model_registry);
    let turn_registry = Arc::clone(&turn_registry);
    let steer_queue = Arc::clone(&steer_queue);
    let memory_store = Arc::clone(&memory_store);
    let snapshot_store = Arc::clone(&snapshot_store);
    let skills_snapshot_store = Arc::clone(&skills_snapshot_store);
    let prefix_snapshot_store = Arc::clone(&prefix_snapshot_store);

    // ── 包装 emit 回调：AppHandle.emit → 闭包 ──
    let emit_app_handle = app_handle.clone();
    let session_id = request.session_id.clone();
    let emit = move |event: IpcEvent| {
        // emit 失败不中断处理（仅日志记录）
        if let Err(e) = emit_app_handle.emit("agent-event", event) {
            eprintln!("[commands] emit 失败: {e}");
        }
    };

    // R-21：持有设置写闸完成本轮配置/工作区快照与 turn 注册。
    // set_settings 也持同一把闸；两边因此只能一前一后，不会出现
    // “检查 active_count 通过 → turn 注册 → root.set 换根”的中间态。
    let settings_gate = settings_write_gate.lock().await;

    // ── 读取请求配置（短暂持锁）──
    // 新前端随请求携带会话级 provider/model/thinking；缺省时回退全局配置，
    // 兼容旧前端。provider 存在但未配置 → 明确报错，不静默换模型。
    let llm_config = {
        let requested_provider = request.provider.as_deref().filter(|p| !p.is_empty());
        let requested_model = request.model.as_deref().filter(|m| !m.is_empty());
        match (requested_provider, requested_model) {
            (Some(provider), Some(model)) => {
                let st = settings_store
                    .lock()
                    .map_err(|e| format!("设置锁获取失败: {e}"))?;
                let creds = credentials
                    .lock()
                    .map_err(|e| format!("凭据锁获取失败: {e}"))?;
                st.settings
                    .llm
                    .resolve_for(provider, model, request.thinking_level.as_deref(), |p| {
                        creds
                            .get(&crate::credentials::provider_key_ref(p))
                            .map(|s| s.to_string())
                    })
                    .ok_or_else(|| format!("会话使用的供应商「{provider}」未配置"))?
            }
            _ => {
                let guard = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
                guard.clone()
            }
        }
    };

    // ── 命令执行模式快照（M3-A）：turn 起始读一次 → 本 turn 内模式不变
    // （processing/streaming 期间锁定的语义底座；M3-B 在 set_settings 的
    // exec patch 上叠加运行时闸）。
    let mode = {
        let s = exec_settings
            .lock()
            .map_err(|e| format!("执行模式配置锁获取失败: {e}"))?;
        s.mode
    };

    // ── 注册活动 turn（M3-B §2.2）：取得 abort 句柄；
    // process 返回（含中止/错误）后无条件注销——busy 口径释放点。
    // conversation_id 为 AgentRequest 已收字段（serde 到位，此前未消费）。
    if turn_registry.is_shutting_down() {
        return Err("应用正在关闭，不能开始新的对话轮".into());
    }
    let conversation_id = request.conversation_id.clone();
    let abort_signal = turn_registry.register(&session_id, &conversation_id);
    // RAII 收尾：注册后任何错误出口（含 `?`）都会注销 busy 位
    let _turn_guard = TurnGuard {
        registry: Arc::clone(&turn_registry),
        session_id: session_id.clone(),
        signal: abort_signal.clone(),
    };
    // 注册成功后立即接管插话队列收尾：后续任何早退都不会留下无归属孤儿。
    let _steer_guard = SteerGuard {
        queue: Arc::clone(&steer_queue),
        session_id: session_id.clone(),
        conversation_id: conversation_id.clone(),
    };

    // ── workspace_root（地域性信任门信任根）：settings.workspace 段取一次
    // （与 lib.rs 装配 Policy/RunCommandTool 同源），owned 注入 process。
    let workspace_root = {
        let st = settings_store
            .lock()
            .map_err(|e| format!("设置锁获取失败: {e}"))?;
        st.workspace_root()
    };
    // 快照与注册已完成；后续长轮不再占用设置写闸。
    drop(settings_gate);

    // ── 记忆区冻结快照（Phase 4 任务二）：按 conversationId 取 ──
    // 有真实 conversationId → 组装 + 落盘 sidecar（对话内冻结，唯一刷新点＝新对话）；
    // **缺 conversationId（旧前端）→ 只组装、不落盘**（sessionId 每请求唯一，
    // 用它当冻结键会名存实亡并产生垃圾快照，批 1 审查修复 🟡3）。
    let conversation_key = if conversation_id.is_empty() {
        None
    } else {
        Some(conversation_id.as_str())
    };
    let memory_block = {
        let mem = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        let mut snaps = snapshot_store
            .lock()
            .map_err(|e| format!("快照存储锁获取失败: {e}"))?;
        snaps.get_or_assemble(conversation_key, &mem)
    };

    // ── 技能目录冻结快照（批 2）：与记忆区同点位、同 key ──
    // 首见冻结（活体目录文本 → sidecar）；活体不可读时不冻结（下轮重试）。
    let skill_catalog = {
        let live = crate::skills_index::SkillIndex::load(&crate::skills_index::skills_dir())
            .map(|idx| idx.catalog_text())
            .map_err(|e| eprintln!("[commands] 技能目录加载失败（本次不冻结）：{e}"))
            .ok();
        let mut snaps = skills_snapshot_store
            .lock()
            .map_err(|e| format!("技能快照存储锁获取失败: {e}"))?;
        snaps.get_or_freeze(conversation_key, live.as_deref())
    };

    // ── 完整前缀冻结快照（批 5）：system 全文 + 工具定义 ──
    // 对话创建即冻结，此后一律照用（跨重启、跨应用升级逐字节不变）；
    // 刷新点＝新对话 ＋ 压缩（见 refresh_frozen_snapshots）。
    // 人格/界面段是编译期常量、工具面来自注册表，两者随版本变——冻结住
    // 创建时的那份，旧对话前缀才不会因升级而破。
    let (system_prompt, tool_defs, snapshot_stale) = {
        let live_system = crate::orchestrator::context::assemble_system_prompt_with_memory(
            &skill_catalog,
            &memory_block,
        );
        let live_tools = tools.list_defs();
        let mut snaps = prefix_snapshot_store
            .lock()
            .map_err(|e| format!("前缀快照存储锁获取失败: {e}"))?;
        let snap = snaps.get_or_freeze(conversation_key, &live_system, &live_tools);
        let stale = snap.is_stale_against(&live_system, &live_tools);
        (
            snap.system,
            snap.tool_defs,
            stale.then_some(snap.app_version),
        )
    };
    // 上一轮状态不修改 system prompt：前缀必须冻结，状态经前端
    // [prts_reminder] 块随 user 消息传递。
    if let Some(version) = snapshot_stale {
        if let Err(e) = app_handle.emit(
            "agent-event",
            crate::events::IpcEvent::Agent(crate::events::AgentEvent::conversation_snapshot_stale(
                &conversation_id,
                &version,
            )),
        ) {
            eprintln!("[commands] 旧快照提示推送失败: {e}");
        }
    }

    // ── 插话保留领取（2026-09-25）：同对话上一轮中止/错误留下的未交付
    // 插话并入本轮队列，dispatch 首个 step 请求前领取注入（轮次边界领取，
    // 对齐上游「turn 边界领取 next-step」）。无 conversation 归属跳过。
    if !conversation_id.is_empty() {
        steer_queue.adopt(&conversation_id, &session_id);
    }

    let log = Arc::new(Mutex::new(SessionLog::new()));
    let user_content = if request.user_content.is_empty() {
        request.input.as_str()
    } else {
        request.user_content.as_str()
    };
    let user_event = if request.user_attachments.is_empty() {
        SessionEvent::user_message(&session_id, user_content)
    } else {
        SessionEvent::user_message_with_attachments(
            &session_id,
            user_content,
            request.user_attachments.clone(),
        )
    };
    // 轮内 journal：在 LLM 请求前先落 meta + turn/start + user。
    // 后续 chunk/tool 事件由 SessionLog 追加；进程被杀时启动恢复读取。
    let inflight_meta = InflightMeta {
        conversation_id: conversation_id.clone(),
        session_id: session_id.clone(),
        title: request.conversation_title.clone(),
        created_at: request.conversation_created_at,
        model_name: request.model_name.clone(),
        context_length: request.context_length,
        provider: request.provider.clone(),
        model: request.model.clone(),
        thinking_level: request.thinking_level.clone(),
        last_notice_seq: request.last_notice_seq,
        user_event: Some(user_event.clone()),
    };
    let journal_path = if conversation_id.is_empty() {
        None
    } else {
        let s = store
            .lock()
            .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        Some(s.begin_inflight(
            &inflight_meta,
            &[SessionEvent::turn_start(&session_id), user_event.clone()],
        )?)
    };
    if let Some(path) = &journal_path {
        let attached = log
            .lock()
            .map_err(|e| format!("会话日志锁获取失败: {e}"))
            .and_then(|mut log| log.attach_journal(path));
        if let Err(e) = attached {
            let persisted =
                persist_preflight_failure(&store, &log, &request, &llm_config, &user_event, &e);
            return Err(combine_preflight_error(e, persisted));
        }
    }

    // 预检阶段：还没请求模型。这里的失败必须按真实原因归档成 error 轮，
    // 不能只留下 journal 让重启恢复猜成 interrupted。
    let preflight = (|| -> Result<HistorySeed, String> {
        let title = request.conversation_title.as_deref().unwrap_or("新对话");
        {
            let mut s = store
                .lock()
                .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
            s.ensure_conversation(
                &conversation_id,
                Some(title),
                request.conversation_created_at,
                request.model_name.as_deref(),
                request.context_length,
                request.provider.as_deref(),
                request.model.as_deref(),
                request.thinking_level.as_deref(),
            )?;
        }
        let (turns, compaction) = {
            let mut s = store
                .lock()
                .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
            let turns = s.load_turns(&conversation_id)?;
            let compaction = s.meta(&conversation_id)?.compaction;
            (turns, compaction)
        };
        Ok(HistorySeed::Log {
            turns,
            user: Box::new(user_event.clone()),
            compaction,
        })
    })();
    let history_seed = match preflight {
        Ok(seed) => seed,
        Err(e) => {
            let persisted =
                persist_preflight_failure(&store, &log, &request, &llm_config, &user_event, &e);
            return Err(combine_preflight_error(e, persisted));
        }
    };

    // ── 调用调度中枢（abort 透传：取消点消费；冻结目录 + 记忆区注入 system）──
    let result = orchestrator::process_with_log(
        &session_id,
        history_seed,
        Arc::clone(&log),
        &tools, // State<Arc<ToolRegistry>> → &Arc<ToolRegistry>（deref 强制）
        &llm,   // Arc<dyn LlmClient> → &dyn LlmClient
        &llm_config,
        &model_registry,
        db,
        mode,
        approval_registry.as_ref(),
        &abort_signal,
        steer_queue.as_ref(),
        workspace_root,
        emit,
        system_prompt,
        tool_defs,
    )
    .await;

    // 错误事件也是本轮持久事实：写入 log 供重启后 UI 重建错误提示；
    // derive_messages 会忽略它，不进入模型历史。
    if let Err(e) = &result {
        if let Err(err) = record_error_terminal(&log, &session_id, &user_event, &e.to_string()) {
            eprintln!("[commands] 错误轮终态写入失败：{err}");
        }
    }

    // ── 本轮事件写官方 log（唯一 writer；前端 append_turn 只更新元数据）──
    {
        let events = {
            let log = log.lock().map_err(|e| format!("会话日志锁获取失败: {e}"))?;
            log.archive_events(&session_id, &user_event)
        };
        let events_json =
            serde_json::to_string(&events).map_err(|e| format!("本轮事件序列化失败: {e}"))?;
        let context_tokens = events.iter().rev().find_map(|event| match event {
            SessionEvent::AssistantMessage { usage, .. } => *usage,
            SessionEvent::StepEnd { usage, .. } => *usage,
            _ => None,
        });
        let usage_deltas = events.iter().find_map(|event| match event {
            SessionEvent::TurnEnd { usage, .. } => usage.map(|stat| UsageDelta {
                category: UsageCategory::Session,
                input_tokens: stat.input_tokens,
                output_tokens: stat.output_tokens,
                cache_hit_tokens: stat.cache_hit_tokens,
                cache_prompt_tokens: stat.cache_prompt_tokens,
            }),
            _ => None,
        });
        let req = AppendTurnReq {
            conversation_id: conversation_id.clone(),
            events_json,
            title: request.conversation_title.clone(),
            created_at: request.conversation_created_at,
            model_name: request
                .model_name
                .clone()
                .or_else(|| Some(llm_config.model.clone())),
            context_length: request.context_length,
            provider: request.provider.clone(),
            model: request.model.clone(),
            thinking_level: request.thinking_level.clone(),
            context_tokens,
            last_notice_seq: request.last_notice_seq,
            usage_deltas: usage_deltas.map(|delta| vec![delta]),
        };
        let mut s = store
            .lock()
            .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        s.append_turn(&req)
            .map_err(|e| format!("本轮历史落盘失败：{e}"))?;
    }

    // journal 句柄必须在删除文件前关闭（Windows 不允许删除仍打开的文件）。
    if let Ok(mut log) = log.lock() {
        log.close_journal();
    }
    if let Ok(s) = store.lock() {
        s.finish_inflight(&session_id);
    }

    // 插话队列与 busy 位分别由 `_steer_guard` / `_turn_guard` Drop 统一收尾
    // （含上方所有错误出口）；正常轮队列在 turn/end 前已消费至空。

    // ── 错误处理：调度中枢报错 → 推送错误事件到前端（新词汇）──
    if let Err(e) = result {
        app_handle
            .emit(
                "agent-event",
                crate::events::IpcEvent::Session(crate::session::SessionEvent::error(
                    &session_id,
                    &e.to_string(),
                    false,
                )),
            )
            .map_err(|e| format!("错误事件推送失败: {e}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("prts-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request() -> AgentRequest {
        AgentRequest {
            input: "查一下".into(),
            session_id: "s1".into(),
            conversation_id: "c1".into(),
            user_content: "查一下".into(),
            user_attachments: Vec::new(),
            conversation_title: Some("预检失败".into()),
            conversation_created_at: Some(1),
            model_name: None,
            context_length: None,
            provider: None,
            model: None,
            thinking_level: None,
            last_notice_seq: None,
        }
    }

    fn inflight_meta(user: &SessionEvent) -> InflightMeta {
        InflightMeta {
            conversation_id: "c1".into(),
            session_id: "s1".into(),
            title: Some("预检失败".into()),
            created_at: Some(1),
            model_name: None,
            context_length: None,
            provider: None,
            model: None,
            thinking_level: None,
            last_notice_seq: None,
            user_event: Some(user.clone()),
        }
    }

    #[test]
    fn protocol_selection_error_records_terminal_and_message() {
        let log = Arc::new(Mutex::new(SessionLog::new()));
        let user = SessionEvent::user_message("s1", "查一下");

        record_error_terminal(&log, "s1", &user, "协议暂未支持").unwrap();

        let events = log.lock().unwrap().archive_events("s1", &user);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, SessionEvent::UserMessage { .. }))
                .count(),
            1,
            "user 只能出现一次"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::Error { message, .. } if message == "协议暂未支持"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Error,
                ..
            }
        )));
        assert!(!events.iter().any(|e| matches!(
            e,
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Interrupted,
                ..
            }
        )));
    }

    #[test]
    fn dispatch_error_does_not_duplicate_terminal() {
        let log = Arc::new(Mutex::new(SessionLog::new()));
        let user = SessionEvent::user_message("s1", "查一下");
        {
            let mut log = log.lock().unwrap();
            log.append(SessionEvent::turn_start("s1"));
            log.append(user.clone());
            log.mark_turn_boundary();
            log.append(SessionEvent::turn_end("s1", TurnEndReason::Error));
        }

        record_error_terminal(&log, "s1", &user, "模型请求失败").unwrap();

        let events = log.lock().unwrap().archive_events("s1", &user);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, SessionEvent::TurnEnd { .. }))
                .count(),
            1,
            "turn/end 只能有一个"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::Error { message, .. } if message == "模型请求失败"
        )));
    }

    #[test]
    fn preflight_failure_is_archived_as_error_not_interrupted() {
        let dir = unique_temp_dir("preflight-error");
        let store = Arc::new(Mutex::new(SessionStore::new(&dir)));
        let log = Arc::new(Mutex::new(SessionLog::new()));
        let request = request();
        let user = SessionEvent::user_message("s1", "查一下");

        let path = store
            .lock()
            .unwrap()
            .begin_inflight(
                &inflight_meta(&user),
                &[SessionEvent::turn_start("s1"), user.clone()],
            )
            .unwrap();
        log.lock().unwrap().attach_journal(&path).unwrap();

        persist_preflight_failure(
            &store,
            &log,
            &request,
            &LlmConfig::default(),
            &user,
            "会话文件读取失败",
        )
        .unwrap();

        assert!(!path.exists(), "提交后 journal 应删除");
        let mut s = store.lock().unwrap();
        assert_eq!(s.recover_inflight().unwrap(), 0, "已提交轮不应再被恢复");
        let turns = s.load_turns("c1").unwrap();
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(
            turn.iter()
                .filter(|e| matches!(e, SessionEvent::UserMessage { .. }))
                .count(),
            1,
            "user 只能出现一次"
        );
        assert!(turn.iter().any(|e| matches!(
            e,
            SessionEvent::Error { message, .. } if message == "会话文件读取失败"
        )));
        assert!(turn.iter().any(|e| matches!(
            e,
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Error,
                ..
            }
        )));
        assert!(!turn.iter().any(|e| matches!(
            e,
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Interrupted,
                ..
            }
        )));

        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
