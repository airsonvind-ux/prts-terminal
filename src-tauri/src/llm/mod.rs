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
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub mod anthropic;
pub mod compat;
#[cfg(test)]
#[cfg(test)]
pub mod mock;
pub mod openai_compatible;
pub mod openai_responses;
#[cfg(test)]
mod smoke_tests;
#[cfg(test)]
mod snapshot_tests;
pub mod sse;
pub mod thinking;
// 协议异常清洗（匿名工具调用），见模块头。
pub mod protocol_guard;

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
///   max_tokens — 用户无需调整，前端不传时 serde 填 4096；chat 适配器把
///                这个 legacy default 视为未设置，Anthropic/Responses 用 8192 兜底
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
    /// API Key。**持久层不留值**：settings.json 上一律写空，钥值由
    /// crate::credentials 加密落盘（DPAPI，绑当前用户与本机），运行时经
    /// LlmSection::resolve_active_with 取；本字段仅作内存载体（历史明文迁移读取用）。
    /// IPC 侧 get_settings 掩码下发、set_settings 三态解析。
    #[serde(default)]
    pub api_key: String,
    /// API 地址。None 时使用 provider 默认地址
    #[serde(default)]
    pub base_url: Option<String>,
    /// 额外 HTTP 请求头（JSON 字符串），用于代理/中转场景。None = 不发送
    #[serde(default)]
    pub headers: Option<String>,
    /// 单次请求 token 上限。前端不传时默认 4096；chat 路径会把 4096 视为
    /// legacy默认值并走协议兜底，utility 路径仍按显式小上限压缩。
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

/// 旧版前端默认值。它表示“没有显式设置”，而不是用户要求的 4096 输出。
pub const LEGACY_DEFAULT_MAX_TOKENS: u32 = 4096;

/// 协议必填输出字段的保守兜底；模型级 `max_output` 落地前使用。
pub const DEFAULT_REQUIRED_OUTPUT_TOKENS: u32 = 8192;

/// chat 输出上限：旧版 4096 默认值视为未设置，默认目标为 8192。
/// 模型级 max_output 只负责向下封顶，不把能力上限直接变成请求预算；
/// utility 路径会先把 max_tokens 压到更小的显式值。
pub fn chat_output_budget(configured: u32, model_limit: Option<u32>) -> u32 {
    let requested = if configured == 0 || configured == LEGACY_DEFAULT_MAX_TOKENS {
        DEFAULT_REQUIRED_OUTPUT_TOKENS
    } else {
        configured
    };
    match model_limit {
        Some(limit) => requested.min(limit),
        None => requested,
    }
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
#[derive(Default)]
pub enum ApiType {
    #[serde(rename = "openai-completions")]
    #[default]
    OpenAiCompletions,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
}

impl ApiType {
    /// 协议名（与 serde 字面量一致，供错误信息与日志使用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "openai-completions",
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenAIResponses => "openai-responses",
        }
    }
}

/// 模型所属的 thinking 家族。
///
/// 思考家族在 models.json 中显式声明。
/// 决定 `resolve_compat()` 如何计算 carrier / request_param / policy。
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

impl ThinkingFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::Anthropic => "anthropic",
            Self::Zhipu => "zhipu",
            Self::Qwen => "qwen",
            Self::Kimi => "kimi",
            Self::Volcengine => "volcengine",
            Self::Openrouter => "openrouter",
        }
    }

    /// 该 thinking 家族能否在当前协议上工作。
    ///
    /// 家族格式与 wire 协议绑定：把 zhipu 的 thinking.type 写到 Anthropic
    /// 协议上不会被适配器读取，加载期就应报错，而不是静默落默认档。
    fn supports(self, api_type: ApiType) -> bool {
        match self {
            // DeepSeek 同时提供 OpenAI 兼容与 Anthropic 兼容两种入口。
            Self::Deepseek => matches!(
                api_type,
                ApiType::OpenAiCompletions | ApiType::AnthropicMessages
            ),
            Self::Anthropic => api_type == ApiType::AnthropicMessages,
            Self::Zhipu | Self::Qwen | Self::Kimi | Self::Volcengine | Self::Openrouter => {
                api_type == ApiType::OpenAiCompletions
            }
        }
    }
}

/// 声明层 compat —— 存于 models.json，描述模型的「原因」属性。
///
/// 字段由供应商级 defaults.compat 与模型级 compat 浅合并。
/// 各字段独立覆盖，无嵌套子对象。
/// `deny_unknown_fields`：键名写错直接报错，不再静默落默认档（09-19 键名错位
/// 曾使 carrier=None，出网前把推理链整套剥掉）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Compat {
    /// 模型所属的 thinking 家族。None = 不需要特殊推理处理
    ///
    /// models.json 手写键为 snake_case（thinking_format），此处 alias 接收；
    /// 序列化仍按容器 rename_all 输出 camelCase（前端契约，见测试）。
    #[serde(
        default,
        alias = "thinking_format",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_format: Option<ThinkingFormat>,
    /// 同一 thinking 家族内的代际差异。生产期只读，不序列化到前端。
    #[serde(default, alias = "thinking_profile", skip_serializing)]
    pub thinking_profile: Option<compat::ThinkingProfile>,
    /// 压缩时是否允许显式清空推理载体。None = 安全默认 false。
    #[serde(default, alias = "reasoning_clearable", skip_serializing)]
    pub reasoning_clearable: Option<bool>,
    /// 推理开启时 assistant 消息是否必须带 carrier 字段。
    #[serde(default, alias = "assistant_reasoning", skip_serializing)]
    pub assistant_reasoning: Option<compat::AssistantReasoning>,
    /// 流式 usage 能力。None = 未声明，目标行为是不发送 stream_options。
    #[serde(default, alias = "stream_usage", skip_serializing)]
    pub stream_usage: Option<compat::StreamUsage>,
    /// 认证方式。None = 默认 "bearer"。"none" = 不发送 Authorization header
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthMode>,
    /// 推理链多轮回传策略覆盖。None = 由 thinking_format 家族默认决定。
    /// models.json 手写键为 snake_case（reasoning_policy），序列化按容器
    /// rename_all 输出 camelCase（前端契约）。
    #[serde(
        default,
        alias = "reasoning_policy",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_policy: Option<ReasoningPolicy>,
    /// 流式响应是否接受 `stream_options.include_usage`。
    /// None = 当前行为（发送，失败后降级重试）；false = 直接不发。
    #[serde(
        default,
        alias = "supports_usage_in_streaming",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_usage_in_streaming: Option<bool>,
}

impl Compat {
    /// 校验声明字段是否适用于当前协议。
    ///
    /// P1 先覆盖 thinking_format；后续新增协议专属字段时在这里补 match 臂。
    /// 校验放在加载期，避免“写了但适配器读不到”的假开关。
    fn validate(&self, api_type: ApiType, at: &str, problems: &mut Vec<String>) {
        if let Some(format) = self.thinking_format {
            if !format.supports(api_type) {
                problems.push(format!(
                    "{at}: thinking_format {} 不适用于协议 {}",
                    format.as_str(),
                    api_type.as_str()
                ));
            }
        }
        if let (Some(profile), Some(format)) = (self.thinking_profile, self.thinking_format) {
            let matches = matches!(
                (profile, format),
                (
                    compat::ThinkingProfile::ZhipuGlm53 | compat::ThinkingProfile::ZhipuGlm53Flash,
                    ThinkingFormat::Zhipu,
                ) | (
                    compat::ThinkingProfile::KimiK26
                        | compat::ThinkingProfile::KimiK3
                        | compat::ThinkingProfile::KimiK27Code,
                    ThinkingFormat::Kimi,
                )
            );
            if !matches {
                problems.push(format!("{at}: thinking_profile 与 thinking_format 不匹配"));
            }
        }
        if self.supports_usage_in_streaming.is_some() && api_type != ApiType::OpenAiCompletions {
            problems.push(format!(
                "{at}: supports_usage_in_streaming 仅适用于协议 openai-completions"
            ));
        }
        if self.auth.is_some() && api_type != ApiType::OpenAiCompletions {
            problems.push(format!("{at}: auth 仅适用于协议 openai-completions"));
        }
        // reasoning_policy 是跨协议的重放策略，不需要协议 gate。
    }
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
    /// 同一 thinking 家族内的代际差异。None = 走该家族的通用 wire。
    pub thinking_profile: Option<compat::ThinkingProfile>,
    /// 压缩/清空路径是否允许显式删除推理载体。
    pub clearable: bool,
    /// assistant 消息是否必须带推理载体字段。
    pub assistant_reasoning: compat::AssistantReasoning,
    /// 流式 usage 三态；只有 Supported 才发送 stream_options。
    pub stream_usage: compat::StreamUsage,
    /// 多轮回传策略
    pub policy: ReasoningPolicy,
    /// 输出预算字段声明（来自新声明层；None = 未声明）
    pub output_budget: Option<compat::OutputBudget>,
    /// 认证方式（直通自 Compat.auth）
    pub auth: Option<AuthMode>,
    /// 流式响应是否发送 `stream_options.include_usage`（默认 true，保持旧行为）
    pub supports_usage_in_streaming: bool,
}

impl Default for ResolvedCompat {
    fn default() -> Self {
        Self {
            carrier: None,
            request_param: None,
            thinking_profile: None,
            clearable: false,
            assistant_reasoning: compat::AssistantReasoning::Optional,
            stream_usage: compat::StreamUsage::Unknown,
            policy: ReasoningPolicy::Preserve,
            output_budget: None,
            auth: None,
            supports_usage_in_streaming: true,
        }
    }
}

/// 推理链多轮回传策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
/// 由 thinking_format + thinking_profile + api_type + has_reasoning 确定
/// carrier / request_param / profile / policy。
/// auth 不参与计算，由调用方从 Compat.auth 直通。
pub fn resolve_compat(
    thinking_format: Option<ThinkingFormat>,
    thinking_profile: Option<compat::ThinkingProfile>,
    api_type: ApiType,
    has_reasoning: bool,
) -> ResolvedCompat {
    if !has_reasoning {
        return ResolvedCompat::default();
    }

    let mut resolved = match (thinking_format, api_type) {
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
        // 智谱 → reasoning_content + thinking.type（clear_thinking 由注入层按历史决定）
        (Some(ThinkingFormat::Zhipu), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            request_param: Some("zhipu".into()),
            policy: ReasoningPolicy::RequireToolCall,
            ..Default::default()
        },
        // 通义千问 → reasoning_content + enable_thinking
        (Some(ThinkingFormat::Qwen), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            request_param: Some("enable_thinking".into()),
            ..Default::default()
        },
        // Kimi → reasoning_content + thinking / reasoning_effort（按模型代际分流）
        // Kimi 的 Preserved Thinking 要求保留普通 assistant 轮次的推理链，
        // 因此 final reasoning 不能按 RequireToolCall 丢弃；工具轮回传由
        // 适配器按 request_param=kimi 单独校验。
        (Some(ThinkingFormat::Kimi), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            request_param: Some("kimi".into()),
            ..Default::default()
        },
        // 火山方舟 → reasoning_content + thinking.type / reasoning_effort
        (Some(ThinkingFormat::Volcengine), ApiType::OpenAiCompletions) => ResolvedCompat {
            carrier: Some("reasoning_content".into()),
            request_param: Some("volcengine".into()),
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
    };
    resolved.thinking_profile = thinking_profile;
    resolved
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
    /// Anthropic thinking / redacted_thinking 原始块（R-27）。
    /// 只对 Anthropic 请求回传；其他协议忽略。不参与 ChatMessage 的 JSON
    /// 序列化（OpenAI 适配器直接序列化整条消息，不能把该字段发出去）。
    #[serde(skip)]
    pub provider_blocks: Option<Vec<ProviderBlock>>,
    /// 本消息附带的图像文件路径（来自 user/message 事件的附件）。
    /// 仅供请求前物化（路径 → base64），不序列化、不进 API。
    #[serde(skip)]
    pub image_paths: Option<Vec<String>>,
    /// 物化后的图像数据（请求前由 orchestrator 从 image_paths 读取填入）。
    /// 各 client 在 build 时展开为 provider 的 content 数组；不序列化。
    #[serde(skip)]
    pub images: Option<Vec<ImagePart>>,
    /// 工具结果是否成功。仅 Anthropic 的 tool_result.is_error 使用；
    /// 不序列化进 OpenAI 消息体，None = 旧数据/未知。
    #[serde(skip)]
    pub tool_ok: Option<bool>,
    /// 派生自 user/message 事件时记下 turn 内相对序号，供 image/delivery 定位。
    /// 用相对序号：归档后 turn 重新拼接，绝对 seq 会漂移。
    #[serde(skip)]
    pub image_event_seq: Option<usize>,
    /// 派生自工具图片消息时记下 call id，供 image/delivery 定位。
    #[serde(skip)]
    pub image_tool_call_id: Option<String>,
}

/// 图像输入块（已编码）。
///
/// 随 [`ChatMessage`] 传到各 client，在请求体构造时展开为 provider 的
/// content 数组（OpenAI `image_url` / Anthropic `image`）。data 为纯 base64，
/// 不带 `data:` 前缀（前缀由各 client 按 provider 格式拼）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePart {
    /// base64 编码的图像数据（不含 data: 前缀）
    pub data: String,
    /// MIME 类型，如 "image/png"
    pub mime_type: String,
}

/// 协议无关的工具配对归一化。
///
/// 在三个适配器构造 body 前统一执行：丢弃孤儿 tool result、丢弃没有后续
/// tool result 的 call、重复 call id 保留首个、清空后仍为空的 assistant 直接移除。
/// Phase 3 选择“丢弃 + 日志”而不是伪造工具结果，避免把不存在的工具输出
/// 发给模型。
pub fn normalize_tool_pairs(messages: &[ChatMessage], _api_type: ApiType) -> Vec<ChatMessage> {
    use std::collections::HashSet;

    let result_ids: HashSet<&str> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .filter_map(|m| m.tool_call_id.as_deref())
        // 空 id 不是身份键；否则多个畸形调用会被折叠成同一条。
        .filter(|id| !id.trim().is_empty())
        .collect();

    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut kept_calls: HashSet<String> = HashSet::new();
    let mut seen_results: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(messages.len());

    for message in messages {
        match message.role {
            MessageRole::Assistant => {
                let Some(calls) = message.tool_calls.as_ref() else {
                    out.push(message.clone());
                    continue;
                };

                let mut kept = Vec::new();
                for call in calls {
                    if call.id.trim().is_empty() {
                        continue;
                    }
                    if !seen_calls.insert(call.id.clone()) {
                        continue;
                    }
                    if !result_ids.contains(call.id.as_str()) {
                        continue;
                    }
                    kept_calls.insert(call.id.clone());
                    kept.push(call.clone());
                }

                if kept.is_empty() {
                    if !message.content.is_empty() {
                        let mut copy = message.clone();
                        copy.tool_calls = None;
                        out.push(copy);
                    }
                    continue;
                }

                let mut copy = message.clone();
                copy.tool_calls = Some(kept);
                out.push(copy);
            }
            MessageRole::Tool => {
                let Some(id) = message.tool_call_id.as_deref() else {
                    continue;
                };
                if id.trim().is_empty()
                    || !kept_calls.contains(id)
                    || !seen_results.insert(id.to_string())
                {
                    continue;
                }
                out.push(message.clone());
            }
            _ => out.push(message.clone()),
        }
    }

    out
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            provider_blocks: None,
            image_paths: None,
            images: None,
            tool_ok: None,
            image_event_seq: None,
            image_tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            provider_blocks: None,
            image_paths: None,
            images: None,
            tool_ok: None,
            image_event_seq: None,
            image_tool_call_id: None,
        }
    }

    /// 带图像路径的 user 消息（来自 history 附件的播种）。物化由请求前完成。
    pub fn user_with_image_paths(content: impl Into<String>, image_paths: Vec<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            provider_blocks: None,
            image_paths: Some(image_paths),
            images: None,
            tool_ok: None,
            image_event_seq: None,
            image_tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            provider_blocks: None,
            image_paths: None,
            images: None,
            tool_ok: None,
            image_event_seq: None,
            image_tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            reasoning_content,
            provider_blocks: None,
            image_paths: None,
            images: None,
            tool_ok: None,
            image_event_seq: None,
            image_tool_call_id: None,
        }
    }

    /// 附加 Anthropic thinking 原始块（R-27 回传用）。
    pub fn with_provider_blocks(mut self, blocks: Vec<ProviderBlock>) -> Self {
        if !blocks.is_empty() {
            self.provider_blocks = Some(blocks);
        }
        self
    }

    /// 工具执行结果消息。tool_call_id 用于 LLM API 侧匹配对应的 tool_call。
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            reasoning_content: None,
            provider_blocks: None,
            image_paths: None,
            images: None,
            tool_ok: None,
            image_event_seq: None,
            image_tool_call_id: None,
        }
    }

    /// 带成功/失败状态的工具结果（Anthropic `tool_result.is_error` 用）。
    pub fn tool_result_with_status(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        ok: bool,
    ) -> Self {
        Self {
            role: MessageRole::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            reasoning_content: None,
            provider_blocks: None,
            image_paths: None,
            images: None,
            tool_ok: Some(ok),
            image_event_seq: None,
            image_tool_call_id: None,
        }
    }
}

/// 供应商原始内容块（R-27）。
///
/// 只存需要原序回传的 Anthropic thinking 块；普通 text/tool_use 仍由
/// `ChatMessage.content` / `tool_calls` 表达。其他供应商不产生该枚举。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProviderBlock {
    #[serde(rename = "anthropic-thinking", rename_all = "camelCase")]
    AnthropicThinking { thinking: String, signature: String },
    #[serde(rename = "anthropic-redacted-thinking", rename_all = "camelCase")]
    AnthropicRedactedThinking { data: String },
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String, // 固定 "function"
    pub function: ToolDefFunction,
}

impl ToolDef {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// 本次调用请求的工具调用（生产路径经事件通道消费；
    /// 本字段当前仅测试读取）。
    #[allow(dead_code)]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 本次调用的 token 用量（provider 未提供 / Mock → None）
    pub usage: Option<TokenUsage>,
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
    /// 上下文水位专用输入总量。
    ///
    /// Anthropic 的 `input_tokens` 只含未命中缓存部分，水位需加上
    /// `cache_read_input_tokens` 与 `cache_creation_input_tokens`；其他协议
    /// 与 `input_tokens` 同义，置 None 由前端回退。用于上下文占用判断；
    /// Anthropic 的缓存命中率分母也使用它，以覆盖缓存写入部分。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_input_tokens: Option<u32>,
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

/// 统计增量：一次调用的归一化用量（缓存口径统一，见 [`usage_stat`]）。
///
/// 与 [`TokenUsage`] 分开：TokenUsage 承载「provider 报什么」（供上下文占用与
/// 该次调用原样锚），UsageStat 承载「统计要什么」（缓存命中率分母已按协议归一）。
/// 二者不混，避免为统计字段改动 TokenUsage 的既有序列化契约。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStat {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 命中缓存的 token（provider 未报 → None，与「命中 0」区分）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_tokens: Option<u32>,
    /// 该次可被缓存命中的 prompt 全量（命中率分母；按协议归一）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_prompt_tokens: Option<u32>,
}

/// 把一次调用的 provider 用量归一化为统计增量。
///
/// 缓存口径：不同协议的 `input_tokens` 含义不同——
/// - openai-completions / openai-responses：`input_tokens` = 完整 prompt（含命中缓存）
/// - anthropic-messages：`input_tokens` 只含未命中部分，命中单列在 cache_read
///
/// 统一输出 `cache_prompt_tokens` = 该次 prompt 全量（命中率分母）。
/// provider 不报缓存（`cache_hit_tokens == None`）或报 0 时两个缓存字段留 None。
/// 0 不进入统计：首轮无前缀可命中、缓存尚未建立、真实 miss 三种情况都会报 0，
/// 把它当命中率会制造“0%”的假精确；原始 0 仍保留在 TokenUsage 供排查。
pub fn usage_stat(usage: &TokenUsage, api_type: ApiType) -> UsageStat {
    let cache_hit = usage.cache_hit_tokens.filter(|hit| *hit > 0);
    let cache_prompt = cache_hit.map(|hit| match api_type {
        ApiType::AnthropicMessages => usage
            .context_input_tokens
            .unwrap_or_else(|| usage.input_tokens.saturating_add(hit)),
        _ => usage.input_tokens,
    });
    UsageStat {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_hit_tokens: cache_hit,
        cache_prompt_tokens: cache_prompt,
    }
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
    /// 供应商原始内容块（当前仅 Anthropic thinking）。
    /// 不参与增量正文；由上层写入 log，下一轮按原序回传。
    ProviderBlock {
        block: ProviderBlock,
    },
    ToolCall {
        /// 工具调用唯一 id（LLM API 分配，用于匹配 tool_result）
        id: String,
        name: String,
        arguments: String,
    },
    /// 供应商已报告的用量快照。
    ///
    /// 正常完成时以 `StreamResult.usage` 为准；这个事件让中止的流也能
    /// 保留已经到达的用量（例如 Anthropic message_start 的输入 token）。
    Usage {
        usage: TokenUsage,
    },
    TextDelta {
        content: String,
    },
}

/// 一次流式调用结束的原因（R-07）。
///
/// `None` 表示适配器未提供结束原因；`Normal` 是明确正常结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Normal,
    MaxTokens,
    ToolCalls,
    ContentFilter,
}

impl FinishReason {
    /// OpenAI Chat Completions 的 `finish_reason`。
    pub fn from_openai(raw: &str) -> Option<Self> {
        match raw {
            "stop" => Some(Self::Normal),
            "length" => Some(Self::MaxTokens),
            "tool_calls" | "function_call" => Some(Self::ToolCalls),
            "content_filter" => Some(Self::ContentFilter),
            _ => None,
        }
    }

    /// Anthropic Messages 的 `stop_reason`。
    pub fn from_anthropic(raw: &str) -> Option<Self> {
        match raw {
            "end_turn" | "stop_sequence" => Some(Self::Normal),
            "max_tokens" => Some(Self::MaxTokens),
            "tool_use" => Some(Self::ToolCalls),
            "refusal" => Some(Self::ContentFilter),
            _ => None,
        }
    }

    /// OpenAI Responses 的 `status` + `incomplete_details.reason`。
    pub fn from_responses(status: Option<&str>, incomplete_reason: Option<&str>) -> Option<Self> {
        match (status, incomplete_reason) {
            (Some("completed"), _) => Some(Self::Normal),
            (Some("incomplete"), Some("max_output_tokens")) => Some(Self::MaxTokens),
            (Some("incomplete"), Some("content_filter")) => Some(Self::ContentFilter),
            (Some("incomplete"), _) => None,
            _ => None,
        }
    }
}

/// 流式调用完成后的聚合产物。
///
/// chat_stream() 结束时返回，供调用方归档：
///   text       — 完整最终回答文本
///   tool_calls — 本轮产出的全部工具调用（空 vec = 未调工具）
///   reasoning  — 完整的推理链（推理模型提供，普通模型为 None）
///   usage      — 本次 API 调用的 token 用量（Mock 填 None）
///   finish_reason — 适配器报告的结束原因；None = 未观测
#[derive(Debug, Clone)]
pub struct StreamResult {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning: Option<String>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: Option<FinishReason>,
}

impl StreamResult {
    /// 正常结束却没有任何可见内容或工具调用。
    ///
    /// 对齐 DSH 的 `EMPTY_RESPONSE`：空响应属于可重试的临时失败，
    /// 不能当成一轮“什么都没说”的成功回答。max-tokens 截断单独处理，
    /// 这里不把“输出上限为 0”误判成供应商空响应。
    pub fn is_empty_response(&self) -> bool {
        self.tool_calls.is_empty()
            && self.text.trim().is_empty()
            && self
                .reasoning
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            && matches!(self.finish_reason, None | Some(FinishReason::Normal))
    }
}

// ═══════════════════════════════════════════════════════
// 错误类型
// ═══════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LlmError {
    /// 配置缺失或无效（api_key 为空、model 名不合法等）
    Config(String),
    /// LLM API 返回 HTTP 错误（通用/旧路径）
    Api(String),
    /// LLM API 返回结构化 HTTP 错误
    Http {
        status: u16,
        message: String,
        /// 供应商通过 `Retry-After` 给出的等待时间（毫秒）。
        /// 缺失时由本地指数退避决定。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
    /// 响应解析失败（JSON 格式不符预期）
    Parse(String),
    /// 网络连接失败（DNS、TLS、连接中断等，非超时）
    Transport(String),
    /// 供应商正常结束，但没有任何正文、推理或工具调用
    EmptyResponse,
    /// 供应商明确以内容安全策略拦截本轮回答
    ContentFilter,
    /// 账户额度、余额或预算已耗尽；重试不会恢复
    Quota(String),
    /// 网络超时
    Timeout(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "LLM 配置错误：{msg}"),
            Self::Api(msg) => write!(f, "LLM API 错误：{msg}"),
            Self::Http {
                status, message, ..
            } => {
                let reason = match *status {
                    400 => "请求格式不被供应商接受",
                    401 | 403 => "认证失败，请检查 API Key",
                    404 => "接口或模型不存在",
                    429 => "请求被供应商限流",
                    500..=599 => "供应商服务暂时不可用",
                    _ => "供应商返回 HTTP 错误",
                };
                if message.trim().is_empty() || *status == 401 || *status == 403 {
                    write!(f, "{reason}（HTTP {status}）")
                } else {
                    write!(f, "{reason}（HTTP {status}）：{message}")
                }
            }
            Self::Parse(msg) => write!(f, "LLM 响应解析错误：{msg}"),
            Self::Transport(msg) => write!(f, "LLM 网络连接失败：{msg}"),
            Self::EmptyResponse => write!(f, "供应商返回了空响应，本轮没有可显示的内容"),
            Self::ContentFilter => write!(f, "供应商安全策略拦截了本轮回答，未生成内容"),
            Self::Quota(msg) => {
                if msg.trim().is_empty() {
                    write!(f, "LLM 账户额度或余额不足")
                } else {
                    write!(f, "LLM 账户额度或余额不足：{msg}")
                }
            }
            Self::Timeout(msg) => write!(f, "LLM 请求超时：{msg}"),
        }
    }
}

impl LlmError {
    /// 构造结构化 HTTP 错误，并截断过长的 provider body。
    pub fn http(status: u16, message: impl Into<String>) -> Self {
        Self::http_with_retry_after(status, message, None)
    }

    /// 构造带供应商等待时间的 HTTP 错误。
    pub fn http_with_retry_after(
        status: u16,
        message: impl Into<String>,
        retry_after_ms: Option<u64>,
    ) -> Self {
        let message = message.into();
        let message = message.trim();
        let message: String = message.chars().take(500).collect();
        if status == 402 || is_quota_error_message(&message) {
            return Self::Quota(message);
        }
        Self::Http {
            status,
            message,
            retry_after_ms,
        }
    }

    /// 供应商要求的最短等待时间；只有 429/5xx 的响应头会被采用。
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::Http { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }
}

/// 从供应商错误响应中提取可诊断的 type/code/message。
///
/// 对照 DSH 的错误归一化：额度判断不能只看 message，因为有些供应商
/// 只在 `error.code` 里写 `insufficient_quota`。
pub(crate) fn provider_error_detail(body: &Value) -> Option<String> {
    let error = body.get("error")?;
    if let Some(text) = error.as_str() {
        return (!text.trim().is_empty()).then(|| text.to_string());
    }
    let mut parts = Vec::new();
    for key in ["type", "code", "message"] {
        if let Some(text) = error.get(key).and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                parts.push(text.to_string());
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// 识别“额度/余额已耗尽”而不是临时限流的措辞。
///
/// 规则对照 DSH 的 `isQuotaExceededError`：只匹配明确的额度、余额、
/// credits、预算或 usage limit 耗尽，避免把 `quota resets in one minute`
/// 这类临时信息误判成终止性失败。
pub(crate) fn is_quota_error_message(message: &str) -> bool {
    use std::sync::OnceLock;
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\binsufficient[\s_-]+(?:quota|balance|credits?)\b|\b(?:quota|usage[\s_-]+limit)[\s_-]+(?:exceeded|exhausted|reached)\b|\bexceed(?:ed|s)?[\s_-]+(?:(?:your|the)[\s_-]+)?(?:current[\s_-]+)?quota\b|\b(?:balance|credits?)[\s_-]+(?:exhausted|depleted)\b|\bout[\s_-]+of[\s_-]+(?:credits?|budget)\b",
        )
        .expect("quota regex 必须有效")
    });
    pattern.is_match(message)
}

/// 从响应头解析 `Retry-After`，支持秒数和 HTTP 日期。
///
/// 只接受正的有限等待时间；非法值返回 `None`，交回本地退避。
pub(crate) fn retry_after_ms_from_headers(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if raw.is_empty() {
        return None;
    }

    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(seconds.saturating_mul(1_000));
    }

    let target = chrono::DateTime::parse_from_rfc2822(raw)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::DateTime::parse_from_str(raw, "%A, %d-%b-%y %H:%M:%S GMT")
                .map(|dt| dt.with_timezone(&chrono::Utc))
        })
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(raw, "%a %b %e %H:%M:%S %Y")
                .map(|dt| dt.and_utc())
        })
        .ok()?;
    let delta = target.signed_duration_since(chrono::Utc::now());
    if delta <= chrono::TimeDelta::zero() {
        return Some(0);
    }
    u64::try_from(delta.num_milliseconds()).ok()
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
/// - **I4**：usage 先于 finish——流式过程可以发出 `Usage` 快照，
///   但聚合结果 `StreamResult.usage` 在返回时已给出并以它为准
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
            StreamEvent::ToolCall {
                id,
                name,
                arguments,
            } => Some((id.as_str(), name.as_str(), arguments.as_str())),
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
    /// 模型级输出能力上限；None = 未收录，不猜。
    pub max_output: Option<u32>,
    /// Phase 1 并行解析的新声明层；生产请求仍读 `compat`。
    pub declared_compat: Option<compat::Compat>,
    /// 思考强度白名单（models.json `thinking_levels`；空 = 未声明 → UI 不显示
    /// 思考强度行、请求不干预）。canonical 档：off/low/medium/high/max。
    pub thinking_levels: Vec<String>,
    /// 模型默认思考档（已设档不在白名单时静默回退到此档；未声明 → None）。
    pub default_thinking_level: Option<String>,
}

/// 供应商条目。
/// 思考强度的 canonical 档位全集（models.json 的 thinking_levels 自检用）。
pub const CANONICAL_THINKING_LEVELS: &[&str] = &["off", "low", "medium", "high", "max"];

/// 模型 id 归一化：去掉 `provider/` 前缀并转小写（仅用于查表匹配，不动目录原值）。
pub fn normalize_model_id(id: &str) -> String {
    id.rsplit('/')
        .next()
        .unwrap_or(id)
        .trim()
        .to_ascii_lowercase()
}

#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub api_type: ApiType,
    pub base_url: String,
    pub default_compat: Compat,
    /// Phase 1 并行解析的供应商级新声明层。
    pub default_declared_compat: compat::Compat,
    pub models: HashMap<String, ModelEntry>,
}

/// 模型元数据注册表。
///
/// 启动时从 models.json 加载，常驻内存（Tauri State）。
/// 提供 compat 合并查询和供应商/模型列表枚举。
#[derive(Debug)]
pub struct ModelRegistry {
    providers: HashMap<String, ProviderEntry>,
    /// provider id → 展示名（硬编码映射表）
    display_names: HashMap<String, String>,
    /// provider id → （归一化模型 id → 目录内 canonical id）
    /// 远端 /models 的写法差异（`openai/gpt-5.6`、大小写不同）靠它吸收。
    norm_ids: HashMap<String, HashMap<String, String>>,
}

impl ModelRegistry {
    /// 并行解析新旧声明。旧声明供生产路径使用；新声明只做合并与校验。
    fn parse_compat_decl_strict(
        value: Option<&Value>,
        _api_type: ApiType,
        at: &str,
    ) -> Result<(Option<Compat>, compat::Compat, bool), String> {
        let Some(value) = value else {
            return Ok((None, compat::Compat::default(), false));
        };
        let wire: compat::CompatWire = serde_json::from_value(value.clone())
            .map_err(|e| format!("{at}: 新声明解析失败: {e}"))?;
        let old = if wire.uses_new_schema {
            wire.legacy_compat()
        } else {
            Some(
                serde_json::from_value::<Compat>(value.clone())
                    .map_err(|e| format!("{at}: 旧声明解析失败: {e}"))?,
            )
        };
        Ok((old, wire.compat, wire.uses_new_schema))
    }

    /// 从 models.json 加载，不附带模型输出上限。
    pub fn from_json(json: &str) -> Result<Self, String> {
        Self::from_json_with_limits(json, "{}")
    }

    /// 从 models.json 和独立的输出上限词典加载。
    pub fn from_json_with_limits(json: &str, limits_json: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct OutputLimitsFile {
            #[serde(default)]
            limits: HashMap<String, u32>,
        }

        let limits: HashMap<String, u32> = serde_json::from_str::<OutputLimitsFile>(limits_json)
            .map_err(|e| format!("model-output-limits.json 解析失败: {e}"))?
            .limits;

        #[derive(Deserialize)]
        struct ModelsFile {
            #[serde(flatten)]
            providers: HashMap<String, ProviderJson>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ProviderJson {
            defaults: DefaultsJson,
            models: HashMap<String, ModelJson>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct DefaultsJson {
            api_type: ApiType,
            base_url: String,
            #[serde(default)]
            compat: Option<Value>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ModelJson {
            name: String,
            context: u32,
            #[serde(default)]
            reasoning: bool,
            #[serde(default, rename = "supports_vision")]
            supports_vision: bool,
            #[serde(default)]
            compat: Option<Value>,
            /// 思考强度白名单（canonical：off/low/medium/high/max）
            #[serde(default, rename = "thinking_levels")]
            thinking_levels: Vec<String>,
            #[serde(default, rename = "default_thinking_level")]
            default_thinking_level: Option<String>,
        }

        let file: ModelsFile =
            serde_json::from_str(json).map_err(|e| format!("models.json 解析失败: {e}"))?;

        // ── 目录自检（2026-10-08）──
        // 错误在加载时暴露，不在运行时静默降级。键名/多写字段已被上面的
        // deny_unknown_fields 挡住；这里补「值必须合法」的三项。
        let mut problems: Vec<String> = Vec::new();
        for (pid, pj) in file.providers.iter() {
            if pj.defaults.base_url.trim().is_empty() {
                problems.push(format!("{pid}: defaults.base_url 为空"));
            }
            let provider_at = format!("{pid}: defaults.compat");
            let (provider_old, provider_new, provider_uses_new) =
                match Self::parse_compat_decl_strict(
                    pj.defaults.compat.as_ref(),
                    pj.defaults.api_type,
                    &provider_at,
                ) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        problems.push(err);
                        (None, compat::Compat::default(), false)
                    }
                };
            if let Some(ref compat) = provider_old {
                compat.validate(pj.defaults.api_type, &provider_at, &mut problems);
            }
            if provider_uses_new {
                if let Err(err) = provider_new.validate(pj.defaults.api_type, &provider_at) {
                    problems.push(err);
                }
            }
            for (mid, m) in pj.models.iter() {
                let at = format!("{pid}/{mid}");
                if m.name.trim().is_empty() {
                    problems.push(format!("{at}: name 为空"));
                }
                let (model_old, model_new, model_uses_new) = match Self::parse_compat_decl_strict(
                    m.compat.as_ref(),
                    pj.defaults.api_type,
                    &format!("{at}: compat"),
                ) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        problems.push(err);
                        (None, compat::Compat::default(), false)
                    }
                };
                if let Some(ref compat) = model_old {
                    compat.validate(
                        pj.defaults.api_type,
                        &format!("{at}: compat"),
                        &mut problems,
                    );
                }
                if model_uses_new {
                    // Model declarations inherit the provider format, so
                    // validate the merged contract rather than the model patch
                    // in isolation.
                    let merged = compat::Compat::merge(&provider_new, &model_new);
                    if let Err(err) =
                        merged.validate(pj.defaults.api_type, &format!("{at}: merged compat"))
                    {
                        problems.push(err);
                    }
                }
                if m.context == 0 {
                    problems.push(format!("{at}: context 为 0"));
                }
                for lv in m.thinking_levels.iter() {
                    if !CANONICAL_THINKING_LEVELS.contains(&lv.as_str()) {
                        problems.push(format!("{at}: 未知思考档位 {lv}"));
                    }
                }
                if let Some(d) = m.default_thinking_level.as_deref() {
                    if !CANONICAL_THINKING_LEVELS.contains(&d) {
                        problems.push(format!("{at}: 未知默认思考档位 {d}"));
                    }
                }
            }
        }
        if !problems.is_empty() {
            return Err(format!(
                "models.json 自检未通过（{} 处）：{}",
                problems.len(),
                problems.join("；")
            ));
        }

        let mut providers = HashMap::new();
        for (id, pj) in file.providers {
            let (provider_old, provider_new, _) = Self::parse_compat_decl_strict(
                pj.defaults.compat.as_ref(),
                pj.defaults.api_type,
                &format!("{id}: defaults.compat"),
            )?;

            let mut models = HashMap::new();
            for (mid, mj) in pj.models {
                let (model_old, model_new, _) = Self::parse_compat_decl_strict(
                    mj.compat.as_ref(),
                    pj.defaults.api_type,
                    &format!("{id}/{mid}: compat"),
                )?;
                let max_output = limits.get(&format!("{id}/{mid}")).copied();
                models.insert(
                    mid,
                    ModelEntry {
                        name: mj.name,
                        context: mj.context,
                        reasoning: mj.reasoning,
                        supports_vision: mj.supports_vision,
                        compat: model_old,
                        max_output,
                        declared_compat: Some(model_new),
                        thinking_levels: mj.thinking_levels,
                        default_thinking_level: mj.default_thinking_level,
                    },
                );
            }

            providers.insert(
                id,
                ProviderEntry {
                    api_type: pj.defaults.api_type,
                    base_url: pj.defaults.base_url,
                    default_compat: provider_old.unwrap_or_default(),
                    default_declared_compat: provider_new,
                    models,
                },
            );
        }

        let display_names = Self::build_display_names();

        // 归一化索引：远端 /models 可能返回 `openai/gpt-5.6` 或大小写不同的写法，
        // 查表时抹平（目录里仍是原样 id，不影响展示与序列化）。
        let norm_ids: HashMap<String, HashMap<String, String>> = providers
            .iter()
            .map(|(pid, pe)| {
                let index = pe
                    .models
                    .keys()
                    .map(|k| (normalize_model_id(k), k.clone()))
                    .collect();
                (pid.clone(), index)
            })
            .collect();

        Ok(Self {
            providers,
            display_names,
            norm_ids,
        })
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
                if mc.thinking_profile.is_some() {
                    merged.thinking_profile = mc.thinking_profile;
                }
                if mc.reasoning_clearable.is_some() {
                    merged.reasoning_clearable = mc.reasoning_clearable;
                }
                if mc.assistant_reasoning.is_some() {
                    merged.assistant_reasoning = mc.assistant_reasoning;
                }
                if mc.stream_usage.is_some() {
                    merged.stream_usage = mc.stream_usage;
                }
                if mc.auth.is_some() {
                    merged.auth = mc.auth;
                }
                if mc.reasoning_policy.is_some() {
                    merged.reasoning_policy = mc.reasoning_policy;
                }
                if mc.supports_usage_in_streaming.is_some() {
                    merged.supports_usage_in_streaming = mc.supports_usage_in_streaming;
                }
            }
        }

        merged
    }

    /// 查询指定模型的 Phase 1 新声明层（供货商默认 + 模型覆盖）。
    pub fn get_declared_compat(&self, provider: &str, model: &str) -> compat::Compat {
        let Some(pe) = self.providers.get(provider) else {
            return compat::Compat::default();
        };
        let mut merged = pe.default_declared_compat.clone();
        if let Some(me) = pe.models.get(model) {
            if let Some(ref declared) = me.declared_compat {
                merged = compat::Compat::merge(&merged, declared);
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
        // Phase 1 的新声明层是 carrier / output_budget 的真实来源；
        // 旧 Compat 只保留兼容字段，不能再覆盖新声明。
        let declared = self.get_declared_compat(provider, model);
        let has_reasoning = self
            .providers
            .get(provider)
            .and_then(|pe| pe.models.get(model))
            .map(|me| me.reasoning)
            .unwrap_or(false);

        let mut resolved = resolve_compat(
            compat.thinking_format,
            compat.thinking_profile,
            api_type,
            has_reasoning,
        );
        resolved.clearable = compat.reasoning_clearable.unwrap_or(false);
        resolved.assistant_reasoning = compat
            .assistant_reasoning
            .unwrap_or(compat::AssistantReasoning::Optional);
        let derived_stream_usage = match compat.supports_usage_in_streaming {
            Some(true) => compat::StreamUsage::Supported,
            Some(false) => compat::StreamUsage::Unsupported,
            None => compat::StreamUsage::Unknown,
        };
        resolved.stream_usage = compat.stream_usage.unwrap_or(derived_stream_usage);
        resolved.supports_usage_in_streaming =
            resolved.stream_usage == compat::StreamUsage::Supported;
        // 模型级 policy 覆盖 thinking_format 家族默认值。GLM-5.3 的
        // preserve 由 models.json 声明，不再按 provider/model 名硬编码。
        if let Some(policy) = compat.reasoning_policy {
            resolved.policy = policy;
        }
        if let Some(supports) = compat.supports_usage_in_streaming {
            resolved.supports_usage_in_streaming = supports;
        }
        // 新声明层覆盖旧投影：carrier / profile / clearable / policy / budget
        // 都以声明为准；旧字段只在未声明时保留家族默认。
        if let Some(profile) = declared.thinking_profile {
            resolved.thinking_profile = Some(profile);
        }
        if let Some(replay) = declared.reasoning_replay.as_ref() {
            if let Some(carrier) = replay.carrier {
                resolved.carrier = Some(
                    match carrier {
                        compat::Carrier::ReasoningContent => "reasoning_content",
                        compat::Carrier::ThinkingBlocks => "thinking_blocks",
                        compat::Carrier::ReasoningItems => "reasoning_items",
                        compat::Carrier::ReasoningDetails => "reasoning_details",
                        compat::Carrier::ThoughtSignature => "thought_signature",
                    }
                    .to_string(),
                );
            }
            if let Some(clearable) = replay.clearable {
                resolved.clearable = clearable;
            }
            if let Some(assistant) = replay.assistant_reasoning {
                resolved.assistant_reasoning = assistant;
            }
            if let Some(policy) = replay.policy {
                resolved.policy = match policy {
                    compat::ReplayPolicy::Preserve => ReasoningPolicy::Preserve,
                    compat::ReplayPolicy::RequireToolCall => ReasoningPolicy::RequireToolCall,
                    // 旧效果层没有“无推理”变体；carrier=None 让传输层剥离。
                    compat::ReplayPolicy::None => {
                        resolved.carrier = None;
                        ReasoningPolicy::Preserve
                    }
                };
            }
        }
        resolved.output_budget = declared.output_budget;
        // auth 不参与 resolve_compat 计算——从声明层直通效果层
        resolved.auth = compat.auth;
        resolved
    }

    /// 供应商展示排序（国内 → 本地 → 国际）。
    /// 不在列表中的供应商排在末尾，按 id 字母序。
    const PROVIDER_ORDER: &'static [&'static str] = &[
        "deepseek",
        "zhipu",
        "kimi",
        "qwen",
        "volcengine",
        "ollama",
        "openai",
        "anthropic",
        "gemini",
    ];

    /// 枚举所有供应商的基本信息（供 list_providers 命令使用）。
    pub fn list_providers(&self) -> Vec<ProviderInfo> {
        let mut list: Vec<ProviderInfo> = self
            .providers
            .iter()
            .map(|(id, pe)| ProviderInfo {
                id: id.clone(),
                display_name: self
                    .display_names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.clone()),
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

    /// 枚举全部已收录模型（provider + api_type + model id），按稳定顺序返回。
    ///
    /// 仅供测试与能力矩阵生成使用；生产路径仍按 provider/model 点查。
    #[cfg(test)]
    pub(crate) fn all_models(&self) -> Vec<(String, ApiType, String)> {
        let mut all: Vec<(String, ApiType, String)> = self
            .providers
            .iter()
            .flat_map(|(provider, entry)| {
                entry
                    .models
                    .keys()
                    .map(move |model| (provider.clone(), entry.api_type, model.clone()))
            })
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
        all
    }

    /// 获取指定模型的能力信息（供 list_models 匹配远端 ID）。
    ///
    /// 先按原样查；未命中再按归一化 id 查（远端清单可能带 `provider/` 前缀或
    /// 大小写不同，见 `normalize_model_id`）。
    pub fn get_model(&self, provider: &str, model: &str) -> Option<&ModelEntry> {
        let pe = self.providers.get(provider)?;
        pe.models.get(model).or_else(|| {
            let canonical = self
                .norm_ids
                .get(provider)?
                .get(&normalize_model_id(model))?;
            pe.models.get(canonical)
        })
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
        for p in [
            "deepseek",
            "openai",
            "anthropic",
            "ollama",
            "zhipu",
            "kimi",
            "qwen",
            "volcengine",
            "gemini",
        ] {
            assert!(default_base_url(p).is_some(), "预设 {p} 应有默认 URL");
        }
        assert!(default_base_url("my-custom").is_none(), "自定义需显式填写");
    }

    // ── 目录自检（2026-10-08）──

    #[test]
    fn registry_rejects_unknown_key() {
        // 键名写错（models.json 少一个 t）→ 加载期报错，不静默落默认
        let json = r#"{"p":{"defaults":{"api_type":"openai-completions","base_url":"https://x/v1"},"models":{"m":{"name":"M","context":1000,"reasoning_flag":true}}}}"#;
        let err = ModelRegistry::from_json(json).unwrap_err();
        assert!(err.contains("解析失败"), "got: {err}");
    }

    #[test]
    fn registry_rejects_invalid_values() {
        let bad_ctx = r#"{"p":{"defaults":{"api_type":"openai-completions","base_url":"https://x/v1"},"models":{"m":{"name":"M","context":0}}}}"#;
        assert!(ModelRegistry::from_json(bad_ctx)
            .unwrap_err()
            .contains("context 为 0"));

        let bad_name = r#"{"p":{"defaults":{"api_type":"openai-completions","base_url":"https://x/v1"},"models":{"m":{"name":"  ","context":1}}}}"#;
        assert!(ModelRegistry::from_json(bad_name)
            .unwrap_err()
            .contains("name 为空"));

        let bad_level = r#"{"p":{"defaults":{"api_type":"openai-completions","base_url":"https://x/v1"},"models":{"m":{"name":"M","context":1,"thinking_levels":["low","ultra"]}}}}"#;
        assert!(ModelRegistry::from_json(bad_level)
            .unwrap_err()
            .contains("未知思考档位"));
    }

    #[test]
    fn get_model_normalizes_remote_id() {
        let json = r#"{"openai":{"defaults":{"api_type":"openai-completions","base_url":"https://x/v1"},"models":{"gpt-5.6":{"name":"GPT-5.6","context":1000}}}}"#;
        let reg = ModelRegistry::from_json(json).unwrap();
        assert!(reg.get_model("openai", "gpt-5.6").is_some());
        assert!(
            reg.get_model("openai", "openai/gpt-5.6").is_some(),
            "带前缀应命中"
        );
        assert!(reg.get_model("openai", "GPT-5.6").is_some(), "大小写应命中");
        assert!(reg.get_model("openai", "nope").is_none());
        assert!(reg.get_model("no-such-provider", "gpt-5.6").is_none());
    }

    use super::*;

    // ── usage_stat 缓存口径（0 = 未观测）──

    fn usage_with_cache(input: u32, hit: Option<u32>) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            context_input_tokens: None,
            output_tokens: 10,
            cache_hit_tokens: hit,
            cache_miss_tokens: None,
        }
    }

    #[test]
    fn usage_stat_zero_cache_is_unobserved() {
        let stat = usage_stat(&usage_with_cache(100, Some(0)), ApiType::OpenAiCompletions);
        assert_eq!(stat.cache_hit_tokens, None);
        assert_eq!(stat.cache_prompt_tokens, None);
    }

    #[test]
    fn usage_stat_positive_cache_keeps_denominator() {
        let stat = usage_stat(&usage_with_cache(100, Some(80)), ApiType::OpenAiCompletions);
        assert_eq!(stat.cache_hit_tokens, Some(80));
        assert_eq!(stat.cache_prompt_tokens, Some(100));
    }

    #[test]
    fn usage_stat_anthropic_cache_adds_hit_to_denominator() {
        let stat = usage_stat(&usage_with_cache(20, Some(80)), ApiType::AnthropicMessages);
        assert_eq!(stat.cache_hit_tokens, Some(80));
        assert_eq!(stat.cache_prompt_tokens, Some(100));
    }

    #[test]
    fn usage_stat_anthropic_denominator_includes_cache_creation() {
        let mut usage = usage_with_cache(20, Some(80));
        usage.context_input_tokens = Some(120);
        let stat = usage_stat(&usage, ApiType::AnthropicMessages);
        assert_eq!(stat.cache_hit_tokens, Some(80));
        assert_eq!(stat.cache_prompt_tokens, Some(120));
    }

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
            function: ToolCallFunction {
                name: "query".into(),
                arguments: "{}".into(),
            },
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
        assert_eq!(
            serde_json::to_string(&MessageRole::System).unwrap(),
            "\"system\""
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::User).unwrap(),
            "\"user\""
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::Tool).unwrap(),
            "\"tool\""
        );
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
        let e = LlmError::http(401, "Incorrect API key");
        let text = e.to_string();
        assert!(text.contains("认证失败"), "{text}");
        assert!(
            !text.contains("Incorrect API key"),
            "401 不回显 provider body"
        );
        let e = LlmError::http(400, "bad request body");
        assert!(e.to_string().contains("bad request body"));
        let e = LlmError::http_with_retry_after(429, "rate limited", Some(3_000));
        assert_eq!(e.retry_after_ms(), Some(3_000));
        let e = LlmError::http(429, "rate limited");
        assert!(e.to_string().contains("限流"));
        assert!(!e.to_string().contains("额度"));
        let e = LlmError::http_with_retry_after(429, "insufficient_quota", None);
        assert!(matches!(e, LlmError::Quota(_)));
    }

    #[test]
    fn provider_error_detail_includes_code_and_type() {
        let body = serde_json::json!({
            "error": {
                "type": "insufficient_quota",
                "code": "insufficient_quota",
                "message": "You exceeded your current quota"
            }
        });
        let detail = provider_error_detail(&body).unwrap();
        assert!(detail.contains("insufficient_quota"));
        assert!(detail.contains("You exceeded your current quota"));
        assert!(is_quota_error_message(&detail));
    }

    #[test]
    fn quota_wording_is_distinguished_from_rate_limit() {
        for text in [
            "insufficient_quota",
            "You exceeded your current quota",
            "usage limit reached",
            "balance depleted",
            "out of credits",
        ] {
            assert!(is_quota_error_message(text), "应识别为额度耗尽: {text}");
        }
        for text in ["rate limit reached", "quota resets in one minute"] {
            assert!(!is_quota_error_message(text), "不应识别为额度耗尽: {text}");
        }
    }

    #[test]
    fn empty_response_detection_excludes_max_tokens() {
        let mut result = StreamResult {
            text: String::new(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: Some(FinishReason::Normal),
        };
        assert!(result.is_empty_response());
        result.finish_reason = Some(FinishReason::MaxTokens);
        assert!(!result.is_empty_response());
        result.finish_reason = Some(FinishReason::ContentFilter);
        assert!(!result.is_empty_response());
    }

    #[test]
    fn retry_after_header_parses_seconds_and_http_date() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("3"));
        assert_eq!(retry_after_ms_from_headers(&headers), Some(3_000));

        headers.insert(RETRY_AFTER, HeaderValue::from_static("not-a-date"));
        assert_eq!(retry_after_ms_from_headers(&headers), None);

        let future = chrono::Utc::now() + chrono::TimeDelta::seconds(30);
        let raw = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert(RETRY_AFTER, HeaderValue::from_str(&raw).unwrap());
        let parsed = retry_after_ms_from_headers(&headers).unwrap();
        assert!(
            (20_000..=31_000).contains(&parsed),
            "HTTP 日期应换算为约 30 秒，实际 {parsed}ms"
        );
    }

    // ── default_base_url ──

    #[test]
    fn default_base_url_known_providers() {
        assert_eq!(
            default_base_url("deepseek"),
            Some("https://api.deepseek.com/v1")
        );
        assert_eq!(
            default_base_url("openai"),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            default_base_url("anthropic"),
            Some("https://api.anthropic.com")
        );
        assert_eq!(
            default_base_url("ollama"),
            Some("http://localhost:11434/v1")
        );
    }

    #[test]
    fn registry_applies_output_limits_only_when_provided() {
        let json = r#"{"p":{"defaults":{"api_type":"openai-completions","base_url":"https://x"},"models":{"m":{"name":"M","context":1}}}}"#;
        let limits = r#"{"limits":{"p/m":32000,"p/nope":999}}"#;
        let reg = ModelRegistry::from_json_with_limits(json, limits).unwrap();
        assert_eq!(reg.get_model("p", "m").unwrap().max_output, Some(32000));
        assert_eq!(
            ModelRegistry::from_json(json)
                .unwrap()
                .get_model("p", "m")
                .unwrap()
                .max_output,
            None
        );
    }

    #[test]
    fn chat_output_budget_treats_legacy_default_as_unset() {
        assert_eq!(chat_output_budget(4096, None), 8192);
        assert_eq!(chat_output_budget(0, None), 8192);
        assert_eq!(chat_output_budget(16000, None), 16000);
        assert_eq!(chat_output_budget(64, None), 64);
        assert_eq!(chat_output_budget(4096, Some(32000)), 8192);
        assert_eq!(chat_output_budget(4096, Some(4096)), 4096);
        assert_eq!(chat_output_budget(16000, Some(8192)), 8192);
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
            thinking_profile: None,
            reasoning_clearable: None,
            assistant_reasoning: None,
            stream_usage: None,
            auth: Some(AuthMode::None),
            reasoning_policy: Some(ReasoningPolicy::Preserve),
            supports_usage_in_streaming: Some(false),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(
            json.contains("\"thinkingFormat\""),
            "Expected camelCase thinkingFormat, got: {json}"
        );
        assert!(json.contains("\"auth\""), "Expected auth, got: {json}");
        assert!(
            json.contains("\"reasoningPolicy\":\"preserve\""),
            "Expected camelCase reasoningPolicy, got: {json}"
        );
        assert!(
            json.contains("\"supportsUsageInStreaming\":false"),
            "Expected camelCase supportsUsageInStreaming, got: {json}"
        );
    }

    #[test]
    fn compat_deserializes_camelcase() {
        let json = r#"{"thinkingFormat":"deepseek","auth":"none","reasoningPolicy":"preserve"}"#;
        let c: Compat = serde_json::from_str(json).unwrap();
        assert_eq!(c.thinking_format, Some(ThinkingFormat::Deepseek));
        assert_eq!(c.auth, Some(AuthMode::None));
        assert_eq!(c.reasoning_policy, Some(ReasoningPolicy::Preserve));
    }

    #[test]
    fn compat_deserializes_snake_case_models_json_keys() {
        let json = r#"{"thinking_format":"zhipu","reasoning_policy":"require_tool_call"}"#;
        let c: Compat = serde_json::from_str(json).unwrap();
        assert_eq!(c.thinking_format, Some(ThinkingFormat::Zhipu));
        assert_eq!(c.reasoning_policy, Some(ReasoningPolicy::RequireToolCall));
    }

    #[test]
    fn compat_rejects_thinking_format_for_wrong_protocol() {
        let json = r#"{"anthropic":{"defaults":{"api_type":"anthropic-messages","base_url":"https://x"},"models":{"m":{"name":"M","context":1,"compat":{"thinking_format":"qwen"}}}}}"#;
        let err = ModelRegistry::from_json(json).unwrap_err();
        assert!(err.contains("thinking_format qwen"), "got: {err}");
        assert!(
            err.contains("不适用于协议 anthropic-messages"),
            "got: {err}"
        );
    }

    #[test]
    fn compat_rejects_auth_for_non_completions_protocols() {
        let json = r#"{
            "anthropic": {
                "defaults": {
                    "api_type": "anthropic-messages",
                    "base_url": "https://x",
                    "compat": { "auth": "none" }
                },
                "models": { "m": { "name": "M", "context": 1 } }
            }
        }"#;
        let err = ModelRegistry::from_json(json).unwrap_err();
        assert!(
            err.contains("auth 仅适用于协议 openai-completions"),
            "got: {err}"
        );
    }

    #[test]
    fn compat_rejects_usage_streaming_for_wrong_protocol() {
        let json = r#"{"anthropic":{"defaults":{"api_type":"anthropic-messages","base_url":"https://x","compat":{"supports_usage_in_streaming":false}},"models":{"m":{"name":"M","context":1}}}}"#;
        let err = ModelRegistry::from_json(json).unwrap_err();
        assert!(
            err.contains("supports_usage_in_streaming 仅适用于协议 openai-completions"),
            "got: {err}"
        );
    }

    #[test]
    fn compat_model_usage_streaming_overrides_provider_default() {
        let json = r#"{"p":{"defaults":{"api_type":"openai-completions","base_url":"https://x","compat":{"supports_usage_in_streaming":true}},"models":{"m":{"name":"M","context":1,"compat":{"supports_usage_in_streaming":false}}}}}"#;
        let reg = ModelRegistry::from_json(json).unwrap();
        assert_eq!(
            reg.get_compat("p", "m").supports_usage_in_streaming,
            Some(false)
        );
        assert!(
            !reg.get_resolved_compat("p", "m", ApiType::OpenAiCompletions)
                .supports_usage_in_streaming
        );
    }

    #[test]
    fn compat_model_policy_overrides_provider_default() {
        let json = r#"{"p":{"defaults":{"api_type":"openai-completions","base_url":"https://x","compat":{"thinking_format":"zhipu","reasoning_policy":"require_tool_call"}},"models":{"m":{"name":"M","context":1,"reasoning":true,"compat":{"reasoning_policy":"preserve"}}}}}"#;
        let reg = ModelRegistry::from_json(json).unwrap();

        let c = reg.get_compat("p", "m");
        assert_eq!(c.thinking_format, Some(ThinkingFormat::Zhipu));
        assert_eq!(c.reasoning_policy, Some(ReasoningPolicy::Preserve));

        let r = reg.get_resolved_compat("p", "m", ApiType::OpenAiCompletions);
        assert_eq!(r.carrier.as_deref(), Some("reasoning_content"));
        assert_eq!(r.policy, ReasoningPolicy::Preserve);
    }

    #[test]
    fn declared_carrier_and_output_budget_drive_effective_compat() {
        let json = r#"{
            "p": {
                "defaults": {
                    "api_type": "openai-completions",
                    "base_url": "https://x",
                    "compat": {
                        "thinking_format": "deepseek",
                        "reasoning_replay": {
                            "policy": "preserve",
                            "carrier": "thinking_blocks"
                        },
                        "output_budget": {
                            "field": "max_completion_tokens",
                            "required": true
                        }
                    }
                },
                "models": { "m": { "name": "M", "context": 1000, "reasoning": true } }
            }
        }"#;
        let reg = ModelRegistry::from_json(json).unwrap();
        let resolved = reg.get_resolved_compat("p", "m", ApiType::OpenAiCompletions);
        assert_eq!(resolved.carrier.as_deref(), Some("thinking_blocks"));
        assert_eq!(
            resolved.output_budget.as_ref().and_then(|b| b.field),
            Some(compat::OutputField::MaxCompletionTokens)
        );
    }

    #[test]
    fn model_only_thinking_profile_survives_declared_projection() {
        let json = r#"{
            "p": {
                "defaults": {
                    "api_type": "openai-completions",
                    "base_url": "https://x",
                    "compat": { "thinking_format": "zhipu" }
                },
                "models": {
                    "m": {
                        "name": "M",
                        "context": 1000,
                        "reasoning": true,
                        "compat": { "thinking_profile": "zhipu-glm-5.3" }
                    }
                }
            }
        }"#;
        let reg = ModelRegistry::from_json(json).unwrap();
        let resolved = reg.get_resolved_compat("p", "m", ApiType::OpenAiCompletions);
        assert_eq!(
            resolved.thinking_profile,
            Some(compat::ThinkingProfile::ZhipuGlm53)
        );
    }

    #[test]
    fn new_compat_fields_are_parsed_in_parallel_and_project_to_old_path() {
        let json = r#"{
            "p": {
                "defaults": {
                    "api_type": "openai-completions",
                    "base_url": "https://x",
                    "compat": {
                        "thinking_format": "zhipu",
                        "reasoning_replay": {
                            "policy": "preserve",
                            "carrier": "reasoning_content"
                        }
                    }
                },
                "models": {
                    "m": {
                        "name": "M",
                        "context": 1000,
                        "reasoning": true,
                        "compat": { "stream_usage": "unsupported" }
                    }
                }
            }
        }"#;
        let reg = ModelRegistry::from_json(json).unwrap();

        // New declaration layer: provider defaults + model override.
        let declared = reg.get_declared_compat("p", "m");
        assert_eq!(
            declared.thinking_format,
            Some(compat::ThinkingFormat::Zhipu)
        );
        assert_eq!(
            declared
                .reasoning_replay
                .as_ref()
                .and_then(|replay| replay.policy),
            Some(compat::ReplayPolicy::Preserve)
        );
        assert_eq!(
            declared.stream_usage,
            Some(compat::StreamUsage::Unsupported)
        );

        // Old production path still gets the projected flat fields.
        let old = reg.get_compat("p", "m");
        assert_eq!(old.thinking_format, Some(ThinkingFormat::Zhipu));
        assert_eq!(old.reasoning_policy, Some(ReasoningPolicy::Preserve));
        assert_eq!(old.supports_usage_in_streaming, Some(false));
        let effective = reg.get_resolved_compat("p", "m", ApiType::OpenAiCompletions);
        assert_eq!(effective.carrier.as_deref(), Some("reasoning_content"));
        assert!(!effective.supports_usage_in_streaming);
    }

    #[test]
    fn invalid_new_compat_is_rejected_without_panic() {
        let json = r#"{
            "p": {
                "defaults": {
                    "api_type": "openai-completions",
                    "base_url": "https://x",
                    "compat": {
                        "reasoning_replay": { "policy": "require_tool_call" }
                    }
                },
                "models": { "m": { "name": "M", "context": 1 } }
            }
        }"#;
        let err = ModelRegistry::from_json(json).unwrap_err();
        assert!(err.contains("carrier"), "got: {err}");
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
                thinking_profile: None,
                reasoning_clearable: None,
                assistant_reasoning: None,
                stream_usage: None,
                auth: None,
                reasoning_policy: None,
                supports_usage_in_streaming: None,
            },
        };
        let json = serde_json::to_string(&info).unwrap();
        // ProviderInfo 的 compat 字段是嵌套的 Compat，必须验证 camelCase
        assert!(
            json.contains("\"thinkingFormat\""),
            "Nested compat must be camelCase, got: {json}"
        );
    }

    /// 真实 models.json 解析回归（2026-09-19 修）：文件里 `compat` 用 snake_case
    /// 键（thinking_format），Compat 结构声明 camelCase——键名错位曾静默落默认档，
    /// 使 carrier=None，出网前误剥离全部推理链。此测试直接从真身文件断言效果层
    /// 配置，堵住「测试只覆盖手搭结构、不读文件」的盲区。
    #[test]
    fn real_models_json_provider_compat_matrix() {
        let reg = ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap();
        for (provider, model, carrier, param) in [
            ("anthropic", "claude-fable-5", "thinking_blocks", None),
            ("zhipu", "glm-5-turbo", "reasoning_content", Some("zhipu")),
            ("kimi", "kimi-k3", "reasoning_content", Some("kimi")),
            (
                "qwen",
                "qwen3-max",
                "reasoning_content",
                Some("enable_thinking"),
            ),
            (
                "volcengine",
                "doubao-seed-2-1-pro-260628",
                "reasoning_content",
                Some("volcengine"),
            ),
        ] {
            let r = reg.get_resolved_compat(provider, model, ApiType::OpenAiCompletions);
            assert_eq!(
                r.carrier.as_deref(),
                Some(carrier),
                "{provider}/{model} carrier"
            );
            if provider == "kimi" {
                assert_eq!(
                    r.policy,
                    ReasoningPolicy::Preserve,
                    "Kimi 需要保留普通 assistant 轮次的推理链"
                );
            }
            assert_eq!(
                r.request_param.as_deref(),
                param,
                "{provider}/{model} request_param"
            );
        }
    }

    #[test]
    fn real_models_json_zhipu_53_metadata() {
        let reg = ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap();
        let base = reg.get_model("zhipu", "glm-5.3").unwrap();
        assert_eq!(base.context, 1_000_000);
        assert_eq!(base.thinking_levels, ["low", "high", "max"]);
        assert_eq!(base.default_thinking_level.as_deref(), Some("max"));
        assert!(!base.supports_vision);
        let resolved = reg.get_resolved_compat("zhipu", "glm-5.3", ApiType::OpenAiCompletions);
        assert_eq!(resolved.policy, ReasoningPolicy::Preserve);
        assert_eq!(
            resolved.thinking_profile,
            Some(compat::ThinkingProfile::ZhipuGlm53),
            "5.3 profile 必须来自 models.json"
        );
        assert!(resolved.clearable, "Zhipu 的 replay 声明 clearable=true");

        for id in ["glm-5.3-flash", "glm-5.3-flashx"] {
            let model = reg.get_model("zhipu", id).unwrap();
            assert_eq!(model.context, 1_000_000, "{id}");
            assert!(model.supports_vision, "{id} 应支持视觉");
            assert_eq!(model.default_thinking_level.as_deref(), Some("max"));
            let resolved = reg.get_resolved_compat("zhipu", id, ApiType::OpenAiCompletions);
            assert_eq!(resolved.policy, ReasoningPolicy::Preserve);
            assert_eq!(
                resolved.thinking_profile,
                Some(compat::ThinkingProfile::ZhipuGlm53Flash),
                "{id} 的 profile 必须来自 models.json，不再按模型名硬编码"
            );
        }
    }

    #[test]
    fn real_models_json_kimi_profiles_resolve() {
        let reg = ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap();
        for (id, profile) in [
            ("kimi-k2.6", compat::ThinkingProfile::KimiK26),
            ("kimi-k3", compat::ThinkingProfile::KimiK3),
            ("kimi-k2.7-code", compat::ThinkingProfile::KimiK27Code),
        ] {
            let resolved = reg.get_resolved_compat("kimi", id, ApiType::OpenAiCompletions);
            assert_eq!(
                resolved.thinking_profile,
                Some(profile),
                "{id} profile 必须来自 models.json"
            );
            assert_eq!(
                resolved.assistant_reasoning,
                compat::AssistantReasoning::Required,
                "{id} 的 assistant 推理字段必须存在"
            );
        }
    }

    #[test]
    fn provider_order_domestic_first() {
        let reg = ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap();
        let ids: Vec<String> = reg
            .list_providers()
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "deepseek",
                "zhipu",
                "kimi",
                "qwen",
                "volcengine",
                "ollama",
                "openai",
                "anthropic",
                "gemini",
            ]
        );
    }

    #[test]
    fn normalize_tool_pairs_drops_orphans_and_duplicates() {
        let call = |id: &str| ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        };
        let messages = vec![
            ChatMessage::user("q"),
            ChatMessage::assistant_with_tools("", vec![call("c1")], Some("r1".into())),
            ChatMessage::tool_result("c1", "ok"),
            ChatMessage::tool_result("orphan", "drop"),
            ChatMessage::assistant_with_tools("", vec![call("c2")], Some("r2".into())),
            ChatMessage::assistant_with_tools("", vec![call("c1")], Some("r3".into())),
        ];
        let out = normalize_tool_pairs(&messages, ApiType::OpenAiCompletions);
        assert_eq!(
            out.len(),
            3,
            "orphan result / orphan call / duplicate call 应被移除"
        );
        assert_eq!(out[0].role, MessageRole::User);
        assert_eq!(out[1].tool_calls.as_ref().unwrap()[0].id, "c1");
        assert_eq!(out[2].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn normalize_tool_pairs_does_not_collapse_empty_ids() {
        let call = |id: &str| ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        };
        let messages = vec![
            ChatMessage::user("q"),
            ChatMessage::assistant_with_tools("step1", vec![call("")], None),
            ChatMessage::tool_result("", "one"),
            ChatMessage::assistant_with_tools("step2", vec![call("")], None),
            ChatMessage::tool_result("", "two"),
        ];

        let out = normalize_tool_pairs(&messages, ApiType::OpenAiCompletions);
        assert!(
            out.iter()
                .all(|m| m.tool_calls.is_none() && m.role != MessageRole::Tool),
            "空 id 必须被丢弃，不能被当成同一个调用折叠"
        );
        assert!(out.iter().any(|m| m.content == "step1"));
        assert!(out.iter().any(|m| m.content == "step2"));
    }

    #[test]
    fn normalize_tool_pairs_keeps_empty_assistant_text_without_calls() {
        let messages = vec![ChatMessage::assistant("")];
        let out = normalize_tool_pairs(&messages, ApiType::OpenAiCompletions);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content, "");
    }

    #[test]
    fn real_models_json_deepseek_compat_resolves() {
        let reg = ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap();
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
