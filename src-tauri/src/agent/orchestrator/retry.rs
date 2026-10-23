//! LLM 请求重试策略：只对超时、限流和 5xx 做有限重试。

use crate::llm::LlmError;

/// 通用重试策略。
///
/// 挂靠点 = step 边界：请求失败 → 重试决策 → 重试或终止。
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// 总尝试次数（1 = 不重试；6 = 首次 + 5 次重试）
    pub max_attempts: u32,
    /// 指数退避基数（毫秒）
    pub base_delay_ms: u64,
    /// 本地退避上限（毫秒），也是供应商 `Retry-After` 的接受上限
    pub max_delay_ms: u64,
    /// 本地退避的对称抖动比例，0.1 表示 ±10%
    pub jitter_ratio: f64,
}

impl RetryPolicy {
    /// 失败后的第 n 次重试等待（n 从 1 开始）。
    ///
    /// 对齐 DSH 的有界指数退避：500ms → 1s → 2s → …，封顶 10s，
    /// 并在本地延迟上施加 ±10% 抖动。供应商给出的 `Retry-After`
    /// 不参与抖动，走 [`Self::delay_for_error`]。
    pub fn delay_ms(&self, retry: u32) -> u64 {
        let shift = retry.saturating_sub(1).min(20);
        let nominal = self
            .base_delay_ms
            .saturating_mul(1u64 << shift)
            .min(self.max_delay_ms);
        if self.jitter_ratio <= 0.0 {
            return nominal;
        }
        let factor = 1.0 - self.jitter_ratio + 2.0 * self.jitter_ratio * random_unit();
        ((nominal as f64 * factor).round() as u64).min(self.max_delay_ms)
    }

    /// 本次失败实际应等待多久；`None` 表示不应重试。
    ///
    /// 供应商 `Retry-After` 在不超过本地上限时精确采用，不做抖动；
    /// 超过上限时按 DSH 的 normal 模式放弃重试，避免违反供应商要求
    /// 提前再打一次。缺少响应头时才使用本地指数退避。
    pub fn delay_for_error(&self, retry: u32, error: &LlmError) -> Option<u64> {
        if let Some(retry_after_ms) = error.retry_after_ms() {
            if retry_after_ms == 0 {
                return Some(self.delay_ms(retry));
            }
            if retry_after_ms > self.max_delay_ms {
                return None;
            }
            return Some(retry_after_ms);
        }
        Some(self.delay_ms(retry))
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_delay_ms: 500,
            max_delay_ms: 10_000,
            jitter_ratio: 0.1,
        }
    }
}

/// 0..1 的随机样本；用系统时间纳秒做种子，避免为抖动引入额外依赖。
fn random_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos as f64 / 1_000_000_000.0
}

/// 可重试错误判定：超时恒可重试；API 错误按状态码（429 限流 / 5xx 服务端）。
/// 其余（配置/解析）不重试——重试无意义。
pub fn is_retryable(e: &LlmError) -> bool {
    match e {
        LlmError::Timeout(_) | LlmError::Transport(_) | LlmError::EmptyResponse => true,
        LlmError::Http { status, .. } => *status == 429 || (500..=599).contains(status),
        // 兼容旧路径测试与尚未迁移的调用方；新代码应产生 Http。
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

#[cfg(test)]
mod tests {
    use super::{LlmError, RetryPolicy};

    #[test]
    fn backoff_is_exponential_and_capped_with_jitter() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, 6);
        for (retry, nominal) in [(1u32, 500u64), (2, 1_000), (3, 2_000), (5, 8_000)] {
            let delay = policy.delay_ms(retry);
            assert!(
                delay >= nominal * 9 / 10 && delay <= nominal * 11 / 10,
                "retry {retry} delay {delay} outside ±10% of {nominal}"
            );
        }
        assert!(policy.delay_ms(10) <= 10_000);
    }

    #[test]
    fn zero_jitter_keeps_exact_backoff() {
        let policy = RetryPolicy {
            jitter_ratio: 0.0,
            ..RetryPolicy::default()
        };
        assert_eq!(policy.delay_ms(1), 500);
        assert_eq!(policy.delay_ms(2), 1_000);
        assert_eq!(policy.delay_ms(3), 2_000);
        assert_eq!(policy.delay_ms(5), 8_000);
        assert_eq!(policy.delay_ms(10), 10_000);
    }

    #[test]
    fn provider_retry_after_overrides_local_backoff_without_jitter() {
        let policy = RetryPolicy::default();
        let error = LlmError::http_with_retry_after(429, "rate limited", Some(3_000));
        assert_eq!(policy.delay_for_error(1, &error), Some(3_000));
    }

    #[test]
    fn provider_retry_after_above_cap_stops_retry() {
        let policy = RetryPolicy::default();
        let error = LlmError::http_with_retry_after(429, "rate limited", Some(10_001));
        assert_eq!(policy.delay_for_error(1, &error), None);
    }
}
