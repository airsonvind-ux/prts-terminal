//! OpenAI Compatible LLM 客户端
//!
//! 一个实现覆盖所有 OpenAI Chat Completions 协议供应商
//!（DeepSeek、OpenAI、Ollama、vLLM、LocalAI 等）。
//! 供应商差异通过 `LlmConfig` 字段做条件分支，内部不按 vendor 拆分实现类。
//!
//! ## 供应商差异处理
//!
//! 供应商差异由 compat 驱动，不硬编码 provider 条件分支。
//! 详见 `models.json` + `resolve_compat()`。
//!
//! | 行为                | compat 驱动                    |
//! |---------------------|--------------------------------|
//! | Thinking 参数       | `request_param` 非空时附加      |
//! | reasoning_content   | `carrier` 非 "reasoning_content" 时剥离 |
//! | Authorization       | `auth == "none"` 时不发送       |
//! | 自定义 Headers       | headers 非空                   | 解析 JSON 逐条附加                 |
//!
//! ## SSE 解析策略
//!
//! 不引入第三方 SSE crate。手动解析字节流：按 `\n` 拆行，过滤 `data: ` 前缀，
//! 每行反序列化为 JSON。tool_call arguments 跨 chunk 分片到达，通过
//! `HashMap<usize, ToolCallAccumulator>` 按 index 累积，`[DONE]` 时统一 emit。

use std::collections::HashMap;
use std::pin::Pin;
use std::future::Future;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::header;
use serde_json::Value;

use crate::debug;

use super::{
    AuthMode, ChatMessage, LlmClient, LlmConfig, LlmError, LlmResponse,
    ModelRegistry, ResolvedCompat,
    StreamEvent, StreamResult, TokenUsage, ToolCall, ToolCallFunction, ToolDef,
    default_base_url,
};

// ═══════════════════════════════════════════════════════
// OpenAiCompatibleClient
// ═══════════════════════════════════════════════════════

/// OpenAI Chat Completions 协议客户端。
///
/// 内部持有 `reqwest::Client` 以复用连接池。
/// 无状态——每次调用方法传入 `LlmConfig` 和 `messages`，
/// 不跨请求保持状态。
pub struct OpenAiCompatibleClient {
    http: reqwest::Client,
    registry: Arc<ModelRegistry>,
}

impl OpenAiCompatibleClient {
    pub fn new(registry: Arc<ModelRegistry>) -> Self {
        Self {
            http: super::http_client(),
            registry,
        }
    }

    // ── URL 构造 ──

    /// 拼接完整的 chat/completions 端点 URL。
    fn build_url(config: &LlmConfig) -> Result<String, LlmError> {
        let base = config
            .base_url
            .as_deref()
            .or_else(|| default_base_url(&config.provider))
            .ok_or_else(|| LlmError::Config("未配置 Base URL，请选择供应商".into()))?;
        Ok(format!("{}/chat/completions", base.trim_end_matches('/')))
    }

    /// carrier 为 "reasoning_content" 表示 API 协议原生支持该字段，不剥离。
    /// carrier 为 null 或其他值表示协议不支持，需剥离以免 API 拒收。
    fn should_strip_reasoning(compat: &ResolvedCompat) -> bool {
        compat.carrier.as_deref() != Some("reasoning_content")
    }

    // ── 请求体构造 ──

    /// 构造 HTTP POST 的 JSON body。
    ///
    /// 公共部分：model、messages、tools、stream。
    /// compat 驱动：reasoning_content 剥离、thinking 参数、auth 方式
    /// 均从 ModelRegistry 解析的 ResolvedCompat 读取，不依赖 provider 名。
    fn build_body(
        &self,
        config: &LlmConfig,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        stream: bool,
    ) -> Result<Value, LlmError> {
        if config.model.is_empty() {
            return Err(LlmError::Config("未选择模型".into()));
        }

        // ── 解析 compat：声明层合并 + 效果层计算 ──
        let compat = self.registry.get_resolved_compat(
            &config.provider,
            &config.model,
            config.api_type,
        );

        // 序列化 messages（此时 reasoning_content 字段由 ChatMessage 的
        // #[serde(skip_serializing_if)] 控制——None 时自动省略）
        let messages_json = serde_json::to_value(messages).map_err(|e| {
            LlmError::Parse(format!("消息序列化失败: {e}"))
        })?;

        // compat 驱动剥离：carrier 为 "reasoning_content" 的协议原生支持，不剥离
        let messages_json = if Self::should_strip_reasoning(&compat) {
            Self::strip_reasoning_content(messages_json)
        } else {
            messages_json
        };

        let mut body = serde_json::json!({
            "model": &config.model,
            "messages": messages_json,
            "stream": stream,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools).map_err(|e| {
                LlmError::Parse(format!("工具列表序列化失败: {e}"))
            })?;
        }

        // 思考强度注入（裁决②：未设置 thinking_level = 完全不注入 thinking
        // 相关参数，含 request_param 附加——旧无条件附加 {"type":"enabled"}
        // 对 enable_thinking 布尔参数是错误形态，已移除；已设档才进各族 wire）
        crate::llm::thinking::inject(
            &mut body,
            config.api_type,
            &compat,
            &self.registry,
            &config.provider,
            &config.model,
            config.thinking_level.as_deref(),
        );

        // 流式请求附加 stream_options（用于获取 usage）
        if stream {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }

        Ok(body)
    }

    /// 遍历 messages JSON 数组，逐项移除 `reasoning_content` 键。
    ///
    /// 保留 serde_json::Value 格式，不做字符串替换——确保嵌套结构
    /// 中的 reasoning_content 不会被误删。
    fn strip_reasoning_content(mut messages: Value) -> Value {
        if let Some(arr) = messages.as_array_mut() {
            for msg in arr {
                if let Some(obj) = msg.as_object_mut() {
                    obj.remove("reasoning_content");
                }
            }
        }
        messages
    }

    // ── HTTP 请求发送 ──

    /// 构造带 headers 的 HTTP POST 请求。
    fn build_request(
        &self,
        config: &LlmConfig,
        body: &Value,
    ) -> Result<reqwest::RequestBuilder, LlmError> {
        let url = Self::build_url(config)?;
        let mut req = self.http
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .json(body);

        // compat 驱动认证：auth = "none" 时不发送 Authorization（Ollama 等）
        let compat = self.registry.get_resolved_compat(
            &config.provider,
            &config.model,
            config.api_type,
        );
        if compat.auth != Some(AuthMode::None) {
            req = req.header(header::AUTHORIZATION, format!("Bearer {}", config.api_key));
        }

        // 自定义 Headers（JSON 字符串解析）
        if let Some(ref headers_str) = config.headers {
            if let Ok(json) =
                serde_json::from_str::<Value>(headers_str)
            {
                if let Some(obj) = json.as_object() {
                    for (k, v) in obj {
                        if let Some(val) = v.as_str() {
                            req = req.header(k.as_str(), val);
                        }
                    }
                }
            }
        }

        Ok(req)
    }

    // ── Usage 提取 ──

    /// 从 API 返回的 usage JSON 提取归一化 TokenUsage。
    ///
    /// 兼容 DeepSeek（prompt_tokens / completion_tokens）和
    /// OpenAI（input_tokens / output_tokens）两种字段名。
    fn extract_usage(usage: &Value) -> Option<TokenUsage> {
        let input = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(|v| v.as_u64())?;
        let output = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(|v| v.as_u64())?;
        Some(TokenUsage {
            input_tokens: input as u32,
            output_tokens: output as u32,
        })
    }

    // ── 非流式响应解析 ──

    /// 解析 chat() 的非流式响应体，提取 LlmResponse。
    fn parse_chat_response(body: &Value) -> Result<LlmResponse, LlmError> {
        let choice = body["choices"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| LlmError::Parse("响应缺少 choices 数组".into()))?;

        let msg = choice
            .get("message")
            .ok_or_else(|| LlmError::Parse("响应缺少 message 字段".into()))?;

        let text = msg
            .get("content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let tool_calls = msg.get("tool_calls").and_then(|v| {
            serde_json::from_value::<Vec<ToolCall>>(v.clone()).ok()
        }).filter(|tc| !tc.is_empty());

        Ok(LlmResponse { text, tool_calls })
    }
}

// ═══════════════════════════════════════════════════════
// LlmClient trait 实现
// ═══════════════════════════════════════════════════════

impl LlmClient for OpenAiCompatibleClient {
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
                return Err(LlmError::Api(format!("HTTP {}: {err_msg}", status.as_u16())));
            }

            OpenAiCompatibleClient::parse_chat_response(&body)
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
            // 尝试主请求（含 stream_options）
            match self.do_chat_stream(config, messages, tools, true, &*on_event).await {
                Ok(result) => return Ok(result),
                Err(LlmError::Api(msg)) if msg.contains("stream_options") => {
                    // 降级：移除 stream_options 重试
                    log::warn!("[LLM] stream_options 不被支持，降级重试（无 usage 信息）");
                }
                Err(e) => return Err(e),
            }

            // 降级请求（不含 stream_options）
            self.do_chat_stream(config, messages, tools, false, &*on_event).await
        })
    }
}

impl OpenAiCompatibleClient {
    /// 执行一次流式请求。
    ///
    /// `with_stream_options` 控制是否在请求体中包含 `stream_options` 字段。
    /// 分离此方法的目的：当 API 不支持 stream_options 时，外层可以不重新构造
    /// 整个 body，只需传入 false 重试。
    async fn do_chat_stream(
        &self,
        config: &LlmConfig,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        with_stream_options: bool,
        on_event: &(dyn Fn(StreamEvent) + Send + Sync),
    ) -> Result<StreamResult, LlmError> {
        let mut body = self.build_body(config, messages, tools, true)?;

        if !with_stream_options {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("stream_options");
            }
        }

        let req = self.build_request(config, &body)?;

        log::debug!(
            "[LLM] 请求 → {} {} | 消息{}条 | 流式={}",
            config.provider,
            config.model,
            messages.len(),
            with_stream_options,
        );
        log::trace!("[LLM] 请求体：{}", serde_json::to_string(&body).unwrap_or_default());

        let response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout(e.to_string())
            } else {
                LlmError::Api(format!("HTTP 请求失败: {e}"))
            }
        })?;

        let status = response.status();
        log::debug!("[LLM] 响应 HTTP {}", status.as_u16());
        if !status.is_success() {
            let (err_text, read_failed) = match response.text().await {
                Ok(t) => (t, false),
                Err(e) => (format!("(响应体读取失败: {e})"), true),
            };
            // 尝试提取 JSON error message，失败则用原始文本
            let err_msg = if read_failed {
                err_text
            } else {
                serde_json::from_str::<Value>(&err_text)
                    .ok()
                    .and_then(|v| {
                        v.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or(err_text)
            };
            return Err(LlmError::Api(format!("HTTP {}: {err_msg}", status.as_u16())));
        }

        // 将 reqwest 的 Bytes 流转换为 Vec<u8> 流，交给通用 SSE 解析器
        let byte_stream = response
            .bytes_stream()
            .map(|r| r.map(|b| b.to_vec()).map_err(|e| e.to_string()));

        parse_sse_stream(byte_stream, on_event).await
    }
}

// ═══════════════════════════════════════════════════════
// SSE 流式解析
// ═══════════════════════════════════════════════════════

/// tool_call 跨 chunk 的累积器。
///
/// OpenAI 协议中，单次 tool_call 的 arguments 可能跨多个 SSE chunk 分片到达。
/// 每个 tool_call 用 index 标识，`id` 和 `name` 在首个 chunk 填充，
/// `arguments_str` 在后续 chunk 逐片拼接。
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments_str: String,
}

impl ToolCallAccumulator {
    fn into_tool_call(self) -> Option<ToolCall> {
        let id = self.id?;
        let name = self.name?;
        Some(ToolCall {
            id,
            call_type: "function".into(),
            function: ToolCallFunction {
                name,
                arguments: self.arguments_str,
            },
        })
    }
}

/// 通用 SSE 流式解析函数。
///
/// 从字节流中逐行读取 SSE 事件，反序列化为 OpenAI 兼容格式的 chunk JSON，
/// 根据 delta 类型调用 `on_event` 推送对应事件。流走完后返回 `StreamResult`。
///
/// 泛型参数：
///   `St` — 字节流（`Vec<u8>` 或 `bytes::Bytes`）
///   `E`  — 错误类型（生产环境 reqwest::Error，测试用 &str）
async fn parse_sse_stream<St, E>(
    mut stream: St,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
) -> Result<StreamResult, LlmError>
where
    St: futures_util::Stream<Item = Result<Vec<u8>, E>> + Unpin,
    E: std::fmt::Display,
{
    // ── 累积状态 ──
    let mut text = String::new();              // content delta 累积
    let mut reasoning = String::new();         // reasoning delta 累积
    let mut tool_call_acc: HashMap<usize, ToolCallAccumulator> = HashMap::new();
    let mut usage: Option<TokenUsage> = None;

    // 行缓冲：SSE chunk 可能跨越多个 "\n\n" 分隔的事件帧
    let mut line_buf = String::new();

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                line_buf.push_str(&String::from_utf8_lossy(&bytes));

                // 逐帧处理：按 "\n\n" 分割完整的事件帧
                // 未闭合的帧留在 line_buf 中，等待下一个 chunk 拼接
                while let Some(pos) = line_buf.find("\n\n") {
                    let frame = line_buf[..pos].to_string();
                    line_buf = line_buf[pos + 2..].to_string();

                    if frame.is_empty() {
                        continue;
                    }

                    // 处理当前帧
                    process_sse_frame(
                        &frame,
                        on_event,
                        &mut text,
                        &mut reasoning,
                        &mut tool_call_acc,
                        &mut usage,
                    );
                }
            }
            Err(e) => {
                // 流异常：若有已累积内容，emit 错误但不丢弃已有输出
                let error_msg = format!("流读取异常: {e}");
                if !text.is_empty() || !tool_call_acc.is_empty() {
                    on_event(StreamEvent::TextDelta {
                        content: format!("\n\n[错误] {error_msg}"),
                    });
                }
                return Err(LlmError::Api(error_msg));
            }
        }
    }

    // ── 流结束：emit 所有累积的 tool_call，同时收集到 Vec 供 orchestrator 判断 ──
    let mut indices: Vec<usize> = tool_call_acc.keys().copied().collect();
    indices.sort();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for idx in indices {
        if let Some(acc) = tool_call_acc.remove(&idx) {
            if let Some(tc) = acc.into_tool_call() {
                tool_calls.push(ToolCall {
                    id: tc.id.clone(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    },
                });
                on_event(StreamEvent::ToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                });
            }
        }
    }

    // 关闭未完成的 reasoning 流
    // （不 emit 事件——已在 ReasoningDelta 中逐片推送）

    Ok(StreamResult {
        text,
        tool_calls,  // orchestrator 据此判断是否需要继续循环
        reasoning: if reasoning.is_empty() { None } else { Some(reasoning) },
        usage,
    })
}

/// 处理单个 SSE 事件帧（一行或多行 `data: ...` 组成）。
///
/// 单帧内可能有多条 `data:` 行（OpenAI 协议允许），逐条解析。
/// `data: [DONE]` 标记流终止——此时如果 choices 为空的 chunk 带 usage，
/// 该 usage 已被上一轮 process_sse_chunk 暂存。
fn process_sse_frame(
    frame: &str,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
    text: &mut String,
    reasoning: &mut String,
    tool_call_acc: &mut HashMap<usize, ToolCallAccumulator>,
    usage: &mut Option<TokenUsage>,
) {
    for line in frame.lines() {
        let data = line.strip_prefix("data: ");
        let Some(data) = data else { continue; };
        log::trace!("[LLM] SSE ← {}", data);

        if data == "[DONE]" {
            return; // 流终止标记，不解析
        }

        // 反序列化 JSON chunk
        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        process_sse_chunk(&chunk, on_event, text, reasoning, tool_call_acc, usage);
    }
}

/// 处理反序列化后的单个 JSON chunk。
///
/// 处理顺序：
/// 1. 无条件提取 usage（有就取，不依赖 choices 结构）
/// 2. reasoning delta（DeepSeek: reasoning_content，Ollama: reasoning）
/// 3. text delta（content）
/// 4. tool_call delta（按 index 累积）
fn process_sse_chunk(
    chunk: &Value,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
    text_acc: &mut String,
    reasoning_acc: &mut String,
    tool_call_acc: &mut HashMap<usize, ToolCallAccumulator>,
    usage: &mut Option<TokenUsage>,
) {
    // ── 无条件提取 usage ──
    // 不同供应商在不同 chunk 中返回 usage：有的在单独 chunk（choices 为空或缺失），
    // 有的随最后一条 delta 一起返回。不论 choices 结构如何，有就取。
    if let Some(u) = chunk.get("usage") {
        let extracted = OpenAiCompatibleClient::extract_usage(u);
        if let Some(ref tu) = extracted {
            debug!("action" => "usage_extracted", "input_tokens" => tu.input_tokens, "output_tokens" => tu.output_tokens);
        }
        *usage = extracted;
    }

    // ── 遍历 choices（仅 delta 处理） ──
    let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) else {
        return;
    };
    if choices.is_empty() {
        return;
    }

    for choice in choices {
        let Some(delta) = choice.get("delta") else { continue };

        // reasoning_content（DeepSeek）或 reasoning（Ollama）
        // 双键兼容，priority: reasoning_content > reasoning
        let reasoning_str = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if let Some(rs) = reasoning_str {
            reasoning_acc.push_str(rs);
            on_event(StreamEvent::ReasoningDelta {
                content: rs.to_string(),
            });
        }

        // text delta
        if let Some(ct) = delta.get("content").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            text_acc.push_str(ct);
            on_event(StreamEvent::TextDelta {
                content: ct.to_string(),
            });
        }

        // tool_calls delta（按 index 累积）
        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let Some(index) = tc.get("index").and_then(|v| v.as_u64()) else { continue };
                let idx = index as usize;

                let acc = tool_call_acc.entry(idx).or_default();

                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    acc.id = Some(id.to_string());
                }
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                        acc.name = Some(name.to_string());
                    }
                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                        acc.arguments_str.push_str(args);
                    }
                }
            }
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

    // ── SSE 测试辅助：从字符串数组构造字节流 ──

    /// 将一组 SSE 数据帧转换为字节流。
    /// 每个字符串元素为一条 `data: ...` 行，自动追加 `\n\n`。
    fn sse_stream(lines: &[String]) -> impl futures_util::Stream<Item = Result<Vec<u8>, &str>> {
        let chunks: Vec<Result<Vec<u8>, &str>> = lines
            .iter()
            .map(|l| Ok(format!("{}\n\n", l).into_bytes()))
            .collect();
        stream::iter(chunks)
    }

    /// 运行 SSE 解析并收集事件。
    async fn collect_events<'a>(
        lines: &'a [String],
    ) -> (Vec<StreamEvent>, StreamResult) {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_c = events.clone();
        let on_event = move |e: StreamEvent| {
            events_c.lock().unwrap().push(e);
        };

        let stream = sse_stream(lines);
        let result = parse_sse_stream(stream, &on_event).await.unwrap();

        let collected = events.lock().unwrap().clone();
        (collected, result)
    }

    // ── chunk 构造辅助 ──

    fn text_delta_line(content: &str) -> String {
        format!(
            r#"data: {{"choices":[{{"delta":{{"content":"{}"}}}}]}}"#,
            content
        )
    }

    fn reasoning_delta_line(content: &str) -> String {
        format!(
            r#"data: {{"choices":[{{"delta":{{"reasoning_content":"{}"}}}}]}}"#,
            content
        )
    }

    fn ollama_reasoning_line(content: &str) -> String {
        format!(
            r#"data: {{"choices":[{{"delta":{{"reasoning":"{}"}}}}]}}"#,
            content
        )
    }

    fn usage_chunk_line(prompt: u32, completion: u32) -> String {
        format!(
            r#"data: {{"choices":[],"usage":{{"prompt_tokens":{},"completion_tokens":{}}}}}"#,
            prompt, completion
        )
    }

    fn done_line() -> String {
        "data: [DONE]".to_string()
    }

    // ── 测试 ──

    #[tokio::test]
    async fn pure_text_delta() {
        let lines = vec![
            text_delta_line("你好"),
            text_delta_line("，博士"),
            done_line(),
        ];
        let (events, result) = collect_events(&lines).await;

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::TextDelta { ref content } if content == "你好"));
        assert!(matches!(events[1], StreamEvent::TextDelta { ref content } if content == "，博士"));

        assert_eq!(result.text, "你好，博士");
        assert!(result.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn reasoning_delta_deepseek() {
        let lines = vec![
            reasoning_delta_line("需要查询数据库"),
            done_line(),
        ];
        let (events, result) = collect_events(&lines).await;

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::ReasoningDelta { ref content } if content == "需要查询数据库"));

        assert_eq!(result.reasoning, Some("需要查询数据库".into()));
    }

    #[tokio::test]
    async fn reasoning_delta_ollama_field_name() {
        // Ollama 使用 "reasoning" 字段名而非 "reasoning_content"
        let lines = vec![
            ollama_reasoning_line("分析中"),
            done_line(),
        ];
        let (events, result) = collect_events(&lines).await;

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::ReasoningDelta { ref content } if content == "分析中"));

        assert_eq!(result.reasoning, Some("分析中".into()));
        // 契约不变量 I1~I5（模块 4 P1）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn tool_call_fragmented_arguments() {
        // 模拟 arguments 跨两个 chunk 分片到达
        let lines = vec![
            // 第一个 chunk：id、name、arguments 前半
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":"{\"mess"}}]}}]}"#.to_string(),
            // 第二个 chunk：arguments 后半
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"age\":\"hi\"}"}}]}}]}"#.to_string(),
            done_line(),
        ];
        let (events, result) = collect_events(&lines).await;

        // 两个 chunk 的 tool_call 按 index 累积，[DONE] 时统一 emit
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::ToolCall { id, name, arguments }
            if id == "call_1"
            && name == "echo"
            && arguments == r#"{"message":"hi"}"#
        ));

        // [DONE] 后 tool_calls 被 emit 为事件，同时也在 StreamResult 中提供
        // 供 orchestrator 判断是否需要继续调度循环
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "echo");
        // 契约不变量 I2/I5：arguments 端到端 raw（分片累积后原样直通）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn multi_tool_call_parallel() {
        let lines = vec![
            // index 0 和 index 1 同时出现，各自独立累积
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c0","function":{"name":"f1","arguments":"{}"}},{"index":1,"id":"c1","function":{"name":"f2","arguments":"{}"}}]}}]}"#.to_string(),
            done_line(),
        ];
        let (events, _result) = collect_events(&lines).await;

        assert_eq!(events.len(), 2);
        // 按 index 排序 emit：index 0 先、index 1 后
        assert!(matches!(&events[0], StreamEvent::ToolCall { id, .. } if id == "c0"));
        assert!(matches!(&events[1], StreamEvent::ToolCall { id, .. } if id == "c1"));
    }

    #[tokio::test]
    async fn usage_chunk_extraction() {
        let lines = vec![
            text_delta_line("回答结束"),
            usage_chunk_line(150, 42),
            done_line(),
        ];
        let (_events, result) = collect_events(&lines).await;

        assert_eq!(result.text, "回答结束");
        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 42);
    }

    #[tokio::test]
    async fn empty_reasoning_filtered() {
        // 空字符串的 reasoning 不应 emit 事件
        let lines = vec![
            r#"data: {"choices":[{"delta":{"reasoning_content":""}}]}"#.to_string(),
            done_line(),
        ];
        let (events, result) = collect_events(&lines).await;

        assert!(events.is_empty());
        assert_eq!(result.reasoning, None);
    }

    // ── reasoning_content 剥离 ──

    #[test]
    fn strip_reasoning_removes_from_all_messages() {
        let messages = serde_json::json!([
            { "role": "system", "content": "sys" },
            { "role": "user", "content": "hi", "reasoning_content": "should be removed" },
            { "role": "assistant", "content": "hey", "reasoning_content": "also removed" },
        ]);

        let stripped = OpenAiCompatibleClient::strip_reasoning_content(messages);
        let arr = stripped.as_array().unwrap();

        // system 消息无 reasoning_content，不受影响
        assert!(arr[0].get("reasoning_content").is_none());
        // user 消息的 reasoning_content 被移除
        assert!(arr[1].get("reasoning_content").is_none());
        assert_eq!(arr[1]["content"], "hi");
        // assistant 消息的 reasoning_content 被移除
        assert!(arr[2].get("reasoning_content").is_none());
        assert_eq!(arr[2]["content"], "hey");
    }

    // ── usage 提取 ──

    #[test]
    fn extract_usage_deepseek_format() {
        let json = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
        });
        let usage = OpenAiCompatibleClient::extract_usage(&json).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn extract_usage_openai_format() {
        let json = serde_json::json!({
            "input_tokens": 200,
            "output_tokens": 80,
        });
        let usage = OpenAiCompatibleClient::extract_usage(&json).unwrap();
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 80);
    }

    // ── should_strip_reasoning ──

    #[test]
    fn carrier_reasoning_content_does_not_strip() {
        let compat = ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            ..Default::default()
        };
        assert!(!OpenAiCompatibleClient::should_strip_reasoning(&compat));
    }

    #[test]
    fn null_carrier_strips() {
        let compat = ResolvedCompat::default();
        assert!(OpenAiCompatibleClient::should_strip_reasoning(&compat));
    }

    #[test]
    fn other_carrier_strips() {
        let compat = ResolvedCompat {
            carrier: Some("thinking_blocks".into()),
            ..Default::default()
        };
        assert!(OpenAiCompatibleClient::should_strip_reasoning(&compat));
    }
}
