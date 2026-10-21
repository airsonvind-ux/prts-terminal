use super::prelude::*;

// ═══════════════════════════════════════════════════════
// 请求类型
// ═══════════════════════════════════════════════════════

/// 前端 agent_process invoke 的请求体。
///
/// 对应 `types.ts` 的 AgentRequest：
///   input            — 用户原始输入
///   history          — 完整对话历史（含本轮 user 消息）
///   sessionId        — 本次请求的唯一标识，随所有 AgentEvent 回传
///   conversationId   — 对话标识（模块 C：缓存区按此分区；可选字段，
///                      旧前端不传时后端用 sessionId 兜底）
///
/// rename_all = "camelCase" 将 Rust 的 snake_case 字段名
/// 自动映射为前端传来的 camelCase JSON key，无需手动 rename。
/// 2026-08：input / conversation_id 当前未被后端消费（input 为协议预留字段；
/// conversation_id 随缓存系统停用暂不消费），保留以兼容前端协议。
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
    let (system_prompt, tool_defs) = {
        let live_system = crate::orchestrator::context::assemble_system_prompt_with_memory(
            &skill_catalog,
            &memory_block,
        );
        let live_tools = tools.list_defs();
        let mut snaps = prefix_snapshot_store
            .lock()
            .map_err(|e| format!("前缀快照存储锁获取失败: {e}"))?;
        let snap = snaps.get_or_freeze(conversation_key, &live_system, &live_tools);
        (snap.system, snap.tool_defs)
    };

    // ── 插话保留领取（2026-09-25）：同对话上一轮中止/错误留下的未交付
    // 插话并入本轮队列，dispatch 首个 step 请求前领取注入（轮次边界领取，
    // 对齐上游「turn 边界领取 next-step」）。无 conversation 归属跳过。
    if !conversation_id.is_empty() {
        steer_queue.adopt(&conversation_id, &session_id);
    }

    // ── 调用调度中枢（abort 透传：取消点消费；冻结目录 + 记忆区注入 system）──
    let result = orchestrator::process_with_memory(
        &session_id,
        request.history,
        &tools, // State<Arc<ToolRegistry>> → &Arc<ToolRegistry>（deref 强制）
        &llm,  // Arc<dyn LlmClient> → &dyn LlmClient
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
