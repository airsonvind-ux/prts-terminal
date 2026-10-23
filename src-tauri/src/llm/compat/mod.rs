//! Provider compatibility declarations.
//!
//! This is the Phase 1 declaration layer: parse the old flat compat fields and
//! the new nested fields into one typed model, merge provider defaults with
//! model overrides, and validate the result against the wire protocol.
//!
//! Production adapters still read `crate::llm::ResolvedCompat`; this module is
//! parallel until the consumers for the new fields land. Keeping it separate is
//! what makes the 195-model request-body baseline meaningful.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::ApiType;

/// Thinking wire family. `Generic` means no provider-specific thinking format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingFormat {
    Generic,
    Deepseek,
    Anthropic,
    Zhipu,
    Qwen,
    Kimi,
    Volcengine,
}

impl ThinkingFormat {
    pub fn supports(self, api_type: ApiType) -> bool {
        match self {
            Self::Generic => true,
            Self::Deepseek => matches!(
                api_type,
                ApiType::OpenAiCompletions | ApiType::AnthropicMessages
            ),
            Self::Anthropic => api_type == ApiType::AnthropicMessages,
            Self::Zhipu | Self::Qwen | Self::Kimi | Self::Volcengine => {
                api_type == ApiType::OpenAiCompletions
            }
        }
    }
}

/// Generation/profile differences inside one thinking family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingProfile {
    #[serde(rename = "zhipu-glm-5.3")]
    ZhipuGlm53,
    #[serde(rename = "zhipu-glm-5.3-flash")]
    ZhipuGlm53Flash,
    #[serde(rename = "kimi-k2.6")]
    KimiK26,
    #[serde(rename = "kimi-k3")]
    KimiK3,
    #[serde(rename = "kimi-k2.7-code")]
    KimiK27Code,
}

impl ThinkingProfile {
    pub fn supports(self, format: ThinkingFormat) -> bool {
        match self {
            Self::ZhipuGlm53 | Self::ZhipuGlm53Flash => format == ThinkingFormat::Zhipu,
            Self::KimiK26 | Self::KimiK3 | Self::KimiK27Code => format == ThinkingFormat::Kimi,
        }
    }
}

/// How strongly reasoning must be carried back in later turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPolicy {
    None,
    Preserve,
    RequireToolCall,
}

/// Field or block family that carries reasoning in a request/response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Carrier {
    ReasoningContent,
    ThinkingBlocks,
    ReasoningItems,
    ReasoningDetails,
    ThoughtSignature,
}

/// Whether every assistant message must contain the reasoning carrier field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantReasoning {
    Optional,
    Required,
}

/// Output-token field required by a protocol.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputField {
    MaxTokens,
    MaxCompletionTokens,
    MaxOutputTokens,
}

impl OutputField {
    pub fn supports(self, api_type: ApiType) -> bool {
        match self {
            Self::MaxTokens => matches!(
                api_type,
                ApiType::OpenAiCompletions | ApiType::AnthropicMessages
            ),
            Self::MaxCompletionTokens => api_type == ApiType::OpenAiCompletions,
            Self::MaxOutputTokens => api_type == ApiType::OpenAIResponses,
        }
    }
}

/// Stream usage support. `Unknown` means do not send the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamUsage {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Bearer,
    None,
}

/// Reasoning replay declaration. All fields are optional so provider defaults
/// and model overrides can be merged field by field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ReasoningReplay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ReplayPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<Carrier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearable: Option<bool>,
    #[serde(
        default,
        alias = "assistantReasoning",
        skip_serializing_if = "Option::is_none"
    )]
    pub assistant_reasoning: Option<AssistantReasoning>,
}

/// Output budget declaration. The actual request budget stays in `LlmConfig`
/// until the consumer phase.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct OutputBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<OutputField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// Merged declaration. It is still a patch: validation decides whether the
/// combination is complete enough to become an effective contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Compat {
    #[serde(
        default,
        alias = "thinking_format",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_format: Option<ThinkingFormat>,
    #[serde(
        default,
        alias = "thinking_profile",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_profile: Option<ThinkingProfile>,
    #[serde(
        default,
        alias = "reasoning_replay",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_replay: Option<ReasoningReplay>,
    #[serde(
        default,
        alias = "output_budget",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_budget: Option<OutputBudget>,
    #[serde(
        default,
        alias = "stream_usage",
        skip_serializing_if = "Option::is_none"
    )]
    pub stream_usage: Option<StreamUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthMode>,
}

impl Compat {
    /// Field-wise merge. Model declarations override provider defaults; nested
    /// replay/budget objects are merged field by field, never replaced whole.
    pub fn merge(provider: &Self, model: &Self) -> Self {
        Self {
            thinking_format: model.thinking_format.or(provider.thinking_format),
            thinking_profile: model.thinking_profile.or(provider.thinking_profile),
            reasoning_replay: merge_reasoning(
                provider.reasoning_replay.as_ref(),
                model.reasoning_replay.as_ref(),
            ),
            output_budget: merge_budget(
                provider.output_budget.as_ref(),
                model.output_budget.as_ref(),
            ),
            stream_usage: model.stream_usage.or(provider.stream_usage),
            auth: model.auth.or(provider.auth),
        }
    }

    /// Validate protocol-specific declaration constraints.
    pub fn validate(&self, api_type: ApiType, at: &str) -> Result<(), String> {
        let mut problems = Vec::new();

        if let Some(format) = self.thinking_format {
            if !format.supports(api_type) {
                problems.push(format!(
                    "thinking_format {:?} 不适用于协议 {}",
                    format,
                    api_type.as_str()
                ));
            }
        }

        if let Some(profile) = self.thinking_profile {
            match self.thinking_format {
                Some(format) if profile.supports(format) => {}
                Some(format) => problems.push(format!(
                    "thinking_profile {:?} 与 thinking_format {:?} 不匹配",
                    profile, format
                )),
                None => {
                    problems.push("声明 thinking_profile 时必须同时声明 thinking_format".into())
                }
            }
        }

        if let Some(replay) = self.reasoning_replay.as_ref() {
            match replay.policy {
                None => problems.push("reasoning_replay.policy 未声明".into()),
                Some(ReplayPolicy::None) => {
                    if replay.carrier.is_some() {
                        problems.push("policy=none 时不能声明 carrier".into());
                    }
                    if replay.clearable.is_some() {
                        problems.push("policy=none 时不能声明 clearable".into());
                    }
                }
                Some(_) => {
                    if replay.carrier.is_none() {
                        problems.push("policy 非 none 时必须声明 carrier".into());
                    }
                }
            }
            if replay.assistant_reasoning == Some(AssistantReasoning::Required)
                && replay.carrier.is_none()
            {
                problems.push("assistant_reasoning=required 时必须声明 carrier".into());
            }
        }

        if let Some(budget) = self.output_budget.as_ref() {
            if let Some(field) = budget.field {
                if !field.supports(api_type) {
                    problems.push(format!(
                        "output_budget.field {:?} 不适用于协议 {}",
                        field,
                        api_type.as_str()
                    ));
                }
            }
            if budget.required == Some(true) && budget.field.is_none() {
                problems.push("output_budget.required=true 时必须声明 field".into());
            }
        }

        if self.stream_usage.is_some() && api_type != ApiType::OpenAiCompletions {
            problems.push("stream_usage 仅适用于协议 openai-completions".into());
        }

        if self.auth.is_some() && api_type != ApiType::OpenAiCompletions {
            problems.push("auth 仅适用于协议 openai-completions".into());
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!("{at}: {}", problems.join("；")))
        }
    }
}

fn merge_reasoning(
    provider: Option<&ReasoningReplay>,
    model: Option<&ReasoningReplay>,
) -> Option<ReasoningReplay> {
    match (provider, model) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(over)) => Some(over.clone()),
        (Some(base), Some(over)) => Some(ReasoningReplay {
            policy: over.policy.or(base.policy),
            carrier: over.carrier.or(base.carrier),
            clearable: over.clearable.or(base.clearable),
            assistant_reasoning: over.assistant_reasoning.or(base.assistant_reasoning),
        }),
    }
}

fn merge_budget(
    provider: Option<&OutputBudget>,
    model: Option<&OutputBudget>,
) -> Option<OutputBudget> {
    match (provider, model) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(over)) => Some(over.clone()),
        (Some(base), Some(over)) => Some(OutputBudget {
            field: over.field.or(base.field),
            required: over.required.or(base.required),
        }),
    }
}

/// Wire-level declaration. Accepts both legacy flat keys and the new nested
/// shape, rejects unknown keys and explicit null, and refuses legacy/new
/// semantic conflicts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatWire {
    pub compat: Compat,
    /// True when the declaration used at least one field that only the new
    /// schema knows. Legacy-only declarations remain valid under the old
    /// production path; new declarations get target-contract validation.
    pub uses_new_schema: bool,
}

impl CompatWire {
    /// Project the merged declaration back onto the old flat shape so the
    /// production path can keep reading the same fields while Phase 1 runs.
    /// New-only fields are intentionally ignored here; they have no production
    /// consumer yet.
    pub(crate) fn legacy_compat(&self) -> Option<super::Compat> {
        let thinking_format = match self.compat.thinking_format {
            None | Some(ThinkingFormat::Generic) => None,
            Some(ThinkingFormat::Deepseek) => Some(super::ThinkingFormat::Deepseek),
            Some(ThinkingFormat::Anthropic) => Some(super::ThinkingFormat::Anthropic),
            Some(ThinkingFormat::Zhipu) => Some(super::ThinkingFormat::Zhipu),
            Some(ThinkingFormat::Qwen) => Some(super::ThinkingFormat::Qwen),
            Some(ThinkingFormat::Kimi) => Some(super::ThinkingFormat::Kimi),
            Some(ThinkingFormat::Volcengine) => Some(super::ThinkingFormat::Volcengine),
        };
        let auth = self.compat.auth.map(|auth| match auth {
            AuthMode::Bearer => super::AuthMode::Bearer,
            AuthMode::None => super::AuthMode::None,
        });
        let reasoning_policy = self
            .compat
            .reasoning_replay
            .as_ref()
            .and_then(|replay| replay.policy)
            .and_then(|policy| match policy {
                ReplayPolicy::Preserve => Some(super::ReasoningPolicy::Preserve),
                ReplayPolicy::RequireToolCall => Some(super::ReasoningPolicy::RequireToolCall),
                ReplayPolicy::None => None,
            });
        let supports_usage_in_streaming = self.compat.stream_usage.and_then(|usage| match usage {
            StreamUsage::Supported => Some(true),
            StreamUsage::Unsupported => Some(false),
            StreamUsage::Unknown => None,
        });

        if thinking_format.is_none()
            && auth.is_none()
            && reasoning_policy.is_none()
            && supports_usage_in_streaming.is_none()
            && self.compat.stream_usage.is_none()
        {
            None
        } else {
            Some(super::Compat {
                thinking_format,
                thinking_profile: self.compat.thinking_profile,
                reasoning_clearable: self
                    .compat
                    .reasoning_replay
                    .as_ref()
                    .and_then(|replay| replay.clearable),
                assistant_reasoning: self
                    .compat
                    .reasoning_replay
                    .as_ref()
                    .and_then(|replay| replay.assistant_reasoning),
                stream_usage: self.compat.stream_usage,
                auth,
                reasoning_policy,
                supports_usage_in_streaming,
            })
        }
    }
}

impl<'de> Deserialize<'de> for CompatWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("compat 必须是对象"))?;

        let mut compat = Compat::default();
        let mut legacy_policy: Option<ReplayPolicy> = None;
        let mut legacy_usage: Option<bool> = None;
        let mut uses_new_schema = false;

        for (key, value) in object {
            match key.as_str() {
                "thinking_format" | "thinkingFormat" => {
                    compat.thinking_format = Some(parse_value(value, key)?);
                }
                "thinking_profile" | "thinkingProfile" => {
                    compat.thinking_profile = Some(parse_value(value, key)?);
                    uses_new_schema = true;
                }
                "reasoning_policy" | "reasoningPolicy" => {
                    legacy_policy = Some(parse_value(value, key)?);
                }
                "reasoning_replay" | "reasoningReplay" => {
                    compat.reasoning_replay = Some(parse_value(value, key)?);
                    uses_new_schema = true;
                }
                "output_budget" | "outputBudget" => {
                    compat.output_budget = Some(parse_value(value, key)?);
                    uses_new_schema = true;
                }
                "supports_usage_in_streaming" | "supportsUsageInStreaming" => {
                    legacy_usage = Some(parse_value(value, key)?);
                }
                "stream_usage" | "streamUsage" => {
                    compat.stream_usage = Some(parse_value(value, key)?);
                    uses_new_schema = true;
                }
                "auth" => {
                    compat.auth = Some(parse_value(value, key)?);
                }
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "未知 compat 字段 `{other}`"
                    )));
                }
            }
        }

        if let Some(policy) = legacy_policy {
            match compat.reasoning_replay.as_mut() {
                Some(replay) => match replay.policy {
                    Some(existing) if existing != policy => {
                        return Err(serde::de::Error::custom(
                            "reasoning_policy 与 reasoning_replay.policy 冲突",
                        ));
                    }
                    Some(_) => {}
                    None => replay.policy = Some(policy),
                },
                None => {
                    compat.reasoning_replay = Some(ReasoningReplay {
                        policy: Some(policy),
                        ..Default::default()
                    });
                }
            }
        }

        if let Some(enabled) = legacy_usage {
            let legacy = if enabled {
                StreamUsage::Supported
            } else {
                StreamUsage::Unsupported
            };
            match compat.stream_usage {
                Some(existing) if existing != legacy => {
                    return Err(serde::de::Error::custom(
                        "supports_usage_in_streaming 与 stream_usage 冲突",
                    ));
                }
                Some(_) => {}
                None => compat.stream_usage = Some(legacy),
            }
        }

        Ok(Self {
            compat,
            uses_new_schema,
        })
    }
}

fn parse_value<T, E>(value: &Value, key: &str) -> Result<T, E>
where
    T: DeserializeOwned,
    E: serde::de::Error,
{
    serde_json::from_value(value.clone())
        .map_err(|e| E::custom(format!("compat.{key} 无效（显式 null 也视为错误）: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Compat, String> {
        serde_json::from_str::<CompatWire>(json)
            .map(|wire| wire.compat)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn legacy_flat_fields_parse() {
        let compat = parse(
            r#"{
                "thinking_format": "zhipu",
                "reasoning_policy": "require_tool_call",
                "supports_usage_in_streaming": false
            }"#,
        )
        .unwrap();
        assert_eq!(compat.thinking_format, Some(ThinkingFormat::Zhipu));
        assert_eq!(
            compat.reasoning_replay.as_ref().and_then(|r| r.policy),
            Some(ReplayPolicy::RequireToolCall)
        );
        assert_eq!(compat.stream_usage, Some(StreamUsage::Unsupported));
    }

    #[test]
    fn new_nested_fields_parse() {
        let compat = parse(
            r#"{
                "thinking_format": "kimi",
                "thinking_profile": "kimi-k3",
                "reasoning_replay": {
                    "policy": "preserve",
                    "carrier": "reasoning_content",
                    "clearable": false,
                    "assistant_reasoning": "required"
                },
                "output_budget": { "field": "max_completion_tokens", "required": false },
                "stream_usage": "supported",
                "auth": "bearer"
            }"#,
        )
        .unwrap();
        assert_eq!(compat.thinking_profile, Some(ThinkingProfile::KimiK3));
        let replay = compat.reasoning_replay.unwrap();
        assert_eq!(replay.policy, Some(ReplayPolicy::Preserve));
        assert_eq!(replay.carrier, Some(Carrier::ReasoningContent));
        assert_eq!(replay.clearable, Some(false));
        assert_eq!(
            replay.assistant_reasoning,
            Some(AssistantReasoning::Required)
        );
        assert_eq!(
            compat.output_budget.unwrap().field,
            Some(OutputField::MaxCompletionTokens)
        );
    }

    #[test]
    fn mixed_legacy_and_new_conflict_is_rejected() {
        let err = parse(
            r#"{
                "reasoning_policy": "preserve",
                "reasoning_replay": { "policy": "require_tool_call" }
            }"#,
        )
        .unwrap_err();
        assert!(err.contains("冲突"), "{err}");
    }

    #[test]
    fn mixed_legacy_and_new_same_value_is_ok() {
        let compat = parse(
            r#"{
                "reasoning_policy": "preserve",
                "reasoning_replay": { "policy": "preserve", "carrier": "reasoning_content" }
            }"#,
        )
        .unwrap();
        assert_eq!(
            compat.reasoning_replay.as_ref().and_then(|r| r.policy),
            Some(ReplayPolicy::Preserve)
        );
    }

    #[test]
    fn explicit_null_and_unknown_keys_are_rejected() {
        assert!(parse(r#"{"thinking_format": null}"#)
            .unwrap_err()
            .contains("显式 null"));
        assert!(parse(r#"{"unknown_field": true}"#)
            .unwrap_err()
            .contains("未知 compat 字段"));
    }

    #[test]
    fn merge_is_fieldwise_for_nested_objects() {
        let provider = parse(
            r#"{
                "thinking_format": "zhipu",
                "reasoning_replay": {
                    "policy": "preserve",
                    "carrier": "reasoning_content",
                    "clearable": true
                },
                "output_budget": { "field": "max_tokens", "required": true }
            }"#,
        )
        .unwrap();
        let model = parse(
            r#"{
                "reasoning_replay": { "policy": "require_tool_call" },
                "output_budget": { "required": false }
            }"#,
        )
        .unwrap();
        let merged = Compat::merge(&provider, &model);
        let replay = merged.reasoning_replay.unwrap();
        assert_eq!(replay.policy, Some(ReplayPolicy::RequireToolCall));
        assert_eq!(replay.carrier, Some(Carrier::ReasoningContent));
        assert_eq!(replay.clearable, Some(true));
        let budget = merged.output_budget.unwrap();
        assert_eq!(budget.field, Some(OutputField::MaxTokens));
        assert_eq!(budget.required, Some(false));
    }

    #[test]
    fn validation_rejects_incomplete_and_wrong_protocol_declarations() {
        let replay_without_carrier =
            parse(r#"{"reasoning_replay": {"policy": "require_tool_call"}}"#).unwrap();
        assert!(replay_without_carrier
            .validate(ApiType::OpenAiCompletions, "p/m")
            .unwrap_err()
            .contains("carrier"));

        let none_with_carrier =
            parse(r#"{"reasoning_replay": {"policy": "none", "carrier": "reasoning_content"}}"#)
                .unwrap();
        assert!(none_with_carrier
            .validate(ApiType::OpenAiCompletions, "p/m")
            .unwrap_err()
            .contains("policy=none"));

        let wrong_field = parse(r#"{"output_budget": {"field": "max_output_tokens"}}"#).unwrap();
        assert!(wrong_field
            .validate(ApiType::AnthropicMessages, "p/m")
            .unwrap_err()
            .contains("不适用于协议"));

        let wrong_auth = parse(r#"{"auth": "none"}"#).unwrap();
        assert!(wrong_auth
            .validate(ApiType::AnthropicMessages, "p/m")
            .unwrap_err()
            .contains("auth"));
    }

    #[test]
    fn real_models_json_compat_objects_parse() {
        let root: Value =
            serde_json::from_str(include_str!("../../../resources/models.json")).unwrap();
        let mut count = 0;
        for provider in root.as_object().unwrap().values() {
            let defaults = provider.get("defaults").unwrap();
            if let Some(compat) = defaults.get("compat") {
                let _: CompatWire = serde_json::from_value(compat.clone()).unwrap();
                count += 1;
            }
            for model in provider
                .get("models")
                .unwrap()
                .as_object()
                .unwrap()
                .values()
            {
                if let Some(compat) = model.get("compat") {
                    let _: CompatWire = serde_json::from_value(compat.clone()).unwrap();
                    count += 1;
                }
            }
        }
        assert!(
            count >= 22,
            "expected at least 22 compat objects (provider defaults + models), got {count}"
        );
    }
}
