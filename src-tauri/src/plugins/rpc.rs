//! 请求 / 应答配对与协议错误码（M2b）。
//!
//! 一条管道上同时跑着宿主的请求（`tool.call`）与插件的请求（`host.call`），
//! 两者都靠 `id` 配对。这里只做「发出请求时领一个槽位、收到应答时按 id 唤醒」，
//! 不碰线程模型——并发规则写在 [`crate::plugins::process`] 的读线程里（R3）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use serde_json::{json, Value};

/// 协议级：请求不合法（缺 method、params 形状不对）。
pub const INVALID_REQUEST: i64 = -32600;
/// 协议级：方法不认识。
pub const METHOD_NOT_FOUND: i64 = -32601;
/// 协议级：参数不合法。
pub const INVALID_PARAMS: i64 = -32602;
/// 能力未授权。
pub const CAPABILITY_DENIED: i64 = -32000;
/// 调用超时。
pub const TIMEOUT: i64 = -32001;
/// 插件内部故障（崩溃、非帧输出、写管道失败）。
pub const PLUGIN_FAULT: i64 = -32010;

/// 一次 RPC 失败。`message` 要能被模型直接读懂（规格 §5：错误文案质量）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }

    /// 从插件的 `{code, message}` 还原；缺字段按协议错误处理。
    pub fn from_json(value: &Value) -> Self {
        let code = value["code"].as_i64().unwrap_or(INVALID_REQUEST);
        let message = value["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("插件返回的错误没有 message：{value}"));
        Self { code, message }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}（{}）", self.message, self.code)
    }
}

/// 一次 RPC 的结果。
pub type RpcResult = Result<Value, RpcError>;

/// 在途请求的槽位表。
///
/// `register` 与 `resolve` 之间没有竞态：发出方先领槽位再写管道，读线程不可能
/// 先看到应答。
pub struct Pending {
    next_id: AtomicU64,
    slots: Mutex<HashMap<u64, Sender<RpcResult>>>,
}

impl Pending {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// 领一个 id 与它的接收端。
    pub fn register(&self) -> (u64, Receiver<RpcResult>) {
        let (sender, receiver) = channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.slots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id, sender);
        (id, receiver)
    }

    /// 按 id 唤醒等待者；没人在等（已超时、或 id 未知）时返回 false。
    pub fn resolve(&self, id: u64, outcome: RpcResult) -> bool {
        let slot = self
            .slots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&id);
        match slot {
            Some(sender) => sender.send(outcome).is_ok(),
            None => false,
        }
    }

    /// 通道断开时把所有等待者叫醒，避免每个调用都干等到超时。
    pub fn fail_all(&self, error: &RpcError) {
        let slots = std::mem::take(
            &mut *self
                .slots
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
        for (_, sender) in slots {
            let _ = sender.send(Err(error.clone()));
        }
    }

    /// 当前在途请求数（测试与诊断用）。
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
    }
}

impl Default for Pending {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn resolve_wakes_the_waiter() {
        let pending = Pending::new();
        let (id, receiver) = pending.register();
        assert!(pending.resolve(id, Ok(json!({ "ok": true }))));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            Ok(json!({ "ok": true }))
        );
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn ids_are_unique_and_start_at_one() {
        let pending = Pending::new();
        let (first, _a) = pending.register();
        let (second, _b) = pending.register();
        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_ne!(first, second);
    }

    #[test]
    fn resolving_an_unknown_id_is_a_no_op() {
        let pending = Pending::new();
        assert!(!pending.resolve(42, Ok(json!(null))));
    }

    #[test]
    fn fail_all_wakes_every_waiter() {
        let pending = Pending::new();
        let (_a, first) = pending.register();
        let (_b, second) = pending.register();

        let error = RpcError::new(PLUGIN_FAULT, "插件进程退出了");
        pending.fail_all(&error);

        assert_eq!(
            first.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(error.clone())
        );
        assert_eq!(
            second.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(error)
        );
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn errors_round_trip_through_json() {
        let error = RpcError::new(TIMEOUT, "60s 内没有应答");
        assert_eq!(RpcError::from_json(&error.to_json()), error);
        assert_eq!(
            RpcError::from_json(&json!({ "message": "没有 code" })),
            RpcError::new(INVALID_REQUEST, "没有 code")
        );
    }
}
