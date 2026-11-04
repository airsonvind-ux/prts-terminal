//! conversation_scope —— 本次轮次的会话归属
//!
//! 前端每轮会生成一个新的 `sessionId`（事件路由与 processing 字典的键），
//! 而落盘目录（`session-files/<会话id>/`）必须用**跨轮稳定**的
//! `conversationId`。两者在 `agent_process` 入参里同时存在，但调度循环
//! （`dispatch_turn_with_memory`）只拿到 `sessionId`；为了不改动那条被
//! 二十余处测试调用的签名，这里放一张进程级的短命映射：轮次开始时登记、
//! 轮次收尾时清理（由 `TurnGuard` 负责）。
//!
//! 查不到时调用方回退到 `sessionId`——测试与旧前端（不带 conversationId）
//! 仍按旧口径落盘，不会因为缺映射直接失败。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn table() -> &'static Mutex<HashMap<String, String>> {
    static TABLE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记本轮 `sessionId` 对应的会话 id（空串不登记）。
pub fn register(session_id: &str, conversation_id: &str) {
    if conversation_id.is_empty() {
        return;
    }
    let mut guard = match table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(session_id.to_string(), conversation_id.to_string());
}

/// 取本轮所属的会话 id。
pub fn of(session_id: &str) -> Option<String> {
    let guard = match table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.get(session_id).cloned()
}

/// 轮次收尾清理（幂等）。
pub fn clear(session_id: &str) {
    let mut guard = match table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.remove(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_lookup_and_clear() {
        register("s-1", "conv-1");
        assert_eq!(of("s-1").as_deref(), Some("conv-1"));
        assert_eq!(of("s-2"), None);
        clear("s-1");
        assert_eq!(of("s-1"), None);
    }

    #[test]
    fn empty_conversation_is_not_registered() {
        register("s-3", "");
        assert_eq!(of("s-3"), None);
    }
}
