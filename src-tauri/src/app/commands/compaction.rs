#![allow(clippy::too_many_arguments)] // Tauri 命令参数面与前端 invoke 契约固定，拆 struct 会改变 IPC 形状

use super::prelude::*;
use crate::session::SessionEvent;

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
    /// 折叠区历史（前端派生；链式压缩时含旧摘要消息）。
    /// 仅作为旧客户端/测试兼容路径；生产优先使用后端官方 log 的轮边界。
    pub folded_history: Vec<FrontendMessage>,
    /// 折叠区在会话 turns 中的起始下标（上一次压缩边界）。
    #[serde(default)]
    pub folded_from: Option<usize>,
    /// 本次压缩后保留段第一轮的下标（本次折叠区结束边界）。
    #[serde(default)]
    pub retained_from: Option<usize>,
    /// 前端当前看到的 turns 总数；用于校验后端 log 没有少读/多读轮次。
    #[serde(default)]
    pub total_turns: Option<usize>,
    /// 会话级请求身份；缺省时回退全局配置。思考档由后台任务策略决定。
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
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
    settings_store: State<'_, Arc<Mutex<SettingsStore>>>,
    credentials: State<'_, Arc<Mutex<crate::credentials::CredentialsStore>>>,
    model_registry: State<'_, Arc<ModelRegistry>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    session_store: State<'_, Arc<Mutex<SessionStore>>>,
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
    let mut cfg = match (
        request.provider.as_deref().filter(|p| !p.is_empty()),
        request.model.as_deref().filter(|m| !m.is_empty()),
    ) {
        (Some(provider), Some(model)) => {
            let st = settings_store
                .lock()
                .map_err(|e| format!("设置锁获取失败: {e}"))?;
            let creds = credentials
                .lock()
                .map_err(|e| format!("凭据锁获取失败: {e}"))?;
            st.settings
                .llm
                .resolve_for(provider, model, None, |p| {
                    creds
                        .get(&crate::credentials::provider_key_ref(p))
                        .map(|s| s.to_string())
                })
                .ok_or_else(|| format!("会话使用的供应商「{provider}」未配置"))?
        }
        _ => config
            .lock()
            .map_err(|e| format!("配置锁获取失败: {e}"))?
            .clone(),
    };
    cfg.thinking_level = crate::llm::thinking::utility_level(
        &model_registry,
        &cfg.provider,
        &cfg.model,
        crate::llm::thinking::UtilityTask::Compaction,
    )
    .map(str::to_string);
    eprintln!(
        "[utility] task=compaction conversation={} provider={} model={} api={:?} thinking={}",
        request.conversation_id,
        cfg.provider,
        cfg.model,
        cfg.api_type,
        cfg.thinking_level.as_deref().unwrap_or("none")
    );
    let client = llm
        .select(&cfg.api_type)
        .ok_or_else(|| format!("协议 {:?} 暂未支持", cfg.api_type))?;
    // 窗口与主对话同源（model_registry）；自定义模型查不到时用兜底常量
    let window = model_registry
        .get_model(&cfg.provider, &cfg.model)
        .map(|m| m.context)
        .unwrap_or(orchestrator::compaction::DEFAULT_CONTEXT_WINDOW);
    let replay = model_registry.get_resolved_compat(&cfg.provider, &cfg.model, cfg.api_type);
    // C-lite primary：生产压缩优先消费后端官方 log 的完整事件，
    // 这样 assistant/provider-blocks 不会被前端投影丢掉。
    let official_input = load_compaction_events(
        &session_store,
        &request.conversation_id,
        request.folded_from,
        request.retained_from,
        request.total_turns,
    );
    let official_input = match official_input {
        Ok(input) => input,
        Err(e) => return Err(e),
    };
    let abort = turn_registry.register(&request.session_id, &request.conversation_id);
    let result = match official_input {
        Some((turns, prior_summary)) => {
            orchestrator::compaction::run_summary_with_events(
                &**client,
                &cfg,
                turns,
                prior_summary,
                window,
                &replay,
                &system_prompt,
                &tool_defs,
                &abort,
            )
            .await
        }
        None => {
            orchestrator::compaction::run_summary_with_replay(
                &**client,
                &cfg,
                request.folded_history,
                window,
                &replay,
                &system_prompt,
                &tool_defs,
                &abort,
            )
            .await
        }
    };
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

/// 从后端官方会话 log 读取压缩折叠区。
///
/// 前端传来的 `foldedFrom` / `retainedFrom` 是 turns 下标；两者缺失时返回
/// `None`，保留旧客户端兼容路径。边界与磁盘不一致时返回 `Err`，直接拒绝
/// 压缩，不猜测、不回退到 frontend 路径。
type CompactionEventInput = Option<(Vec<Vec<SessionEvent>>, Option<String>)>;

fn load_compaction_events(
    session_store: &Arc<Mutex<SessionStore>>,
    conversation_id: &str,
    folded_from: Option<usize>,
    retained_from: Option<usize>,
    total_turns: Option<usize>,
) -> Result<CompactionEventInput, String> {
    let (Some(from), Some(to)) = (folded_from, retained_from) else {
        return Ok(None);
    };
    if conversation_id.is_empty() {
        return Ok(None);
    }
    if from > to {
        return Err("压缩边界无效：起始轮晚于保留段。".into());
    }
    let mut store = session_store
        .lock()
        .map_err(|e| format!("会话存储锁获取失败：{e}"))?;
    let meta = store
        .meta(conversation_id)
        .map_err(|e| format!("会话元数据读取失败：{e}"))?;
    let turns = store
        .load_turns(conversation_id)
        .map_err(|e| format!("会话轮读取失败：{e}"))?;
    let total = total_turns
        .ok_or_else(|| "压缩请求缺少总轮数（totalTurns），请重启应用后再压缩。".to_string())?;
    if turns.len() != total {
        return Err(format!(
            "会话历史与磁盘不一致（前端 {total} 轮，磁盘 {} 轮），请重启或修复会话后再压缩。",
            turns.len()
        ));
    }
    if let Some(compaction) = meta.compaction.as_ref() {
        if compaction.retained_from as usize != from {
            return Err(format!(
                "压缩边界与上次压缩记录不一致（记录 {}，请求 {from}），请重启后再压缩。",
                compaction.retained_from
            ));
        }
    } else if from != 0 {
        return Err("压缩边界指向不存在的上次压缩记录，请重启后再压缩。".into());
    }
    if to > turns.len() {
        return Err(format!(
            "压缩边界越界（retainedFrom={to}，磁盘 {} 轮），请重启后再压缩。",
            turns.len()
        ));
    }
    Ok(Some((
        turns[from..to].to_vec(),
        meta.compaction.map(|c| c.summary),
    )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::AppendTurnReq;

    #[test]
    fn compaction_rejects_frontend_backend_turn_mismatch() {
        let dir =
            std::env::temp_dir().join(format!("prts-compaction-boundary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SessionStore::new(&dir);
        store
            .ensure_conversation(
                "c1",
                Some("压缩边界"),
                Some(1),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        store
            .append_turn(&AppendTurnReq {
                conversation_id: "c1".into(),
                events_json: r#"[{"type":"user/message","sessionId":"s1","content":"x"}]"#.into(),
                title: None,
                created_at: None,
                model_name: None,
                context_length: None,
                provider: None,
                model: None,
                thinking_level: None,
                context_tokens: None,
                last_notice_seq: None,
                usage_deltas: None,
            })
            .unwrap();
        let store = Arc::new(Mutex::new(store));
        let err = load_compaction_events(&store, "c1", Some(0), Some(1), Some(2)).unwrap_err();
        assert!(err.contains("不一致"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
