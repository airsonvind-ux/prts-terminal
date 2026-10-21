use super::prelude::*;

// ═══════════════════════════════════════════════════════
// 上下文压缩（模块 D · 上下文衰减）
// ═══════════════════════════════════════════════════════

/// 压缩请求体（compact_conversation 命令）。
///
/// `session_id` 由前端生成（约定 `ctxdecay-{conversationId}`），经
/// TurnRegistry 注册后即可走既有 `abort_turn` 通道取消摘要 run（压缩中
/// 再次点击圆环 → 取消确认 → abort 同一 id）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactRequest {
    pub conversation_id: String,
    /// 压缩注册/取消用 session 标识（约定 ctxdecay-{conversationId}）
    pub session_id: String,
    /// 折叠区历史（前端派生；链式压缩时含旧摘要消息）
    pub folded_history: Vec<FrontendMessage>,
}

/// compact_conversation 成功载荷（camelCase 序列化给前端）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionOutcome {
    /// 结构化 checkpoint 正文（定界块由前端合成）
    pub summary: String,
    /// true = 输入超窗规则降级产出（无 LLM 调用）
    pub degraded: bool,
    /// 折叠区压缩前估算（信息用，随压缩记录落盘）
    pub tokens_before: Option<u64>,
    /// 本次摘要 LLM 调用用量（前端随压缩记录一并提交）；降级产出时 None。
    pub usage: Option<UsageStat>,
}

/// 压缩对话（模块 D · 上下文衰减）：对前端折叠区做一次静默独立 LLM 摘要 run。
///
/// 语义要点：
/// - 压缩 = 前端发起的一次对话级状态转换；本命令只产出摘要正文，**不产生
///   对话轮事件、不 emit agent-event**（压缩不是轮；记录由前端
///   update_conversation_meta 写入会话元数据）；
/// - busy 闸：对话处理中（active_count > 0）拒绝——生成与压缩双向互斥，
///   拒绝而非排队（避免压缩跑在被改写历史上）；
/// - 取消：以 `ctxdecay-{conversationId}` 注册 TurnRegistry，前端可经
///   abort_turn 中止；注册计入 active_count（压缩中设置写闸同样生效）；
/// - 失败语义：Err(String) 文案面向用户直显；失败不改变任何状态，幂等可重试；
/// - 请求形态（2026-09-25 · 上游形态）：system 与主对话同源（含冻结目录与
///   记忆区）、工具面同主请求、指令置折叠区末尾——请求成为主请求的真前缀，
///   复用 KV cache。
/// - 成功语义（批 4）：以活体重刷该对话两份冻结快照（有差才写；best-effort）——
///   后续轮次的 system 基线切到最新。
#[tauri::command]
pub async fn compact_conversation(
    request: CompactRequest,
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    model_registry: State<'_, Arc<ModelRegistry>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    snapshot_store: State<'_, Arc<Mutex<SnapshotStore>>>,
    skills_snapshot_store: State<'_, Arc<Mutex<SkillSnapshotStore>>>,
    prefix_snapshot_store: State<'_, Arc<Mutex<PrefixSnapshotStore>>>,
    tools: State<'_, Arc<ToolRegistry>>,
) -> Result<CompactionOutcome, String> {
    if request.folded_history.is_empty() {
        return Err("没有可压缩的内容".into());
    }
    if turn_registry.active_count() > 0 {
        return Err("正在处理对话，请稍后再压缩".into());
    }
    // ── 请求前缀同源（2026-09-25 裁决 · 上游形态）：system（含冻结目录与
    // 记忆区）与工具面同主对话 → 摘要请求成为主请求真前缀（KV cache 复用）。──
    let conversation_key = if request.conversation_id.is_empty() {
        None
    } else {
        Some(request.conversation_id.as_str())
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
    // 技能目录冻结快照（批 2）：压缩读旧快照——请求须复用主请求真前缀。
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
    // 完整前缀冻结快照（批 5）：压缩请求须与主请求前缀逐字节一致。
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
    let cfg = config
        .lock()
        .map_err(|e| format!("配置锁获取失败: {e}"))?
        .clone();
    let client = llm
        .select(&cfg.api_type)
        .ok_or_else(|| format!("协议 {:?} 暂未支持", cfg.api_type))?;
    // 窗口与主对话同源（model_registry）；自定义模型查不到时用兜底常量
    let window = model_registry
        .get_model(&cfg.provider, &cfg.model)
        .map(|m| m.context)
        .unwrap_or(orchestrator::compaction::DEFAULT_CONTEXT_WINDOW);
    let abort = turn_registry.register(&request.session_id, &request.conversation_id);
    let result = orchestrator::compaction::run_summary(
        &**client,
        &cfg,
        request.folded_history,
        window,
        &system_prompt,
        &tool_defs,
        &abort,
    )
    .await;
    turn_registry.unregister_if(&request.session_id, &abort);
    result.map(|o| {
        // 批 4 · 压缩并更新：摘要成功（含降级规则产出——上下文同样被替换）→
        // 以活体重刷两份冻结快照（有差才写；best-effort，失败不改变压缩结果）。
        let live = crate::skills_index::SkillIndex::load(&crate::skills_index::skills_dir())
            .map(|idx| idx.catalog_text())
            .map_err(|e| eprintln!("[commands] 技能目录加载失败（压缩后不刷新技能快照）：{e}"))
            .ok();
        let live_tools = tools.list_defs();
        refresh_frozen_snapshots(
            &request.conversation_id,
            live.as_deref(),
            &live_tools,
            &memory_store,
            &snapshot_store,
            &skills_snapshot_store,
            &prefix_snapshot_store,
        );
        CompactionOutcome {
            summary: o.summary,
            degraded: o.degraded,
            tokens_before: o.tokens_before,
            usage: o.usage.map(|u| usage_stat(&u, cfg.api_type)),
        }
    })
}

/// 压缩成功后刷新三份冻结快照（批 4 / 批 5 · 压缩并更新）。
///
/// 顺序有依赖：记忆区与技能目录先各自刷新，再用刷新后的两者重组装活体前缀，
/// 刷新完整前缀快照（system 全文 + 工具面）。三者均有差才写。本函数
/// best-effort：锁失败仅终端留痕，不改变压缩结果（调用方照常返回成功）。
pub(super) fn refresh_frozen_snapshots(
    conversation_id: &str,
    live_catalog: Option<&str>,
    live_tools: &[crate::llm::ToolDef],
    memory_store: &Arc<Mutex<MemoryStore>>,
    snapshot_store: &Arc<Mutex<SnapshotStore>>,
    skills_snapshot_store: &Arc<Mutex<SkillSnapshotStore>>,
    prefix_snapshot_store: &Arc<Mutex<PrefixSnapshotStore>>,
) {
    if let Ok(mem) = memory_store.lock() {
        match snapshot_store.lock() {
            Ok(mut snaps) => {
                snaps.refresh(conversation_id, &mem);
            }
            Err(e) => eprintln!("[commands] 记忆快照刷新跳过（快照锁失败）：{e}"),
        }
    } else {
        eprintln!("[commands] 记忆快照刷新跳过（记忆锁失败）");
    }
    match skills_snapshot_store.lock() {
        Ok(mut snaps) => {
            snaps.refresh(conversation_id, live_catalog);
        }
        Err(e) => eprintln!("[commands] 技能快照刷新跳过（锁失败）：{e}"),
    }
    // 完整前缀：目录不可读（None）时保持旧前缀不刷（无法确认新字节）。
    let Some(catalog) = live_catalog else {
        return;
    };
    let live_system = match (memory_store.lock(), snapshot_store.lock()) {
        (Ok(mem), Ok(mut snaps)) => {
            let block = snaps.get_or_assemble(Some(conversation_id), &mem);
            crate::orchestrator::context::assemble_system_prompt_with_memory(catalog, &block)
        }
        _ => {
            eprintln!("[commands] 前缀快照刷新跳过（记忆/记忆快照锁失败）");
            return;
        }
    };
    match prefix_snapshot_store.lock() {
        Ok(mut snaps) => {
            snaps.refresh(conversation_id, &live_system, live_tools);
        }
        Err(e) => eprintln!("[commands] 前缀快照刷新跳过（锁失败）：{e}"),
    }
}
