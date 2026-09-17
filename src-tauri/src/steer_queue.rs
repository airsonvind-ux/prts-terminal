//! SteerQueue —— 插话（steer）待注入队列
//!
//! 按 sessionId 分区的 FIFO 队列，承载「生成中提交插话 → assistant 段
//! 完成后注入为轮内下一条 user 消息」的待办（设计定稿 2026-09）：
//!
//! - 键 = session_id（与 TurnRegistry / 前端 processing 快照同键）；
//! - FIFO 逐条消费：dispatch_turn 的 Completed 出口每次 `take_next` 只取
//!   一条（一条完整走完回答段再注入下一条）；
//! - 条目携带会话内单调递增序号 id（cancel_steer 定位用）；
//! - 中止语义：abort_turn 路径 `clear(session_id)` 清空待注入条目
//!   （中止 = 停当前段 + 清插话队列）。
//!
//! 队列空时零行为变化：dispatch 只在 `take_next` 命中时注入，否则走
//! 既有 turn/end 收尾，不感知队列存在。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// 单条待注入插话。
#[derive(Debug, Clone, PartialEq)]
pub struct SteerEntry {
    /// 条目定位（会话内单调递增序号字符串；注入/取消后失效，幂等无害）
    pub id: String,
    /// 待注入文本（入队时已 trim）
    pub text: String,
}

/// 单个 session 的队列分区。
#[derive(Debug, Default)]
struct SteerPartition {
    entries: VecDeque<SteerEntry>,
    /// 会话内序号计数器（注入即离队不回收，无跨会话复用）
    next_seq: u64,
}

/// 按 sessionId 分区的插话 FIFO 队列。
///
/// 内部 `std::sync::Mutex<HashMap<String, SteerPartition>>`：所有方法同步
/// 短持锁（无 await 跨越），消费方（dispatch_turn 同步复查点 / 命令层）
/// 不引入异步等待面。
#[derive(Debug, Default)]
pub struct SteerQueue {
    partitions: Mutex<HashMap<String, SteerPartition>>,
}

impl SteerQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// 入队一条插话，返回条目定位 id。
    ///
    /// 空白文本拒绝入队（返回 None——命令层已前置校验，此处防御）。
    /// 入队不做活动 turn 校验（命令层以 TurnRegistry::is_active 判定，
    /// 本模块只管存储）。
    pub fn enqueue(&self, session_id: &str, text: &str) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut parts = self.partitions.lock().unwrap();
        let part = parts.entry(session_id.to_string()).or_default();
        let seq = part.next_seq;
        part.next_seq += 1;
        let id = seq.to_string();
        part.entries.push_back(SteerEntry {
            id: id.clone(),
            text: text.to_string(),
        });
        Some(id)
    }

    /// FIFO 弹出队首（无条目 → None）。dispatch 注入口消费。
    ///
    /// 弹出后分区为空 → 移除分区（会话不跨轮复用，不留空壳）。
    pub fn take_next(&self, session_id: &str) -> Option<SteerEntry> {
        let mut parts = self.partitions.lock().unwrap();
        let entry = parts.get_mut(session_id)?.entries.pop_front()?;
        if parts
            .get(session_id)
            .is_some_and(|p| p.entries.is_empty())
        {
            parts.remove(session_id);
        }
        Some(entry)
    }

    /// 按条目 id 移除待注入条目（cancel_steer）。
    ///
    /// 返回是否实际移除（不存在/已注入 → false，幂等）。
    pub fn cancel(&self, session_id: &str, entry_id: &str) -> bool {
        let mut parts = self.partitions.lock().unwrap();
        let Some(part) = parts.get_mut(session_id) else {
            return false;
        };
        let before = part.entries.len();
        part.entries.retain(|e| e.id != entry_id);
        let removed = part.entries.len() != before;
        if part.entries.is_empty() {
            parts.remove(session_id);
        }
        removed
    }

    /// 清空某 session 全部待注入条目（abort_turn 路径：中止 = 停段 + 清队列）。
    ///
    /// 返回是否存在该 session 的分区（false = 原本无待注入，幂等）。
    pub fn clear(&self, session_id: &str) -> bool {
        self.partitions.lock().unwrap().remove(session_id).is_some()
    }

    /// 某 session 待注入条数（测试/诊断）。
    pub fn len(&self, session_id: &str) -> usize {
        self.partitions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|p| p.entries.len())
            .unwrap_or(0)
    }

    /// 某 session 无待注入条目。
    #[allow(dead_code)] // 诊断/API 面（当前无生产调用者；len==0 已覆盖消费面）
    pub fn is_empty(&self, session_id: &str) -> bool {
        self.len(session_id) == 0
    }

    /// 某 session 是否含指定条目（测试辅助：cancel 幂等性断言）。
    #[allow(dead_code)] // 仅测试调用（check 非 test 模式不计入）
    pub fn contains(&self, session_id: &str, entry_id: &str) -> bool {
        self.partitions
            .lock()
            .unwrap()
            .get(session_id)
            .is_some_and(|p| p.entries.iter().any(|e| e.id == entry_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_returns_monotonic_ids_and_appends_in_order() {
        let q = SteerQueue::new();
        let a = q.enqueue("s1", "第一条").unwrap();
        let b = q.enqueue("s1", "第二条").unwrap();
        assert_eq!(a, "0");
        assert_eq!(b, "1");
        assert_eq!(q.len("s1"), 2);

        // FIFO 消费顺序 = 入队顺序
        let first = q.take_next("s1").unwrap();
        assert_eq!(first.id, a);
        assert_eq!(first.text, "第一条");
        let second = q.take_next("s1").unwrap();
        assert_eq!(second.id, b);
        assert_eq!(second.text, "第二条");
        assert_eq!(q.take_next("s1"), None, "队列耗尽 → None");
        assert_eq!(q.len("s1"), 0, "分区随空清出");
    }

    #[test]
    fn enqueue_rejects_blank_and_trims_text() {
        let q = SteerQueue::new();
        assert_eq!(q.enqueue("s1", "   "), None, "空白拒绝");
        assert_eq!(q.enqueue("s1", ""), None);
        assert_eq!(q.len("s1"), 0);
        let id = q.enqueue("s1", "  插话内容  ").unwrap();
        assert_eq!(q.take_next("s1").unwrap().text, "插话内容", "入队即 trim");
        let _ = id;
    }

    #[test]
    fn cancel_removes_specific_entry_and_is_idempotent() {
        let q = SteerQueue::new();
        let a = q.enqueue("s1", "甲").unwrap();
        let b = q.enqueue("s1", "乙").unwrap();
        assert!(q.cancel("s1", &a), "移除队首条目");
        assert!(!q.contains("s1", &a));
        assert!(q.contains("s1", &b));
        assert!(!q.cancel("s1", &a), "重复取消幂等 false");
        // 其余条目不受影响（FIFO 仍保序）
        let next = q.take_next("s1").unwrap();
        assert_eq!(next.id, b);
        assert_eq!(q.len("s1"), 0);
    }

    #[test]
    fn cancel_missing_session_or_after_take_returns_false() {
        let q = SteerQueue::new();
        assert!(!q.cancel("nope", "0"));
        let id = q.enqueue("s1", "被消费").unwrap();
        q.take_next("s1");
        assert!(!q.cancel("s1", &id), "已注入（离队）条目不可取消");
    }

    #[test]
    fn clear_wipes_partition_only_for_given_session() {
        let q = SteerQueue::new();
        q.enqueue("s1", "甲");
        q.enqueue("s1", "乙");
        q.enqueue("s2", "丙");
        assert!(q.clear("s1"), "有分区 → true");
        assert_eq!(q.len("s1"), 0);
        assert_eq!(q.len("s2"), 1, "其他 session 不受影响");
        assert!(!q.clear("s1"), "无分区 → false（幂等）");
        assert_eq!(q.take_next("s2").unwrap().text, "丙");
    }

    #[test]
    fn partitions_are_isolated_with_independent_id_sequences() {
        let q = SteerQueue::new();
        let a = q.enqueue("s1", "甲").unwrap();
        let b = q.enqueue("s2", "乙").unwrap();
        assert_eq!(a, "0");
        assert_eq!(b, "0", "各分区独立计数");
        assert_eq!(q.len("s1"), 1);
        assert_eq!(q.len("s2"), 1);
    }

    #[test]
    fn interleaved_take_and_enqueue_keeps_fifo_per_partition() {
        let q = SteerQueue::new();
        let a = q.enqueue("s1", "甲").unwrap();
        let first = q.take_next("s1").unwrap();
        assert_eq!(first.id, a);
        let b = q.enqueue("s1", "乙").unwrap();
        let c = q.enqueue("s1", "丙").unwrap();
        assert_eq!(q.take_next("s1").unwrap().id, b);
        assert_eq!(q.take_next("s1").unwrap().id, c);
        assert_eq!(q.take_next("s1"), None);
    }
}
