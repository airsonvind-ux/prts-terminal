//! Provider capability matrix and request-body snapshot baselines.
//!
//! These tests lock the *current* effective contract and the exact request body
//! produced for every model in `resources/models.json`. They are intentionally
//! descriptive, not aspirational: Phase 1 must keep both files unchanged while
//! the new compat types are introduced behind the old production path.
//!
//! Regenerate intentionally with:
//!   UPDATE_PROVIDER_SNAPSHOTS=1 cargo test --lib provider_
//!
//! The generated files live in `src-tauri/fixtures/` and are consumed by this
//! module. A model-count or body diff is a review signal, not an automatic
//! update.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::anthropic::AnthropicMessagesClient;
use super::openai_compatible::OpenAiCompatibleClient;
use super::openai_responses::OpenAIResponsesClient;
use super::{ApiType, ChatMessage, LlmConfig, ModelRegistry, ToolCall, ToolCallFunction, ToolDef};

const MODEL_COUNT: usize = 195;
const MATRIX_FILE: &str = "fixtures/provider-capabilities.json";
const BODIES_FILE: &str = "fixtures/provider-request-bodies.json";
const MATRIX_DOC: &str = "../docs/provider-compat-matrix.md";
const UPDATE_ENV: &str = "UPDATE_PROVIDER_SNAPSHOTS";

fn registry() -> Arc<ModelRegistry> {
    Arc::new(
        ModelRegistry::from_json_with_limits(
            include_str!("../../resources/models.json"),
            include_str!("../../resources/model-output-limits.json"),
        )
        .expect("real models.json / model-output-limits.json must load"),
    )
}

fn fixture_messages() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("You are a test harness."),
        ChatMessage::user("Look up the value and report it."),
        ChatMessage::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "call_snapshot_1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "lookup".into(),
                    arguments: r#"{"q":"snapshot"}"#.into(),
                },
            }],
            Some("I should call lookup.".into()),
        ),
        ChatMessage::tool_result("call_snapshot_1", "snapshot-result"),
        ChatMessage::user("Continue with the result."),
    ]
}

fn fixture_tools() -> Vec<ToolDef> {
    vec![ToolDef::new(
        "lookup",
        "Look up a value.",
        json!({
            "type": "object",
            "properties": { "q": { "type": "string" } },
            "required": ["q"]
        }),
    )]
}

fn config_for(
    registry: &ModelRegistry,
    provider: &str,
    api_type: ApiType,
    model: &str,
) -> LlmConfig {
    LlmConfig {
        provider: provider.to_string(),
        api_type,
        model: model.to_string(),
        api_key: "snapshot-key".into(),
        base_url: registry.get_base_url(provider).map(str::to_string),
        headers: None,
        max_tokens: 4096,
        thinking_level: None,
    }
}

fn build_body(
    registry: &Arc<ModelRegistry>,
    config: &LlmConfig,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Result<Value, String> {
    match config.api_type {
        ApiType::OpenAiCompletions => OpenAiCompatibleClient::new(registry.clone())
            .build_body_for_snapshot(config, messages, tools, true),
        ApiType::AnthropicMessages => AnthropicMessagesClient::new(registry.clone())
            .build_body_for_snapshot(config, messages, tools, true),
        ApiType::OpenAIResponses => OpenAIResponsesClient::new(registry.clone())
            .build_body_for_snapshot(config, messages, tools, true),
    }
    .map_err(|e| format!("{e}"))
}

fn capability_matrix(registry: &ModelRegistry) -> Value {
    let models: Vec<Value> = registry
        .all_models()
        .into_iter()
        .map(|(provider, api_type, model)| {
            let entry = registry
                .get_model(&provider, &model)
                .expect("model from all_models must be resolvable");
            let compat = registry.get_compat(&provider, &model);
            let resolved = registry.get_resolved_compat(&provider, &model, api_type);
            let max_output_status = if entry.max_output.is_some() {
                "known"
            } else if provider == "openai" && model.starts_with("text-embedding-") {
                "not_applicable"
            } else {
                "unverified"
            };
            let output_budget = match api_type {
                ApiType::AnthropicMessages => json!({
                    "field": "max_tokens",
                    "required": true,
                    "fallback": super::DEFAULT_REQUIRED_OUTPUT_TOKENS,
                    "source": "protocol"
                }),
                ApiType::OpenAIResponses => json!({
                    "field": "max_output_tokens",
                    "required": true,
                    "fallback": super::DEFAULT_REQUIRED_OUTPUT_TOKENS,
                    "source": "protocol"
                }),
                ApiType::OpenAiCompletions => Value::Null,
            };
            json!({
                "provider": provider,
                "model": model,
                "name": entry.name,
                "apiType": api_type.as_str(),
                "context": entry.context,
                "reasoning": entry.reasoning,
                "vision": entry.supports_vision,
                "thinkingLevels": entry.thinking_levels,
                "defaultThinkingLevel": entry.default_thinking_level,
                "declaredCompat": compat.clone(),
                "declaredContract": registry.get_declared_compat(&provider, &model),
                "effective": {
                    "carrier": resolved.carrier,
                    "requestParam": resolved.request_param,
                    "policy": resolved.policy,
                    "clearable": resolved.clearable,
                    "assistantReasoning": resolved.assistant_reasoning,
                    "streamUsage": resolved.stream_usage,
                    "supportsUsageInStreaming": resolved.supports_usage_in_streaming,
                    "auth": resolved.auth,
                },
                // Explicit U markers for fields the current declaration layer
                // does not model yet. They become non-null only when a real
                // consumer and evidence land.
                "maxOutput": entry.max_output,
                "maxOutputStatus": max_output_status,
                "thinkingProfile": compat.thinking_profile,
                "reasoningReplay": {
                    "clearable": resolved.clearable,
                    "assistantReasoning": resolved.assistant_reasoning,
                },
                "outputBudget": output_budget,
            })
        })
        .collect();

    json!({
        "schema": 1,
        "count": models.len(),
        "source": "src-tauri/resources/models.json",
        "note": "Current effective contract. Null means not modeled yet, not a supported value.",
        "models": models,
    })
}

fn capability_matrix_markdown(registry: &ModelRegistry) -> String {
    let all = registry.all_models();
    let reasoning = all
        .iter()
        .filter(|(provider, _, model)| {
            registry
                .get_model(provider, model)
                .map(|entry| entry.reasoning)
                .unwrap_or(false)
        })
        .count();
    let reasoning_without_levels = all
        .iter()
        .filter(|(provider, _, model)| {
            registry
                .get_model(provider, model)
                .map(|entry| entry.reasoning && entry.thinking_levels.is_empty())
                .unwrap_or(false)
        })
        .count();
    let model_compat = all
        .iter()
        .filter(|(provider, _, model)| {
            registry
                .get_model(provider, model)
                .and_then(|entry| entry.compat.as_ref())
                .is_some()
        })
        .count();
    let thinking_profiles = all
        .iter()
        .filter(|(provider, _, model)| {
            registry
                .get_compat(provider, model)
                .thinking_profile
                .is_some()
        })
        .count();
    let max_output_known = all
        .iter()
        .filter(|(provider, _, model)| {
            registry
                .get_model(provider, model)
                .and_then(|entry| entry.max_output)
                .is_some()
        })
        .count();
    let max_output_not_applicable = all
        .iter()
        .filter(|(provider, _, model)| {
            *provider == "openai" && model.starts_with("text-embedding-")
        })
        .count();
    let clearable = all
        .iter()
        .filter(|(provider, api_type, model)| {
            registry
                .get_resolved_compat(provider, model, *api_type)
                .clearable
        })
        .count();
    let assistant_required = all
        .iter()
        .filter(|(provider, api_type, model)| {
            registry
                .get_resolved_compat(provider, model, *api_type)
                .assistant_reasoning
                == super::compat::AssistantReasoning::Required
        })
        .count();
    let stream_supported = all
        .iter()
        .filter(|(provider, api_type, model)| {
            registry
                .get_resolved_compat(provider, model, *api_type)
                .stream_usage
                == super::compat::StreamUsage::Supported
        })
        .count();
    let stream_unsupported = all
        .iter()
        .filter(|(provider, api_type, model)| {
            registry
                .get_resolved_compat(provider, model, *api_type)
                .stream_usage
                == super::compat::StreamUsage::Unsupported
        })
        .count();

    let mut out = String::new();
    out.push_str("# Provider Compatibility Matrix\n\n");
    out.push_str("> Generated from `src-tauri/resources/models.json` by `src-tauri/src/llm/snapshot_tests.rs`.\n");
    out.push_str("> This is the current effective contract, not a support claim. `U` means not modeled yet.\n\n");
    out.push_str(&format!(
        "- Models: {}\n- Reasoning: {}\n- Reasoning without explicit thinking levels: {}\n- Models with model-level compat: {}\n- Models with thinking profile: {}\n- `maxOutput`: known {} / not applicable {} / unverified {}\n- `thinkingProfile`: U for {} models\n- `clearable=true`: {} models\n- `assistant_reasoning=required`: {} models\n- `stream_usage`: supported {} / unsupported {} / unknown {}\n\n",
        all.len(),
        reasoning,
        reasoning_without_levels,
        model_compat,
        thinking_profiles,
        max_output_known,
        max_output_not_applicable,
        all.len() - max_output_known - max_output_not_applicable,
        all.len() - thinking_profiles,
        clearable,
        assistant_required,
        stream_supported,
        stream_unsupported,
        all.len() - stream_supported - stream_unsupported,
    ));
    out.push_str("| provider | model | protocol | context | reasoning | vision | thinking | levels (default) | policy | carrier | param | stream | max output | profile |\n");
    out.push_str("|---|---|---|---:|---|---|---|---|---|---|---|---|---|---|\n");

    for (provider, api_type, model) in all {
        let entry = registry
            .get_model(&provider, &model)
            .expect("model from all_models must be resolvable");
        let compat = registry.get_compat(&provider, &model);
        let resolved = registry.get_resolved_compat(&provider, &model, api_type);
        let thinking = compat
            .thinking_format
            .as_ref()
            .and_then(|format| serde_json::to_value(format).ok())
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "-".into());
        let levels = if entry.thinking_levels.is_empty() {
            "-".to_string()
        } else {
            format!(
                "{} ({})",
                entry.thinking_levels.join(","),
                entry.default_thinking_level.as_deref().unwrap_or("-")
            )
        };
        let policy = match resolved.policy {
            super::ReasoningPolicy::Preserve => "preserve",
            super::ReasoningPolicy::RequireToolCall => "require_tool_call",
        };
        let profile = compat
            .thinking_profile
            .and_then(|profile| serde_json::to_value(profile).ok())
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "U".into());
        let max_output = if let Some(limit) = entry.max_output {
            limit.to_string()
        } else if provider == "openai" && model.starts_with("text-embedding-") {
            "n/a".into()
        } else {
            "U".into()
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            provider.replace('|', "\\|"),
            model.replace('|', "\\|"),
            api_type.as_str(),
            entry.context,
            if entry.reasoning { "yes" } else { "no" },
            if entry.supports_vision { "yes" } else { "no" },
            thinking,
            levels,
            policy,
            resolved.carrier.as_deref().unwrap_or("-"),
            resolved.request_param.as_deref().unwrap_or("-"),
            if resolved.supports_usage_in_streaming {
                "send"
            } else {
                "omit"
            },
            max_output,
            profile,
        ));
    }

    out
}

fn request_bodies(registry: &Arc<ModelRegistry>) -> Value {
    let messages = fixture_messages();
    let tools = fixture_tools();
    let mut bodies = BTreeMap::new();

    for (provider, api_type, model) in registry.all_models() {
        let config = config_for(registry, &provider, api_type, &model);
        let body = build_body(registry, &config, &messages, &tools)
            .unwrap_or_else(|e| panic!("{provider}/{model}: build_body failed: {e}"));
        bodies.insert(format!("{provider}/{model}"), body);
    }

    // Responses has no route in the current models.json, but the adapter is
    // still shipped. Keep one protocol-level fixture so its baseline is not
    // silently absent from the 195-model matrix.
    let responses_config = config_for(registry, "openai", ApiType::OpenAIResponses, "gpt-5");
    let responses_body = build_body(registry, &responses_config, &messages, &tools)
        .expect("Responses protocol fixture must build");

    json!({
        "schema": 1,
        "count": bodies.len(),
        "fixture": {
            "stream": true,
            "maxTokens": 4096,
            "thinkingLevel": Value::Null,
            "messages": "system, user, assistant(tool_call + reasoning), tool, user",
            "tools": ["lookup"],
        },
        "bodies": bodies,
        "protocolFixtures": {
            "openai-responses": responses_body,
        },
    })
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn check_or_update(path: &Path, actual: &str, label: &str) {
    if std::env::var_os(UPDATE_ENV).is_some() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture directory");
        }
        fs::write(path, actual).expect("write fixture");
        eprintln!("updated {label}: {}", path.display());
        return;
    }

    let expected = fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "missing {label} fixture {}: {e}. Regenerate with {UPDATE_ENV}=1.",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{label} changed. If this is intentional, regenerate with {UPDATE_ENV}=1 and review the diff."
    );
}

fn pretty(value: &Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("serialize fixture")
    )
}

#[test]
fn provider_capability_matrix_matches_baseline() {
    let registry = registry();
    let all = registry.all_models();
    assert_eq!(
        all.len(),
        MODEL_COUNT,
        "models.json model count changed; update the matrix baseline intentionally"
    );

    let actual = pretty(&capability_matrix(&registry));
    check_or_update(&fixture_path(MATRIX_FILE), &actual, "capability matrix");
}

#[test]
fn provider_capability_matrix_doc_matches_baseline() {
    let registry = registry();
    let actual = capability_matrix_markdown(&registry);
    check_or_update(&fixture_path(MATRIX_DOC), &actual, "capability matrix doc");
}

#[test]
fn provider_request_bodies_match_baseline() {
    let registry = registry();
    let all = registry.all_models();
    assert_eq!(
        all.len(),
        MODEL_COUNT,
        "models.json model count changed; update the request-body baseline intentionally"
    );

    let actual = pretty(&request_bodies(&registry));
    check_or_update(&fixture_path(BODIES_FILE), &actual, "request-body baseline");
}
