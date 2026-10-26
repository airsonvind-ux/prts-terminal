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
//! | stream_options      | `supports_usage_in_streaming == false` 时省略 |
//! | Authorization       | `auth == "none"` 时不发送       |
//! | 自定义 Headers       | headers 非空                   | 解析 JSON 逐条附加                 |
//!
//! ## SSE 解析策略
//!
//! 不引入第三方 SSE crate。手动解析字节流：按 `\n` 拆行，过滤 `data: ` 前缀，
//! 每行反序列化为 JSON。tool_call arguments 跨 chunk 分片到达，通过
//! `HashMap<usize, ToolCallAccumulator>` 按 index 累积，流终止时统一 emit。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::header;
use serde_json::Value;

use crate::debug;

use super::{
    default_base_url, provider_error_detail, retry_after_ms_from_headers, AuthMode, ChatMessage,
    FinishReason, LlmClient, LlmConfig, LlmError, LlmResponse, ModelRegistry, ReasoningPolicy,
    ResolvedCompat, StreamEvent, StreamResult, TokenUsage, ToolCall, ToolCallFunction, ToolDef,
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
        let base = base.trim_end_matches('/');
        // 用户可能填的是完整端点；已带 /chat/completions 时不再重复拼接。
        if base.ends_with("/chat/completions") {
            return Ok(base.to_string());
        }
        Ok(format!("{base}/chat/completions"))
    }

    /// carrier 为 "reasoning_content" 表示 API 协议原生支持该字段，不剥离。
    /// carrier 为 null 或其他值表示协议不支持，需剥离以免 API 拒收。
    fn should_strip_reasoning(compat: &ResolvedCompat) -> bool {
        compat.carrier.as_deref() != Some("reasoning_content")
    }

    // ── 请求体构造 ──

    /// 序列化消息为 OpenAI Chat Completions 请求体格式。
    ///
    /// 仅「带 images 的消息」把 content 从字符串展开为 content block 数组
    /// （`[{"type":"text"},{"type":"image_url"}...]`，OpenAI 多模态格式）；
    /// 其余消息逐字节保持既有输出（不影响无图路径与 KV cache 前缀）。
    fn messages_to_json(messages: &[ChatMessage]) -> Result<Value, LlmError> {
        let mut out: Vec<Value> = Vec::with_capacity(messages.len());
        for m in messages {
            let mut v = serde_json::to_value(m)
                .map_err(|e| LlmError::Parse(format!("消息序列化失败: {e}")))?;
            if let Some(images) = m.images.as_ref().filter(|v| !v.is_empty()) {
                let mut parts: Vec<Value> = vec![serde_json::json!({
                    "type": "text",
                    "text": m.content,
                })];
                for img in images {
                    parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", img.mime_type, img.data),
                        },
                    }));
                }
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("content".to_string(), Value::Array(parts));
                }
            }
            out.push(v);
        }
        Ok(Value::Array(out))
    }

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
        let compat =
            self.registry
                .get_resolved_compat(&config.provider, &config.model, config.api_type);

        // 序列化 messages（此时 reasoning_content 字段由 ChatMessage 的
        // #[serde(skip_serializing_if)] 控制——None 时自动省略）。
        // 带图像的 user 消息把 content 展开为 [text, image_url...] 数组。
        let normalized = super::normalize_tool_pairs(messages, config.api_type);
        let messages_json = Self::messages_to_json(&normalized)?;

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

        // 只有模型显式声明 output_budget 时才写输出字段；未声明模型保持
        // 既有“不发送 max_tokens”行为，避免改变全部请求体。
        if let Some(field) = compat
            .output_budget
            .as_ref()
            .and_then(|budget| budget.field)
        {
            let key = match field {
                crate::llm::compat::OutputField::MaxTokens => "max_tokens",
                crate::llm::compat::OutputField::MaxCompletionTokens => "max_completion_tokens",
                crate::llm::compat::OutputField::MaxOutputTokens => {
                    return Err(LlmError::Config(
                        "OpenAI-compatible 协议不支持 output_budget.field=max_output_tokens".into(),
                    ));
                }
            };
            let model_limit = self
                .registry
                .get_model(&config.provider, &config.model)
                .and_then(|entry| entry.max_output);
            body[key] = serde_json::json!(crate::llm::chat_output_budget(
                config.max_tokens,
                model_limit
            ));
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)
                .map_err(|e| LlmError::Parse(format!("工具列表序列化失败: {e}")))?;
        }

        // 思考强度注入（生效档 = 请求档或模型默认档；非 reasoning / 未知模型
        // 不注入。旧无条件附加 {"type":"enabled"} 对 enable_thinking 布尔参数
        // 是错误形态，已移除；各族 wire 由 compat.request_param 分流）
        let effective_level = crate::llm::thinking::inject(
            &mut body,
            config.api_type,
            &compat,
            &self.registry,
            &config.provider,
            &config.model,
            config.thinking_level.as_deref(),
        );

        // Kimi 等声明的模型要求推理开启时 assistant 消息带 carrier 键；
        // 空串合法，但不能缺键。必须在 require_tool_call 校验前补齐。
        if compat.assistant_reasoning == crate::llm::compat::AssistantReasoning::Required
            && effective_level.is_some_and(|level| level != "off")
        {
            body["messages"] = Self::ensure_assistant_reasoning_field(body["messages"].take());
        }

        // 思考开启时，需要回传真实 reasoning_content 的协议不能带缺失历史出网。
        // 旧会话/异常轮缺字段时在本地拦截，避免供应商返回难以理解的 400。
        if compat.policy == ReasoningPolicy::RequireToolCall
            && effective_level.is_some_and(|level| level != "off")
        {
            Self::ensure_reasoning_replay(&body)?;
        }

        // 流式请求附加 stream_options（用于获取 usage）。
        // 模型声明不支持时直接省略，避免先发一次注定失败、再降级重试。
        if stream && compat.stream_usage == crate::llm::compat::StreamUsage::Supported {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }

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

    /// 给所有 assistant 消息补齐 `reasoning_content` 键（空串合法）。
    ///
    /// 只在模型声明 `assistant_reasoning=required` 且思考开启时调用。
    fn ensure_assistant_reasoning_field(mut messages: Value) -> Value {
        if let Some(arr) = messages.as_array_mut() {
            for msg in arr {
                let Some(obj) = msg.as_object_mut() else {
                    continue;
                };
                if obj.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                obj.entry("reasoning_content".to_string())
                    .or_insert_with(|| Value::String(String::new()));
            }
        }
        messages
    }

    /// 拒绝把缺少 reasoning_content 的工具轮历史发给要求回传的供应商。
    ///
    /// 只检查 assistant 的 tool_calls 消息；普通 assistant 文本不要求推理链。
    fn ensure_reasoning_replay(body: &Value) -> Result<(), LlmError> {
        let Some(messages) = body.get("messages").and_then(Value::as_array) else {
            return Ok(());
        };
        for message in messages {
            let Some(obj) = message.as_object() else {
                continue;
            };
            if obj.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let has_tool_calls = obj
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty());
            if !has_tool_calls {
                continue;
            }
            // 空串是合法的“已记录为空”carrier；只有缺键才是历史缺失。
            let has_reasoning = obj
                .get("reasoning_content")
                .and_then(Value::as_str)
                .is_some();
            if !has_reasoning {
                return Err(LlmError::Config(
                    "当前模型的思考模式要求工具调用轮回传推理内容，但历史中缺失；请压缩会话或新建会话后重试。"
                        .into(),
                ));
            }
        }
        Ok(())
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
        let mut req = self
            .http
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .json(body);

        // compat 驱动认证：auth = "none" 时不发送 Authorization（Ollama 等）
        let compat =
            self.registry
                .get_resolved_compat(&config.provider, &config.model, config.api_type);
        if compat.auth != Some(AuthMode::None) {
            req = req.header(header::AUTHORIZATION, format!("Bearer {}", config.api_key));
        }

        // 自定义 Headers（JSON 字符串解析）
        if let Some(ref headers_str) = config.headers {
            if let Ok(json) = serde_json::from_str::<Value>(headers_str) {
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
        // 缓存观测（2026-10-03）：DeepSeek 直给命中 / 未命中两个字段；
        // OpenAI 系只有 cached_tokens（命中；未命中不可得 → None）。
        let cache_hit = usage
            .get("prompt_cache_hit_tokens")
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
            })
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let cache_miss = usage
            .get("prompt_cache_miss_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        Some(TokenUsage {
            input_tokens: input as u32,
            context_input_tokens: None,
            output_tokens: output as u32,
            cache_hit_tokens: cache_hit,
            cache_miss_tokens: cache_miss,
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

        let tool_calls = msg
            .get("tool_calls")
            .and_then(|v| serde_json::from_value::<Vec<ToolCall>>(v.clone()).ok())
            .map(|mut calls| {
                for call in &mut calls {
                    if call.id.trim().is_empty() {
                        call.id = fallback_tool_call_id();
                    }
                }
                calls
            })
            .filter(|tc| !tc.is_empty());

        let usage = body.get("usage").and_then(Self::extract_usage);
        Ok(LlmResponse {
            text,
            tool_calls,
            usage,
        })
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
            match self
                .do_chat_stream(config, messages, tools, true, &*on_event)
                .await
            {
                Ok(result) => return Ok(result),
                Err(LlmError::Api(msg)) if msg.contains("stream_options") => {
                    // 降级：移除 stream_options 重试
                    log::warn!("[LLM] stream_options 不被支持，降级重试（无 usage 信息）");
                }
                Err(e) => return Err(e),
            }

            // 降级请求（不含 stream_options）
            self.do_chat_stream(config, messages, tools, false, &*on_event)
                .await
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
        log::trace!(
            "[LLM] 请求体：{}",
            serde_json::to_string(&body).unwrap_or_default()
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
        log::debug!("[LLM] 响应 HTTP {}", status.as_u16());
        if !status.is_success() {
            let (err_text, read_failed) = match response.text().await {
                Ok(t) => (t, false),
                Err(e) => (format!("(响应体读取失败: {e})"), true),
            };
            // 尝试提取 JSON error 的 type/code/message，失败则用原始文本
            let err_msg = if read_failed {
                err_text
            } else {
                serde_json::from_str::<Value>(&err_text)
                    .ok()
                    .and_then(|v| provider_error_detail(&v))
                    .unwrap_or(err_text)
            };
            return Err(LlmError::http_with_retry_after(
                status.as_u16(),
                err_msg,
                retry_after_ms,
            ));
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

/// 供应商未提供可用 tool_call id 时的兜底。
/// 不能只用分片 index：同一会话的多个 step 都会从 index 0 开始。
/// 还要跨进程重启保持不重复，否则旧历史里的 `call_prts_1` 会把新调用当成
/// 重复 id 丢弃。进程号 + 毫秒时间 + 进程内计数器足以避免这种碰撞。
static NEXT_FALLBACK_TOOL_CALL_ID: AtomicU64 = AtomicU64::new(1);

fn fallback_tool_call_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "call_prts_{}_{}_{}",
        std::process::id(),
        millis,
        NEXT_FALLBACK_TOOL_CALL_ID.fetch_add(1, Ordering::Relaxed)
    )
}

impl ToolCallAccumulator {
    fn into_tool_call(self) -> Option<ToolCall> {
        let id = self
            .id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(fallback_tool_call_id);
        let name = self.name.filter(|name| !name.trim().is_empty())?;
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
    stream: St,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
) -> Result<StreamResult, LlmError>
where
    St: futures_util::Stream<Item = Result<Vec<u8>, E>> + Unpin,
    E: std::fmt::Display,
{
    // ── 累积状态 ──
    let mut text = String::new(); // content delta 累积
    let mut reasoning = String::new(); // reasoning delta 累积
    let mut tool_call_acc: HashMap<usize, ToolCallAccumulator> = HashMap::new();
    let mut usage: Option<TokenUsage> = None;
    let mut finish_reason: Option<FinishReason> = None;
    let mut saw_done = false;

    // 帧读取收口（2026-10-09）：字节缓冲、跨 chunk 无损，见 crate::llm::sse。
    let read = crate::llm::sse::read_frames(stream, |frame| {
        if crate::llm::sse::frame_data(frame).as_deref() == Some("[DONE]") {
            saw_done = true;
        }
        process_sse_frame(
            frame,
            on_event,
            &mut text,
            &mut reasoning,
            &mut tool_call_acc,
            &mut usage,
            &mut finish_reason,
        );
    })
    .await;

    if let Err(e) = read {
        // 流异常：若有已累积内容，emit 错误但不丢弃已有输出
        let error_msg = format!("流读取异常: {e}");
        if !text.is_empty() || !tool_call_acc.is_empty() {
            on_event(StreamEvent::TextDelta {
                content: format!("\n\n[错误] {error_msg}"),
            });
        }
        return Err(LlmError::Transport(error_msg));
    }

    // 合法终止有两种：`[DONE]`，或流在收到 finish_reason 后正常结束。
    // 后者兼容不发 [DONE] 的 OpenAI-compatible 网关（MiniMax 兼容通道、
    // 企业内网网关等）；finish_reason 是供应商给出的明确完成信号。
    // 两者都没有时仍 fail closed，避免把真正截断的流归档为 completed。
    // 残余风险：供应商发完 finish_reason 后连接立即断开，会被视为完成；
    // 这个取舍只影响“已明确声称完成”的流，不再让整类供应商每轮全失败。
    log::debug!(
        "[LLM] stream terminal saw_done={} finish_reason={:?} text_len={} tool_calls={}",
        saw_done,
        finish_reason,
        text.chars().count(),
        tool_call_acc.len()
    );
    if !saw_done && finish_reason.is_none() {
        return Err(LlmError::Transport(
            "流提前结束：未收到 [DONE] 或 finish_reason".into(),
        ));
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
        tool_calls, // orchestrator 据此判断是否需要继续循环
        reasoning: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        usage,
        finish_reason,
    })
}

/// 处理单个 SSE 事件帧（一帧即一条 JSON，多 `data:` 行属同一 JSON 的换行拼接）。
///
/// `data: [DONE]` 是标准终止标记；不发 `[DONE]` 时，收到 `finish_reason`
/// 后正常 EOF 也算终止。终止前如果 choices 为空的 chunk 带 usage，
/// 该 usage 已被上一轮 process_sse_chunk 暂存。
fn process_sse_frame(
    frame: &str,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
    text: &mut String,
    reasoning: &mut String,
    tool_call_acc: &mut HashMap<usize, ToolCallAccumulator>,
    usage: &mut Option<TokenUsage>,
    finish_reason: &mut Option<FinishReason>,
) {
    let Some(payload) = crate::llm::sse::frame_data(frame) else {
        return;
    };
    log::trace!("[LLM] SSE ← {}", payload);

    if payload == "[DONE]" {
        return; // 流终止标记，不解析
    }

    // 一帧即一条 JSON。个别供应商把多条 JSON 塞进多个 data 行（非规范）→
    // 整段解析失败时逐行兜底。
    let chunks: Vec<Value> = match serde_json::from_str::<Value>(&payload) {
        Ok(v) => vec![v],
        Err(_) => payload
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect(),
    };

    for chunk in &chunks {
        process_sse_chunk(
            chunk,
            on_event,
            text,
            reasoning,
            tool_call_acc,
            usage,
            finish_reason,
        );
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
    finish_reason: &mut Option<FinishReason>,
) {
    // ── 无条件提取 usage ──
    // 不同供应商在不同 chunk 中返回 usage：有的在单独 chunk（choices 为空或缺失），
    // 有的随最后一条 delta 一起返回。不论 choices 结构如何，有就取。
    if let Some(u) = chunk.get("usage") {
        let extracted = OpenAiCompatibleClient::extract_usage(u);
        if let Some(ref tu) = extracted {
            debug!(
                "action" => "usage_extracted",
                "input_tokens" => tu.input_tokens,
                "output_tokens" => tu.output_tokens,
                "cache_hit_tokens" => tu.cache_hit_tokens,
                "cache_miss_tokens" => tu.cache_miss_tokens
            );
            on_event(StreamEvent::Usage { usage: *tu });
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
        // finish_reason 可能出现在没有 delta 的收尾 chunk，先于 delta 解析。
        if let Some(raw) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            if let Some(reason) = FinishReason::from_openai(raw) {
                *finish_reason = Some(reason);
            }
        }

        let Some(delta) = choice.get("delta") else {
            continue;
        };

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
        if let Some(ct) = delta
            .get("content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            text_acc.push_str(ct);
            on_event(StreamEvent::TextDelta {
                content: ct.to_string(),
            });
        }

        // tool_calls delta（按 index 累积）
        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let Some(index) = tc.get("index").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let idx = index as usize;

                let acc = tool_call_acc.entry(idx).or_default();

                // 后续分片可能带空 id/name；空值不能覆盖首片已拿到的真实值。
                if let Some(id) = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|id| !id.trim().is_empty())
                {
                    acc.id = Some(id.to_string());
                }
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func
                        .get("name")
                        .and_then(|v| v.as_str())
                        .filter(|name| !name.trim().is_empty())
                    {
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
    use crate::llm::ApiType;
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
    async fn collect_events(lines: &[String]) -> (Vec<StreamEvent>, StreamResult) {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_c = events.clone();
        let on_event = move |e: StreamEvent| {
            events_c.lock().unwrap().push(e);
        };

        let stream = sse_stream(lines);
        let result = parse_sse_stream(stream, &on_event).await.unwrap();

        let collected = events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| !matches!(event, StreamEvent::Usage { .. }))
            .cloned()
            .collect();
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

    /// 带缓存字段的 usage chunk（DeepSeek 命中 / 未命中）
    fn usage_chunk_line_with_cache(prompt: u32, completion: u32, hit: u32, miss: u32) -> String {
        format!(
            r#"data: {{"choices":[],"usage":{{"prompt_tokens":{},"completion_tokens":{},"prompt_cache_hit_tokens":{},"prompt_cache_miss_tokens":{}}}}}"#,
            prompt, completion, hit, miss
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
    async fn finish_reason_length_maps_to_max_tokens() {
        let lines = vec![
            text_delta_line("半截回答"),
            r#"data: {"choices":[{"finish_reason":"length","delta":{}}]}"#.to_string(),
            done_line(),
        ];
        let (_, result) = collect_events(&lines).await;

        assert_eq!(
            result.finish_reason,
            Some(crate::llm::FinishReason::MaxTokens)
        );
        assert_eq!(result.text, "半截回答");
    }

    #[tokio::test]
    async fn stream_without_done_or_finish_reason_is_transport_error() {
        let lines = vec![text_delta_line("半截回答")];
        let stream = sse_stream(&lines);
        let on_event = |_e: StreamEvent| {};
        let err = parse_sse_stream(stream, &on_event).await.unwrap_err();
        assert!(
            matches!(err, LlmError::Transport(ref msg) if msg.contains("finish_reason")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn finish_reason_without_done_is_accepted() {
        // 兼容不发 [DONE] 的 OpenAI-compatible 网关：finish_reason 已是
        // 供应商给出的明确完成信号。
        let lines = vec![
            text_delta_line("完整回答"),
            r#"data: {"choices":[{"finish_reason":"stop","delta":{}}]}"#.to_string(),
        ];
        let (_, result) = collect_events(&lines).await;
        assert_eq!(result.text, "完整回答");
        assert_eq!(result.finish_reason, Some(crate::llm::FinishReason::Normal));
    }

    #[tokio::test]
    async fn reasoning_delta_deepseek() {
        let lines = vec![reasoning_delta_line("需要查询数据库"), done_line()];
        let (events, result) = collect_events(&lines).await;

        assert_eq!(events.len(), 1);
        assert!(
            matches!(events[0], StreamEvent::ReasoningDelta { ref content } if content == "需要查询数据库")
        );

        assert_eq!(result.reasoning, Some("需要查询数据库".into()));
    }

    #[tokio::test]
    async fn reasoning_delta_ollama_field_name() {
        // Ollama 使用 "reasoning" 字段名而非 "reasoning_content"
        let lines = vec![ollama_reasoning_line("分析中"), done_line()];
        let (events, result) = collect_events(&lines).await;

        assert_eq!(events.len(), 1);
        assert!(
            matches!(events[0], StreamEvent::ReasoningDelta { ref content } if content == "分析中")
        );

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
        assert!(
            matches!(&events[0], StreamEvent::ToolCall { id, name, arguments }
                if id == "call_1"
                && name == "echo"
                && arguments == r#"{"message":"hi"}"#
            )
        );

        // [DONE] 后 tool_calls 被 emit 为事件，同时也在 StreamResult 中提供
        // 供 orchestrator 判断是否需要继续调度循环
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "echo");
        // 契约不变量 I2/I5：arguments 端到端 raw（分片累积后原样直通）
        super::super::assert_stream_contract(&events, &result);
    }

    #[tokio::test]
    async fn tool_call_with_finish_reason_without_done_is_emitted() {
        let lines = vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":"{}"}}]}}]}"#.to_string(),
            r#"data: {"choices":[{"finish_reason":"tool_calls","delta":{}}]}"#.to_string(),
        ];
        let (events, result) = collect_events(&lines).await;

        assert!(matches!(
            &events[0],
            StreamEvent::ToolCall { id, name, .. } if id == "call_1" && name == "echo"
        ));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(
            result.finish_reason,
            Some(crate::llm::FinishReason::ToolCalls)
        );
    }

    #[tokio::test]
    async fn empty_id_in_later_chunk_does_not_overwrite_real_id() {
        let lines = vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_real","function":{"name":"echo","arguments":"{}"}}]}}]}"#.to_string(),
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"arguments":""}}]}}]}"#.to_string(),
            done_line(),
        ];
        let (_, result) = collect_events(&lines).await;

        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_real");
    }

    #[tokio::test]
    async fn empty_or_missing_id_gets_fallback() {
        let lines = vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"echo","arguments":"{}"}}]}}]}"#.to_string(),
            done_line(),
        ];
        let (_, result) = collect_events(&lines).await;

        assert_eq!(result.tool_calls.len(), 1);
        assert!(!result.tool_calls[0].id.trim().is_empty());
        assert!(result.tool_calls[0].id.starts_with("call_prts_"));
    }

    #[test]
    fn fallback_ids_do_not_collide_with_previous_processes() {
        let a = fallback_tool_call_id();
        let b = fallback_tool_call_id();
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("call_prts_{}_", std::process::id())));
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
        // 无缓存字段 → 未观测（None），不是 0
        assert_eq!(usage.cache_hit_tokens, None);
        assert_eq!(usage.cache_miss_tokens, None);
    }

    #[tokio::test]
    async fn usage_chunk_cache_extraction() {
        // 缓存观测（2026-10-03）：DeepSeek 命中 / 未命中字段原样提取
        let lines = vec![
            text_delta_line("回答结束"),
            usage_chunk_line_with_cache(150, 42, 128, 22),
            done_line(),
        ];
        let (_events, result) = collect_events(&lines).await;

        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.cache_hit_tokens, Some(128));
        assert_eq!(usage.cache_miss_tokens, Some(22));
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
    fn extract_usage_zhipu_cached_tokens() {
        let json = serde_json::json!({
            "prompt_tokens": 1075,
            "completion_tokens": 241,
            "prompt_tokens_details": { "cached_tokens": 1024 }
        });
        let usage = OpenAiCompatibleClient::extract_usage(&json).unwrap();
        assert_eq!(usage.input_tokens, 1075);
        assert_eq!(usage.cache_hit_tokens, Some(1024));
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

    #[test]
    fn build_url_keeps_full_chat_completions_endpoint() {
        let config = LlmConfig {
            provider: "openai".into(),
            base_url: Some("https://example.com/v1/chat/completions".into()),
            ..Default::default()
        };
        assert_eq!(
            OpenAiCompatibleClient::build_url(&config).unwrap(),
            "https://example.com/v1/chat/completions"
        );
    }

    #[test]
    fn build_url_appends_to_v1_base() {
        let config = LlmConfig {
            provider: "openai".into(),
            base_url: Some("https://example.com/v1/".into()),
            ..Default::default()
        };
        assert_eq!(
            OpenAiCompatibleClient::build_url(&config).unwrap(),
            "https://example.com/v1/chat/completions"
        );
    }

    #[test]
    fn missing_reasoning_on_tool_call_is_rejected() {
        let body = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": "", "tool_calls": [{"id": "c1"}]}
            ]
        });
        let err = OpenAiCompatibleClient::ensure_reasoning_replay(&body).unwrap_err();
        assert!(err.to_string().contains("压缩会话或新建会话"));
    }

    #[test]
    fn reasoning_on_tool_call_passes() {
        let body = serde_json::json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "先查资料",
                    "tool_calls": [{"id": "c1"}]
                }
            ]
        });
        assert!(OpenAiCompatibleClient::ensure_reasoning_replay(&body).is_ok());
    }

    #[test]
    fn empty_reasoning_carrier_on_tool_call_passes() {
        let body = serde_json::json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "",
                    "tool_calls": [{"id": "c1"}]
                }
            ]
        });
        assert!(OpenAiCompatibleClient::ensure_reasoning_replay(&body).is_ok());
    }

    #[test]
    fn assistant_reasoning_required_fills_empty_string() {
        let messages = serde_json::json!([
            {"role": "user", "content": "q"},
            {"role": "assistant", "content": "answer"}
        ]);
        let out = OpenAiCompatibleClient::ensure_assistant_reasoning_field(messages);
        assert_eq!(out[1]["reasoning_content"], "");
        assert!(out[0].get("reasoning_content").is_none());
    }

    // ── stream_options 由 compat 决定 ──

    fn stream_body(model_compat: &str) -> Value {
        let json = format!(
            r#"{{"p":{{"defaults":{{"api_type":"openai-completions","base_url":"https://x"}},"models":{{"m":{{"name":"M","context":1000,"compat":{model_compat}}}}}}}}}"#
        );
        let registry = std::sync::Arc::new(ModelRegistry::from_json(&json).unwrap());
        let client = OpenAiCompatibleClient::new(registry);
        let config = LlmConfig {
            provider: "p".into(),
            model: "m".into(),
            api_type: ApiType::OpenAiCompletions,
            max_tokens: 100,
            ..Default::default()
        };
        client.build_body(&config, &[], &[], true).unwrap()
    }

    #[test]
    fn stream_options_present_when_supported() {
        let body = stream_body(r#"{"stream_usage":"supported"}"#);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn stream_options_omitted_when_unsupported() {
        let body = stream_body(r#"{"stream_usage":"unsupported"}"#);
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn stream_options_omitted_when_unknown() {
        let body = stream_body("{}");
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn output_budget_uses_declared_openai_field() {
        let body =
            stream_body(r#"{"output_budget":{"field":"max_completion_tokens","required":true}}"#);
        assert_eq!(body["max_completion_tokens"], 100);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn output_budget_absent_keeps_existing_body_shape() {
        let body = stream_body("{}");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
    }
}
