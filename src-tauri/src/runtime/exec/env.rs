//! 子进程环境过滤。
//!
//! 命令子进程不应继承应用环境里的密钥形状变量，也不应看到 PRTS 内部开关。
//! 过滤只按变量名判断，不看值；`PATH`、`HOME`、语言和代理变量保留，保证
//! 普通命令仍能运行。
//!
//! 口径参照 DSH 的 `scrubbedParentEnv`：名字含 KEY/SECRET/TOKEN/PASSWORD
//! 的变量清掉，内部前缀变量清掉，其余原样保留。

use std::ffi::{OsStr, OsString};

/// 变量名中一旦出现这些词，就不传给子进程。
const SENSITIVE_MARKERS: [&str; 4] = ["KEY", "PASSWORD", "SECRET", "TOKEN"];
/// PRTS 内部开关/事实变量，不交给子进程。
const INTERNAL_PREFIX: &str = "PRTS_";

/// 判断一个环境变量名是否应被过滤。Windows 环境名不区分大小写。
pub(crate) fn is_sensitive_env_name(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    upper.starts_with(INTERNAL_PREFIX)
        || SENSITIVE_MARKERS
            .iter()
            .any(|marker| upper.contains(marker))
}

/// 当前进程环境的安全副本：过滤敏感名，其余保持原值/原顺序。
pub(crate) fn scrubbed_env() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| !is_sensitive_env_name(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_credential_shaped_names() {
        for name in [
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "DB_PASSWORD",
            "CLIENT_SECRET",
            "prts_exec_unrestricted",
        ] {
            assert!(is_sensitive_env_name(OsStr::new(name)), "{name} 应被过滤");
        }
    }

    #[test]
    fn keeps_normal_runtime_names() {
        for name in ["PATH", "SystemRoot", "TEMP", "TMP", "HTTPS_PROXY", "LANG"] {
            assert!(
                !is_sensitive_env_name(OsStr::new(name)),
                "{name} 不应被过滤"
            );
        }
    }
}
