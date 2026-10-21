use super::prelude::*;

// ═══════════════════════════════════════════════════════
// 中止当前对话轮（M3-B §2.1）
// ═══════════════════════════════════════════════════════

/// 中止一次活动对话轮次（幂等）。
///
/// - `Ok(true)` = 找到活动 turn 并已置位中止（进程经取消点收敛退出）；
/// - `Ok(false)` = 无该 session 的活动 turn（前端幂等处理，非错误）。
///
/// 命令**只置标志**，不碰 approvals/进程——清理收敛在 orchestrator::process
/// 的 Aborted 出口（clear_session + turn/end(aborted) emit），避免双头清理。
/// 置位后 busy 仍保持（快照未删），直到 process 返回、命令层 unregister。
#[tauri::command]
pub fn abort_turn(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    session_id: String,
) -> Result<bool, String> {
    // 中止语义（2026-09-25 修订）：中止 = 立断当前段；插话队列不再清空——
    // 未交付条目由 agent_process 收尾置保留态，同对话下一轮领取交付
    // （中止不丢字；前端 pending 气泡留存、可点击取消）。
    Ok(turn_registry.abort(&session_id))
}

// ═══════════════════════════════════════════════════════
// 插话（steer）——生成中入队（设计定稿）
// ═══════════════════════════════════════════════════════

/// 生成中提交插话：入队待注入（不打断当前段）。
///
/// - 仅该 session 有活动 turn 时可入队；空闲返回明确拒绝（前端据拒绝文案
///   走「关窗降级」= 退化为普通发送，见 ChatView.handleSteer 同步口径）；
/// - 返回条目定位 id（cancel_steer / 前端 pending 气泡取消用）；
/// - 队列消费（2026-09-25 修订）：dispatch 循环每个 step 的请求发出前领取
///   队首注入为轮内下一条 user 消息（步边界领取——工具步之间不再等回答段
///   收尾）；assistant 段收尾出口复查队列，有留存则续跑、空才发 turn/end。
///   同轮 streaming 不断，turn/end 只在队列空时发一次；
/// - 拒绝文案「当前没有处理中的对话，请等待处理结束后直接发送」是前端
///   关窗降级的判定标记，改动需同步（ChatView.handleSteer）。
#[tauri::command]
pub fn steer_message(
    steer_queue: State<'_, Arc<SteerQueue>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    session_id: String,
    text: String,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err("插话内容不能为空".into());
    }
    if !turn_registry.is_active(&session_id) {
        return Err("当前没有处理中的对话，请等待处理结束后直接发送".into());
    }
    steer_queue
        .enqueue(&session_id, &text)
        .ok_or_else(|| "插话入队失败（内容为空）".into())
}

/// 取消一条待注入插话（pending 气泡点击取消）。
///
/// Ok(true) = 已移除；Ok(false) = 无此条目（已注入 / 已取消 / 会话无队列）——
/// 前端幂等处理（气泡消失即反馈，不回填输入）。
#[tauri::command]
pub fn cancel_steer(
    steer_queue: State<'_, Arc<SteerQueue>>,
    session_id: String,
    entry_id: String,
) -> Result<bool, String> {
    Ok(steer_queue.cancel(&session_id, &entry_id))
}

// ═══════════════════════════════════════════════════════
// 审批决策回传（M3-A §4.3/§5.2：确认卡 → 后端决策通道）
// ═══════════════════════════════════════════════════════

/// 前端确认卡按钮的决策载荷（invoke resolve_tool_approval）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolutionRequest {
    pub session_id: String,
    pub call_id: String,
    /// "approved" | "rejected"（其余值命令层拒绝）
    pub decision: String,
}

/// 用户对一次工具审批的决策回传。
///
/// 返回 true = 决策已送达等待方（agent turn 继续）；false = 无此 pending
/// （已超时 / 已决 / 会话已结束）——前端应幂等处理（按事件终态收起卡片）。
#[tauri::command]
pub fn resolve_tool_approval(
    approval_registry: State<'_, Arc<crate::approval::ApprovalRegistry>>,
    request: ApprovalResolutionRequest,
) -> Result<bool, String> {
    let decision = match request.decision.as_str() {
        "approved" => crate::approval::UserDecision::Approved,
        "rejected" => crate::approval::UserDecision::Rejected,
        other => return Err(format!("decision 仅支持 approved/rejected，got: {other}")),
    };
    Ok(approval_registry.resolve(&request.session_id, &request.call_id, decision))
}
