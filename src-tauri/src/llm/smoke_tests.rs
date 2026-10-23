//! Optional real-provider smoke tests.
//!
//! These are ignored by default because they require credentials and may incur
//! cost. They deliberately use the cheapest model in each catalog family and a
//! tiny output budget. A missing env key means "not run", never "passed".
//!
//! Run one provider:
//!   PRTS_SMOKE_OPENAI_KEY=... cargo test --lib provider_smoke_openai -- --ignored
//!
//! Run all configured providers:
//!   cargo test --lib provider_smoke_ -- --ignored
//!
//! Real requests are serialized in-process; Kimi additionally uses a 3-per-minute
//! sliding window because its smoke account's RPM is low. This keeps a single
//! all-provider run from rate-limiting itself.
//!
//! Ollama uses `PRTS_SMOKE_OLLAMA=1` instead of a key.
//! DeepSeek Anthropic compatibility reuses `PRTS_SMOKE_DEEPSEEK_KEY` and can
//! override the model with `PRTS_SMOKE_DEEPSEEK_ANTHROPIC_MODEL`.
//! The OpenAI key also exercises the Responses adapter; override its model with
//! `PRTS_SMOKE_OPENAI_RESPONSES_MODEL`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::anthropic::AnthropicMessagesClient;
use super::openai_compatible::OpenAiCompatibleClient;
use super::openai_responses::OpenAIResponsesClient;
use super::{ApiType, ChatMessage, LlmClient, LlmConfig, LlmResponse, ModelRegistry, ToolDef};

/// Real-provider tests share one account and are easy to rate-limit. Keep the
/// request side serial even when cargo runs test functions in parallel.
static SMOKE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Kimi's smoke account is limited to 3 requests per minute, not 3 per second.
/// Track the sliding window across all Kimi tests so the full suite can finish
/// without manufacturing its own 429.
static KIMI_REQUESTS: tokio::sync::Mutex<VecDeque<Instant>> =
    tokio::sync::Mutex::const_new(VecDeque::new());

async fn smoke_guard() -> tokio::sync::MutexGuard<'static, ()> {
    SMOKE_LOCK.lock().await
}

async fn wait_kimi_slot() {
    const LIMIT: usize = 3;
    const WINDOW: Duration = Duration::from_secs(60);

    loop {
        let now = Instant::now();
        let mut times = KIMI_REQUESTS.lock().await;
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= WINDOW)
        {
            times.pop_front();
        }
        if times.len() < LIMIT {
            times.push_back(now);
            return;
        }
        let wait = WINDOW.saturating_sub(now.duration_since(*times.front().unwrap()));
        drop(times);
        tokio::time::sleep(wait + Duration::from_millis(100)).await;
    }
}

fn registry() -> Arc<ModelRegistry> {
    Arc::new(
        ModelRegistry::from_json_with_limits(
            include_str!("../../resources/models.json"),
            include_str!("../../resources/model-output-limits.json"),
        )
        .expect("smoke registry must load"),
    )
}

fn key(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn model_override(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into())
}

fn client(registry: Arc<ModelRegistry>, api_type: ApiType) -> Box<dyn LlmClient> {
    match api_type {
        ApiType::OpenAiCompletions => Box::new(OpenAiCompatibleClient::new(registry)),
        ApiType::AnthropicMessages => Box::new(AnthropicMessagesClient::new(registry)),
        ApiType::OpenAIResponses => Box::new(OpenAIResponsesClient::new(registry)),
    }
}

async fn run(provider: &str, model: &str, api_key: String, base_url: Option<String>) {
    let registry = registry();
    let api_type = registry
        .list_providers()
        .into_iter()
        .find(|p| p.id == provider)
        .map(|p| p.api_type)
        .unwrap_or_else(|| panic!("provider {provider} missing from catalog"));
    run_with(provider, model, api_key, base_url, api_type).await;
}

struct SmokeCall<'a> {
    provider: &'a str,
    model: &'a str,
    api_key: &'a str,
    base_url: Option<&'a str>,
    api_type: ApiType,
    thinking_level: &'a str,
    max_tokens: u32,
}

async fn chat_once(
    call: SmokeCall<'_>,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    label: &str,
) -> LlmResponse {
    // Kimi 的 smoke 账号是 3 RPM；按滑动窗口排队，避免 off / thinking /
    // tool / tool-round 自己把自己限流。
    if call.provider == "kimi" {
        wait_kimi_slot().await;
    }

    let registry = registry();
    let config = LlmConfig {
        provider: call.provider.into(),
        api_type: call.api_type,
        model: call.model.into(),
        api_key: call.api_key.into(),
        base_url: call.base_url.map(str::to_string),
        headers: None,
        max_tokens: call.max_tokens,
        thinking_level: Some(call.thinking_level.into()),
    };
    match client(registry, call.api_type)
        .chat(&config, messages, tools)
        .await
    {
        Ok(response) => response,
        Err(error) => panic!(
            "{}/{model} {label} smoke failed: {error:?}",
            call.provider,
            model = call.model
        ),
    }
}

async fn run_with(
    provider: &str,
    model: &str,
    api_key: String,
    base_url: Option<String>,
    api_type: ApiType,
) {
    let _guard = smoke_guard().await;
    let messages = vec![ChatMessage::user("Reply with the single word OK.")];
    chat_once(
        SmokeCall {
            provider,
            model,
            api_key: &api_key,
            base_url: base_url.as_deref(),
            api_type,
            thinking_level: "off",
            max_tokens: 64,
        },
        &messages,
        &[],
        "chat",
    )
    .await;
}

fn smoke_tools() -> Vec<ToolDef> {
    vec![ToolDef::new(
        "smoke_echo",
        "Echo a value. This tool must be used when asked.",
        json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" }
            },
            "required": ["value"],
            "additionalProperties": false
        }),
    )]
}

/// `api_type=None` follows the catalog route. A smoke-only route such as
/// DeepSeek Anthropic must pass `Some(AnthropicMessages)` explicitly, otherwise
/// the provider default (completions) silently wins.
async fn run_matrix(
    provider: &str,
    model: &str,
    thinking_model: &str,
    api_key: String,
    base_url: Option<String>,
    api_type: Option<ApiType>,
) {
    let _guard = smoke_guard().await;
    let registry = registry();
    let api_type = api_type.unwrap_or_else(|| {
        registry
            .list_providers()
            .into_iter()
            .find(|p| p.id == provider)
            .map(|p| p.api_type)
            .unwrap_or_else(|| panic!("provider {provider} missing from catalog"))
    });

    let off_messages = vec![ChatMessage::user("Reply with the single word OK.")];
    let _ = chat_once(
        SmokeCall {
            provider,
            model,
            api_key: &api_key,
            base_url: base_url.as_deref(),
            api_type,
            thinking_level: "off",
            max_tokens: 64,
        },
        &off_messages,
        &[],
        "off",
    )
    .await;
    eprintln!("[smoke] {provider}/{model} off ok");

    let on_messages = vec![ChatMessage::user(
        "Think briefly, then reply with the single word OK.",
    )];
    let _ = chat_once(
        SmokeCall {
            provider,
            model: thinking_model,
            api_key: &api_key,
            base_url: base_url.as_deref(),
            api_type,
            thinking_level: "high",
            max_tokens: 256,
        },
        &on_messages,
        &[],
        "thinking",
    )
    .await;
    eprintln!("[smoke] {provider}/{thinking_model} thinking ok");

    let tool_messages = vec![
        ChatMessage::system(
            "You must call the provided tool exactly once. Do not answer in natural language.",
        ),
        ChatMessage::user("Call smoke_echo with value \"prts\"."),
    ];
    let tools = smoke_tools();
    let response = chat_once(
        SmokeCall {
            provider,
            model,
            api_key: &api_key,
            base_url: base_url.as_deref(),
            api_type,
            thinking_level: "off",
            max_tokens: 256,
        },
        &tool_messages,
        &tools,
        "tool",
    )
    .await;
    let calls = response
        .tool_calls
        .clone()
        .filter(|calls| !calls.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{provider}/{model} tool smoke returned no tool_calls: text={:?}",
                response.text
            )
        });
    assert!(
        calls.iter().all(|call| !call.id.trim().is_empty()),
        "{provider}/{model} tool smoke returned an empty call id: {calls:?}"
    );
    eprintln!("[smoke] {provider}/{model} tool ok");

    // 第二轮：把 assistant(tool_calls) 和工具结果原样回传，确认模型能读到结果
    // 并收尾。用 thinking=off，避免非流式路径缺少 Anthropic thinking block。
    let mut round_messages = tool_messages.clone();
    round_messages.push(ChatMessage::assistant_with_tools(
        response.text.clone().unwrap_or_default(),
        calls.clone(),
        None,
    ));
    for call in &calls {
        round_messages.push(ChatMessage::tool_result(&call.id, "prts"));
    }
    let second = chat_once(
        SmokeCall {
            provider,
            model,
            api_key: &api_key,
            base_url: base_url.as_deref(),
            api_type,
            thinking_level: "off",
            max_tokens: 256,
        },
        &round_messages,
        &tools,
        "tool-round",
    )
    .await;
    let second_text = second.text.clone().unwrap_or_default();
    assert!(
        second
            .tool_calls
            .as_ref()
            .is_none_or(|calls| calls.is_empty()),
        "{provider}/{model} tool-round called another tool instead of answering: {:?}",
        second.tool_calls
    );
    assert!(
        second_text.to_ascii_lowercase().contains("prts"),
        "{provider}/{model} tool-round did not use the result: {second_text:?}"
    );
    eprintln!("[smoke] {provider}/{model} tool-round ok");
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_OPENAI_KEY; real network + cost"]
async fn provider_smoke_openai() {
    let Some(key) = key("PRTS_SMOKE_OPENAI_KEY") else {
        eprintln!("[smoke] skipped openai: PRTS_SMOKE_OPENAI_KEY absent");
        return;
    };
    run("openai", "gpt-4.1-nano", key, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_OPENAI_KEY; real network + cost"]
async fn provider_smoke_openai_responses() {
    let Some(key) = key("PRTS_SMOKE_OPENAI_KEY") else {
        eprintln!("[smoke] skipped openai-responses: PRTS_SMOKE_OPENAI_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_OPENAI_RESPONSES_MODEL", "gpt-4.1-nano");
    run_with("openai", &model, key, None, ApiType::OpenAIResponses).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_DEEPSEEK_KEY; real network + cost"]
async fn provider_smoke_deepseek_anthropic() {
    let Some(key) = key("PRTS_SMOKE_DEEPSEEK_KEY") else {
        eprintln!("[smoke] skipped deepseek-anthropic: PRTS_SMOKE_DEEPSEEK_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_DEEPSEEK_ANTHROPIC_MODEL", "deepseek-v4-flash");
    run_with(
        "deepseek",
        &model,
        key,
        Some("https://api.deepseek.com/anthropic".into()),
        ApiType::AnthropicMessages,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_ANTHROPIC_KEY; real network + cost"]
async fn provider_smoke_anthropic() {
    let Some(key) = key("PRTS_SMOKE_ANTHROPIC_KEY") else {
        eprintln!("[smoke] skipped anthropic: PRTS_SMOKE_ANTHROPIC_KEY absent");
        return;
    };
    run("anthropic", "claude-haiku-4-5", key, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_GEMINI_KEY; real network + cost"]
async fn provider_smoke_gemini() {
    let Some(key) = key("PRTS_SMOKE_GEMINI_KEY") else {
        eprintln!("[smoke] skipped gemini: PRTS_SMOKE_GEMINI_KEY absent");
        return;
    };
    run("gemini", "gemini-2.0-flash", key, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_DEEPSEEK_KEY; real network + cost"]
async fn provider_smoke_deepseek() {
    let Some(key) = key("PRTS_SMOKE_DEEPSEEK_KEY") else {
        eprintln!("[smoke] skipped deepseek: PRTS_SMOKE_DEEPSEEK_KEY absent");
        return;
    };
    run("deepseek", "deepseek-flash", key, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_ZHIPU_KEY; real network + cost"]
async fn provider_smoke_zhipu() {
    let Some(key) = key("PRTS_SMOKE_ZHIPU_KEY") else {
        eprintln!("[smoke] skipped zhipu: PRTS_SMOKE_ZHIPU_KEY absent");
        return;
    };
    run("zhipu", "glm-4-flash", key, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_KIMI_KEY; real network + cost"]
async fn provider_smoke_kimi() {
    let Some(key) = key("PRTS_SMOKE_KIMI_KEY") else {
        eprintln!("[smoke] skipped kimi: PRTS_SMOKE_KIMI_KEY absent");
        return;
    };
    run("kimi", "kimi-k2.6", key, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_QWEN_KEY; real network + cost"]
async fn provider_smoke_qwen() {
    let Some(key) = key("PRTS_SMOKE_QWEN_KEY") else {
        eprintln!("[smoke] skipped qwen: PRTS_SMOKE_QWEN_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_QWEN_MODEL", "qwen-plus");
    run("qwen", &model, key, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_VOLCENGINE_KEY; real network + cost"]
async fn provider_smoke_volcengine() {
    let Some(key) = key("PRTS_SMOKE_VOLCENGINE_KEY") else {
        eprintln!("[smoke] skipped volcengine: PRTS_SMOKE_VOLCENGINE_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_VOLCENGINE_MODEL", "doubao-seed-2-1-lite-260915");
    let base_url = std::env::var("PRTS_SMOKE_VOLCENGINE_BASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    run("volcengine", &model, key, base_url).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_DEEPSEEK_KEY; real network + cost"]
async fn provider_smoke_deepseek_matrix() {
    let Some(key) = key("PRTS_SMOKE_DEEPSEEK_KEY") else {
        eprintln!("[smoke] skipped deepseek-matrix: PRTS_SMOKE_DEEPSEEK_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_DEEPSEEK_MODEL", "deepseek-flash");
    let thinking_model = model_override("PRTS_SMOKE_DEEPSEEK_THINKING_MODEL", &model);
    run_matrix("deepseek", &model, &thinking_model, key, None, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_ZHIPU_KEY; real network + cost"]
async fn provider_smoke_zhipu_matrix() {
    let Some(key) = key("PRTS_SMOKE_ZHIPU_KEY") else {
        eprintln!("[smoke] skipped zhipu-matrix: PRTS_SMOKE_ZHIPU_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_ZHIPU_MODEL", "glm-4-flash");
    let thinking_model = model_override("PRTS_SMOKE_ZHIPU_THINKING_MODEL", "glm-5.3-flash");
    run_matrix("zhipu", &model, &thinking_model, key, None, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_KIMI_KEY; real network + cost"]
async fn provider_smoke_kimi_matrix() {
    let Some(key) = key("PRTS_SMOKE_KIMI_KEY") else {
        eprintln!("[smoke] skipped kimi-matrix: PRTS_SMOKE_KIMI_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_KIMI_MODEL", "kimi-k2.6");
    let thinking_model = model_override("PRTS_SMOKE_KIMI_THINKING_MODEL", &model);
    run_matrix("kimi", &model, &thinking_model, key, None, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_QWEN_KEY; real network + cost"]
async fn provider_smoke_qwen_matrix() {
    let Some(key) = key("PRTS_SMOKE_QWEN_KEY") else {
        eprintln!("[smoke] skipped qwen-matrix: PRTS_SMOKE_QWEN_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_QWEN_MODEL", "qwen-plus");
    let thinking_model = model_override("PRTS_SMOKE_QWEN_THINKING_MODEL", "qwen3.5-flash");
    run_matrix("qwen", &model, &thinking_model, key, None, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_VOLCENGINE_KEY; real network + cost"]
async fn provider_smoke_volcengine_matrix() {
    let Some(key) = key("PRTS_SMOKE_VOLCENGINE_KEY") else {
        eprintln!("[smoke] skipped volcengine-matrix: PRTS_SMOKE_VOLCENGINE_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_VOLCENGINE_MODEL", "doubao-seed-2-1-lite-260915");
    let thinking_model = model_override("PRTS_SMOKE_VOLCENGINE_THINKING_MODEL", &model);
    let base_url = std::env::var("PRTS_SMOKE_VOLCENGINE_BASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    run_matrix("volcengine", &model, &thinking_model, key, base_url, None).await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_DEEPSEEK_KEY; real network + cost"]
async fn provider_smoke_deepseek_anthropic_matrix() {
    let Some(key) = key("PRTS_SMOKE_DEEPSEEK_KEY") else {
        eprintln!("[smoke] skipped deepseek-anthropic-matrix: PRTS_SMOKE_DEEPSEEK_KEY absent");
        return;
    };
    let model = model_override("PRTS_SMOKE_DEEPSEEK_ANTHROPIC_MODEL", "deepseek-v4-flash");
    let thinking_model = model_override("PRTS_SMOKE_DEEPSEEK_ANTHROPIC_THINKING_MODEL", &model);
    run_matrix(
        "deepseek",
        &model,
        &thinking_model,
        key,
        Some("https://api.deepseek.com/anthropic".into()),
        Some(ApiType::AnthropicMessages),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires PRTS_SMOKE_OLLAMA=1 and a local Ollama server"]
async fn provider_smoke_ollama() {
    if std::env::var("PRTS_SMOKE_OLLAMA").ok().as_deref() != Some("1") {
        eprintln!("[smoke] skipped ollama: PRTS_SMOKE_OLLAMA != 1");
        return;
    }
    run("ollama", "llama3.2", String::new(), None).await;
}
