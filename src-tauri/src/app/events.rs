// IPC 事件协议 —— 后端到前端的结构化事件推送
//
// 架构升级 · 模块 1：事件拆两平面。
// - 持久事实（turn/start|end、user/message、assistant/chunk、
//   assistant/message、tool/call、tool/result、request/header、error）
//   由 `session.rs` 的 `SessionEvent` 承载——它是后端 session log 的
//   事实事件，同时作为 IPC 载荷推送给前端（前端 = log 的投影缓存）。
// - 本文件的 `AgentEvent` 仅剩 **phase**（瞬时 UI 信号，不入 log）。
//
// 前端 `src/types.ts` 的 AgentEvent（Discriminated Union）与
// SessionEvent + Phase 逐条对齐。
// 序列化使用 serde(tag = "type")，产生前端期望的 { "type": "...", ... } JSON。
//
// 注意：serde 的 internally tagged enum 不支持 enum 级别的 rename_all 对 variant
// 内部字段生效——必须在每个 variant 上单独声明 #[serde(rename_all = "camelCase")]。
// 确认：本文件末尾有序列化测试验证 session_id → sessionId。

use serde::Serialize;

/// 后端推送的瞬时 UI 信号（唯一保留在 AgentEvent 中的事件）。
///
/// 所有事件均携带 session_id，用于前端多对话路由。
/// 事件路由不依赖全局状态：useAgentEvents 读 event.sessionId
/// 直接写入 processing[event.sessionId]，与当前活跃对话完全解耦。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    /// 模式切换：Agent 处理流程的当前阶段
    ///
    /// reasoning  = LLM 思考中（前端渲染思维链面板）
    /// executing  = 调用工具中（前端渲染工具卡片）
    /// generating = 生成最终回答（前端进入打字机流式输出状态）
    #[serde(rename = "phase", rename_all = "camelCase")]
    Phase {
        session_id: String,
        phase: AgentPhase,
    },

    /// 会话标题更新（后端生成标题后推送；前端刷新显示）。
    ///
    /// **非 session log 事实**——标题是会话级元数据，不入 `SessionLog`；
    /// 放 `AgentEvent`（瞬时/旁路信号平面）而非 `SessionEvent`，避免污染
    /// `derive_messages`。携带 `conversation_id`（非 session_id）——turn 快照
    /// 在轮末删除，session_id 无法路由；标题可能在轮末之后到达。
    #[serde(rename = "conversation/title", rename_all = "camelCase")]
    ConversationTitle {
        conversation_id: String,
        title: String,
        /// 本次标题生成用量（统计增量；前端未归档时携带至下次 append_turn）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<crate::llm::UsageStat>,
    },

    /// 旧对话的冻结前缀与当前应用版本/工具面不一致（R-08）。
    /// 旁路信号：不改变本轮前缀，只告知用户这个对话仍基于旧快照。
    #[serde(rename = "conversation/snapshot-stale", rename_all = "camelCase")]
    ConversationSnapshotStale {
        conversation_id: String,
        /// 快照创建时的应用版本（旧快照可能为空）。
        app_version: String,
    },

    /// 记忆管线用量（后端内部累加后推送；前端仅镜像进投影用于展示，不回写）。
    /// 与 conversation/title 同属旁路信号平面（非 session log 事实）。
    #[serde(rename = "memory/usage", rename_all = "camelCase")]
    MemoryUsage {
        conversation_id: String,
        usage: crate::llm::UsageStat,
    },
}

/// Agent 处理阶段
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum AgentPhase {
    #[serde(rename = "reasoning")]
    Reasoning,
    #[serde(rename = "executing")]
    Executing,
    #[serde(rename = "generating")]
    Generating,
}

// ─── 构造器 ──────────────────────────────────────────

impl AgentEvent {
    pub fn phase(session_id: &str, phase: AgentPhase) -> Self {
        AgentEvent::Phase {
            session_id: session_id.to_string(),
            phase,
        }
    }

    /// 构造会话标题更新事件（`conversation/title`）。
    #[allow(dead_code)] // 当前仅测试引用（2 参旧构造器；生产走 conversation_title_with_usage）
    pub fn conversation_title(conversation_id: &str, title: &str) -> Self {
        AgentEvent::ConversationTitle {
            conversation_id: conversation_id.to_string(),
            title: title.to_string(),
            usage: None,
        }
    }

    /// 构造会话标题更新事件（携带本次标题生成用量）。
    pub fn conversation_title_with_usage(
        conversation_id: &str,
        title: &str,
        usage: Option<crate::llm::UsageStat>,
    ) -> Self {
        AgentEvent::ConversationTitle {
            conversation_id: conversation_id.to_string(),
            title: title.to_string(),
            usage,
        }
    }

    /// 构造旧快照提示事件。
    pub fn conversation_snapshot_stale(conversation_id: &str, app_version: &str) -> Self {
        AgentEvent::ConversationSnapshotStale {
            conversation_id: conversation_id.to_string(),
            app_version: app_version.to_string(),
        }
    }

    /// 构造记忆用量事件（`memory/usage`）。
    pub fn memory_usage(conversation_id: &str, usage: crate::llm::UsageStat) -> Self {
        AgentEvent::MemoryUsage {
            conversation_id: conversation_id.to_string(),
            usage,
        }
    }
}

/// IPC 通道载荷：持久事实（SessionEvent）+ 瞬时 UI 信号（AgentEvent::Phase）。
///
/// 两者都以 `type` 判别（"turn/start"… 与 "phase"），untagged 序列化
/// 直接产出前端期望的 Discriminated Union JSON。
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum IpcEvent {
    /// 持久事实（session log 的事件镜像，前端投影缓存消费）
    Session(SessionEvent),
    /// 瞬时 UI 信号（仅 phase）
    Agent(AgentEvent),
}

use crate::session::SessionEvent;

/// 前端传入的单条历史消息。
///
/// 用于 agent_process 命令的 history 参数反序列化。
/// 注意：role: "error" 的消息仅在前端 UI 渲染，不传给 LLM——
/// session log 的播种（seed_from_history）负责过滤。
/// reasoning_content / tool_calls / tool_call_id 为可选字段：
/// 历史播种时以事件形式（assistant/chunk、tool/call）入 log，
/// 多轮穿透由 derive_messages 从 log 重建，不再需要前端透传。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendMessage {
    pub role: String, // "user" | "assistant" | "error"
    pub content: String,
    /// 推理模型的思维链内容（DeepSeek thinking mode），serde default = None
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// assistant 消息携带的工具调用请求（历史播种）
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// tool 消息携带的 tool_call_id（工具结果匹配）
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// user 消息附带的附件引用（不含字节）。serde default = 空（旧前端不传兼容）。
    #[serde(default)]
    pub attachments: Vec<crate::attachments::Attachment>,
}

use crate::llm::ToolCall;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_serializes_camelcase() {
        let event = AgentEvent::phase("abc123", AgentPhase::Reasoning);
        let json = serde_json::to_string(&event).unwrap();
        println!("Serialized: {json}");
        assert!(
            json.contains("\"sessionId\":\"abc123\""),
            "expected sessionId, got: {json}"
        );
        assert!(
            json.contains("\"type\":\"phase\""),
            "expected type=phase, got: {json}"
        );
        assert!(
            json.contains("\"phase\":\"reasoning\""),
            "expected phase=reasoning, got: {json}"
        );
    }

    #[test]
    fn phase_generating_serializes() {
        let event = AgentEvent::phase("abc123", AgentPhase::Generating);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"phase\":\"generating\""), "got: {json}");
    }

    #[test]
    fn conversation_title_serializes_camelcase() {
        let event = AgentEvent::conversation_title("conv-1", "伤害计算");
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"type\":\"conversation/title\""),
            "got: {json}"
        );
        assert!(
            json.contains("\"conversationId\":\"conv-1\""),
            "got: {json}"
        );
        assert!(json.contains("\"title\":\"伤害计算\""), "got: {json}");
        // 会话/标题事件可经 IpcEvent 平面序列化
        let ipc = IpcEvent::Agent(event);
        let ipc_json = serde_json::to_string(&ipc).unwrap();
        assert!(ipc_json.contains("conversation/title"), "got: {ipc_json}");
    }

    #[test]
    fn frontend_message_defaults_new_fields() {
        // 旧前端不传 reasoning_content / tool_calls / tool_call_id 时应正常反序列化
        let json = r#"{"role":"user","content":"你好"}"#;
        let msg: FrontendMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "你好");
        assert!(msg.reasoning_content.is_none());
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn frontend_message_deserializes_tool_calls_camelcase() {
        let json = r#"{
            "role": "assistant",
            "content": "",
            "toolCalls": [{"id":"call_1","function":{"name":"echo","arguments":"{}"}}],
            "reasoningContent": "让我用工具...",
            "toolCallId": null
        }"#;
        let msg: FrontendMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "assistant");
        assert!(msg.tool_calls.is_some());
        let tcs = msg.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_1");
        assert_eq!(tcs[0].function.name, "echo");
        assert_eq!(msg.reasoning_content.as_deref(), Some("让我用工具..."));
    }

    #[test]
    fn frontend_message_deserializes_tool_call_id() {
        let json = r#"{"role":"tool","content":"result","toolCallId":"call_1"}"#;
        let msg: FrontendMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
    }
}
