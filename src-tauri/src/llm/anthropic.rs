//! Anthropic Messages API 客户端
//!
//! 参考 Hanako `cn()` 中 `anthropic-messages` 分支。
//!
//! 与 OpenAI Chat Completions 的核心差异：
//! - 端点：POST /v1/messages（非 /chat/completions）
//! - 鉴权：x-api-key header（非 Authorization: Bearer）
//! - 固定头：anthropic-version: 2023-06-01
//! - 系统提示词：独立的 system 字段（非 messages 中的 system role）
//! - 消息格式：user/assistant role，content 可以是字符串或 content block 数组
//! - 工具定义：name/description/input_schema（非 type/function/parameters）
//! - 响应：content 数组（text / tool_use / thinking blocks）
//! - SSE：event:/data: 行对（非 data: {"choices":[{"delta":...}]}）

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
// AnthropicMessagesClient
// ═══════════════════════════════════════════════════════

pub struct AnthropicMessagesClient {
    http: reqwest::Client,
    #[allow(dead_code)]
    registry: Arc<ModelRegistry>,
}

impl AnthropicMessagesClient {
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
            .unwrap_or("https://api.anthropic.com");
        // 标准化：去除尾部 /v1 避免路径重复
        let base = base
            .trim_end_matches('/')
            .strip_suffix("/v1")
            .unwrap_or(base.trim_end_matches('/'));
        // DeepSeek 的 Anthropic 端点路径为 /messages（base 已含 /anthropic），
        // 其他供应商（Anthropic 官方、MiniMax 等）使用 /v1/messages
        if base.contains("api.deepseek.com") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        }
    }

    // ── 消息转换：IR ChatMessage → Anthropic 格式 ──

    /// 将 IR 消息列表转换为 Anthropic 格式。
    ///
    /// 返回 (system_text, messages_array)：
    /// - system_text：所有 system role 消息的内容拼接（Anthropic 用独立字段而非消息 role）
    /// - messages_array：仅 user/assistant/tool 角色。
    ///   tool role 转为 user role 的 tool_result content block。
    ///   assistant 含 tool_calls 时转为多个 content block（text + tool_use）。
    fn convert_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut anthropic_msgs: Vec<Value> = Vec::new();

        for msg in messages {
            use crate::llm::MessageRole;
            match msg.role {
                MessageRole::System => {
                    if !msg.content.is_empty() {
                        system_parts.push(msg.content.clone());
                    }
                }
                MessageRole::User => {
                    // Anthropic 约定：图像 block 在前、文本在后。
                    // 无图时保持字符串 content（与既有输出逐字节一致）。
                    match msg.images.as_ref().filter(|v| !v.is_empty()) {
                        Some(images) => {
                            let mut blocks: Vec<Value> = Vec::new();
                            for img in images {
                                blocks.push(serde_json::json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": img.mime_type,
                                        "data": img.data,
                                    },
                                }));
                            }
                            if !msg.content.is_empty() {
                                blocks.push(serde_json::json!({
                                    "type": "text",
                                    "text": msg.content,
                                }));
                            }
                            anthropic_msgs.push(serde_json::json!({
                                "role": "user",
                                "content": blocks,
                            }));
                        }
                        None => {
                            anthropic_msgs.push(serde_json::json!({
                                "role": "user",
                                "content": msg.content,
                            }));
                        }
                    }
                }
                MessageRole::Assistant => {
                    if let Some(ref tcs) = msg.tool_calls {
                        // 含 tool_calls 的 assistant → content 数组
                        let mut blocks: Vec<Value> = Vec::new();
                        if !msg.content.is_empty() {
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": msg.content,
                            }));
                        }
                        for tc in tcs {
                            let input: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or(Value::Object(Default::default()));
                            blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            }));
                        }
                        anthropic_msgs.push(serde_json::json!({
                            "role": "assistant",
                            "content": blocks,
                        }));
                    } else {
                        // 纯文本 assistant → 字符串 content
                        anthropic_msgs.push(serde_json::json!({
                            "role": "assistant",
                            "content": msg.content,
                        }));
                    }
                }
                MessageRole::Tool => {
                    // 工具结果 → user role 的 tool_result block
                    let tc_id = msg.tool_call_id.as_deref().unwrap_or("");
                    anthropic_msgs.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tc_id,
                            "content": msg.content,
                        }],
                    }));
                }
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };

        (system, anthropic_msgs)
    }

    // ── 工具定义转换 ──

    /// OpenAI ToolDef → Anthropic tool 格式。
    ///
    /// 去掉 `type`/`function` 外层包装，`parameters` 重命名为 `input_schema`。
    fn convert_tools(tools: &[ToolDef]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "input_schema": t.function.parameters,
                })
            })
            .collect()
    }

    // ── 请求体构造 ──

    fn build_body(
        &self,
        config: &LlmConfig,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        stream: bool,
    ) -> Result<Value, LlmError> {
        let model = if config.model.is_empty() {
            "claude-sonnet-4-20250514"
        } else {
            &config.model
        };

        let (system, anthropic_msgs) = Self::convert_messages(messages);

        // Anthropic 要求 messages 非空
        let anthropic_msgs = if anthropic_msgs.is_empty() {
            vec![serde_json::json!({"role": "user", "content": ""})]
        } else {
            anthropic_msgs
        };

        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": config.max_tokens,
            "messages": anthropic_msgs,
            "stream": stream,
        });

        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::json!(Self::convert_tools(tools));
        }

        // 思考强度注入（设置页思考强度；None = 不干预——已设档才进 anthropic wire）。
        // 取真实效果层 compat，不用 default：本族 wire 按 api_type 分支即可（budget_tokens），
        // 但这是「声明了就被读取」的数据通路——某模型将来在本族需要按 compat 区分时，
        // 不必再回来改适配器。
        let thinking_compat =
            self.registry
                .get_resolved_compat(&config.provider, &config.model, config.api_type);
        crate::llm::thinking::inject(
            &mut body,
            config.api_type,
            &thinking_compat,
            &self.registry,
            &config.provider,
            &config.model,
            config.thinking_level.as_deref(),
        );

        Ok(body)
    }

    // ── HTTP 请求发送 ──

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
            .header("anthropic-version", "2023-06-01")
            .json(body);

        if !config.api_key.is_empty() {
            // Anthropic Messages 协议统一使用 x-api-key 鉴权
            req = req.header("x-api-key", &config.api_key);
        }

        Ok(req)
    }

    // ── 非流式响应解析 ──

    /// 解析 Anthropic 非流式响应 → IR。
    ///
    /// 遍历 content 数组：text block 的 text 拼接为 LlmResponse.text，
    /// tool_use block 转换为 ToolCall。
    fn parse_chat_response(body: &Value) -> Result<LlmResponse, LlmError> {
        let content = body
            .get("content")
            .and_then(|v| v.as_array())
            .ok_or_else(|| LlmError::Parse("响应缺少 content 数组".into()))?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in content {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            text_parts.push(t.to_string());
                        }
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let arguments = serde_json::to_string(&input).unwrap_or_default();

                    tool_calls.push(ToolCall {
                        id,
                        call_type: "function".into(),
                        function: ToolCallFunction { name, arguments },
                    });
                }
                // thinking blocks 在当前 context 中跳过
                // 推理链由 SSE streaming 路径处理
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

        let usage = body.get("usage").and_then(Self::extract_usage);
        Ok(LlmResponse { text, tool_calls, usage })
    }

    // ── Usage 提取 ──

    fn extract_usage(usage: &Value) -> Option<TokenUsage> {
        let input = usage.get("input_tokens").and_then(|v| v.as_u64())?;
        let output = usage.get("output_tokens").and_then(|v| v.as_u64())?;
        // 缓存观测（2026-10-03）：命中 = cache_read_input_tokens；
        // Anthropic 无直接“未命名字段 → None（不硬造口径）。
        let cache_hit = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        Some(TokenUsage {
            input_tokens: input as u32,
            output_tokens: output as u32,
            cache_hit_tokens: cache_hit,
            cache_miss_tokens: None,
        })
    }
}

// ═══════════════════════════════════════════════════════
// LlmClient trait 实现
// ═══════════════════════════════════════════════════════

impl LlmClient for AnthropicMessagesClient {
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
                "[Anthropic] 请求 → {} {} | 消息{}条 | 流式",
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

            parse_anthropic_sse_stream(byte_stream, &*on_event).await
        })
    }
}

// ═══════════════════════════════════════════════════════
// Anthropic SSE 流式解析
// ═══════════════════════════════════════════════════════

/// Anthropic SSE 事件类型及其携带的数据。
/// parse_anthropic_frame 是纯函数，所有状态修改在 process_anthropic_event 中完成。
#[derive(Debug)]
enum AnthropicSseEvent {
    MessageStart {
        input_tokens: u32,
        /// 缓存读取命中（cache_read_input_tokens；无该字段 → None = 未观测）
        cache_read_input_tokens: Option<u32>,
    },
    ContentBlockStart { index: usize, block_type: String, block_data: Value },
    ContentBlockDelta { index: usize, delta: Value },
    ContentBlockStop { index: usize },
    MessageDelta {
        /// SSE 的 stop_reason（end_turn / tool_use 等）。**当前只解析、无人读取**
        /// （消费点形如 `stop_reason: _`）；保留以便将来判结束原因，
        /// 测试也据此断言解析正确。
        #[allow(dead_code)]
        stop_reason: Option<String>,
        output_tokens: u32,
    },
    MessageStop,
    Ping,
}

/// 按 content block index 累积状态。
#[derive(Debug, Default)]
struct BlockAccumulator {
    block_type: String,
    /// text block → 累积文本
    text: String,
    /// tool_use block → id
    tool_id: Option<String>,
    /// tool_use block → name
    tool_name: Option<String>,
    /// tool_use block → 累积的 JSON 片段
    input_json: String,
    /// thinking block → 累积推理文本
    thinking: String,
}

/// 解析 Anthropic SSE 字节流。
///
/// Anthropic SSE 使用 `event:` + `data:` 行对，完全不同于
/// OpenAI 的 `data: {"choices":[...]}` 格式。
async fn parse_anthropic_sse_stream<St, E>(
    stream: St,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
) -> Result<StreamResult, LlmError>
where
    St: futures_util::Stream<Item = Result<Vec<u8>, E>> + Unpin,
    E: std::fmt::Display,
{
    // ── 累积状态 ──
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut blocks: HashMap<usize, BlockAccumulator> = HashMap::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage: Option<TokenUsage> = None;

    // 帧读取收口（2026-10-09）：字节缓冲、跨 chunk 无损，见 crate::llm::sse。
    if let Err(e) = crate::llm::sse::read_frames(stream, |frame| {
        if frame.trim().is_empty() {
            return;
        }
        if let Some(ev) = parse_anthropic_frame(frame) {
            process_anthropic_event(
                &ev,
                on_event,
                &mut text,
                &mut reasoning,
                &mut blocks,
                &mut tool_calls,
                &mut usage,
            );
        }
    })
    .await
    {
        return Err(LlmError::Api(format!("流读取异常: {e}")));
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

/// 解析单帧 Anthropic SSE 帧。
///
/// 格式：
/// ```text
/// event: content_block_delta
/// data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}
/// ```
fn parse_anthropic_frame(frame: &str) -> Option<AnthropicSseEvent> {
    let mut event_type = String::new();
    for line in frame.lines() {
        if let Some(ev) = line.strip_prefix("event:") {
            event_type = ev.trim().to_string();
        }
    }

    // data 负载经共享读取器取（前缀空格可有可无、多行按规范拼接）
    let data_str = crate::llm::sse::frame_data(frame).unwrap_or_default();
    if data_str.is_empty() {
        return None;
    }

    let data: Value = serde_json::from_str(&data_str).ok()?;

    match event_type.as_str() {
        "message_start" => {
            let usage = data.get("message").and_then(|m| m.get("usage"));
            let input_tokens = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            // 缓存观测（2026-10-03）：cache_read_input_tokens → 命中；
            // Anthropic 无直接“未命名字段（input_tokens 语义与未命中不完全
            // 对齐）→ 不硬造，留 None。
            let cache_read_input_tokens = usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            Some(AnthropicSseEvent::MessageStart {
                input_tokens,
                cache_read_input_tokens,
            })
        }
        "content_block_start" => {
            let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let block_data = data.get("content_block").cloned().unwrap_or(Value::Null);
            let block_type = block_data
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AnthropicSseEvent::ContentBlockStart {
                index,
                block_type,
                block_data,
            })
        }
        "content_block_delta" => {
            let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let delta = data.get("delta").cloned().unwrap_or(Value::Null);
            Some(AnthropicSseEvent::ContentBlockDelta { index, delta })
        }
        "content_block_stop" => {
            let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            Some(AnthropicSseEvent::ContentBlockStop { index })
        }
        "message_delta" => {
            let stop_reason = data
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let output_tokens = data
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            Some(AnthropicSseEvent::MessageDelta {
                stop_reason,
                output_tokens,
            })
        }
        "message_stop" => Some(AnthropicSseEvent::MessageStop),
        "ping" => Some(AnthropicSseEvent::Ping),
        _ => None,
    }
}

/// 处理单个 Anthropic SSE 事件，转换为 StreamEvent 推送。
fn process_anthropic_event(
    ev: &AnthropicSseEvent,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
    text_acc: &mut String,
    reasoning_acc: &mut String,
    blocks: &mut HashMap<usize, BlockAccumulator>,
    tool_calls: &mut Vec<ToolCall>,
    usage: &mut Option<TokenUsage>,
) {
    match ev {
        AnthropicSseEvent::MessageStart {
            input_tokens,
            cache_read_input_tokens,
        } => {
            *usage = Some(TokenUsage {
                input_tokens: *input_tokens,
                output_tokens: 0,
                cache_hit_tokens: *cache_read_input_tokens,
                cache_miss_tokens: None,
            });
        }
        AnthropicSseEvent::ContentBlockStart {
            index,
            block_type,
            block_data,
        } => {
            log::trace!(
                "[Anthropic SSE] block_start idx={} type={}",
                index,
                block_type
            );
            let acc = blocks.entry(*index).or_default();
            acc.block_type = block_type.clone();
            if block_type == "tool_use" {
                acc.tool_id = block_data
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                acc.tool_name = block_data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
        }
        AnthropicSseEvent::ContentBlockDelta { index, delta } => {
            let acc = blocks.entry(*index).or_default();

            let delta_type = delta
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match delta_type {
                "text_delta" => {
                    if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                        acc.text.push_str(t);
                        text_acc.push_str(t);
                        on_event(StreamEvent::TextDelta {
                            content: t.to_string(),
                        });
                    }
                }
                "input_json_delta" => {
                    if let Some(j) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        acc.input_json.push_str(j);
                    }
                }
                "thinking_delta" => {
                    if let Some(t) = delta.get("thinking").and_then(|v| v.as_str()) {
                        acc.thinking.push_str(t);
                        reasoning_acc.push_str(t);
                        on_event(StreamEvent::ReasoningDelta {
                            content: t.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
        AnthropicSseEvent::ContentBlockStop { index } => {
            if let Some(acc) = blocks.remove(index) {
                if acc.block_type == "tool_use" {
                    if let (Some(id), Some(name)) = (acc.tool_id, acc.tool_name) {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            call_type: "function".into(),
                            function: ToolCallFunction {
                                name: name.clone(),
                                arguments: acc.input_json.clone(),
                            },
                        });
                        on_event(StreamEvent::ToolCall {
                            id,
                            name,
                            arguments: acc.input_json,
                        });
                    }
                }
            }
        }
        AnthropicSseEvent::MessageDelta {
            stop_reason: _,
            output_tokens,
        } => {
            if let Some(ref mut existing) = usage {
                existing.output_tokens = *output_tokens;
            }
        }
        AnthropicSseEvent::MessageStop => {
            // 流终止
        }
        AnthropicSseEvent::Ping => {
            // 心跳，忽略
        }
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

    // ── 测试辅助 ──

    /// 构造 Anthropic SSE 帧字节流。
    /// 每帧格式：`event: {event}\ndata: {data}\n\n`
    fn anthropic_sse_stream<'a>(
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

    async fn collect_anthropic_events(
        frames: &[(&str, &str)],
    ) -> (Vec<StreamEvent>, StreamResult) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_c = events.clone();
        let on_event = move |e: StreamEvent| {
            events_c.lock().unwrap().push(e);
        };

        let s = anthropic_sse_stream(frames);
        let result = parse_anthropic_sse_stream(s, &on_event)
            .await
            .unwrap();

        let collected = events.lock().unwrap().clone();
        (collected, result)
    }

    // ── 消息转换 ──

    #[test]
    fn convert_system_to_separate_field() {
        let msgs = vec![
            ChatMessage::system("你是 PRTS"),
            ChatMessage::user("你好"),
        ];
        let (sys, an_msgs) = AnthropicMessagesClient::convert_messages(&msgs);
        assert_eq!(sys, Some("你是 PRTS".into()));
        assert_eq!(an_msgs.len(), 1);
        assert_eq!(an_msgs[0]["role"], "user");
    }

    #[test]
    fn convert_multiple_system_joined() {
        let msgs = vec![
            ChatMessage::system("规则 A"),
            ChatMessage::system("规则 B"),
        ];
        let (sys, _) = AnthropicMessagesClient::convert_messages(&msgs);
        assert_eq!(sys, Some("规则 A\n\n规则 B".into()));
    }

    #[test]
    fn convert_tool_result_to_user_block() {
        let msgs = vec![ChatMessage::tool_result("toolu_1", "HP: 100")];
        let (_sys, an_msgs) = AnthropicMessagesClient::convert_messages(&msgs);
        assert_eq!(an_msgs.len(), 1);
        assert_eq!(an_msgs[0]["role"], "user");
        let content = &an_msgs[0]["content"];
        assert!(content.is_array());
        let arr = content.as_array().unwrap();
        assert_eq!(arr[0]["type"], "tool_result");
        assert_eq!(arr[0]["tool_use_id"], "toolu_1");
        assert_eq!(arr[0]["content"], "HP: 100");
    }

    #[test]
    fn convert_assistant_with_tool_calls() {
        let tc = ToolCall {
            id: "toolu_1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "query_enemy".into(),
                arguments: r#"{"name":"源石虫"}"#.into(),
            },
        };
        let msg = ChatMessage {
            role: crate::llm::MessageRole::Assistant,
            content: "让我查一下".into(),
            tool_calls: Some(vec![tc]),
            tool_call_id: None,
            reasoning_content: None,
            image_paths: None,
            images: None,
        };
        let (_sys, an_msgs) = AnthropicMessagesClient::convert_messages(&[msg]);
        assert_eq!(an_msgs.len(), 1);
        assert_eq!(an_msgs[0]["role"], "assistant");
        let content = an_msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2); // text + tool_use
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "让我查一下");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "query_enemy");
        assert_eq!(
            content[1]["input"]["name"],
            "源石虫"
        );
    }

    // ── 工具转换 ──

    #[test]
    fn convert_tool_def() {
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "敌人名称" }
            }
        });
        let tools = vec![ToolDef::new("query_enemy", "查询敌人", params)];
        let an_tools = AnthropicMessagesClient::convert_tools(&tools);
        assert_eq!(an_tools.len(), 1);
        assert_eq!(an_tools[0]["name"], "query_enemy");
        assert_eq!(an_tools[0]["input_schema"]["type"], "object");
        // 确认没有 type/function 外层包装
        assert!(an_tools[0].get("type").is_none());
        assert!(an_tools[0].get("function").is_none());
    }

    // ── SSE 解析 ──

    #[tokio::test]
    async fn pure_text_stream() {
        let frames = &[
            ("message_start", r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":10}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"博士"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (events, result) = collect_anthropic_events(frames).await;

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::TextDelta { ref content } if content == "你好"));
        assert!(matches!(events[1], StreamEvent::TextDelta { ref content } if content == "博士"));
        assert_eq!(result.text, "你好博士");
        assert!(result.tool_calls.is_empty());
        assert!(result.reasoning.is_none());
        let u = result.usage.unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 5);
    }

    #[tokio::test]
    async fn tool_use_stream() {
        let frames = &[
            ("message_start", r#"{"type":"message_start","message":{"id":"msg_2","usage":{"input_tokens":20}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"echo","input":{}}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"mess"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"age\":\"hi\"}"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (events, result) = collect_anthropic_events(frames).await;

        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::ToolCall { id, name, arguments }
            if id == "toolu_1"
            && name == "echo"
            && arguments == r#"{"message":"hi"}"#
        ));

        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "echo");
        assert!(result.text.is_empty());
        // 契约不变量 I1~I5（模块 4 P1）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn mixed_text_and_tool_use() {
        let frames = &[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#),
            // text block
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"我来查询"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            // tool_use block
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"echo","input":{}}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":15}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (events, result) = collect_anthropic_events(frames).await;

        // 应有 text_delta + tool_call 两个事件
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::TextDelta { .. }));
        assert!(matches!(events[1], StreamEvent::ToolCall { .. }));
        assert_eq!(result.text, "我来查询");
        assert_eq!(result.tool_calls.len(), 1);
        // 契约不变量 I1~I5（模块 4 P1）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn thinking_delta() {
        let frames = &[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"需要查询数据库"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            // text block after thinking
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"结果如下"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (events, result) = collect_anthropic_events(frames).await;

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::ReasoningDelta { ref content } if content == "需要查询数据库"));
        assert!(matches!(events[1], StreamEvent::TextDelta { .. }));
        assert_eq!(result.reasoning, Some("需要查询数据库".into()));
        assert_eq!(result.text, "结果如下");
    }

    // ── 响应解析 ──

    #[test]
    fn parse_text_and_tool_use_response() {
        let body = serde_json::json!({
            "id": "msg_3",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "查到以下敌人："},
                {"type": "tool_use", "id": "toolu_2", "name": "query_enemy", "input": {"name": "源石虫"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        });

        let resp = AnthropicMessagesClient::parse_chat_response(&body).unwrap();
        assert_eq!(resp.text, Some("查到以下敌人：".into()));
        let tcs = resp.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "query_enemy");
        assert_eq!(tcs[0].function.arguments, r#"{"name":"源石虫"}"#);
    }

    #[test]
    fn parse_pure_text_response() {
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": "你好博士"}
            ],
            "stop_reason": "end_turn"
        });

        let resp = AnthropicMessagesClient::parse_chat_response(&body).unwrap();
        assert_eq!(resp.text, Some("你好博士".into()));
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn parse_empty_content() {
        let body = serde_json::json!({
            "content": [],
            "stop_reason": "end_turn"
        });

        let resp = AnthropicMessagesClient::parse_chat_response(&body).unwrap();
        assert!(resp.text.is_none());
        assert!(resp.tool_calls.is_none());
    }
}
