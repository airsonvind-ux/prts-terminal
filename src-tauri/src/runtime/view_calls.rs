//! ViewCallQueue —— 视图发起的工具调用待执行队列
//!
//! 视图（沙箱 iframe）里的 `call` 原语不能自己执行工具：它只能把「想调谁、
//! 带什么参数」交给宿主，由宿主在**当前轮次的步边界**执行，这样结果才能和
//! 模型发起的调用一样进上下文、进过程卡。
//!
//! 语义（与 SteerQueue 对齐）：
//! - 按 session_id 分区，FIFO 每步只领一条；领取点在插话领取**之后**——
//!   用户手打的字永远排在视图点击之前；
//! - 只在活动轮次里有效：没人在跑时命令层直接拒绝，让前端退化成草稿；
//! - 插件停用/卸载、轮次收尾都要清队列，否则会冒出「来源已不存在」的调用。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// 一条待执行的视图调用。
#[derive(Debug, Clone, PartialEq)]
pub struct ViewCall {
    /// 事件里的 callId（`view-<n>`，与模型生成的 id 不会撞）。
    pub id: String,
    pub tool: String,
    /// JSON 文本，直接喂 `ToolRegistry::execute`。
    pub args: String,
    /// 来源视图标题，事件与上下文里都要标出来。
    pub view_title: String,
}

#[derive(Debug, Default)]
struct Partition {
    entries: VecDeque<ViewCall>,
}

/// 进程级队列。视图调用只活在活动轮次里，单例足够，不必逐层透传。
#[derive(Debug, Default)]
pub struct ViewCallQueue {
    partitions: Mutex<HashMap<String, Partition>>,
    next_id: AtomicU64,
}

impl ViewCallQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// 入队一条，返回 callId。
    pub fn push(&self, session_id: &str, tool: &str, args: &str, view_title: &str) -> String {
        let id = format!("view-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let mut partitions = self.partitions.lock().unwrap();
        partitions
            .entry(session_id.to_string())
            .or_default()
            .entries
            .push_back(ViewCall {
                id: id.clone(),
                tool: tool.to_string(),
                args: args.to_string(),
                view_title: view_title.to_string(),
            });
        id
    }

    /// 领取队首（每步一条）。
    pub fn take_next(&self, session_id: &str) -> Option<ViewCall> {
        let mut partitions = self.partitions.lock().unwrap();
        let entry = partitions.get_mut(session_id)?.entries.pop_front();
        if partitions
            .get(session_id)
            .is_some_and(|partition| partition.entries.is_empty())
        {
            partitions.remove(session_id);
        }
        entry
    }

    /// 丢弃某会话还没领取的调用（轮次收尾）。
    pub fn clear(&self, session_id: &str) {
        self.partitions.lock().unwrap().remove(session_id);
    }

    /// 丢弃全部（插件停用/卸载时，来源已经不在了）。
    pub fn clear_all(&self) {
        self.partitions.lock().unwrap().clear();
    }

    /// 仅测试与诊断用（生产路径不按长度做判断）。
    #[allow(dead_code)]
    pub fn len(&self, session_id: &str) -> usize {
        self.partitions
            .lock()
            .unwrap()
            .get(session_id)
            .map_or(0, |partition| partition.entries.len())
    }

    #[allow(dead_code)]
    pub fn is_empty(&self, session_id: &str) -> bool {
        self.len(session_id) == 0
    }
}

static SHARED: OnceLock<ViewCallQueue> = OnceLock::new();

/// 进程级单例（命令层入队、turn loop 领取）。
pub fn queue() -> &'static ViewCallQueue {
    SHARED.get_or_init(ViewCallQueue::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_per_session_and_ids_are_unique() {
        let q = ViewCallQueue::new();
        let a = q.push("s1", "map_locate", "{\"x\":1}", "地图");
        let b = q.push("s1", "map_locate", "{\"x\":2}", "地图");
        q.push("s2", "other", "{}", "别的视图");
        assert_ne!(a, b);
        assert_eq!(q.len("s1"), 2);
        assert_eq!(q.take_next("s1").unwrap().id, a);
        assert_eq!(q.take_next("s1").unwrap().id, b);
        assert!(q.take_next("s1").is_none());
        assert_eq!(q.len("s2"), 1, "会话之间互不影响");
    }

    #[test]
    fn clear_and_clear_all_drop_pending_calls() {
        let q = ViewCallQueue::new();
        q.push("s1", "t", "{}", "视图");
        q.push("s2", "t", "{}", "视图");
        q.clear("s1");
        assert!(q.is_empty("s1"));
        assert_eq!(q.len("s2"), 1);
        q.clear_all();
        assert!(q.is_empty("s2"));
    }
}
