//! taint —— 单轮工具调用的来源状态（R-02b）
//!
//! 只记录两类事实：
//! - `external_content`：本轮成功读过外部资料（web_fetch / web_search）；
//! - `outside_read`：本轮成功读过工作区外内容（文件工具 / run_command 审计位）。
//!
//! 状态绑定 `ToolContext`，每次 `process_with_memory` 新建，跨轮自动重置。
//! 用原子布尔而非锁：并行工具执行线程可能同时写入，只需要“是否发生过”的
//! 单调事实，不需要顺序。

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Default)]
pub struct TaintState {
    external_content: AtomicBool,
    outside_read: AtomicBool,
}

impl TaintState {
    pub fn mark_external_content(&self) {
        self.external_content.store(true, Ordering::SeqCst);
    }

    pub fn mark_outside_read(&self) {
        self.outside_read.store(true, Ordering::SeqCst);
    }

    pub fn has_external_content(&self) -> bool {
        self.external_content.load(Ordering::SeqCst)
    }

    pub fn has_outside_read(&self) -> bool {
        self.outside_read.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_are_monotonic() {
        let t = TaintState::default();
        assert!(!t.has_external_content());
        assert!(!t.has_outside_read());
        t.mark_external_content();
        t.mark_external_content();
        assert!(t.has_external_content());
        assert!(!t.has_outside_read());
        t.mark_outside_read();
        assert!(t.has_outside_read());
    }
}
