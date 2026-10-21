//! thinking —— 思考强度（thinking level）canonical 解析与请求注入
//!
//! 移植自既有实现的思考档位与协议兼容逻辑（
//! lib/providers/deepseek*.ts）的"canonical 档 → 各族 wire 参数"语义，按
//! PRTS 的 api_type 三族 + 既有 request_param（resolve_compat）收口：
//!
//! - canonical 档：off / low / medium / high / max（白名单由 models.json
//!   `thinking_levels` 声明；**无声明的 reasoning 模型 = 推断集
//!   [off, medium, high]，默认 medium**——修正 2，对齐既有实现）
//! - **None = 跟随模型默认档**：resolve 在 reasoning=true 时恒返回一档
//!   （声明 default 或推断 medium）并注入——默认档即生效档，请求携带显式
//!   档位（用户可感知、防服务端默认漂移）；非 reasoning / 未知模型 → 不注入
//!
//! 各族 wire（逐函数对照既有实现）：
//! - completions + request_param="thinking"（DeepSeek）：照
//!   lib/providers/deepseek.ts reasoningEffortForLevel / thinking 形态——
//!   off → `thinking:{type:"disabled"}` + 删 reasoning_effort；其余 →
//!   enabled + reasoning_effort（low→low、medium/high→high、max→max）
//! - completions + request_param="enable_thinking"（Qwen/Zhipu 族）：照
//!   core/provider-compat/qwen.ts——off → false；其余 → true（布尔形态；
//!   修正旧 `{"type":"enabled"}` 非布尔附加）
//! - completions + request_param=None（OpenAI 系无 thinking_format）：发
//!   `reasoning_effort`（effort 无 off/max 档，由模型声明集约束；off → 删
//!   字段 = 服务端默认；max → 降 high——OpenAI chat effort 无 max）
//! - anthropic-messages：照 core/provider-compat/anthropic.ts——off →
//!   `thinking:{type:"disabled"}`；非 off → enabled + budget_tokens
//!   （PRTS 无既有实现的预算层，enabled 暂以 8192 起步，
//!   **待 anthropic 推理族声明接入时复核**——见任务书上报原则）
//! - openai-responses：沿用既有 responses 族——off → `reasoning.effort=
//!   "none"`；其余 effort 直传（low/medium/high/max）

use serde_json::Value;

use super::{ApiType, ModelRegistry, ResolvedCompat};

/// canonical 白名单（UI/声明/设置共用词汇）。
pub const LEVELS: [&str; 5] = ["off", "low", "medium", "high", "max"];

/// 归一 canonical：未知档返回 None（调用方按"未设置"处理，不臆造）。
pub fn canonical(level: &str) -> Option<&'static str> {
    let t = level.trim().to_ascii_lowercase();
    // 兼容别名（既有实现 normalize：xhigh 对外即 max 档内部值）
    if t == "xhigh" {
        return Some("max");
    }
    LEVELS.iter().copied().find(|&l| l == t)
}

/// 推断白名单（无显式声明的 reasoning 模型；沿用既有 DEFAULT_VISIBLE
/// [off, medium, high] 与全局默认 medium）。
pub const INFERRED_LEVELS: [&str; 3] = ["off", "medium", "high"];
/// 推断默认档（既有实现 normalizeSessionThinkingLevel 全局默认 medium）。
pub const INFERRED_DEFAULT: &str = "medium";

/// 解析本次请求最终思考档。
///
/// 修正 2 语义（对齐既有实现，替换"未声明不显示/不注入"）：
/// - 模型 reasoning=false 或不在注册表 → None（不注入、UI 不显示）
/// - **reasoning=true 恒返回一档**：requested None → 模型默认档（有声明 =
///   声明 default；无声明 = medium 推断默认）——默认档即生效档，请求携带
///   显式档位（用户可感知）；requested 在白名单（声明集或推断集）→ 原档；
///   越界 → 模型默认档
pub fn resolve_requested(
    registry: &ModelRegistry,
    provider: &str,
    model: &str,
    requested: Option<&str>,
) -> Option<&'static str> {
    let entry = registry.get_model(provider, model)?;
    if !entry.reasoning {
        return None;
    }
    let req = requested.and_then(canonical);

    // 无显式声明 → 推断集 [off, medium, high] / 默认 medium
    if entry.thinking_levels.is_empty() {
        return match req {
            None => Some(INFERRED_DEFAULT),
            Some(r) if INFERRED_LEVELS.contains(&r) => Some(r),
            Some(_) => Some(INFERRED_DEFAULT),
        };
    }

    // 有声明 → 白名单 = 声明集，默认 = 声明 default（缺省防御 high）
    let default_level = entry
        .default_thinking_level
        .as_deref()
        .and_then(canonical)
        .or(Some("high"));
    let declared = |r: &str| {
        entry
            .thinking_levels
            .iter()
            .any(|l| canonical(l) == Some(r))
    };
    match req {
        None => Some(default_level.unwrap_or(INFERRED_DEFAULT)),
        Some(r) if declared(r) => Some(r),
        Some(_) => Some(default_level.unwrap_or(INFERRED_DEFAULT)),
    }
}

/// 注入 completions 通道的 thinking 档参数。
///
/// `request_param` 来自 resolve_compat（"thinking"=DeepSeek、
/// "enable_thinking"=Qwen/Zhipu 族、None=OpenAI 系 effort）。
/// **resolve 未返回档（非 reasoning/未知模型）时调用方不应调用本函数**。
pub fn apply_completions(
    body: &mut Value,
    request_param: Option<&str>,
    level: &'static str,
) {
    match request_param {
        Some("thinking") => apply_deepseek_completions(body, level),
        Some("enable_thinking") => {
            // 布尔形态（qwen.ts：enable_thinking: bool 控制思考开关；无档位细分）
            body["enable_thinking"] = Value::Bool(level != "off");
        }
        _ => apply_openai_effort(body, level),
    }
}

/// DeepSeek completions（thinking 参数形态；照 deepseek.ts）。
fn apply_deepseek_completions(body: &mut Value, level: &'static str) {
    if level == "off" {
        body["thinking"] = serde_json::json!({"type": "disabled"});
        body.as_object_mut().map(|o| o.remove("reasoning_effort"));
        return;
    }
    body["thinking"] = serde_json::json!({"type": "enabled"});
    // effort 映射（deepseek.ts reasoningEffortForLevel）：
    // low→low、medium/high→high、max→max（deepseek 官方 low/high/max）
    let effort = match level {
        "low" => "low",
        "max" => "max",
        _ => "high", // medium / high → high
    };
    body["reasoning_effort"] = Value::String(effort.into());
}

/// OpenAI 系 completions（无 thinking_format，effort 通道）。
fn apply_openai_effort(body: &mut Value, level: &'static str) {
    if level == "off" {
        body.as_object_mut().map(|o| o.remove("reasoning_effort"));
        return;
    }
    // OpenAI chat effort 无 max 档 → 降 high（openai-completions 词汇）
    let effort = if level == "max" { "high" } else { level };
    body["reasoning_effort"] = Value::String(effort.into());
}

/// 注入 anthropic-messages 通道 thinking（照 anthropic.ts 语义；预算映射自定）。
///
/// budget_tokens 公开约束：最小值 1024 且必须 < max_tokens（Anthropic/Bedrock
/// 文档一致）；**无官方档位标准表**（既有实现的预算在内部，PRTS 无此层）。
/// 映射按档递增取合理值（裁决①）：low→2048 / medium→8192 / high→16384 /
/// max→32768，均满足 ≥1024。开启 thinking 时若 max_tokens 不足（PRTS 默认
/// 4096 会违反 budget < max_tokens），**联动抬升 max_tokens** = max(现值,
/// budget + 4096)（参考既有 max 档抬 max_tokens 的做法）。
pub fn apply_anthropic(body: &mut Value, level: &'static str) {
    if level == "off" {
        body["thinking"] = serde_json::json!({"type": "disabled"});
        return;
    }
    let budget: u64 = match level {
        "low" => 2048,
        "high" => 16384,
        "max" => 32768,
        _ => 8192, // medium
    };
    // 联动抬升 max_tokens：budget 必须 < max_tokens
    let current_max = body.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let need = budget + 4096; // 思考 + 输出预留
    if current_max <= budget || current_max < need {
        body["max_tokens"] = Value::from(need);
    }
    body["thinking"] = serde_json::json!({
        "type": "enabled",
        "budget_tokens": budget,
    });
}

/// 注入 responses 通道 thinking（沿用既有 responses：effort none/low/medium/high/max）。
pub fn apply_responses(body: &mut Value, level: &'static str) {
    // off → effort "none"（Responses 的显式关闭）
    let effort = if level == "off" { "none" } else { level };
    body["reasoning"] = serde_json::json!({ "effort": effort });
}

/// 适配器通用入口：解析有效档并执行注入。
/// body 会被修改；返回 false 表示未注入（非 reasoning/未知模型——
/// reasoning 模型恒返回 true：None 请求档 = 模型默认档注入）。
pub fn inject(
    body: &mut Value,
    api_type: ApiType,
    compat: &ResolvedCompat,
    registry: &ModelRegistry,
    provider: &str,
    model: &str,
    requested: Option<&str>,
) -> bool {
    let Some(level) = resolve_requested(registry, provider, model, requested) else {
        return false;
    };
    match api_type {
        ApiType::AnthropicMessages => apply_anthropic(body, level),
        ApiType::OpenAIResponses => apply_responses(body, level),
        ApiType::OpenAiCompletions => {
            apply_completions(body, compat.request_param.as_deref(), level)
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_declared_model() -> ModelRegistry {
        // 内嵌最小 models.json：一个声明模型 + 一个未声明模型
        let json = r#"{
            "deepseek": {
                "defaults": {"api_type": "openai-completions", "base_url": "https://api.deepseek.com/v1"},
                "models": {
                    "deepseek-v4-flash": {
                        "name": "DeepSeek V4 Flash", "context": 1000000, "reasoning": true,
                        "compat": {"thinking_format": "deepseek"},
                        "thinking_levels": ["off", "low", "high", "max"],
                        "default_thinking_level": "high"
                    },
                    "deepseek-v4-pro": {
                        "name": "DeepSeek V4 Pro", "context": 1000000, "reasoning": true,
                        "compat": {"thinking_format": "deepseek"}
                    }
                }
            }
        }"#;
        ModelRegistry::from_json(json).expect("fixture 应可解析")
    }

    #[test]
    fn canonical_normalizes_and_aliases() {
        assert_eq!(canonical("off"), Some("off"));
        assert_eq!(canonical("HIGH"), Some("high"));
        assert_eq!(canonical("xhigh"), Some("max"));
        assert_eq!(canonical("unknown"), None);
        assert_eq!(canonical(""), None);
    }

    #[test]
    fn resolve_uses_whitelist_and_falls_back_to_model_default() {
        let reg = registry_with_declared_model();
        // 白名单内直通
        assert_eq!(
            resolve_requested(&reg, "deepseek", "deepseek-v4-flash", Some("low")),
            Some("low")
        );
        // 不在白名单（medium）→ 回退声明默认 high
        assert_eq!(
            resolve_requested(&reg, "deepseek", "deepseek-v4-flash", Some("medium")),
            Some("high")
        );
        // 未设置 → 声明默认档（默认档即生效档）
        assert_eq!(
            resolve_requested(&reg, "deepseek", "deepseek-v4-flash", None),
            Some("high")
        );
        // 未声明 reasoning 模型 → 推断集 [off,medium,high] / 默认 medium
        assert_eq!(
            resolve_requested(&reg, "deepseek", "deepseek-v4-pro", None),
            Some("medium")
        );
        assert_eq!(
            resolve_requested(&reg, "deepseek", "deepseek-v4-pro", Some("high")),
            Some("high")
        );
        // 推断集外（low）→ 回退 medium
        assert_eq!(
            resolve_requested(&reg, "deepseek", "deepseek-v4-pro", Some("low")),
            Some("medium")
        );
        // 未知模型 → None（不注入）
        assert_eq!(
            resolve_requested(&reg, "deepseek", "nope", Some("high")),
            None
        );
    }

    #[test]
    fn non_reasoning_model_returns_none() {
        let json = r#"{
            "deepseek": {
                "defaults": {"api_type": "openai-completions", "base_url": "https://api.deepseek.com/v1"},
                "models": {
                    "deepseek-chat": {"name": "DeepSeek Chat", "context": 128000, "reasoning": false}
                }
            }
        }"#;
        let reg = ModelRegistry::from_json(json).unwrap();
        assert_eq!(resolve_requested(&reg, "deepseek", "deepseek-chat", None), None);
        assert_eq!(resolve_requested(&reg, "deepseek", "deepseek-chat", Some("high")), None);
    }

    fn completions_body(request_param: Option<&str>) -> Value {
        let mut v = serde_json::json!({"model": "m", "messages": []});
        if let Some(p) = request_param {
            v[p] = serde_json::json!({"type": "enabled"});
        }
        v
    }

    #[test]
    fn deepseek_completions_wire_off_and_levels() {
        // off → thinking disabled + 删 effort
        let mut b = completions_body(Some("thinking"));
        apply_deepseek_completions(&mut b, "off");
        assert_eq!(b["thinking"]["type"], "disabled");
        assert!(b.get("reasoning_effort").is_none());

        // low → enabled + effort low
        let mut b = completions_body(Some("thinking"));
        apply_deepseek_completions(&mut b, "low");
        assert_eq!(b["thinking"]["type"], "enabled");
        assert_eq!(b["reasoning_effort"], "low");

        // medium/high → effort high（deepseek 官方无 medium）
        let mut b = completions_body(Some("thinking"));
        apply_deepseek_completions(&mut b, "high");
        assert_eq!(b["reasoning_effort"], "high");

        // max → effort max
        let mut b = completions_body(Some("thinking"));
        apply_deepseek_completions(&mut b, "max");
        assert_eq!(b["reasoning_effort"], "max");
    }

    #[test]
    fn enable_thinking_boolean_form() {
        let mut b = completions_body(Some("enable_thinking"));
        apply_completions(&mut b, Some("enable_thinking"), "off");
        assert_eq!(b["enable_thinking"], false);
        let mut b = completions_body(Some("enable_thinking"));
        apply_completions(&mut b, Some("enable_thinking"), "high");
        assert_eq!(b["enable_thinking"], true);
    }

    #[test]
    fn openai_effort_channel_off_removes_and_max_downgrades() {
        let mut b = completions_body(None);
        apply_completions(&mut b, None, "off");
        assert!(b.get("reasoning_effort").is_none());

        let mut b = completions_body(None);
        apply_completions(&mut b, None, "medium");
        assert_eq!(b["reasoning_effort"], "medium");

        // max → high（OpenAI chat effort 无 max）
        let mut b = completions_body(None);
        apply_completions(&mut b, None, "max");
        assert_eq!(b["reasoning_effort"], "high");
    }

    #[test]
    fn anthropic_and_responses_wire() {
        let mut b = serde_json::json!({"model": "claude", "max_tokens": 100});
        apply_anthropic(&mut b, "off");
        assert_eq!(b["thinking"]["type"], "disabled");
        let mut b = serde_json::json!({"model": "claude", "max_tokens": 100});
        apply_anthropic(&mut b, "high");
        assert_eq!(b["thinking"]["type"], "enabled");
        assert_eq!(b["thinking"]["budget_tokens"], 16384);
        // budget < max_tokens 约束：默认 4096 不足 → 联动抬升
        assert!(b["max_tokens"].as_u64().unwrap() > b["thinking"]["budget_tokens"].as_u64().unwrap());
        // 档位预算映射
        let mut b = serde_json::json!({"model": "claude", "max_tokens": 99999});
        apply_anthropic(&mut b, "low");
        assert_eq!(b["thinking"]["budget_tokens"], 2048);
        assert_eq!(b["max_tokens"], 99999, "已有充足 max_tokens 不抬升");

        let mut b = serde_json::json!({"model": "gpt"});
        apply_responses(&mut b, "off");
        assert_eq!(b["reasoning"]["effort"], "none");
        let mut b = serde_json::json!({"model": "gpt"});
        apply_responses(&mut b, "max");
        assert_eq!(b["reasoning"]["effort"], "max");
    }
}
