//! LLM 请求重试策略：只对超时、限流和 5xx 做有限重试。

use crate::llm::LlmError;

/// 通用重试策略。
///
/// 挂靠点 = step 边界：请求失败 → 重试决策 → 重试或终止。
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// 总尝试次数（1 = 不重试）
    pub max_attempts: u32,
    /// 退避基数（毫秒）；第 n 次重试前等待 `base_delay_ms * n`
    pub base_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            base_delay_ms: 500,
        }
    }
}

/// 可重试错误判定：超时恒可重试；API 错误按状态码（429 限流 / 5xx 服务端）。
/// 其余（配置/解析）不重试——重试无意义。
pub fn is_retryable(e: &LlmError) -> bool {
    match e {
        LlmError::Timeout(_) => true,
        LlmError::Api(msg) => {
            msg.contains("HTTP 429")
                || msg.contains("HTTP 500")
                || msg.contains("HTTP 502")
                || msg.contains("HTTP 503")
                || msg.contains("HTTP 504")
        }
        _ => false,
    }
}
