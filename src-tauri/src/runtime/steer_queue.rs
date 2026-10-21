//! SteerQueue —— 插话（steer）待注入队列
//!
//! 按 sessionId 分区的 FIFO 队列，承载「生成中提交插话 → 步边界注入为
//! 轮内下一条 user 消息」的待办（2026-09 定稿；2026-09-25 步边界领取修订）：
//!
//! - 键 = session_id（与 TurnRegistry / 前端 processing 快照同键）；
//! - FIFO 逐条消费：dispatch_turn 每个 step 的请求前 `take_next` 一条
//!   （步边界领取——工具步之间也不再等回答段收尾；一条走完其回答段
//!   再注入下一条）；
//! - 条目携带会话内单调递增序号 id（cancel_steer 定位用）；
//! - 中止/错误语义（2026-09-25 修订）：不再随中止清队列——命令层收尾把
//!   未交付条目置保留态（`preserve`，归属 conversationId），同对话下一轮
//!   `adopt` 领取合并（中止不丢字，对齐上游 keepInbox）。
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
    /// 保留标记（2026-09-25）：非空 = 该分区在中止/错误轮收尾后留存，
    /// 值 = 归属 conversationId；同对话下一轮 adopt 时领取合并。
    preserved_for: Option<String>,
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
        if parts.get(session_id).is_some_and(|p| p.entries.is_empty()) {
            parts.remove(session_id);
        }
        Some(entry)
    }

    /// 置保留态（命令层收尾调用，2026-09-25）：该分区在中止/错误轮结束后
    /// 留存，值 = 归属 conversationId。保留期条目仍以原 session_id 寻址
    /// （cancel_steer 可继续取消）；同对话下一轮 `adopt` 领取合并。
    ///
    /// 返回是否实际标记（无此分区/已空 → false，幂等）。
    pub fn preserve(&self, session_id: &str, conversation_id: &str) -> bool {
        let mut parts = self.partitions.lock().unwrap();
        match parts.get_mut(session_id) {
            Some(part) if !part.entries.is_empty() => {
                part.preserved_for = Some(conversation_id.to_string());
                true
            }
            _ => false,
        }
    }

    /// 领取某 conversation 的全部保留分区，合并进新轮 session 分区。
    ///
    /// FIFO：保留条目（先入队）排在新轮既有条目之前；源分区随后清出。
    /// 同一会话同时存在多个保留分区在实战中不可达（任一后续轮次先
    /// adopt 再可能 preserve），此处置于防御——此时合并顺序未定义。
    ///
    /// 返回合并的条目数（无保留分区 → 0，幂等）。
    pub fn adopt(&self, conversation_id: &str, into_session_id: &str) -> usize {
        let mut parts = self.partitions.lock().unwrap();
        let sources: Vec<String> = parts
            .iter()
            .filter(|(_, p)| p.preserved_for.as_deref() == Some(conversation_id))
            .map(|(sid, _)| sid.clone())
            .collect();
        if sources.is_empty() {
            return 0;
        }
        let mut retained: VecDeque<SteerEntry> = VecDeque::new();
        for sid in &sources {
            if let Some(part) = parts.remove(sid) {
                retained.extend(part.entries);
            }
        }
        let count = retained.len();
        if count == 0 {
            return 0;
        }
        let target = parts.entry(into_session_id.to_string()).or_default();
        // 保留条目（旧）在前：倒序 push_front；目标既有条目自然落其后
        while let Some(entry) = retained.pop_back() {
            target.entries.push_front(entry);
        }
        count
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

    /// 清空某 session 全部待注入条目（命令层兜底：无 conversation 归属的
    /// 残留清理；2026-09-25 修订后不再用于中止路径——中止走 preserve）。
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

    #[test]
    fn preserve_marks_partition_and_keeps_entries_addressable() {
        let q = SteerQueue::new();
        let a = q.enqueue("s1", "甲").unwrap();
        assert!(q.preserve("s1", "conv-1"), "有分区有条目 → 标记成功");
        assert_eq!(q.len("s1"), 1, "保留不移动条目");
        assert!(q.contains("s1", &a), "保留期仍可按原 session 寻址");
        assert!(q.cancel("s1", &a), "保留期 cancel 仍可用");
        assert_eq!(q.len("s1"), 0);
    }

    #[test]
    fn preserve_without_partition_or_after_drain_is_false() {
        let q = SteerQueue::new();
        assert!(!q.preserve("nope", "conv-1"), "无分区 → false");
        q.enqueue("s1", "甲");
        q.take_next("s1");
        assert!(!q.preserve("s1", "conv-1"), "空分区（已清出）→ false");
    }

    #[test]
    fn adopt_merges_retained_entries_before_existing_fifo() {
        let q = SteerQueue::new();
        q.enqueue("s1", "旧甲");
        q.enqueue("s1", "旧乙");
        assert!(q.preserve("s1", "conv-1"));
        // 目标分区已有新条目（极限窗口：adopt 前新轮已入队）
        q.enqueue("s2", "新丙");
        assert_eq!(q.adopt("conv-1", "s2"), 2, "合并两条保留条目");
        assert_eq!(q.len("s1"), 0, "保留分区随合并清出");
        assert_eq!(
            q.take_next("s2").unwrap().text,
            "旧甲",
            "保留条目（旧）在前"
        );
        assert_eq!(q.take_next("s2").unwrap().text, "旧乙");
        assert_eq!(q.take_next("s2").unwrap().text, "新丙", "既有条目保序在后");
    }

    #[test]
    fn adopt_is_scoped_and_idempotent() {
        let q = SteerQueue::new();
        q.enqueue("s1", "甲");
        assert!(q.preserve("s1", "conv-1"));
        q.enqueue("s9", "别家");
        assert_eq!(q.adopt("conv-2", "s2"), 0, "不串对话");
        assert_eq!(q.adopt("conv-1", "s2"), 1);
        assert_eq!(q.adopt("conv-1", "s3"), 0, "重复 adopt 幂等");
        assert_eq!(q.len("s9"), 1, "他会话不受影响");
        assert_eq!(q.take_next("s2").unwrap().text, "甲");
    }
}
