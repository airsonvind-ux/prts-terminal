//! exec_settings —— 命令执行模式（全局声明层设置，M3-A §4.2）
//!
//! 两模式（非会话级、非既有实现的四模式）：
//! - `Ask`（默认 + 兜底）：每个 run_command 调用都弹用户确认
//! - `Auto`：低危自动执行、高危询问、极危拒绝（用户确认风险后手动切换）
//!
//! 存储形态：app State `Arc<Mutex<ExecutionSettings>>`（与 LlmConfig 平行）。
//! turn 开始时快照一次（turn 内模式不变 = processing/streaming 期间锁定的
//! 语义底座；M3-B 在 setter 与前端开关上叠加运行时闸，不改本存储结构）。
//! 持久化与 LlmConfig 现状同待遇（先内存，第二轮统一落盘 data_dir/settings.json）。

use serde::{Deserialize, Serialize};

/// 命令执行模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum ExecutionMode {
    /// 操作前询问（默认）：每个 run_command 调用都弹确认
    #[default]
    Ask,
    /// 自动化：低危自动 / 高危询问 / 极危拒绝
    Auto,
}

impl ExecutionMode {
    /// 展示标签（UI / 事件语义用；前端 M3-A 子步 7 接线后消费）。
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ask => "操作前询问",
            Self::Auto => "自动化",
        }
    }
}

/// 执行设置容器（当前仅模式一项；未来扩展直接加字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSettings {
    pub mode: ExecutionMode,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Ask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_is_ask() {
        assert_eq!(ExecutionSettings::default().mode, ExecutionMode::Ask);
        assert_eq!(ExecutionMode::default(), ExecutionMode::Ask);
    }

    #[test]
    fn mode_serde_kebab_case_roundtrip() {
        let v = serde_json::to_value(ExecutionMode::Auto).unwrap();
        assert_eq!(v, serde_json::json!("auto"));
        let v = serde_json::to_value(ExecutionMode::Ask).unwrap();
        assert_eq!(v, serde_json::json!("ask"));

        let m: ExecutionMode = serde_json::from_value(serde_json::json!("ask")).unwrap();
        assert_eq!(m, ExecutionMode::Ask);
        let s: ExecutionSettings =
            serde_json::from_value(serde_json::json!({ "mode": "auto" })).unwrap();
        assert_eq!(s.mode, ExecutionMode::Auto);
        // 非法值拒绝
        assert!(serde_json::from_value::<ExecutionMode>(serde_json::json!("operate")).is_err());
    }
}
