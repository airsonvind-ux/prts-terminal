//! LLM 适配层 —— 类型地基与能力契约
//!
//! 这是整个后端代码的类型基石。所有模块（mock、tools、orchestrator、commands）
//! 围绕此处定义的「数据结构」和「LlmClient trait」进行协作。
//!
//! 职责边界：
//!   本模块只定义"LLM 世界"的通用类型，不关心具体 provider 怎么实现，
//!   不关心这些类型怎么映射为前端 AgentEvent（那是 orchestrator 的活），
//!   不关心消息怎么从用户输入构造而来（那是 context 的活）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub mod mock;
pub mod openai_compatible;
pub mod anthropic;
pub mod openai_responses;
pub mod thinking;

// ═══════════════════════════════════════════════════════
// 统一 HTTP 客户端
// ═══════════════════════════════════════════════════════

/// 单次 LLM 请求的总超时（含流式响应体读取）。
///
/// 缺陷 2 修复：此前各适配器使用 `reqwest::Client::new()`（无超时），
/// `LlmError::Timeout` 成为死分支——连接挂死时请求永远不返回。
/// 取 10 分钟是权衡：推理模型（如 DeepSeek reasoner）思维链可能很长，
/// 流式响应需要足够的总预算；挂死连接最长阻塞 10 分钟后被切断。
const HTTP_TIMEOUT: Duration = Duration::from_secs(600);

/// 构建带超时的统一 HTTP 客户端（所有 LlmClient 适配器共用）。
///
/// 后续模块 9 配置层可将其改为可配置超时（默认 600s）。
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("构建 HTTP 客户端失败")
}

// ═══════════════════════════════════════════════════════
// 配置
// ═══════════════════════════════════════════════════════

/// LLM 连接配置。
///
/// 开发阶段：Mock 实现不读此配置，但类型在此定义，避免后续轮次改接口。
/// 第二轮接入真实 LLM 时：从环境变量或 Tauri 设置页加载。
///
/// model 和 max_tokens 不在设置页 UI 中展示：
///   model — 正确归属是"读取模型"功能（下拉选择，非文本输入），
///           当前阶段后端不依赖此字段（Mock 忽略），前端 set_config 不传时 serde 填默认
///   max_tokens — 用户无需调整，前端不传时 serde 填 4096
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Provider 标识，如 "openai"、"anthropic" 等。空字符串表示未选择
    /// （serde default：settings.json 手写/旧文件缺字段容忍）
    #[serde(default)]
    pub provider: String,
    /// API 协议类型，决定选哪个 LlmClient 实现。前端不传时默认 OpenAiCompletions
    #[serde(default)]
    pub api_type: ApiType,
    /// 模型名。设置页不暴露（留待模型列表功能以选择器形式提供），前端不传时默认空
    #[serde(default)]
    pub model: String,
    /// API Key。明文存 settings.json（data_dir ACL 隔离）；IPC 侧 get_settings
    /// 掩码下发、set_settings 三态解析（serde default：缺字段容忍）
    #[serde(default)]
    pub api_key: String,
    /// API 地址。None 时使用 provider 默认地址
    #[serde(default)]
    pub base_url: Option<String>,
    /// 额外 HTTP 请求头（JSON 字符串），用于代理/中转场景。None = 不发送
    #[serde(default)]
    pub headers: Option<String>,
    /// 单次请求 token 上限。前端不传时默认 4096
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// 思考强度（canonical 档：off/low/medium/high/max 等，见 models.json
    /// 声明的 thinking_levels 白名单）。None = 未设置 → 请求不干预（不传参，
    /// 跟服务端默认）；已设档不在新模型白名单 → 请求侧静默回退模型默认。
    /// 序列化 snake_case（thinking_level），随 settings.json llm 段落盘。
    #[serde(default)]
    pub thinking_level: Option<String>,
}

fn default_max_tokens() -> u32 {
    4096
}

/// provider → 默认 base_url 映射（**与 models.json 各 provider defaults.base_url
/// 同步**——覆盖全部 9 家预设；models.json 是唯一来源，此处为请求/verify
/// 的 fallback 副本。新增预设须两处同步）。未识别的 provider（自定义供应商）
/// 返回 None → 调用方提示显式填写（自定义名不在预设表中，须自行填 URL）。
pub(crate) fn default_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "deepseek" => Some("https://api.deepseek.com/v1"),
        "openai" => Some("https://api.openai.com/v1"),
        "anthropic" => Some("https://api.anthropic.com"),
        "ollama" => Some("http://localhost:11434/v1"),
        "zhipu" => Some("https://open.bigmodel.cn/api/paas/v4"),
        "kimi" => Some("https://api.moonshot.cn/v1"),
        "qwen" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        "volcengine" => Some("https://ark.cn-beijing.volces.com/api/v3"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        _ => None,
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            api_type: ApiType::OpenAiCompletions,
            model: String::new(),
            api_key: String::new(),
            base_url: None,
            headers: None,
            max_tokens: 4096,
            thinking_level: None,
        }
    }
}

// ═══════════════════════════════════════════════════════
// ApiType / ThinkingFormat / Compat / ResolvedCompat
// ═══════════════════════════════════════════════════════

/// LLM API 协议类型。
///
/// 决定 ClientRegistry 选用哪个 LlmClient 实现。
/// 当前仅 OpenAiCompletions 已实现，其余返回错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiType {
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
}

impl Default for ApiType {
    fn default() -> Self {
        Self::OpenAiCompletions
    }
}

/// 模型所属的 thinking 家族。
///
/// 来自 Hanako `fa()` 的检测结果，在 models.json 中显式声明。
/// 决定 `resolve_compat()` 如何计算 carrier / request_param / policy / clearable。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingFormat {
    Deepseek,
    Anthropic,
    Zhipu,
    Qwen,
    Kimi,
    Volcengine,
    Openrouter,
}

/// 声明层 compat —— 存于 models.json，描述模型的「原因」属性。
///
/// 字段由供应商级 defaults.compat 与模型级 compat 浅合并。
/// 各字段独立覆盖，无嵌套子对象。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Compat {
    /// 模型所属的 thinking 家族。None = 不需要特殊推理处理
    ///
    /// models.json 手写键为 snake_case（thinking_format），此处 alias 接收；
    /// 序列化仍按容器 rename_all 输出 camelCase（前端契约，见测试）。
    #[serde(default, alias = "thinking_format", skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    /// 认证方式。None = 默认 "bearer"。"none" = 不发送 Authorization header
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthMode>,
}

/// 效果层 compat —— 由 resolve_compat() 运行时计算，不序列化到 JSON。
///
/// 适配器直接读取此结构做行为决策，不做二次判断。
#[derive(Debug, Clone)]
pub struct ResolvedCompat {
    /// 推理内容在消息中的载体字段名。None = 不保留 reasoning 字段
    pub carrier: Option<String>,
    /// 请求体中开启推理模式的参数名。None = 不附加 thinking 参数
    pub request_param: Option<String>,
    /// 多轮回传策略
    pub policy: ReasoningPolicy,
    /// 回传后是否可清除
    pub clearable: bool,
    /// 认证方式（直通自 Compat.auth）
    pub auth: Option<AuthMode>,
}

impl Default for ResolvedCompat {
    fn default() -> Self {
        Self {
            carrier: None,
            request_param: None,
            policy: ReasoningPolicy::Preserve,
            clearable: false,
            auth: None,
        }
    }
}

/// 推理链多轮回传策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningPolicy {
    /// 始终保留推理链在历史中
    Preserve,
    /// 仅在 tool_call 轮次回传推理链
    RequireToolCall,
}

/// 认证方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMode {
    #[serde(rename = "bearer")]
    Bearer,
    #[serde(rename = "none")]
    None,
}

/// 从声明层 compat 计算效果层 ResolvedCompat。
///
/// 翻译 Hanako `K0()` 逻辑：thinking_format + api_type + has_reasoning
/// 三个输入唯一确定 carrier / request_param / policy / clearable。
/// auth 不参与计算，由调用方从 Compat.auth 直通。
pub fn resolve_compat(
    thinking_format: Option<ThinkingFormat>,
    api_type: ApiType,
    has_reasoning: bool,
) -> ResolvedCompat {
    if !has_reasoning {
        return ResolvedCompat::default();
    }

    match (thinking_format, api_type) {
        // DeepSeek 走 OpenAI 兼容协议 → reasoning_content + thinking 参数
        (Some(ThinkingFormat::Deepseek), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            request_param: Some("thinking".into()),
            policy: ReasoningPolicy::RequireToolCall,
            ..Default::default()
        },
        // DeepSeek 走 Anthropic 协议 → thinking_blocks
        (Some(ThinkingFormat::Deepseek), ApiType::AnthropicMessages) => ResolvedCompat {
            carrier: Some("thinking_blocks".into()),
            ..Default::default()
        },
        // Anthropic 推理模型
        (Some(ThinkingFormat::Anthropic), _) => ResolvedCompat {
            carrier: Some("thinking_blocks".into()),
            ..Default::default()
        },
        // 智谱 → reasoning_content + enable_thinking + clearable
        (Some(ThinkingFormat::Zhipu), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            request_param: Some("enable_thinking".into()),
            policy: ReasoningPolicy::RequireToolCall,
            clearable: true,
            ..Default::default()
        },
        // 通义千问 → reasoning_content + enable_thinking
        (Some(ThinkingFormat::Qwen), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            request_param: Some("enable_thinking".into()),
            policy: ReasoningPolicy::RequireToolCall,
            ..Default::default()
        },
        // Kimi → reasoning_content
        (Some(ThinkingFormat::Kimi), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            policy: ReasoningPolicy::RequireToolCall,
            ..Default::default()
        },
        // OpenAI Responses → reasoning_items
        (_, ApiType::OpenAIResponses) => ResolvedCompat {
            carrier: Some("reasoning_items".into()),
            ..Default::default()
        },
        // OpenRouter → reasoning_details
        (Some(ThinkingFormat::Openrouter), _) => ResolvedCompat {
            carrier: Some("reasoning_details".into()),
            ..Default::default()
        },
        // 无匹配 → 默认（无推理处理）
        _ => ResolvedCompat::default(),
    }
}

// ═══════════════════════════════════════════════════════
// 消息类型
// ═══════════════════════════════════════════════════════

/// 消息角色（模块 4 P1：role 从 String 改为枚举，编译期防拼写错误）。
///
/// 序列化为 lowercase 字符串（"system"/"user"/"assistant"/"tool"），
/// 与各 provider API 的 role 字面量一致；反序列化不识别未知 role（内部类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// LLM API 层的单条消息。
///
/// 注意：此类型与前端 `types.ts` 的 `ChatMessage` 是**不同层级**的概念。
/// 前端的 ChatMessage 是 UI 扁平结构（role: user | assistant | error |
/// tool，播种源，保持 String 防御性解析），
/// 此处对齐 OpenAI/Claude API 完整格式，额外支持：
///   system — 系统提示词
///   tool   — 工具执行结果
///   assistant 携带 tool_calls — LLM 请求调工具
///
/// reasoning_content 是推理模型的思维链内容（DeepSeek thinking mode 产物）。
/// 按 provider 在 API 边界做条件剥离（非 DeepSeek 的请求中移除该字段），
/// 详见 `OpenAiCompatibleClient` 的请求构造逻辑。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    /// assistant 消息携带的工具调用请求（LLM 决定调工具时）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// tool 消息携带的 tool_call_id（工具执行结果回传给 LLM 时匹配）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 推理模型的思维链内容（DeepSeek thinking mode 等）。
    /// None 时序列化自动省略；Some 时携带，由 LlmClient 实现按 provider 决定是否剥离。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: MessageRole::System, content: content.into(), tool_calls: None, tool_call_id: None, reasoning_content: None }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: MessageRole::User, content: content.into(), tool_calls: None, tool_call_id: None, reasoning_content: None }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: MessageRole::Assistant, content: content.into(), tool_calls: None, tool_call_id: None, reasoning_content: None }
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>, reasoning_content: Option<String>) -> Self {
        Self { role: MessageRole::Assistant, content: content.into(), tool_calls: Some(tool_calls), tool_call_id: None, reasoning_content }
    }

    /// 工具执行结果消息。tool_call_id 用于 LLM API 侧匹配对应的 tool_call。
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self { role: MessageRole::Tool, content: content.into(), tool_calls: None, tool_call_id: Some(tool_call_id.into()), reasoning_content: None }
    }
}

/// LLM 返回的单个工具调用请求（对应 OpenAI function calling 的 tool_calls 数组元素）
///
/// `call_type` 序列化为 JSON key `type`，始终为 "function"。
/// 前端历史消息回传时可能不含此字段（旧格式），`default_call_type` 兜底。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    /// 始终为 "function"，OpenAI/DeepSeek API 要求此字段必须存在
    #[serde(rename = "type", default = "default_call_type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

fn default_call_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// 参数 JSON 字符串，交由工具系统解析后路由到具体 handler
    pub arguments: String,
}

/// 工具定义（发送给 LLM 的 tools 数组元素）。
///
/// parameters 字段使用 serde_json::Value（非结构化 JSON Schema），
/// 由各个工具注册时自行构建。不在此处强制结构化的原因：
/// JSON Schema 可嵌套、可引用 $defs，预先穷举字段会让类型僵化。
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,  // 固定 "function"
    pub function: ToolDefFunction,
}

impl ToolDef {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            tool_type: "function".into(),
            function: ToolDefFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ═══════════════════════════════════════════════════════
// LLM 响应（非流式）
// ═══════════════════════════════════════════════════════

/// 一次非流式调用的完整返回。
///
/// 两种互斥结果：
///   text 有值、tool_calls 为 None → LLM 直接给了文本回答
///   tool_calls 有值、text 通常为 None → LLM 请求执行工具
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

// ═══════════════════════════════════════════════════════
// Token 用量
// ═══════════════════════════════════════════════════════

/// Provider 无关的归一化 token 计数。
///
/// 不同 provider 的 usage 字段名和结构不同（DeepSeek 用 prompt_tokens/completion_tokens，
/// Ollama 用 eval_count 等），各 LlmClient 实现负责将原生 usage 映射为此结构。
/// Mock 实现填 None。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// 输入 token 数（prompt + 对话历史 + 系统提示词）
    pub input_tokens: u32,
    /// 输出 token 数（LLM 生成的文本 + reasoning）
    pub output_tokens: u32,
    /// 缓存命中 token 数（2026-10-03 缓存观测）。
    ///
    /// `None` = 未观测（provider 不提供该字段，或旧数据缺字段——`serde(default)`
    /// 兼容）。语义：DeepSeek = `prompt_cache_hit_tokens`；Anthropic =
    /// `cache_read_input_tokens`（流式 message_start 同源）；OpenAI 系 =
    /// `input_tokens_details.cached_tokens`。
    /// 只作开发期观测（终端遥测），不参与业务判断；`None` 时序列化省略，
    /// 旧快照输出逐字节不变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_tokens: Option<u32>,
    /// 缓存未命中 token 数（provider 报告）。`None` = 未观测。
    /// DeepSeek = `prompt_cache_miss_tokens`；其余 provider 无直接字段时
    /// 不强造口径（留 `None`——“未观测”与“未命中 0”是两种事实）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_miss_tokens: Option<u32>,
}

// ═══════════════════════════════════════════════════════
// 流式事件
// ═══════════════════════════════════════════════════════

/// 流式推送的单个片段。
///
/// chat_stream() 过程中，LLM 的推理链和回答以增量形式逐片产出。
/// 三个事件类型覆盖了 LLM 一次调用的完整生命周期：
///
///   ReasoningDelta → 推理模型的思维链片段（可能多段）
///   ToolCall       → LLM 决定执行工具时的结构化请求（可能多个，并行调用）
///   TextDelta      → 最终回答文本的逐 token 流（连续推送直到结束）
///
/// 不承诺事件出现的顺序和次数。reasoning 和 tool_call 可能交替：
/// LLM 想一小段 → 决定调工具 → 拿到结果后继续想 → 再调或给答案。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    ReasoningDelta {
        content: String,
    },
    ToolCall {
        /// 工具调用唯一 id（LLM API 分配，用于匹配 tool_result）
        id: String,
        name: String,
        arguments: String,
    },
    TextDelta {
        content: String,
    },
}

/// 流式调用完成后的聚合产物。
///
/// chat_stream() 结束时返回，供调用方归档：
///   text       — 完整最终回答文本
///   tool_calls — 本轮产出的全部工具调用（空 vec = 未调工具）
///   reasoning  — 完整的推理链（推理模型提供，普通模型为 None）
///   usage      — 本次 API 调用的 token 用量（Mock 填 None）
#[derive(Debug, Clone)]
pub struct StreamResult {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning: Option<String>,
    pub usage: Option<TokenUsage>,
}

// ═══════════════════════════════════════════════════════
// 错误类型
// ═══════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LlmError {
    /// 配置缺失或无效（api_key 为空、model 名不合法等）
    Config(String),
    /// LLM API 返回 HTTP 错误
    Api(String),
    /// 响应解析失败（JSON 格式不符预期）
    Parse(String),
    /// 网络超时
    Timeout(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "LLM 配置错误：{msg}"),
            Self::Api(msg)    => write!(f, "LLM API 错误：{msg}"),
            Self::Parse(msg)  => write!(f, "LLM 响应解析错误：{msg}"),
            Self::Timeout(msg) => write!(f, "LLM 请求超时：{msg}"),
        }
    }
}

impl std::error::Error for LlmError {}

// ═══════════════════════════════════════════════════════
// LlmClient trait — LLM 能力契约
// ═══════════════════════════════════════════════════════

/// 流式契约不变量（模块 4 P1 显式化；各适配器测试统一验证）。
///
/// 任何 `LlmClient` 实现必须满足：
/// - **I1**：`StreamResult.text` 恒等于流式 `TextDelta` 事件的拼接
///   （无 TextDelta 则为空串）
/// - **I2**：`StreamResult.tool_calls` 与流式 `ToolCall` 事件一一对应
///   （id/name/arguments 逐字段一致）
/// - **I3**：`StreamResult.reasoning` 恒等于 `ReasoningDelta` 的拼接；
///   无推理事件时为 `None`
/// - **I4**：usage 先于 finish——流式事件不含 usage，聚合结果
///   `StreamResult.usage` 在返回时已给出（调用方无需在流中收集）
/// - **I5**：tool call arguments 端到端 raw——适配器不得解析/改写
///   provider 原始 JSON（由 I2 的逐字段一致保证）
///
/// 调用方（orchestrator）依赖这些不变量做双写与派生，
/// 违反即视为适配器缺陷。
#[allow(dead_code)] // 契约验证辅助：仅测试调用（各适配器/测试套件）
pub fn assert_stream_contract(events: &[StreamEvent], result: &StreamResult) {
    // I1：text == TextDelta 拼接
    let text_from_events: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        result.text, text_from_events,
        "契约 I1 违反：StreamResult.text 应等于 TextDelta 拼接"
    );

    // I2 + I5：tool_calls 与流式事件一致（含 arguments 端到端 raw）
    let calls_from_events: Vec<(&str, &str, &str)> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolCall { id, name, arguments } => {
                Some((id.as_str(), name.as_str(), arguments.as_str()))
            }
            _ => None,
        })
        .collect();
    let calls_from_result: Vec<(&str, &str, &str)> = result
        .tool_calls
        .iter()
        .map(|tc| {
            (
                tc.id.as_str(),
                tc.function.name.as_str(),
                tc.function.arguments.as_str(),
            )
        })
        .collect();
    assert_eq!(
        calls_from_result, calls_from_events,
        "契约 I2/I5 违反：tool_calls 应与流式事件一一对应（arguments 原样）"
    );

    // I3：reasoning == ReasoningDelta 拼接（无推理 → None）
    let reasoning_from_events: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ReasoningDelta { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    let expected_reasoning = if reasoning_from_events.is_empty() {
        None
    } else {
        Some(reasoning_from_events)
    };
    assert_eq!(
        result.reasoning, expected_reasoning,
        "契约 I3 违反：reasoning 应等于 ReasoningDelta 拼接"
    );

    // I4：usage 先于 finish——usage 由聚合结果携带（事件不含 usage 字段），
    // 实现方在返回 Result 前已给出，此处无额外断言（类型层面保证）
}

/// LLM 客户端的统一接口。
///
/// 所有实现（Mock / OpenAI / DeepSeek / Anthropic）遵循同一份契约，
/// 上层 orchestrator 通过 `Arc<dyn LlmClient>` 持有，不感知具体 provider。
///
/// 流式契约不变量（I1~I5）见 [`assert_stream_contract`]——
/// 实现方测试必须调用该辅助验证。
///
/// 使用 `Pin<Box<dyn Future>>` 而非 `async fn` 的原因：
/// async trait 方法需要额外的 `async_trait` crate。
/// `Pin<Box<dyn Future>>` 是标准库原生方案，不引入新依赖。
pub trait LlmClient: Send + Sync {
    /// 非流式调用：发送消息列表，等待完整返回。
    ///
    /// 适用场景：RePlan 循环中需要 LLM 做工具选择判断，但不需流式推送中间过程。
    /// 返回 LlmResponse：要么是文本回答，要么是工具调用请求。
    fn chat<'a>(
        &'a self,
        config: &'a LlmConfig,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDef],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>>;

    /// 流式调用：发送消息列表，逐片段推送事件，最后返回聚合结果。
    ///
    /// 适用场景：需要实时展示 LLM 思考过程或打字机效果的回答生成。
    ///
    /// `on_event` 是回调闭包。每收到一个可推送的 StreamEvent 片段，
    /// 实现方调用 `on_event(event)` 通知上层。上层（orchestrator）
    /// 在回调中将 StreamEvent 转换为 AgentEvent 推送前端。
    ///
    /// 流结束后返回 StreamResult（完整文本 + 工具调用汇总 + 推理链）。
    fn chat_stream<'a>(
        &'a self,
        config: &'a LlmConfig,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDef],
        on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>>;
}

// ═══════════════════════════════════════════════════════
// ModelRegistry —— 模型元数据注册表
// ═══════════════════════════════════════════════════════

/// 供应商下的单个模型条目。
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub name: String,
    pub context: u32,
    pub reasoning: bool,
    pub supports_vision: bool,
    pub compat: Option<Compat>,
    /// 思考强度白名单（models.json `thinking_levels`；空 = 未声明 → UI 不显示
    /// 思考强度行、请求不干预）。canonical 档：off/low/medium/high/max。
    pub thinking_levels: Vec<String>,
    /// 模型默认思考档（已设档不在白名单时静默回退到此档；未声明 → None）。
    pub default_thinking_level: Option<String>,
}

/// 供应商条目。
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub api_type: ApiType,
    pub base_url: String,
    pub default_compat: Compat,
    pub models: HashMap<String, ModelEntry>,
}

/// 模型元数据注册表。
///
/// 启动时从 models.json 加载，常驻内存（Tauri State）。
/// 提供 compat 合并查询和供应商/模型列表枚举。
pub struct ModelRegistry {
    providers: HashMap<String, ProviderEntry>,
    /// provider id → 展示名（硬编码映射表）
    display_names: HashMap<String, String>,
}

impl ModelRegistry {
    /// 从 models.json 的 JSON 字符串加载。
    pub fn from_json(json: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct ModelsFile {
            #[serde(flatten)]
            providers: HashMap<String, ProviderJson>,
        }

        #[derive(Deserialize)]
        struct ProviderJson {
            defaults: DefaultsJson,
            models: HashMap<String, ModelJson>,
        }

        #[derive(Deserialize)]
        struct DefaultsJson {
            api_type: ApiType,
            base_url: String,
            #[serde(default)]
            compat: Option<Compat>,
        }

        #[derive(Deserialize)]
        struct ModelJson {
            name: String,
            context: u32,
            #[serde(default)]
            reasoning: bool,
            #[serde(default, rename = "supports_vision")]
            supports_vision: bool,
            #[serde(default)]
            compat: Option<Compat>,
            /// 思考强度白名单（canonical：off/low/medium/high/max）
            #[serde(default, rename = "thinking_levels")]
            thinking_levels: Vec<String>,
            #[serde(default, rename = "default_thinking_level")]
            default_thinking_level: Option<String>,
        }

        let file: ModelsFile = serde_json::from_str(json)
            .map_err(|e| format!("models.json 解析失败: {e}"))?;

        let mut providers = HashMap::new();
        for (id, pj) in file.providers {
            let models: HashMap<String, ModelEntry> = pj.models
                .into_iter()
                .map(|(mid, mj)| {
                    (mid, ModelEntry {
                        name: mj.name,
                        context: mj.context,
                        reasoning: mj.reasoning,
                        supports_vision: mj.supports_vision,
                        compat: mj.compat,
                        thinking_levels: mj.thinking_levels,
                        default_thinking_level: mj.default_thinking_level,
                    })
                })
                .collect();

            providers.insert(id, ProviderEntry {
                api_type: pj.defaults.api_type,
                base_url: pj.defaults.base_url,
                default_compat: pj.defaults.compat.unwrap_or_default(),
                models,
            });
        }

        let display_names = Self::build_display_names();

        Ok(Self { providers, display_names })
    }

    /// 硬编码的 provider id → 展示名映射。
    fn build_display_names() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("deepseek".into(), "DeepSeek".into());
        m.insert("openai".into(), "OpenAI".into());
        m.insert("anthropic".into(), "Anthropic".into());
        m.insert("gemini".into(), "Gemini".into());
        m.insert("ollama".into(), "Ollama (本地)".into());
        m.insert("zhipu".into(), "智谱 AI (GLM)".into());
        m.insert("kimi".into(), "Moonshot (Kimi)".into());
        m.insert("qwen".into(), "阿里云百炼 (通义千问)".into());
        m.insert("volcengine".into(), "火山引擎 (豆包)".into());
        m
    }

    /// 查询指定模型的合并后声明层 compat。
    ///
    /// 模型级字段覆盖供应商级，逐字段独立覆盖。
    /// provider 或 model 不存在时返回空 Compat。
    pub fn get_compat(&self, provider: &str, model: &str) -> Compat {
        let Some(pe) = self.providers.get(provider) else {
            return Compat::default();
        };
        let mut merged = pe.default_compat.clone();

        if let Some(me) = pe.models.get(model) {
            if let Some(ref mc) = me.compat {
                if mc.thinking_format.is_some() {
                    merged.thinking_format = mc.thinking_format;
                }
                if mc.auth.is_some() {
                    merged.auth = mc.auth;
                }
            }
        }

        merged
    }

    /// 查询指定模型的效果层 compat（声明层合并 + resolve_compat 计算）。
    ///
    /// 由 orchestrator 在进入调度循环前调用，结果传递给适配器内部方法。
    pub fn get_resolved_compat(
        &self,
        provider: &str,
        model: &str,
        api_type: ApiType,
    ) -> ResolvedCompat {
        let compat = self.get_compat(provider, model);
        let has_reasoning = self
            .providers
            .get(provider)
            .and_then(|pe| pe.models.get(model))
            .map(|me| me.reasoning)
            .unwrap_or(false);

        let mut resolved = resolve_compat(compat.thinking_format, api_type, has_reasoning);
        // auth 不参与 resolve_compat 计算——从声明层直通效果层
        resolved.auth = compat.auth;
        resolved
    }

    /// 供应商展示排序（国际 → 国内 → 本地）。
    /// 不在列表中的供应商排在末尾，按 id 字母序。
    const PROVIDER_ORDER: &'static [&'static str] = &[
        "openai",
        "anthropic",
        "gemini",
        "deepseek",
        "zhipu",
        "kimi",
        "qwen",
        "volcengine",
        "ollama",
    ];

    /// 枚举所有供应商的基本信息（供 list_providers 命令使用）。
    pub fn list_providers(&self) -> Vec<ProviderInfo> {
        let mut list: Vec<ProviderInfo> = self
            .providers
            .iter()
            .map(|(id, pe)| ProviderInfo {
                id: id.clone(),
                display_name: self.display_names.get(id).cloned().unwrap_or_else(|| id.clone()),
                api_type: pe.api_type,
                base_url: pe.base_url.clone(),
                default_compat: pe.default_compat.clone(),
            })
            .collect();
        list.sort_by(|a, b| {
            let rank_a = Self::PROVIDER_ORDER.iter().position(|&p| p == a.id);
            let rank_b = Self::PROVIDER_ORDER.iter().position(|&p| p == b.id);
            match (rank_a, rank_b) {
                (Some(ra), Some(rb)) => ra.cmp(&rb),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.id.cmp(&b.id),
            }
        });
        list
    }

    /// 获取指定供应商的 base_url。
    pub fn get_base_url(&self, provider: &str) -> Option<&str> {
        self.providers.get(provider).map(|pe| pe.base_url.as_str())
    }

    /// 获取指定供应商的 ApiType。
    pub fn get_api_type(&self, provider: &str) -> Option<ApiType> {
        self.providers.get(provider).map(|pe| pe.api_type)
    }

    /// 获取指定模型的能力信息（供 list_models 匹配远端 ID）。
    pub fn get_model(&self, provider: &str, model: &str) -> Option<&ModelEntry> {
        self.providers.get(provider)?.models.get(model)
    }
}

/// 供应商信息（list_providers 命令返回）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub api_type: ApiType,
    pub base_url: String,
    #[serde(rename = "compat")]
    pub default_compat: Compat,
}

/// 模型信息（list_models 命令返回）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_length: u32,
    pub supports_reasoning: bool,
    pub supports_vision: bool,
    pub compat: Compat,
    /// 思考强度白名单（空 = 未声明）
    pub thinking_levels: Vec<String>,
    /// 模型默认思考档
    pub default_thinking_level: Option<String>,
    /// 是否已收录能力信息（false = 远端有而内置表无条目 → 前端显示 U 徽章；
    /// 能力字段为缺省值，不视作"确认为不支持"）
    pub enriched: bool,
}

// ═══════════════════════════════════════════════════════
// ClientRegistry —— 协议类型 → LlmClient 映射
// ═══════════════════════════════════════════════════════

/// 按 ApiType 路由到对应 LlmClient 实现的注册表。
///
/// 启动时装配，每次 agent_process 调用时通过 select() 获取适配器。
pub struct ClientRegistry {
    clients: HashMap<ApiType, Arc<dyn LlmClient>>,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    /// 注册一个 LlmClient 实现到指定的 ApiType。
    /// 同一 ApiType 重复注册会覆盖前一个。
    pub fn register(&mut self, api_type: ApiType, client: Arc<dyn LlmClient>) {
        self.clients.insert(api_type, client);
    }

    /// 按 ApiType 获取对应的 LlmClient。
    /// 返回 None 表示该协议类型未注册适配器。
    pub fn select(&self, api_type: &ApiType) -> Option<&Arc<dyn LlmClient>> {
        self.clients.get(api_type)
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    /// default_base_url 需覆盖 models.json 全部预设（请求/verify fallback；
    /// 曾缺 zhipu 等致空 Base URL 连接报"未配置 Base URL"回归）
    #[test]
    fn default_base_url_covers_all_presets() {
        for p in ["deepseek", "openai", "anthropic", "ollama", "zhipu", "kimi", "qwen", "volcengine", "gemini"] {
            assert!(default_base_url(p).is_some(), "预设 {p} 应有默认 URL");
        }
        assert!(default_base_url("my-custom").is_none(), "自定义需显式填写");
    }

    use super::*;

    #[test]
    fn chat_message_constructors() {
        let s = ChatMessage::system("你是 PRTS");
        assert_eq!(s.role, MessageRole::System);
        assert_eq!(s.content, "你是 PRTS");

        let u = ChatMessage::user("查询源石虫");
        assert_eq!(u.role, MessageRole::User);

        let a = ChatMessage::assistant("已查到");
        assert_eq!(a.role, MessageRole::Assistant);
        assert!(a.tool_calls.is_none());

        let tc = ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: ToolCallFunction { name: "query".into(), arguments: "{}".into() },
        };
        let at = ChatMessage::assistant_with_tools("", vec![tc], None);
        assert_eq!(at.role, MessageRole::Assistant);
        assert!(at.tool_calls.is_some());

        let tr = ChatMessage::tool_result("call_1", "结果");
        assert_eq!(tr.role, MessageRole::Tool);
        assert_eq!(tr.tool_call_id, Some("call_1".into()));
    }

    #[test]
    fn message_role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&MessageRole::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&MessageRole::User).unwrap(), "\"user\"");
        assert_eq!(serde_json::to_string(&MessageRole::Assistant).unwrap(), "\"assistant\"");
        assert_eq!(serde_json::to_string(&MessageRole::Tool).unwrap(), "\"tool\"");
    }

    /// ModelInfo 序列化契约：camelCase 键名 + enriched 字段（前端 U 徽章依据）。
    #[test]
    fn model_info_serializes_camel_case_with_enriched() {
        let info = ModelInfo {
            id: "deepseek-flash".into(),
            display_name: "DeepSeek V4.1 Flash".into(),
            context_length: 1_000_000,
            supports_reasoning: true,
            supports_vision: true,
            compat: Compat::default(),
            thinking_levels: vec!["off".into(), "low".into(), "high".into(), "max".into()],
            default_thinking_level: Some("high".into()),
            enriched: true,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["displayName"], "DeepSeek V4.1 Flash");
        assert_eq!(json["contextLength"], 1_000_000);
        assert_eq!(json["supportsReasoning"], true);
        assert_eq!(json["enriched"], true);
    }

    #[test]
    fn llm_config_default() {
        let c = LlmConfig::default();
        assert_eq!(c.provider, "");
        assert!(matches!(c.api_type, ApiType::OpenAiCompletions));
        assert_eq!(c.max_tokens, 4096);
        assert!(c.headers.is_none());
    }

    #[test]
    fn tool_def_new() {
        let params = serde_json::json!({"type": "object", "properties": {}});
        let t = ToolDef::new("query", "查询工具", params.clone());
        assert_eq!(t.tool_type, "function");
        assert_eq!(t.function.name, "query");
    }

    #[test]
    fn llm_error_display() {
        let e = LlmError::Api("401 Unauthorized".into());
        assert!(e.to_string().contains("401"));
        let e = LlmError::Config("api_key 为空".into());
        assert!(e.to_string().contains("api_key"));
    }

    // ── default_base_url ──

    #[test]
    fn default_base_url_known_providers() {
        assert_eq!(default_base_url("deepseek"), Some("https://api.deepseek.com/v1"));
        assert_eq!(default_base_url("openai"), Some("https://api.openai.com/v1"));
        assert_eq!(default_base_url("anthropic"), Some("https://api.anthropic.com"));
        assert_eq!(default_base_url("ollama"), Some("http://localhost:11434/v1"));
    }

    #[test]
    fn default_base_url_unknown_returns_none() {
        assert_eq!(default_base_url("unknown_vendor"), None);
        assert_eq!(default_base_url(""), None);
    }

    // ── Compat 序列化：确保 camelCase 输出 ──

    #[test]
    fn compat_serializes_camelcase() {
        let c = Compat {
            thinking_format: Some(ThinkingFormat::Deepseek),
            auth: Some(AuthMode::None),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"thinkingFormat\""), "Expected camelCase thinkingFormat, got: {json}");
        assert!(json.contains("\"auth\""), "Expected auth, got: {json}");
    }

    #[test]
    fn compat_deserializes_camelcase() {
        let json = r#"{"thinkingFormat":"deepseek","auth":"none"}"#;
        let c: Compat = serde_json::from_str(json).unwrap();
        assert_eq!(c.thinking_format, Some(ThinkingFormat::Deepseek));
        assert_eq!(c.auth, Some(AuthMode::None));
    }

    #[test]
    fn provider_info_compat_is_camelcase() {
        let info = ProviderInfo {
            id: "deepseek".into(),
            display_name: "DeepSeek".into(),
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.deepseek.com/v1".into(),
            default_compat: Compat {
                thinking_format: Some(ThinkingFormat::Deepseek),
                auth: None,
            },
        };
        let json = serde_json::to_string(&info).unwrap();
        // ProviderInfo 的 compat 字段是嵌套的 Compat，必须验证 camelCase
        assert!(json.contains("\"thinkingFormat\""), "Nested compat must be camelCase, got: {json}");
    }

    /// 真实 models.json 解析回归（2026-09-19 修）：文件里 `compat` 用 snake_case
    /// 键（thinking_format），Compat 结构声明 camelCase——键名错位曾静默落默认档，
    /// 使 carrier=None，出网前误剥离全部推理链。此测试直接从真身文件断言效果层
    /// 配置，堵住「测试只覆盖手搭结构、不读文件」的盲区。
    #[test]
    fn real_models_json_deepseek_compat_resolves() {
        let reg =
            ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap();
        let r = reg.get_resolved_compat("deepseek", "deepseek-flash", ApiType::OpenAiCompletions);
        assert_eq!(
            r.carrier.as_deref(),
            Some("reasoning_content"),
            "carrier 必须从真身文件解析成功（thinking_format 别名）"
        );
        assert_eq!(
            r.request_param.as_deref(),
            Some("thinking"),
            "request_param 必须从真身文件解析成功"
        );
        assert_eq!(
            r.policy,
            ReasoningPolicy::RequireToolCall,
            "policy 必须为 RequireToolCall"
        );
    }
}
