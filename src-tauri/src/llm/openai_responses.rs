//! OpenAI Responses API 客户端
//!
//! 参考 Hanako `cn()` 中 `openai-responses` 分支。
//!
//! 与 Chat Completions 的核心差异：
//! - 端点：POST /responses（非 /chat/completions）
//! - 系统提示词：instructions 字段（非 messages 中的 system role）
//! - 消息列表：input 字段（非 messages）
//! - 最大 token：max_output_tokens（非 max_tokens）
//! - 响应结构：output 数组（type: message / function_call）
//! - SSE：event: response.output_text.delta / function_call_arguments.delta

use std::collections::HashMap;
use std::pin::Pin;
use std::future::Future;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::header;
use serde_json::Value;

use super::{
    ChatMessage, LlmClient, LlmConfig, LlmError, LlmResponse,
    ModelRegistry,
    StreamEvent, StreamResult, TokenUsage, ToolCall, ToolCallFunction, ToolDef,
};

// ═══════════════════════════════════════════════════════
// OpenAIResponsesClient
// ═══════════════════════════════════════════════════════

pub struct OpenAIResponsesClient {
    http: reqwest::Client,
    #[allow(dead_code)]
    registry: Arc<ModelRegistry>,
}

impl OpenAIResponsesClient {
    pub fn new(registry: Arc<ModelRegistry>) -> Self {
        Self {
            http: super::http_client(),
            registry,
        }
    }

    // ── URL ──

    fn build_url(config: &LlmConfig) -> String {
        let base = config
            .base_url
            .as_deref()
            .unwrap_or("https://api.openai.com/v1");
        format!("{}/responses", base.trim_end_matches('/'))
    }

    // ── 消息转换 ──

    /// 提取系统消息内容 → instructions 字段。
    fn extract_instructions(messages: &[ChatMessage]) -> Option<String> {
        let parts: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == crate::llm::MessageRole::System)
            .map(|m| m.content.as_str())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// 过滤 user/assistant/tool 消息 → input 数组。
    ///
    /// tool 消息转为 input 中的 function_call_output item。
    fn build_input(messages: &[ChatMessage]) -> Vec<Value> {
        use crate::llm::MessageRole;
        messages
            .iter()
            .filter(|m| {
                m.role == MessageRole::User
                    || m.role == MessageRole::Assistant
                    || m.role == MessageRole::Tool
            })
            .map(|m| match m.role {
                MessageRole::Tool => {
                    let call_id = m.tool_call_id.as_deref().unwrap_or("");
                    serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": m.content,
                    })
                }
                _ => {
                    // MessageRole 序列化为 lowercase 字符串，直通 API role 字面量
                    serde_json::json!({
                        "role": m.role,
                        "content": m.content,
                    })
                }
            })
            .collect()
    }

    // ── 请求体 ──

    fn build_body(
        &self,
        config: &LlmConfig,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        stream: bool,
    ) -> Result<Value, LlmError> {
        let model = if config.model.is_empty() {
            "gpt-4o"
        } else {
            &config.model
        };

        let instructions = Self::extract_instructions(messages);
        let input = Self::build_input(messages);

        let mut body = serde_json::json!({
            "model": model,
            "max_output_tokens": config.max_tokens,
            "input": input,
            "stream": stream,
        });

        if let Some(inst) = instructions {
            body["instructions"] = serde_json::json!(inst);
        }

        if !tools.is_empty() {
            // Responses API 使用与 Chat Completions 相同的工具格式
            body["tools"] = serde_json::to_value(tools).map_err(|e| {
                LlmError::Parse(format!("工具列表序列化失败: {e}"))
            })?;
        }

        // 思考强度注入（设置页思考强度；None = 不干预——已设档才进 responses wire）
        crate::llm::thinking::inject(
            &mut body,
            config.api_type,
            &crate::llm::ResolvedCompat::default(),
            &self.registry,
            &config.provider,
            &config.model,
            config.thinking_level.as_deref(),
        );

        Ok(body)
    }

    // ── HTTP 请求 ──

    fn build_request(
        &self,
        config: &LlmConfig,
        body: &Value,
    ) -> Result<reqwest::RequestBuilder, LlmError> {
        let url = Self::build_url(config);
        let mut req = self
            .http
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .json(body);

        if !config.api_key.is_empty() {
            req = req.header(
                header::AUTHORIZATION,
                format!("Bearer {}", config.api_key),
            );
        }

        Ok(req)
    }

    // ── 非流式响应解析 ──

    /// 解析 Responses API 响应 → IR。
    ///
    /// 遍历 output 数组：
    /// - type: "message" → 取 content 中的 output_text
    /// - type: "function_call" → 转为 ToolCall
    fn parse_chat_response(body: &Value) -> Result<LlmResponse, LlmError> {
        let output = body
            .get("output")
            .and_then(|v| v.as_array())
            .ok_or_else(|| LlmError::Parse("响应缺少 output 数组".into()))?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for item in output {
            match item.get("type").and_then(|v| v.as_str()) {
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                        for block in content {
                            if block.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                    if !t.is_empty() {
                                        text_parts.push(t.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let id = item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    tool_calls.push(ToolCall {
                        id,
                        call_type: "function".into(),
                        function: ToolCallFunction { name, arguments },
                    });
                }
                _ => {}
            }
        }

        let text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        };

        let tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        Ok(LlmResponse { text, tool_calls })
    }

    // ── Usage ──

    fn extract_usage(usage: &Value) -> Option<TokenUsage> {
        let input = usage.get("input_tokens").and_then(|v| v.as_u64())?;
        let output = usage.get("output_tokens").and_then(|v| v.as_u64())?;
        Some(TokenUsage {
            input_tokens: input as u32,
            output_tokens: output as u32,
        })
    }
}

// ═══════════════════════════════════════════════════════
// LlmClient trait 实现
// ═══════════════════════════════════════════════════════

impl LlmClient for OpenAIResponsesClient {
    fn chat<'a>(
        &'a self,
        config: &'a LlmConfig,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDef],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let body = self.build_body(config, messages, tools, false)?;
            let req = self.build_request(config, &body)?;

            let response = req.send().await.map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(e.to_string())
                } else {
                    LlmError::Api(format!("HTTP 请求失败: {e}"))
                }
            })?;

            let status = response.status();
            let body: Value = response.json().await.map_err(|e| {
                LlmError::Parse(format!("响应 JSON 解析失败: {e}"))
            })?;

            if !status.is_success() {
                let err_msg = body
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知错误");
                return Err(LlmError::Api(format!(
                    "HTTP {}: {err_msg}",
                    status.as_u16()
                )));
            }

            Self::parse_chat_response(&body)
        })
    }

    fn chat_stream<'a>(
        &'a self,
        config: &'a LlmConfig,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDef],
        on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let body = self.build_body(config, messages, tools, true)?;
            let req = self.build_request(config, &body)?;

            log::debug!(
                "[Responses] 请求 → {} {} | 消息{}条 | 流式",
                config.provider,
                config.model,
                messages.len(),
            );

            let response = req.send().await.map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(e.to_string())
                } else {
                    LlmError::Api(format!("HTTP 请求失败: {e}"))
                }
            })?;

            let status = response.status();
            if !status.is_success() {
                let err_text = response.text().await.unwrap_or_default();
                let err_msg = serde_json::from_str::<Value>(&err_text)
                    .ok()
                    .and_then(|v| {
                        v.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or(err_text);
                return Err(LlmError::Api(format!(
                    "HTTP {}: {err_msg}",
                    status.as_u16()
                )));
            }

            let byte_stream = response
                .bytes_stream()
                .map(|r| r.map(|b| b.to_vec()).map_err(|e| e.to_string()));

            parse_responses_sse_stream(byte_stream, &*on_event).await
        })
    }
}

// ═══════════════════════════════════════════════════════
// Responses API SSE 解析
// ═══════════════════════════════════════════════════════

/// Responses API SSE 事件。
///
/// 格式与 Chat Completions 不同：event 行放在 data 之前。
/// ```text
/// event: response.output_text.delta
/// data: {"type":"response.output_text.delta","delta":"你好"}
/// ```
#[derive(Debug)]
enum ResponsesSseEvent {
    TextDelta { content: String },
    ReasoningDelta { content: String },
    FunctionCallDelta { call_id: String, name: Option<String>, arguments_delta: String },
    Completed { usage: Option<Value> },
}

/// 解析 Responses API SSE 字节流。
async fn parse_responses_sse_stream<St, E>(
    mut stream: St,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
) -> Result<StreamResult, LlmError>
where
    St: futures_util::Stream<Item = Result<Vec<u8>, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut line_buf = String::new();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage: Option<TokenUsage> = None;

    // 按 call_id 累积 tool_call 的 arguments 和 name
    let mut pending_calls: HashMap<String, (Option<String>, String)> = HashMap::new();

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                line_buf.push_str(&String::from_utf8_lossy(&bytes));

                while let Some(pos) = line_buf.find("\n\n") {
                    let frame = line_buf[..pos].to_string();
                    line_buf = line_buf[pos + 2..].to_string();

                    if frame.trim().is_empty() {
                        continue;
                    }

                    if let Some(ev) = parse_responses_frame(&frame) {
                        match ev {
                            ResponsesSseEvent::TextDelta { content } => {
                                text.push_str(&content);
                                on_event(StreamEvent::TextDelta {
                                    content: content.clone(),
                                });
                            }
                            ResponsesSseEvent::ReasoningDelta { content } => {
                                reasoning.push_str(&content);
                                on_event(StreamEvent::ReasoningDelta {
                                    content: content.clone(),
                                });
                            }
                            ResponsesSseEvent::FunctionCallDelta {
                                call_id,
                                name,
                                arguments_delta,
                            } => {
                                let entry = pending_calls.entry(call_id).or_default();
                                if let Some(n) = name {
                                    entry.0 = Some(n);
                                }
                                entry.1.push_str(&arguments_delta);
                            }
                            ResponsesSseEvent::Completed { usage: u } => {
                                if let Some(u) = u {
                                    usage = OpenAIResponsesClient::extract_usage(&u);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                return Err(LlmError::Api(format!("流读取异常: {e}")));
            }
        }
    }

    // 流结束后 emit 所有累积的 tool_call
    for (call_id, (name, arguments)) in pending_calls {
        if let Some(ref n) = name {
            tool_calls.push(ToolCall {
                id: call_id.clone(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: n.clone(),
                    arguments: arguments.clone(),
                },
            });
            on_event(StreamEvent::ToolCall {
                id: call_id,
                name: n.clone(),
                arguments,
            });
        }
    }

    Ok(StreamResult {
        text,
        tool_calls,
        reasoning: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        usage,
    })
}

/// 解析单帧 Responses API SSE 事件。
///
/// 帧格式：
/// ```text
/// event: response.output_text.delta
/// data: {"type":"response.output_text.delta","delta":"你好"}
/// ```
fn parse_responses_frame(frame: &str) -> Option<ResponsesSseEvent> {
    let mut event_type = String::new();
    let mut data_str = String::new();

    for line in frame.lines() {
        if let Some(ev) = line.strip_prefix("event: ") {
            event_type = ev.trim().to_string();
        } else if let Some(d) = line.strip_prefix("data: ") {
            data_str = d.to_string();
        }
    }

    if data_str.is_empty() {
        return None;
    }

    let data: Value = serde_json::from_str(&data_str).ok()?;

    match event_type.as_str() {
        "response.output_text.delta" => {
            let content = data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if content.is_empty() {
                None
            } else {
                Some(ResponsesSseEvent::TextDelta { content })
            }
        }
        "response.reasoning_text.delta" => {
            let content = data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if content.is_empty() {
                None
            } else {
                Some(ResponsesSseEvent::ReasoningDelta { content })
            }
        }
        "response.function_call_arguments.delta" => {
            let call_id = data
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = data
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let arguments_delta = data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ResponsesSseEvent::FunctionCallDelta {
                call_id,
                name,
                arguments_delta,
            })
        }
        "response.completed" => {
            let usage = data
                .get("response")
                .and_then(|r| r.get("usage"))
                .cloned();
            Some(ResponsesSseEvent::Completed { usage })
        }
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::sync::Mutex;

    // ── 辅助 ──

    fn responses_sse_stream<'a>(
        frames: &'a [(&'a str, &'a str)],
    ) -> impl futures_util::Stream<Item = Result<Vec<u8>, &'a str>> {
        let chunks: Vec<Result<Vec<u8>, &str>> = frames
            .iter()
            .map(|(ev, data)| {
                Ok(format!("event: {}\ndata: {}\n\n", ev, data).into_bytes())
            })
            .collect();
        stream::iter(chunks)
    }

    async fn collect_events(
        frames: &[(&str, &str)],
    ) -> (Vec<StreamEvent>, StreamResult) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_c = events.clone();
        let on_event = move |e: StreamEvent| {
            events_c.lock().unwrap().push(e);
        };

        let s = responses_sse_stream(frames);
        let result = parse_responses_sse_stream(s, &on_event).await.unwrap();

        let collected = events.lock().unwrap().clone();
        (collected, result)
    }

    // ── 消息转换 ──

    #[test]
    fn extract_instructions_from_system() {
        let msgs = vec![
            ChatMessage::system("规则 A"),
            ChatMessage::user("你好"),
        ];
        let inst = OpenAIResponsesClient::extract_instructions(&msgs);
        assert_eq!(inst, Some("规则 A".into()));
    }

    #[test]
    fn extract_instructions_none_when_no_system() {
        let msgs = vec![ChatMessage::user("你好")];
        let inst = OpenAIResponsesClient::extract_instructions(&msgs);
        assert!(inst.is_none());
    }

    #[test]
    fn build_input_filters_system() {
        let msgs = vec![
            ChatMessage::system("规则"),
            ChatMessage::user("你好"),
            ChatMessage::assistant("你好博士"),
        ];
        let input = OpenAIResponsesClient::build_input(&msgs);
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
    }

    #[test]
    fn build_input_converts_tool_result() {
        let msgs = vec![ChatMessage::tool_result("fc_1", "结果文本")];
        let input = OpenAIResponsesClient::build_input(&msgs);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "fc_1");
        assert_eq!(input[0]["output"], "结果文本");
    }

    // ── 响应解析 ──

    #[test]
    fn parse_text_response() {
        let body = serde_json::json!({
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "你好博士"}
                    ]
                }
            ]
        });
        let resp = OpenAIResponsesClient::parse_chat_response(&body).unwrap();
        assert_eq!(resp.text, Some("你好博士".into()));
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn parse_function_call_response() {
        let body = serde_json::json!({
            "status": "completed",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "name": "query_enemy",
                    "arguments": "{\"name\":\"源石虫\"}"
                }
            ]
        });
        let resp = OpenAIResponsesClient::parse_chat_response(&body).unwrap();
        assert!(resp.text.is_none());
        let tcs = resp.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "fc_1");
        assert_eq!(tcs[0].function.name, "query_enemy");
        assert_eq!(tcs[0].function.arguments, r#"{"name":"源石虫"}"#);
    }

    #[test]
    fn parse_mixed_response() {
        let body = serde_json::json!({
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "查到以下："}
                    ]
                },
                {
                    "type": "function_call",
                    "id": "fc_2",
                    "name": "echo",
                    "arguments": "{}"
                }
            ]
        });
        let resp = OpenAIResponsesClient::parse_chat_response(&body).unwrap();
        assert_eq!(resp.text, Some("查到以下：".into()));
        assert_eq!(resp.tool_calls.unwrap().len(), 1);
    }

    // ── SSE 解析 ──

    #[tokio::test]
    async fn pure_text_stream() {
        let frames = &[
            ("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"你好"}"#),
            ("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"博士"}"#),
            ("response.completed", r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":5}}}"#),
        ];
        let (events, result) = collect_events(frames).await;

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::TextDelta { ref content } if content == "你好"));
        assert!(matches!(events[1], StreamEvent::TextDelta { ref content } if content == "博士"));
        assert_eq!(result.text, "你好博士");
        let u = result.usage.unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 5);
        // 契约不变量 I1~I5（模块 4 P1；I4：usage 先于 finish）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn function_call_stream() {
        let frames = &[
            ("response.function_call_arguments.delta", r#"{"type":"response.function_call_arguments.delta","call_id":"fc_1","name":"echo","delta":"{\"mess"}"#),
            ("response.function_call_arguments.delta", r#"{"type":"response.function_call_arguments.delta","call_id":"fc_1","delta":"age\":\"hi\"}"}"#),
            ("response.completed", r#"{"type":"response.completed","response":{"usage":{"input_tokens":8,"output_tokens":6}}}"#),
        ];
        let (events, result) = collect_events(frames).await;

        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::ToolCall { id, name, arguments }
            if id == "fc_1"
            && name == "echo"
            && arguments == r#"{"message":"hi"}"#
        ));
        assert_eq!(result.tool_calls.len(), 1);
        // 契约不变量 I2/I5：arguments 端到端 raw（分片累积后原样直通）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn reasoning_stream() {
        let frames = &[
            ("response.reasoning_text.delta", r#"{"type":"response.reasoning_text.delta","delta":"需要查询"}"#),
            ("response.reasoning_text.delta", r#"{"type":"response.reasoning_text.delta","delta":"数据库"}"#),
            ("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"结果"}"#),
            ("response.completed", r#"{"type":"response.completed","response":{"usage":{"input_tokens":5,"output_tokens":3}}}"#),
        ];
        let (events, result) = collect_events(frames).await;

        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::ReasoningDelta { .. }));
        assert!(matches!(events[1], StreamEvent::ReasoningDelta { .. }));
        assert!(matches!(events[2], StreamEvent::TextDelta { .. }));
        assert_eq!(result.reasoning, Some("需要查询数据库".into()));
        assert_eq!(result.text, "结果");
    }
}
