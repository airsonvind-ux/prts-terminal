//! 单次 turn 的累积状态：用量、推理链与停滞检测。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::llm::{TokenUsage, UsageStat};

/// 单次 `agent_process` 调用（一个 turn）期间的累积状态。
/// 不跨轮次——每次 process() 入口创建新实例。
pub(super) struct TurnState {
    /// 工具执行指纹的滑动窗口（N=10）
    pub(super) fingerprint_window: Vec<u64>,
    /// **最后一次** LLM 调用的 token 用量快照（最终上下文状态）。
    pub(super) final_usage: Option<TokenUsage>,
    /// 本轮总成本（跨调用求和，供遥测/成本展示；与上下文占用区分）
    pub(super) total_input_tokens: u32,
    pub(super) total_output_tokens: u32,
    /// 本轮统计增量（缓存口径已按协议归一；随 turn/end 交前端归档）
    pub(super) stat: UsageStat,
    /// 最后一轮 LLM 推理的完整推理链（取最后一次非空值）
    pub(super) final_reasoning: Option<String>,
}

impl TurnState {
    /// 滑动窗口最大容量
    const WINDOW_SIZE: usize = 10;
    /// 同一指纹在窗口内重复出现 ≥ 此值 → 停滞
    const STAGNATION_THRESHOLD: usize = 5;

    pub(super) fn new() -> Self {
        Self {
            fingerprint_window: Vec::with_capacity(Self::WINDOW_SIZE),
            final_usage: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            stat: UsageStat::default(),
            final_reasoning: None,
        }
    }

    /// 记录一次 LLM 调用：usage 双轨记账（勿混为一谈）——
    /// - `final_usage` 覆盖式为最后一次调用快照（上下文占用）。
    /// - `total_input/output_tokens` 跨调用求和 = 成本账本。
    /// 推理链取最后一次非空值。
    pub(super) fn record_call(
        &mut self,
        usage: Option<TokenUsage>,
        reasoning: Option<String>,
    ) {
        if let Some(u) = usage {
            self.final_usage = Some(u);
            self.total_input_tokens += u.input_tokens;
            self.total_output_tokens += u.output_tokens;
        }
        if reasoning.is_some() {
            self.final_reasoning = reasoning;
        }
    }

    /// 累加一次调用的统计增量（缓存口径已按协议归一）。
    pub(super) fn add_stat(&mut self, s: UsageStat) {
        self.stat.input_tokens += s.input_tokens;
        self.stat.output_tokens += s.output_tokens;
        if let Some(h) = s.cache_hit_tokens {
            self.stat.cache_hit_tokens =
                Some(self.stat.cache_hit_tokens.unwrap_or(0).saturating_add(h));
        }
        if let Some(p) = s.cache_prompt_tokens {
            self.stat.cache_prompt_tokens =
                Some(self.stat.cache_prompt_tokens.unwrap_or(0).saturating_add(p));
        }
    }

    /// 记录一次工具执行的三元组指纹：(工具名, args, result) 的复合哈希。
    /// 窗口满时淘汰最早的条目（FIFO）。
    pub(super) fn record_fingerprint(&mut self, tool_name: &str, args: &str, result: &str) {
        let mut hasher = DefaultHasher::new();
        tool_name.hash(&mut hasher);
        args.hash(&mut hasher);
        result.hash(&mut hasher);
        let fingerprint = hasher.finish();

        if self.fingerprint_window.len() >= Self::WINDOW_SIZE {
            self.fingerprint_window.remove(0);
        }
        self.fingerprint_window.push(fingerprint);
    }

    /// 检查窗口内是否存在停滞：同一指纹出现次数 ≥ STAGNATION_THRESHOLD。
    /// 返回 true 时应终止循环。
    pub(super) fn check_stagnation(&self) -> bool {
        use std::collections::HashMap;
        let mut counts: HashMap<u64, usize> = HashMap::new();
        for &fp in &self.fingerprint_window {
            let count = counts.entry(fp).or_insert(0);
            *count += 1;
            if *count >= Self::STAGNATION_THRESHOLD {
                return true;
            }
        }
        false
    }

    /// 提取聚合结果（借用版）：(推理链, 最后一次调用 token 快照)。
    pub(super) fn snapshot(&self) -> (Option<String>, Option<TokenUsage>) {
        (self.final_reasoning.clone(), self.final_usage.clone())
    }

    /// 提取聚合结果：(推理链, 最后一次调用 token 快照)。
    #[allow(dead_code)]
    pub(super) fn finish(self) -> (Option<String>, Option<TokenUsage>) {
        (self.final_reasoning, self.final_usage)
    }
}
