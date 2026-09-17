//! approval —— 工具审批注册表（M3-A §4.3）
//!
//! 职责：把"工具调用期间的用户输入"接到 agent turn 的异步等待上。
//! orchestrator 预检通道（preflight）在需要询问时 `register` 取得
//! oneshot receiver 并 await；前端确认/拒绝经 `resolve_tool_approval`
//! 命令调用 `resolve` 向对应通道发送决策。
//!
//! 键 = (session_id, call_id)——turn 级唯一。等待方超时（300s）/通道
//! drop（会话销毁）统一按 Rejected 处理（调用方负责，本模块只保证
//! `resolve` 对已消失条目幂等返回 false）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 审批等待超时（M3-A 数值定稿：300s；超时按 Rejected 处理，turn 不卡死）。
pub const APPROVAL_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

/// 用户对一次审批的决策（resolve_tool_approval 的决策载荷；
/// 序列化形态 = 前端 invoke 的 decision 字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserDecision {
    Approved,
    Rejected,
}

/// 待决审批条目。
/// call_id / created_at 供诊断方法（pending_for_session / age_of）与
/// M3-B 中止清理使用；非测试构建无生产读者，标注 allow(dead_code)。
struct PendingApproval {
    #[allow(dead_code)]
    pub call_id: String,
    #[allow(dead_code)]
    pub created_at: Instant,
    pub tx: tokio::sync::oneshot::Sender<UserDecision>,
}

/// 审批注册表：session × call_id → oneshot 发送端。
#[derive(Default)]
pub struct ApprovalRegistry {
    pending: Mutex<HashMap<(String, String), PendingApproval>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一次待决审批，返回等待端（receiver）。
    /// 同一 (session, call_id) 重复注册：旧条目被替换（旧 receiver 收到
    /// Canceled → 调用方按 Rejected 处理——防御性清理）。
    pub fn register(
        &self,
        session_id: &str,
        call_id: &str,
    ) -> tokio::sync::oneshot::Receiver<UserDecision> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut pending = self.pending.lock().unwrap();
        pending.insert(
            (session_id.to_string(), call_id.to_string()),
            PendingApproval {
                call_id: call_id.to_string(),
                created_at: Instant::now(),
                tx,
            },
        );
        rx
    }

    /// 用户决策回传（前端 resolve_tool_approval 落点）。
    /// 返回 true = 决策已送达等待方；false = 无此 pending（已超时/已决/会话
    /// 已结束），调用方应幂等处理（前端按事件终态收起卡片）。
    pub fn resolve(&self, session_id: &str, call_id: &str, decision: UserDecision) -> bool {
        let entry = {
            let mut pending = self.pending.lock().unwrap();
            pending.remove(&(session_id.to_string(), call_id.to_string()))
        };
        match entry {
            Some(entry) => entry.tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// 清理某会话的全部待决审批（turn 异常终止 / M3-B 中止时调用；
    /// receiver 收到 Canceled，等待方按 Rejected 兜底）。
    /// M3-B 落地前无生产调用者：标注 allow(dead_code)（测试覆盖语义）。
    #[allow(dead_code)]
    pub fn clear_session(&self, session_id: &str) -> usize {
        let mut pending = self.pending.lock().unwrap();
        let keys: Vec<(String, String)> = pending
            .keys()
            .filter(|(sid, _)| sid == session_id)
            .cloned()
            .collect();
        let mut cleared = 0;
        for key in keys {
            if let Some(entry) = pending.remove(&key) {
                // drop sender → receiver Err(Canceled)
                drop(entry.tx);
                cleared += 1;
            }
        }
        cleared
    }

    /// 当前待决条目数（测试/诊断）。
    #[allow(dead_code)]
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// 指定会话的待决审批 call_id 列表（诊断）。
    #[allow(dead_code)]
    pub fn pending_for_session(&self, session_id: &str) -> Vec<String> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .filter(|((sid, _), _)| sid == session_id)
            .map(|((_, _), entry)| entry.call_id.clone())
            .collect()
    }

    /// 条目已存在时长（诊断）。
    #[allow(dead_code)]
    pub fn age_of(&self, session_id: &str, call_id: &str) -> Option<Duration> {
        self.pending
            .lock()
            .unwrap()
            .get(&(session_id.to_string(), call_id.to_string()))
            .map(|e| e.created_at.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_resolve_delivers_decision() {
        let reg = ApprovalRegistry::new();
        let mut rx = reg.register("s1", "call_1");
        assert_eq!(reg.pending_count(), 1);
        assert_eq!(reg.pending_for_session("s1"), vec!["call_1".to_string()]);

        let ok = reg.resolve("s1", "call_1", UserDecision::Approved);
        assert!(ok, "决策应送达");
        assert_eq!(reg.pending_count(), 0, "决策后条目应移除");

        let decision = rx.try_recv().expect("receiver 应收到决策");
        assert_eq!(decision, UserDecision::Approved);
    }

    #[test]
    fn resolve_unknown_pending_returns_false_idempotent() {
        let reg = ApprovalRegistry::new();
        assert!(!reg.resolve("s1", "nope", UserDecision::Rejected));
        assert!(!reg.resolve("s2", "call_1", UserDecision::Rejected));
        // 双 resolve（第一次成功后的重复决策）幂等 false。
        // rx 需存活到 resolve 之后（send 送达要求等待方存在——
        // 生产路径 preflight await 持有 receiver）。
        let mut rx = reg.register("s1", "call_1");
        assert!(reg.resolve("s1", "call_1", UserDecision::Rejected));
        assert!(!reg.resolve("s1", "call_1", UserDecision::Rejected));
        assert_eq!(rx.try_recv().expect("决策应送达"), UserDecision::Rejected);
    }

    #[test]
    fn sessions_are_isolated_by_key() {
        let reg = ApprovalRegistry::new();
        reg.register("s1", "call_1");
        reg.register("s2", "call_1");
        assert_eq!(reg.pending_count(), 2);
        // 只清 s1
        let cleared = reg.clear_session("s1");
        assert_eq!(cleared, 1);
        assert_eq!(reg.pending_count(), 1);
        assert_eq!(reg.pending_for_session("s2"), vec!["call_1".to_string()]);
        // s1 的 receiver 收到 Canceled（drop 语义）
    }

    #[test]
    fn drop_without_resolve_cancels_receiver() {
        let reg = ApprovalRegistry::new();
        let mut rx = reg.register("s1", "call_1");
        reg.clear_session("s1");
        let err = rx.try_recv().expect_err("drop 后 receiver 应收到 Canceled");
        match err {
            tokio::sync::oneshot::error::TryRecvError::Closed => {}
            other => panic!("应 Closed，got {other:?}"),
        }
    }

    #[tokio::test]
    async fn re_register_replaces_old_entry() {
        let reg = ApprovalRegistry::new();
        let mut old_rx = reg.register("s1", "call_1");
        let new_rx = reg.register("s1", "call_1");
        // 旧 receiver 被替换 → Canceled
        assert!(old_rx.await.is_err());
        assert!(reg.resolve("s1", "call_1", UserDecision::Approved));
        assert_eq!(new_rx.await, Ok(UserDecision::Approved));
    }
}
