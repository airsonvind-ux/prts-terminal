#![allow(clippy::too_many_arguments)] // Tauri 命令参数面与前端 invoke 契约固定，拆 struct 会改变 IPC 形状

use super::prelude::*;

use crate::attachments::Attachment;
use crate::orchestrator::HistorySeed;
use crate::session::{SessionEvent, SessionLog};
use crate::settings::BackendHistoryMode;

// ═══════════════════════════════════════════════════════
// 请求类型
// ═══════════════════════════════════════════════════════

/// 前端 agent_process invoke 的请求体。
///
/// 对应 `types.ts` 的 AgentRequest：
///   input            — 用户原始输入
///   history          — legacy/shadow 的完整历史；primary 传空数组
///   sessionId        — 本次请求的唯一标识，随所有 AgentEvent 回传
///   conversationId   — 对话标识（模块 C：缓存区按此分区；可选字段，
///                      旧前端不传时后端用 sessionId 兜底）
///
/// rename_all = "camelCase" 将 Rust 的 snake_case 字段名
/// 自动映射为前端传来的 camelCase JSON key，无需手动 rename。
/// 2026-10（C-lite）：conversation_id 与 user_content 在 primary 模式被消费；
/// input 仍是纯用户输入（不含 reminder 块），legacy 兼容保留。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct AgentRequest {
    pub input: String,
    pub history: Vec<FrontendMessage>,
    pub session_id: String,
    /// 对话标识（可选：旧前端不传 → 反序列化为空串，命令层兜底为 session_id）
    #[serde(default)]
    pub conversation_id: String,
    /// 本轮 user 消息的完整正文（含 `[prts_reminder]` 块）。
    /// primary 模式下写入官方 log 的就是这一份；legacy 模式仍用 history。
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
    steer_queue: State<'_, Arc<SteerQueue>>,
    settings_store: State<'_, Arc<Mutex<SettingsStore>>>,
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
    // 缓存 State 已注释（2026-08：缓存系统停用，待升级为记忆系统）

    // ── 包装 emit 回调：AppHandle.emit → 闭包 ──
    let emit_app_handle = app_handle.clone();
    let session_id = request.session_id.clone();
    let emit = move |event: IpcEvent| {
        // emit 失败不中断处理（仅日志记录）
        if let Err(e) = emit_app_handle.emit("agent-event", event) {
            eprintln!("[commands] emit 失败: {e}");
        }
    };

    // ── 读取配置（短暂持锁） ──
    let llm_config = {
        let guard = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        guard.clone()
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
    let conversation_id = request.conversation_id.clone();
    let abort_signal = turn_registry.register(&session_id, &conversation_id);
    // RAII 收尾：注册后任何错误出口（含 `?`）都会注销 busy 位
    let _turn_guard = TurnGuard {
        registry: Arc::clone(&turn_registry),
        session_id: session_id.clone(),
        signal: abort_signal.clone(),
    };

    // ── workspace_root（地域性信任门信任根）：settings.workspace 段取一次
    // （与 lib.rs 装配 Policy/RunCommandTool 同源），owned 注入 process。
    let workspace_root = {
        let st = settings_store
            .lock()
            .map_err(|e| format!("设置锁获取失败: {e}"))?;
        st.workspace_root()
    };

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

    // ── 历史来源（C-lite）：primary 从官方 log 读，legacy 仍用前端 history ──
    let history_mode = {
        let st = settings_store
            .lock()
            .map_err(|e| format!("设置锁获取失败: {e}"))?;
        st.settings.backend_history_mode()
    };
    let primary_history =
        matches!(history_mode, BackendHistoryMode::Primary) && !conversation_id.is_empty();

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
    let history_seed = if primary_history {
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
        HistorySeed::Log {
            turns,
            user: Box::new(user_event.clone()),
            compaction,
        }
    } else {
        HistorySeed::Frontend(request.history)
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

    // ── shadow：写影子日志并对比派生消息；仍走 legacy 读写，不影响用户 ──
    if matches!(history_mode, BackendHistoryMode::Shadow) && !conversation_id.is_empty() {
        let events = log
            .lock()
            .map_err(|e| format!("会话日志锁获取失败: {e}"))?
            .events()
            .to_vec();
        let mut current = log
            .lock()
            .map_err(|e| format!("会话日志锁获取失败: {e}"))?
            .turn_events()
            .to_vec();
        // legacy 播种时当前 user 在 history 里，turn_events 不含它；
        // 影子日志按轮追加时必须补回，否则相邻轮会被派生合并。
        current.insert(0, user_event.clone());
        if !events.is_empty() {
            let mut s = store
                .lock()
                .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
            // 压缩是对话级状态转换：legacy 从摘要 + 保留段重新开始，
            // shadow 也必须用同一基线，不能继续拿完整历史对比。
            if let Some(compaction) = s.meta(&conversation_id)?.compaction {
                let marker = s.shadow_compaction_marker(&conversation_id)?;
                if marker != Some(compaction.at) {
                    s.reset_shadow(&conversation_id)?;
                    s.set_shadow_compaction_marker(&conversation_id, compaction.at)?;
                }
            }
            let prior = s.load_shadow_turns(&conversation_id)?;
            let line = if prior.is_empty() { &events } else { &current };
            if !line.is_empty() {
                let json =
                    serde_json::to_string(line).map_err(|e| format!("影子事件序列化失败: {e}"))?;
                s.append_shadow_events(&conversation_id, &json)?;
            }
            let policy = model_registry
                .get_resolved_compat(&llm_config.provider, &llm_config.model, llm_config.api_type)
                .policy;
            let legacy = log
                .lock()
                .map_err(|e| format!("会话日志锁获取失败: {e}"))?
                .derive_messages(policy);
            let mut shadow_log = SessionLog::new();
            for turn in s.load_shadow_turns(&conversation_id)? {
                shadow_log.seed_events(turn);
            }
            let shadow = shadow_log.derive_messages(policy);
            if serde_json::to_value(&legacy).ok() != serde_json::to_value(&shadow).ok() {
                let legacy_json = serde_json::to_string(&legacy).unwrap_or_default();
                let shadow_json = serde_json::to_string(&shadow).unwrap_or_default();
                let diff = format!(
                    "[{}] shadow diff\nlegacy: {}\nshadow: {}\n\n",
                    conversation_id, legacy_json, shadow_json
                );
                let _ = s.append_shadow_diff(&diff);
                eprintln!("[shadow] 派生消息不一致：{conversation_id}");
            }
        }
    }

    // ── primary：本轮事件写官方 log（唯一 writer；前端 append_turn 在 primary
    // 只更新元数据，见 conversation 命令）──
    if primary_history {
        let mut events: Vec<SessionEvent> = vec![SessionEvent::turn_start(&session_id)];
        {
            let log = log.lock().map_err(|e| format!("会话日志锁获取失败: {e}"))?;
            events.extend(log.turn_events().iter().cloned());
        }
        if events.len() <= 1 {
            return Err("本轮没有产生可落盘的事件".into());
        }
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

    // ── 插话队列收尾（保留语义，2026-09-25 修订）：不再无条件清空——
    // 未交付条目在有 conversation 归属时进入保留态：同对话下一轮
    // agent_process 开头 adopt 领取，于首步请求前注入交付（中止/错误也
    // 不丢字，对齐上游 keepInbox）。无 conversation 归属（旧前端）保持
    // 原兜底清空；正常轮队列在 turn/end 前已消费至空，此处零行为。
    if steer_queue.len(&session_id) > 0 && !conversation_id.is_empty() {
        steer_queue.preserve(&session_id, &conversation_id);
    } else {
        steer_queue.clear(&session_id);
    }
    // busy 位由 `_turn_guard` Drop 统一注销（含上方所有错误出口）

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

// ═══════════════════════════════════════════════════════
// clear_conversation_cache —— 已注释（2026-08：缓存系统停用，
// 待升级为记忆系统。此前为 AI 自行添加、前端无对应功能，一并停用）
// ═══════════════════════════════════════════════════════

// /// 清空指定对话的缓存分区（前端删除对话时调用）。
// ///
// /// 缓存区是单对话工作记忆，生命周期绑定对话（见 cache.rs 顶部的
// /// 「边界（缓存 ≠ 记忆）」声明）：对话删除即整体失效。此处移除对应
// /// 分区，释放进程内内存；分区不存在时幂等（remove_partition 返回 false），
// /// 命令仍返回 Ok——重复清理不视为错误。
// #[tauri::command]
// pub fn clear_conversation_cache(
//     conversation_id: String,
//     cache_stores: State<'_, Arc<Mutex<crate::cache::CacheStores>>>,
// ) -> Result<(), String> {
//     let mut stores = cache_stores
//         .lock()
//         .map_err(|e| format!("缓存区锁获取失败: {e}"))?;
//     stores.remove_partition(&conversation_id);
//     Ok(())
// }
