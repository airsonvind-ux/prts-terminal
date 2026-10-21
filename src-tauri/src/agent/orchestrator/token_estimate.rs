// token 估算（模块 8 双锚的 estimate 兜底锚）
//
// 来源：https://api-docs.deepseek.com/quick_start/token_usage
// 仅作近似参考，精确 token 数以 API 返回的 usage 为准。

use crate::llm::ChatMessage;

/// 每条消息的模板开销（特殊标记如 `<｜User｜>`、`<｜Assistant｜>` 等）。
/// DeepSeek chat template 中每条消息前后各插入约 2 个特殊 token。
const TOKEN_OVERHEAD_PER_MESSAGE: usize = 4;

/// DeepSeek 官方：英文字符（含数字、标点、符号）≈ 0.3 token
const TOKEN_PER_ASCII: f64 = 0.3;

/// DeepSeek 官方：中文字符 ≈ 0.6 token
const TOKEN_PER_CJK: f64 = 0.6;

/// 逐字符估算 token 数。
///
/// 模块 8（token-meter 双锚）：作为 provider 基线锚缺失时的 estimate 兜底锚。
/// 每次 LLM 请求前估算（`estimate_messages`）并随 step/start 事件入 log，
/// 与 step/end 的 provider usage（基线锚）对齐成双锚数据；
/// 上下文压缩（模块 D）的触发估算同样消费此函数。
/// API 返回的 `usage` 字段是事后值，不适合"发请求前判断"的场景。
pub fn estimate_tokens(text: &str) -> usize {
    let mut ascii_count = 0usize;
    let mut non_ascii_count = 0usize;

    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        if c.is_ascii() {
            ascii_count += 1;
        } else {
            non_ascii_count += 1;
        }
    }

    // 整数计数 → 一次性浮点乘法，避免循环累加误差
    let tokens = (ascii_count as f64 * TOKEN_PER_ASCII
        + non_ascii_count as f64 * TOKEN_PER_CJK)
        .ceil() as usize;

    tokens.max(1)
}

/// 估算整个消息列表的 token 数（system + 历史）。
///
/// 文本 + 推理链 + 工具调用名/参数 + 每条消息的模板开销
/// （`TOKEN_OVERHEAD_PER_MESSAGE`，DeepSeek chat template 特殊标记）。
/// 与 provider 返回的 usage 双锚对照：estimate 是请求前兜底，usage 是基线锚。
pub fn estimate_messages(messages: &[ChatMessage]) -> usize {
    let mut total: usize = 0;
    for m in messages {
        total += estimate_tokens(&m.content);
        if let Some(ref reasoning) = m.reasoning_content {
            total += estimate_tokens(reasoning);
        }
        if let Some(ref calls) = m.tool_calls {
            for c in calls {
                total += estimate_tokens(&c.function.name);
                total += estimate_tokens(&c.function.arguments);
            }
        }
    }
    total + messages.len() * TOKEN_OVERHEAD_PER_MESSAGE
}
