#![allow(clippy::too_many_arguments)] // Tauri 命令参数面与前端 invoke 契约固定，拆 struct 会改变 IPC 形状

use super::prelude::*;

// ═══════════════════════════════════════════════════════
// 会话持久化（架构升级 · 模块 1 · Phase 4：对话跨重启恢复）
// ═══════════════════════════════════════════════════════
//
// 命令层只做参数透传与锁获取，语义全部收敛在 session_store（纯存储层，
// 不解析业务）。写盘触发 = 前端轮末归档（turn/end 与错误轮都提交）；
// 空对话不落盘（createConversation 不触发 IPC）；删除 = 删持久文件。
// 线程口径：读重命令异步执行（窗口线程不因读档阻塞）；写命令保持同步——
// 主线程顺序执行承载着标题 CAS / metaSnapshot 等顺序假设，不轻动。

/// 列出全部会话元数据（来源 index.json——列表不扫内容文件；附带孤儿清理）。
#[tauri::command]
pub fn list_conversations(
    store: State<'_, Arc<Mutex<SessionStore>>>,
) -> Result<Vec<ConversationMeta>, String> {
    let mut s = store
        .lock()
        .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    s.list()
}

/// 读取单个会话：meta + 轮行（坏行已跳过；恢复时前端 JSON.parse 逐行重建）。
///
/// **读出侧合并流式 chunk（2026-09 · 单遍）**：归档按事件落盘，每个流式 chunk
/// 一条，长对话单行可达数 MB；返回前把「连续且同 kind 的 assistant/chunk」合并
/// 为一条（`SessionStore::get_coalesced`——校验与合并在同一次解析上完成，输出与
/// 旧两遍路径逐字节一致），payload 与前端解析量下降数量级。**落盘原文不动**——
/// 历史冻结语义不变。
///
/// 异步执行（线程池）：读档与合并不占窗口线程，启动恢复期间窗口保持可拖。
#[tauri::command(async)]
pub fn get_conversation(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    conversation_id: String,
) -> Result<ConversationRecord, String> {
    let mut s = store
        .lock()
        .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    s.get_coalesced(&conversation_id)
}

/// 追加一轮（前端轮末归档触发）。首轮建会话文件（header + 轮行 + index 插入），
/// 后续轮追加轮行 + index 更新 updatedAt 与随轮提交的 contextTokens/lastNoticeSeq。
/// 可缺省字段：JS 侧 null/不传 = 缺省（Option）。
#[tauri::command]
pub fn append_turn(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    settings_store: State<'_, Arc<Mutex<SettingsStore>>>,
    conversation_id: String,
    events_json: String,
    title: Option<String>,
    created_at: Option<u64>,
    model_name: Option<String>,
    context_length: Option<u64>,
    context_tokens: Option<TokenUsage>,
    last_notice_seq: Option<u64>,
    usage_deltas: Option<Vec<UsageDelta>>,
) -> Result<AppendTurnOutcome, String> {
    let req = AppendTurnReq {
        conversation_id,
        events_json,
        title,
        created_at,
        model_name,
        context_length,
        context_tokens,
        last_notice_seq,
        usage_deltas,
    };
    let primary = {
        let st = settings_store
            .lock()
            .map_err(|e| format!("设置锁获取失败: {e}"))?;
        st.settings.backend_history_mode() == crate::settings::BackendHistoryMode::Primary
    };
    let mut s = store
        .lock()
        .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    if primary {
        s.append_meta_only(&req)
    } else {
        s.append_turn(&req)
    }
}

/// 更新会话元数据（rename / contextTokens / lastNoticeSeq / compaction；
/// 只更新非 None 字段；clearContextTokens / clearNoticeSeq 为显式清空开关）。游标正常随轮
/// 提交，本命令是独立更新路径（低频；模块 D 压缩记录的落盘通道）。
///
/// `expectedTitle` 为生成标题的 CAS 守卫：Some 时仅当 index 当前 title 等于
/// 该值才应用 `title`（用户已改名 → 放弃写入，不报错）。
#[tauri::command]
pub fn update_conversation_meta(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    conversation_id: String,
    title: Option<String>,
    expected_title: Option<String>,
    context_tokens: Option<TokenUsage>,
    last_notice_seq: Option<u64>,
    // 压缩记录（模块 D）：传对象 = 覆盖写入；null/缺省 = 不更新。
    // （tauri command 参数上的 doc 注释不合法，用普通注释）
    compaction: Option<CompactionMeta>,
    // 显式清空 contextTokens（模块 D：压缩归零的持久面——contextTokens=null
    // 与缺省同为 None 无法区分，故独立布尔；压缩完成时前端随 compaction 置 true）
    clear_context_tokens: Option<bool>,
    // 显式清空 lastNoticeSeq（批 4：压缩后游标复位的持久面——复位为“空”才能
    // 走“新对话”分支重发三值快照；None 同样无法表达清空，故独立布尔）
    clear_notice_seq: Option<bool>,
    usage_deltas: Option<Vec<UsageDelta>>,
) -> Result<(), String> {
    let patch = MetaPatch {
        title,
        expected_title,
        context_tokens,
        last_notice_seq,
        compaction,
        context_tokens_clear: clear_context_tokens.unwrap_or(false),
        last_notice_seq_clear: clear_notice_seq.unwrap_or(false),
        usage_deltas: usage_deltas.unwrap_or_default(),
    };
    let mut s = store
        .lock()
        .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    s.update_meta(&conversation_id, &patch)
}

// ── 会话自动标题（生成）───────────────────────────────

/// 生成会话标题（主模型静默 run）：从用户首问 + 首答可见文本生成短标题，
/// CAS 写入 index.json 后推送 `conversation/title` 事件。
///
/// 语义（与前端触发层配合）：
/// - **命令 fire-and-forget**：前端 invoke 不 await 返回值；结果经事件回推
///   （与 append_turn / agent-event 同模式）。
/// - **CAS 三态写入**（`try_set_title_if_expected`）：会话已存在且标题仍为
///   默认 → 写入；会话未落盘（首轮流式期）→ 不写但推送，由前端携带至其后
///   append_turn；用户已改名 → 放弃（不写不推）。
/// - **失败静默降级**：生成失败（无 client / 超时 / 空）→ 直接 Ok(())，
///   保留默认标题，不重试、不报错。
///
/// `expectedTitle` = 前端产出的默认标题哨兵（`deriveTitle(首问)`）——后端
/// 不复算，只做字符串相等比较（规避跨语言截取差异）。
#[tauri::command]
pub async fn generate_conversation_title(
    app_handle: AppHandle,
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    store: State<'_, Arc<Mutex<SessionStore>>>,
    conversation_id: String,
    user_text: String,
    assistant_text: String,
    expected_title: String,
) -> Result<(), String> {
    // turn 起始配置快照（短暂持锁）
    let llm_config = {
        let guard = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        guard.clone()
    };
    let client = llm
        .select(&llm_config.api_type)
        .ok_or_else(|| format!("协议 {:?} 暂未支持", llm_config.api_type))?;

    let title_result = orchestrator::title::generate_title_full(
        &**client,
        &llm_config,
        &user_text,
        &assistant_text,
        orchestrator::title::TITLE_TIMEOUT,
    )
    .await;
    let title = match title_result.title {
        Some(t) => t,
        None => return Ok(()), // 失败静默降级（保留默认标题）
    };
    let title_usage = title_result
        .usage
        .map(|u| usage_stat(&u, llm_config.api_type));

    let outcome = {
        let mut s = store
            .lock()
            .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        s.try_set_title_if_expected(&conversation_id, &title, &expected_title)
    };

    match outcome {
        Ok(TitleWriteOutcome::TitleChanged) => {
            // 用户已改名/已生成过 → 放弃（不推送，避免覆盖用户标题）
            crate::debug!(
                "action" => "title_generation_skipped",
                "conversation_id" => conversation_id,
                "reason" => "title_changed"
            );
            Ok(())
        }
        Ok(outcome) => {
            // Written / NotExists → 推送事件（NotExists 由前端携带至 append_turn）
            let event = IpcEvent::Agent(AgentEvent::conversation_title_with_usage(
                &conversation_id,
                &title,
                title_usage,
            ));
            if let Err(e) = app_handle.emit("agent-event", event) {
                eprintln!("[commands] 标题事件推送失败: {e}");
            }
            if matches!(outcome, TitleWriteOutcome::NotExists) {
                crate::debug!(
                    "action" => "title_generation_not_persisted",
                    "conversation_id" => conversation_id
                );
            }
            Ok(())
        }
        Err(e) => {
            // 写盘错误（非致命）：不发事件（避免前端显示未落盘的标题）
            eprintln!("[commands] 标题写入失败: {e}");
            Ok(())
        }
    }
}

/// 删除会话：先删持久文件（jsonl + index 条目），再清冻结快照 sidecar
/// （记忆区 + 技能目录 + 完整前缀三份，同生共死）。
#[tauri::command]
pub fn delete_conversation(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    snapshot_store: State<'_, Arc<Mutex<SnapshotStore>>>,
    skills_snapshot_store: State<'_, Arc<Mutex<SkillSnapshotStore>>>,
    prefix_snapshot_store: State<'_, Arc<Mutex<PrefixSnapshotStore>>>,
    conversation_id: String,
) -> Result<(), String> {
    // 先删会话（失败上抛，不静默）；再清 sidecar（冻结快照同生共死；
    // 裁决 1：先写快照后归档 → 孤儿在删除侧清扫）。
    {
        let mut s = store
            .lock()
            .map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        s.delete(&conversation_id)?;
    }
    {
        let mut snaps = snapshot_store
            .lock()
            .map_err(|e| format!("快照存储锁获取失败: {e}"))?;
        snaps.remove(&conversation_id);
    }
    let mut snaps = skills_snapshot_store
        .lock()
        .map_err(|e| format!("技能快照存储锁获取失败: {e}"))?;
    snaps.remove(&conversation_id);
    let mut snaps = prefix_snapshot_store
        .lock()
        .map_err(|e| format!("前缀快照存储锁获取失败: {e}"))?;
    snaps.remove(&conversation_id);
    Ok(())
}
