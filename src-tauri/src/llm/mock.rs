//! Mock LLM 客户端 —— 预设脚本驱动的假 LLM
//!
//! 实现 `LlmClient` trait，不需要网络、不需要 API key。
//! 内部持有预设的事件序列和响应结果，按调用次数逐次回放。
//! 用于验证 orchestrator 的控制流而不依赖真实 LLM。
//!
//! 预设脚本在创建 MockLlmClient 时传入：
//!   let mock = MockLlmClient::new(vec![
//!       MockResponse { events: ..., result: ... },  // 第 1 次调用
//!       MockResponse { events: ..., result: ... },  // 第 2 次调用
//!   ]);
//!
//! 第 n 次 chat() / chat_stream() 回放第 n 组预设。
//! 调用次数超出预设数量时返回错误。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::future::Future;
use std::pin::Pin;

use super::{
    ChatMessage, LlmClient, LlmConfig, LlmError, LlmResponse,
    StreamEvent, StreamResult, ToolDef,
};

// ═══════════════════════════════════════════════════════
// 预设数据结构
// ═══════════════════════════════════════════════════════

/// 一次 LLM 调用的完整预设：流式过程中推送哪些事件 + 流结束后的聚合结果。
///
/// `events` 按时间顺序排列，mock 在 chat_stream 中逐个回调推送。
/// `result` 是流结束后的 StreamResult。
///
/// 简单问候场景示例：
///   MockResponse {
///       events: vec![StreamEvent::TextDelta { content: "你好，博士".into() }],
///       result: StreamResult { text: "你好，博士".into(), tool_calls: vec![], reasoning: None },
///   }
#[derive(Debug, Clone)]
pub struct MockResponse {
    /// 流式过程中逐个推送的事件（按时间顺序）
    pub events: Vec<StreamEvent>,
    /// 流结束后的聚合结果
    pub result: StreamResult,
}

// ═══════════════════════════════════════════════════════
// Mock 客户端
// ═══════════════════════════════════════════════════════

/// 预设脚本驱动的假 LLM 客户端。
///
/// 线程安全（AtomicUsize 计数器），满足 `LlmClient: Send + Sync` 约束。
/// orchestrator 通过 `Arc<MockLlmClient>` 持有。
///
/// M3-B 挂起扩展（`new_with_hang`）：第 `hang_at` 次（0 基，与响应索引同源）
/// `chat_stream` 在推完预设事件后**永不返回**——模拟真实 LLM 流中途挂起，
/// 供 abort-in-LLM 集成测试使用。挂起的 future 无法自行完成：调用方必须以
/// 取消/abort 结束它（drop 即取消，无线程泄漏——纯 async 无阻塞）。
pub struct MockLlmClient {
    responses: Vec<MockResponse>,
    call_count: AtomicUsize,
    /// 挂起索引（None = 不挂起；Some(n) = 第 n 次 stream 调用推完事件后挂起）
    hang_at: Option<usize>,
}

impl MockLlmClient {
    /// 创建 Mock 客户端。
    ///
    /// `responses` 按调用顺序排列：第一次 chat/chat_stream 用 responses[0]，
    /// 第二次用 responses[1]，以此类推。
    pub fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            responses,
            call_count: AtomicUsize::new(0),
            hang_at: None,
        }
    }

    /// 创建带挂起的 Mock 客户端：第 `hang_at` 次（0 基）**chat_stream** 调用
    /// 推完预设 events 后挂起（永不返回结果），其余调用照常回放。
    ///
    /// 语义说明：chat/chat_stream 共享调用计数器；调度循环只走 chat_stream，
    /// 测试不混用 chat 时 hang_at 即「第 N 个响应」。
    pub fn new_with_hang(responses: Vec<MockResponse>, hang_at: usize) -> Self {
        Self {
            responses,
            call_count: AtomicUsize::new(0),
            hang_at: Some(hang_at),
        }
    }

    /// 快捷构造：只返回一段文本的 Mock（最简场景——问候）。
    ///
    /// 等价于 `new(vec![MockResponse { events: vec![TextDelta], result: StreamResult { text, ... } }])`
    pub fn simple_reply(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::new(vec![MockResponse {
            events: vec![StreamEvent::TextDelta { content: text.clone() }],
            result: StreamResult {
                text,
                tool_calls: vec![],
                reasoning: None,
                usage: None,
            },
        }])
    }

    // ── 内部公用逻辑 ──

    /// 获取下一次调用的预设，计数器自增。
    /// 超出数量时返回错误。
    fn take_response(&self) -> Result<(usize, &MockResponse), LlmError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        self.responses
            .get(idx)
            .map(|r| (idx, r))
            .ok_or_else(|| {
                LlmError::Api(format!(
                    "Mock 预设不足：第 {} 次调用，但只有 {} 组预设",
                    idx + 1,
                    self.responses.len()
                ))
            })
    }
}

// ═══════════════════════════════════════════════════════
// LlmClient trait 实现
// ═══════════════════════════════════════════════════════

impl LlmClient for MockLlmClient {
    fn chat<'a>(
        &'a self,
        _config: &'a LlmConfig,
        _messages: &'a [ChatMessage],
        _tools: &'a [ToolDef],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let (_idx, response) = self.take_response()?;

            let text = if response.result.text.is_empty() {
                None
            } else {
                Some(response.result.text.clone())
            };

            let tool_calls = if response.result.tool_calls.is_empty() {
                None
            } else {
                Some(response.result.tool_calls.clone())
            };

            Ok(LlmResponse { text, tool_calls })
        })
    }

    fn chat_stream<'a>(
        &'a self,
        _config: &'a LlmConfig,
        _messages: &'a [ChatMessage],
        _tools: &'a [ToolDef],
        on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let (_idx, response) = self.take_response()?;

            for event in &response.events {
                on_event(event.clone());
            }

            // M3-B：挂起点——推完事件后永不返回（模拟流中途挂起；
            // drop 即取消，abort 分支经 select 丢弃本 future）
            if self.hang_at == Some(_idx) {
                std::future::pending::<()>().await;
            }

            Ok(response.result.clone())
        })
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, LlmConfig, ToolCall, ToolCallFunction};

    #[tokio::test]
    async fn simple_reply_works() {
        let mock = MockLlmClient::simple_reply("你好，博士");
        let config = LlmConfig::default();
        let messages = vec![ChatMessage::user("你好")];
        let tools = vec![];

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let result = mock
            .chat_stream(
                &config,
                &messages,
                &tools,
                Box::new(move |e| events_clone.lock().unwrap().push(e)),
            )
            .await
            .unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { content } => assert_eq!(content, "你好，博士"),
            _ => panic!("期望 TextDelta"),
        }

        assert_eq!(result.text, "你好，博士");
        assert!(result.tool_calls.is_empty());
        // 契约不变量 I1~I5（模块 4 P1）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn tool_call_flow() {
        let mock = MockLlmClient::new(vec![
            MockResponse {
                events: vec![
                    StreamEvent::ReasoningDelta { content: "需要查询".into() },
                    StreamEvent::ToolCall {
                        id: "c1".into(),
                        name: "echo".into(),
                        arguments: "{\"x\":1}".into(),
                    },
                ],
                result: StreamResult {
                    text: "".into(),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        call_type: "function".into(),
                        function: ToolCallFunction { name: "echo".into(), arguments: "{\"x\":1}".into() },
                    }],
                    reasoning: Some("需要查询".into()),
                    usage: None,
                },
            },
            MockResponse {
                events: vec![
                    StreamEvent::TextDelta { content: "结果: x=1".into() },
                ],
                result: StreamResult {
                    text: "结果: x=1".into(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                },
            },
        ]);

        let config = LlmConfig::default();
        let msgs = vec![ChatMessage::user("测试")];
        let tools = vec![];

        let events_1 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_1_c = events_1.clone();
        let r1 = mock
            .chat_stream(&config, &msgs, &tools, Box::new(move |e| events_1_c.lock().unwrap().push(e)))
            .await
            .unwrap();

        let e1 = events_1.lock().unwrap();
        assert_eq!(e1.len(), 2);
        assert!(matches!(e1[0], StreamEvent::ReasoningDelta { .. }));
        assert!(matches!(e1[1], StreamEvent::ToolCall { .. }));
        assert_eq!(r1.tool_calls.len(), 1);
        assert_eq!(r1.tool_calls[0].function.name, "echo");
        // 契约不变量 I1~I5（模块 4 P1）
        super::super::assert_stream_contract(&e1, &r1);

        let events_2 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_2_c = events_2.clone();
        let r2 = mock
            .chat_stream(&config, &msgs, &tools, Box::new(move |e| events_2_c.lock().unwrap().push(e)))
            .await
            .unwrap();

        let e2 = events_2.lock().unwrap();
        assert_eq!(e2.len(), 1);
        assert!(matches!(e2[0], StreamEvent::TextDelta { .. }));
        assert_eq!(r2.text, "结果: x=1");
        super::super::assert_stream_contract(&e2, &r2);
    }

    #[tokio::test]
    async fn non_streaming_chat() {
        let mock = MockLlmClient::new(vec![MockResponse {
            events: vec![],
            result: StreamResult {
                text: "直接回答".into(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
            },
        }]);

        let r = mock
            .chat(&LlmConfig::default(), &[], &[])
            .await
            .unwrap();

        assert_eq!(r.text, Some("直接回答".into()));
        assert!(r.tool_calls.is_none());
    }

    #[tokio::test]
    async fn out_of_presets() {
        let mock = MockLlmClient::new(vec![]);

        let err = mock
            .chat(&LlmConfig::default(), &[], &[])
            .await
            .unwrap_err();

        assert!(matches!(err, LlmError::Api(_)));
        assert!(err.to_string().contains("预设不足"));
    }

    #[tokio::test]
    async fn hanging_mock_streams_events_then_parks() {
        // M3-B 挂起原语：推完事件后永不返回（drop 即取消）
        let mock = MockLlmClient::new_with_hang(
            vec![MockResponse {
                events: vec![
                    StreamEvent::TextDelta { content: "半截一".into() },
                    StreamEvent::TextDelta { content: "半截二".into() },
                ],
                result: StreamResult {
                    text: "半截一半截二".into(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                },
            }],
            0, // 第 0 次调用挂起
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let task = tokio::spawn(async move {
            mock.chat_stream(
                &LlmConfig::default(),
                &[],
                &[],
                Box::new(move |e| events_clone.lock().unwrap().push(e)),
            )
            .await
        });

        // 事件在挂起前已推送（首次 poll 同步触发）
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2, "挂起前预设事件应已全部推送");
        drop(events);

        // 挂起中：任务不完成（timeout 证明仍在跑）
        let pending = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            task,
        )
        .await;
        assert!(pending.is_err(), "挂起的 chat_stream 不应自行完成");
        // 放弃任务（drop 取消挂起 future，无泄漏）——cancel 安全
    }
}
