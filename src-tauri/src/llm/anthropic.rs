//! Anthropic Messages API 客户端
//!
//! 实现 Anthropic Messages 协议的请求、流式事件与用量解析。
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
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::header;
use serde_json::Value;

use super::{
    provider_error_detail, retry_after_ms_from_headers, ChatMessage, FinishReason, LlmClient,
    LlmConfig, LlmError, LlmResponse, ModelRegistry, ProviderBlock, StreamEvent, StreamResult,
    TokenUsage, ToolCall, ToolCallFunction, ToolDef,
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
            .unwrap_or("https://api.anthropic.com")
            .trim_end_matches('/');
        // 用户可能填的是完整端点；已带 messages 时不再重复拼接。
        if base.ends_with("/v1/messages") || base.ends_with("/messages") {
            return base.to_string();
        }
        // 标准化：去除尾部 /v1 避免路径重复
        let base = base.strip_suffix("/v1").unwrap_or(base);
        // DeepSeek 的 Anthropic 端点路径为 /messages（base 已含 /anthropic），
        // 其他供应商（Anthropic 官方、MiniMax 等）使用 /v1/messages
        if base.contains("api.deepseek.com") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
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
        // 同一轮多个工具结果必须放进同一条 user 消息。Anthropic 的
        // tool_result 要求紧跟 assistant 的 tool_use；拆成多条 user 消息
        // 会让并行工具结果丢失配对，甚至直接 400。
        let mut pending_tool_results: Vec<Value> = Vec::new();

        for msg in messages {
            use crate::llm::MessageRole;
            match msg.role {
                MessageRole::System => {
                    if !msg.content.is_empty() {
                        system_parts.push(msg.content.clone());
                    }
                }
                MessageRole::Tool => {
                    let tc_id = msg.tool_call_id.as_deref().unwrap_or("");
                    pending_tool_results.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tc_id,
                        "content": msg.content,
                    }));
                }
                MessageRole::User => {
                    let image_blocks: Vec<Value> = msg
                        .images
                        .as_ref()
                        .filter(|v| !v.is_empty())
                        .map(|images| {
                            images
                                .iter()
                                .map(|img| {
                                    serde_json::json!({
                                        "type": "image",
                                        "source": {
                                            "type": "base64",
                                            "media_type": img.mime_type,
                                            "data": img.data,
                                        },
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    if pending_tool_results.is_empty() && image_blocks.is_empty() {
                        // 无图时保持字符串 content（与既有输出逐字节一致）。
                        anthropic_msgs.push(serde_json::json!({
                            "role": "user",
                            "content": msg.content,
                        }));
                    } else {
                        let mut blocks: Vec<Value> = std::mem::take(&mut pending_tool_results);
                        // Anthropic 约定：tool_result 必须在最前，图像随后，文本最后。
                        blocks.extend(image_blocks);
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
                }
                MessageRole::Assistant => {
                    Self::flush_tool_results(&mut anthropic_msgs, &mut pending_tool_results);
                    // Anthropic 要求 thinking / redacted_thinking 按原序回到
                    // assistant content 开头；缺签名会导致工具续跑 400（R-27）。
                    let has_provider_blocks = msg
                        .provider_blocks
                        .as_ref()
                        .is_some_and(|blocks| !blocks.is_empty());
                    if has_provider_blocks || msg.tool_calls.is_some() {
                        let mut blocks: Vec<Value> = Vec::new();
                        if let Some(provider_blocks) = &msg.provider_blocks {
                            for block in provider_blocks {
                                match block {
                                    ProviderBlock::AnthropicThinking {
                                        thinking,
                                        signature,
                                    } => blocks.push(serde_json::json!({
                                        "type": "thinking",
                                        "thinking": thinking,
                                        "signature": signature,
                                    })),
                                    ProviderBlock::AnthropicRedactedThinking { data } => blocks
                                        .push(serde_json::json!({
                                            "type": "redacted_thinking",
                                            "data": data,
                                        })),
                                }
                            }
                        }
                        if !msg.content.is_empty() {
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": msg.content,
                            }));
                        }
                        if let Some(ref tcs) = msg.tool_calls {
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
                        }
                        anthropic_msgs.push(serde_json::json!({
                            "role": "assistant",
                            "content": blocks,
                        }));
                    } else {
                        // 纯文本 assistant → 字符串 content（与既有输出逐字节一致）
                        anthropic_msgs.push(serde_json::json!({
                            "role": "assistant",
                            "content": msg.content,
                        }));
                    }
                }
            }
        }

        Self::flush_tool_results(&mut anthropic_msgs, &mut pending_tool_results);

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };

        (system, anthropic_msgs)
    }

    /// 把已经收齐的工具结果作为一条 user 消息发出。
    fn flush_tool_results(anthropic_msgs: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if pending.is_empty() {
            return;
        }
        let blocks = std::mem::take(pending);
        anthropic_msgs.push(serde_json::json!({
            "role": "user",
            "content": blocks,
        }));
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

        let normalized = super::normalize_tool_pairs(messages, config.api_type);
        let (system, anthropic_msgs) = Self::convert_messages(&normalized);

        // Anthropic 要求 messages 非空
        let anthropic_msgs = if anthropic_msgs.is_empty() {
            vec![serde_json::json!({"role": "user", "content": ""})]
        } else {
            anthropic_msgs
        };

        let model_limit = self
            .registry
            .get_model(&config.provider, &config.model)
            .and_then(|entry| entry.max_output);
        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": crate::llm::chat_output_budget(config.max_tokens, model_limit),
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
        let _ = crate::llm::thinking::inject(
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

    /// 测试入口：让能力矩阵/请求体快照复用生产构造路径。
    #[cfg(test)]
    pub(crate) fn build_body_for_snapshot(
        &self,
        config: &LlmConfig,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        stream: bool,
    ) -> Result<Value, LlmError> {
        self.build_body(config, messages, tools, stream)
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
        Ok(LlmResponse {
            text,
            tool_calls,
            usage,
        })
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
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let context_input_tokens = Some(
            (input as u32)
                .saturating_add(cache_hit.unwrap_or(0))
                .saturating_add(cache_creation.unwrap_or(0)),
        );
        Some(TokenUsage {
            input_tokens: input as u32,
            context_input_tokens,
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
                    LlmError::Transport(format!("HTTP 请求失败: {e}"))
                }
            })?;

            let status = response.status();
            let retry_after_ms = retry_after_ms_from_headers(response.headers());
            let body: Value = response
                .json()
                .await
                .map_err(|e| LlmError::Parse(format!("响应 JSON 解析失败: {e}")))?;

            if !status.is_success() {
                let err_msg =
                    provider_error_detail(&body).unwrap_or_else(|| "未知错误".to_string());
                return Err(LlmError::http_with_retry_after(
                    status.as_u16(),
                    err_msg,
                    retry_after_ms,
                ));
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
                    LlmError::Transport(format!("HTTP 请求失败: {e}"))
                }
            })?;

            let status = response.status();
            let retry_after_ms = retry_after_ms_from_headers(response.headers());
            if !status.is_success() {
                let err_text = response.text().await.unwrap_or_default();
                let err_msg = serde_json::from_str::<Value>(&err_text)
                    .ok()
                    .and_then(|v| provider_error_detail(&v))
                    .unwrap_or(err_text);
                return Err(LlmError::http_with_retry_after(
                    status.as_u16(),
                    err_msg,
                    retry_after_ms,
                ));
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
        /// 缓存写入（cache_creation_input_tokens；无该字段 → None = 未观测）
        cache_creation_input_tokens: Option<u32>,
    },
    ContentBlockStart {
        index: usize,
        block_type: String,
        block_data: Value,
    },
    ContentBlockDelta {
        index: usize,
        delta: Value,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        /// SSE 的 stop_reason（end_turn / tool_use / max_tokens 等），
        /// 由 process_anthropic_event 映射为 FinishReason。
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
    /// thinking block → 签名（signature_delta 累积）
    signature: String,
    /// redacted_thinking block → 原始 data
    redacted: Option<String>,
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
    let mut finish_reason: Option<FinishReason> = None;
    let mut saw_message_stop = false;

    // 帧读取收口（2026-10-09）：字节缓冲、跨 chunk 无损，见 crate::llm::sse。
    if let Err(e) = crate::llm::sse::read_frames(stream, |frame| {
        if frame.trim().is_empty() {
            return;
        }
        if let Some(ev) = parse_anthropic_frame(frame) {
            if matches!(ev, AnthropicSseEvent::MessageStop) {
                saw_message_stop = true;
            }
            process_anthropic_event(
                &ev,
                on_event,
                &mut text,
                &mut reasoning,
                &mut blocks,
                &mut tool_calls,
                &mut usage,
                &mut finish_reason,
            );
        }
    })
    .await
    {
        return Err(LlmError::Transport(format!("流读取异常: {e}")));
    }

    if !saw_message_stop {
        return Err(LlmError::Transport(
            "流提前结束：未收到 message_stop".into(),
        ));
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
        finish_reason,
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
            let cache_creation_input_tokens = usage
                .and_then(|u| u.get("cache_creation_input_tokens"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            Some(AnthropicSseEvent::MessageStart {
                input_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
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
#[allow(clippy::too_many_arguments)]
fn process_anthropic_event(
    ev: &AnthropicSseEvent,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
    text_acc: &mut String,
    reasoning_acc: &mut String,
    blocks: &mut HashMap<usize, BlockAccumulator>,
    tool_calls: &mut Vec<ToolCall>,
    usage: &mut Option<TokenUsage>,
    finish_reason: &mut Option<FinishReason>,
) {
    match ev {
        AnthropicSseEvent::MessageStart {
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        } => {
            let context_input_tokens = Some(
                (*input_tokens)
                    .saturating_add((*cache_read_input_tokens).unwrap_or(0))
                    .saturating_add((*cache_creation_input_tokens).unwrap_or(0)),
            );
            let snapshot = TokenUsage {
                input_tokens: *input_tokens,
                context_input_tokens,
                output_tokens: 0,
                cache_hit_tokens: *cache_read_input_tokens,
                cache_miss_tokens: None,
            };
            *usage = Some(snapshot);
            on_event(StreamEvent::Usage { usage: snapshot });
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
            match block_type.as_str() {
                "tool_use" => {
                    acc.tool_id = block_data
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    acc.tool_name = block_data
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                "thinking" => {
                    if let Some(t) = block_data.get("thinking").and_then(|v| v.as_str()) {
                        acc.thinking.push_str(t);
                    }
                    if let Some(s) = block_data.get("signature").and_then(|v| v.as_str()) {
                        acc.signature.push_str(s);
                    }
                }
                "redacted_thinking" => {
                    acc.redacted = block_data
                        .get("data")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                _ => {}
            }
        }
        AnthropicSseEvent::ContentBlockDelta { index, delta } => {
            let acc = blocks.entry(*index).or_default();

            let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

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
                "signature_delta" => {
                    if let Some(s) = delta.get("signature").and_then(|v| v.as_str()) {
                        acc.signature.push_str(s);
                    }
                }
                _ => {}
            }
        }
        AnthropicSseEvent::ContentBlockStop { index } => {
            if let Some(acc) = blocks.remove(index) {
                match acc.block_type.as_str() {
                    "tool_use" => {
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
                    "thinking" if !acc.signature.is_empty() => {
                        on_event(StreamEvent::ProviderBlock {
                            block: ProviderBlock::AnthropicThinking {
                                thinking: acc.thinking,
                                signature: acc.signature,
                            },
                        });
                    }
                    "redacted_thinking" => {
                        if let Some(data) = acc.redacted {
                            on_event(StreamEvent::ProviderBlock {
                                block: ProviderBlock::AnthropicRedactedThinking { data },
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        AnthropicSseEvent::MessageDelta {
            stop_reason,
            output_tokens,
        } => {
            if let Some(raw) = stop_reason.as_deref() {
                if let Some(reason) = FinishReason::from_anthropic(raw) {
                    *finish_reason = Some(reason);
                }
            }
            if let Some(ref mut existing) = usage {
                existing.output_tokens = *output_tokens;
                on_event(StreamEvent::Usage { usage: *existing });
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
            .map(|(ev, data)| Ok(format!("event: {}\ndata: {}\n\n", ev, data).into_bytes()))
            .collect();
        stream::iter(chunks)
    }

    async fn collect_anthropic_events(frames: &[(&str, &str)]) -> (Vec<StreamEvent>, StreamResult) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_c = events.clone();
        let on_event = move |e: StreamEvent| {
            events_c.lock().unwrap().push(e);
        };

        let s = anthropic_sse_stream(frames);
        let result = parse_anthropic_sse_stream(s, &on_event).await.unwrap();

        let collected = events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| !matches!(event, StreamEvent::Usage { .. }))
            .cloned()
            .collect();
        (collected, result)
    }

    // ── 消息转换 ──

    #[test]
    fn convert_system_to_separate_field() {
        let msgs = vec![ChatMessage::system("你是 PRTS"), ChatMessage::user("你好")];
        let (sys, an_msgs) = AnthropicMessagesClient::convert_messages(&msgs);
        assert_eq!(sys, Some("你是 PRTS".into()));
        assert_eq!(an_msgs.len(), 1);
        assert_eq!(an_msgs[0]["role"], "user");
    }

    #[test]
    fn convert_multiple_system_joined() {
        let msgs = vec![ChatMessage::system("规则 A"), ChatMessage::system("规则 B")];
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
    fn convert_parallel_tool_results_into_one_user_message() {
        let call = |id: &str| ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        };
        let msgs = vec![
            ChatMessage::assistant_with_tools("", vec![call("toolu_1"), call("toolu_2")], None),
            ChatMessage::tool_result("toolu_1", "first"),
            ChatMessage::tool_result("toolu_2", "second"),
        ];
        let (_sys, an_msgs) = AnthropicMessagesClient::convert_messages(&msgs);
        assert_eq!(an_msgs.len(), 2);
        assert_eq!(an_msgs[1]["role"], "user");
        let results = an_msgs[1]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "toolu_1");
        assert_eq!(results[1]["tool_use_id"], "toolu_2");
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
            provider_blocks: None,
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
        assert_eq!(content[1]["input"]["name"], "源石虫");
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
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":10}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"博士"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            ),
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
    async fn max_tokens_stop_reason_is_reported() {
        let frames = &[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_max","usage":{"input_tokens":10}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"半截回答"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":5}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (_, result) = collect_anthropic_events(frames).await;

        assert_eq!(
            result.finish_reason,
            Some(crate::llm::FinishReason::MaxTokens)
        );
        assert_eq!(result.text, "半截回答");
    }

    #[tokio::test]
    async fn stream_without_message_stop_is_transport_error() {
        let frames = &[
            (
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":3}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"半截回答"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            ),
        ];
        let on_event = |_e: StreamEvent| {};
        let stream = anthropic_sse_stream(frames);
        let err = parse_anthropic_sse_stream(stream, &on_event)
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Transport(_)), "{err:?}");
    }

    #[tokio::test]
    async fn tool_use_stream() {
        let frames = &[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_2","usage":{"input_tokens":20}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"echo","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"mess"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"age\":\"hi\"}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (events, result) = collect_anthropic_events(frames).await;

        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], StreamEvent::ToolCall { id, name, arguments }
                if id == "toolu_1"
                && name == "echo"
                && arguments == r#"{"message":"hi"}"#
            )
        );

        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "echo");
        assert!(result.text.is_empty());
        // 契约不变量 I1~I5（模块 4 P1）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn mixed_text_and_tool_use() {
        let frames = &[
            (
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#,
            ),
            // text block
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"我来查询"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            // tool_use block
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"echo","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":1}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":15}}"#,
            ),
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
            (
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"需要查询数据库"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            // text block after thinking
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"结果如下"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":1}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (events, result) = collect_anthropic_events(frames).await;

        assert_eq!(events.len(), 2);
        assert!(
            matches!(events[0], StreamEvent::ReasoningDelta { ref content } if content == "需要查询数据库")
        );
        assert!(matches!(events[1], StreamEvent::TextDelta { .. }));
        assert_eq!(result.reasoning, Some("需要查询数据库".into()));
        assert_eq!(result.text, "结果如下");
    }

    #[tokio::test]
    async fn thinking_signature_is_emitted_and_roundtrips_to_request() {
        let frames = &[
            (
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"echo","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":1}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":15}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ];

        let (events, _result) = collect_anthropic_events(frames).await;
        let block = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::ProviderBlock {
                    block:
                        ProviderBlock::AnthropicThinking {
                            thinking,
                            signature,
                        },
                } => Some((thinking.as_str(), signature.as_str())),
                _ => None,
            })
            .expect("thinking 签名应随流式事件发出");
        assert_eq!(block, ("想", "sig-abc"));

        let mut msg = ChatMessage::assistant("继续");
        msg.provider_blocks = Some(vec![ProviderBlock::AnthropicThinking {
            thinking: "想".into(),
            signature: "sig-abc".into(),
        }]);
        let (_sys, msgs) = AnthropicMessagesClient::convert_messages(&[msg]);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["signature"], "sig-abc");
        assert_eq!(blocks[1]["type"], "text");
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

    #[test]
    fn usage_context_includes_cache_read_and_creation() {
        let usage = serde_json::json!({
            "input_tokens": 1000,
            "output_tokens": 10,
            "cache_read_input_tokens": 90000,
            "cache_creation_input_tokens": 500
        });
        let parsed = AnthropicMessagesClient::extract_usage(&usage).unwrap();
        assert_eq!(parsed.input_tokens, 1000, "原始 input 不变");
        assert_eq!(parsed.context_input_tokens, Some(91500));
    }

    #[test]
    fn build_url_keeps_full_messages_endpoint() {
        let config = LlmConfig {
            provider: "anthropic".into(),
            base_url: Some("https://api.anthropic.com/v1/messages".into()),
            ..Default::default()
        };
        assert_eq!(
            AnthropicMessagesClient::build_url(&config),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn build_url_normalizes_v1_base() {
        let config = LlmConfig {
            provider: "anthropic".into(),
            base_url: Some("https://api.anthropic.com/v1/".into()),
            ..Default::default()
        };
        assert_eq!(
            AnthropicMessagesClient::build_url(&config),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn build_url_keeps_deepseek_anthropic_path() {
        let config = LlmConfig {
            provider: "deepseek".into(),
            base_url: Some("https://api.deepseek.com/anthropic".into()),
            ..Default::default()
        };
        assert_eq!(
            AnthropicMessagesClient::build_url(&config),
            "https://api.deepseek.com/anthropic/messages"
        );
    }
}
