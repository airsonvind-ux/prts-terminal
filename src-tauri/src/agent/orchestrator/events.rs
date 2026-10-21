//! 事件双写通道：同一事实 → IPC 事件 + 会话日志。

use std::sync::{Arc, Mutex};

use crate::events::{AgentEvent, IpcEvent};
use crate::llm::ToolDef;
use crate::session::{SessionEvent, SessionLog};

use super::AgentPhase;

/// 事件双写通道（前端 = log 的投影缓存）。
///
/// IPC 推 `IpcEvent`（SessionEvent 持久事实 + Phase 瞬时信号），
/// 与 session log 双写同一事实：
///
/// - `emit(...)`：持久事实 → 先入 log 再推 IPC（前端投影缓存消费）
/// - `ipc_only(...)`：仅推 IPC（流式 tool/call：实时卡片，log 由执行阶段批量写）
/// - `log_only(...)`：仅写 log（step/start、request/header、批量 tool/call）
/// - `phase(...)`：仅推 IPC（Phase 是纯 UI 信号，不入 log）
///
/// 写 log 先于推 IPC：事实落账优先，IPC 失败不丢账。
#[derive(Clone)]
pub(super) struct EventSink {
    pub(super) log: Arc<Mutex<SessionLog>>,
    emit: Arc<dyn Fn(IpcEvent) + Send + Sync>,
}

impl EventSink {
    pub(super) fn new(
        log: Arc<Mutex<SessionLog>>,
        emit: impl Fn(IpcEvent) + Send + Sync + 'static,
    ) -> Self {
        Self {
            log,
            emit: Arc::new(emit),
        }
    }

    /// 持久事实双写：先入 log，再推 IPC。
    pub(super) fn emit(&self, event: SessionEvent) {
        self.log.lock().unwrap().append(event.clone());
        (self.emit)(IpcEvent::Session(event));
    }

    /// 仅推 IPC（不写 log）。
    pub(super) fn ipc_only(&self, event: SessionEvent) {
        (self.emit)(IpcEvent::Session(event));
    }

    /// 仅写 log（不推 IPC）。
    pub(super) fn log_only(&self, event: SessionEvent) {
        self.log.lock().unwrap().append(event);
    }

    /// 追加 request/header 快照（仅变化时真正追加）。
    pub(super) fn push_header(&self, session_id: &str, system_prompt: &str, tool_defs: &[ToolDef]) {
        self.log
            .lock()
            .unwrap()
            .push_header(session_id, system_prompt, tool_defs);
    }

    /// 推 Phase 瞬时信号（仅 IPC）。
    pub(super) fn phase(&self, session_id: &str, phase: AgentPhase) {
        (self.emit)(IpcEvent::Agent(AgentEvent::phase(session_id, phase)));
    }
}
