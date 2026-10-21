//! workspace —— 当前工作根（单一事实源）
//!
//! 工作目录可在设置页随时更改，而消费方（沙箱策略 `policy`、路径守卫
//! `path_guard`、执行工具 `run_command`、仿真工具 `sim_path`）都在装配期就
//! 构造完毕。若各自留一份启动时的拷贝，改完设置就会出现「面板列的是新目录、
//! AI 写的是旧目录」的分裂——这比"全都不生效"更容易骗到人。
//!
//! 因此工作根只存一份：本模块的共享句柄。装配期注入各消费方，改设置时就地
//! 更新（写入点唯一：`set_settings`，它在 busy 闸之内，故不可能在对话处理中
//! 更换），消费方每次使用读当前值。
//!
//! # 两种形态
//!
//! 同一条事实有两种用法，句柄里各存一份（都在写入时一次算出）：
//!
//! - **原样**（[`WorkspaceRoot::current`]）：用户设定的路径。工具的落点与文案
//!   用它——不能把 canonicalize 带出的 `\\?\` 前缀写进给用户看的东西。
//! - **canonical**（[`WorkspaceRoot::canonical`]）：沙箱判定用它。守卫传入的
//!   待判路径必然已 canonicalize（`path_guard` 保证边界检查发生在
//!   canonicalize 之后），两侧必须同基准。目录不存在时保留原样（与 `policy`
//!   的既有口径一致）。
//!
//! 两者同时更新，读时不再 canonicalize——没有缓存失效问题。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// canonicalize 尝试：存在则取 canonical，不存在保留原样。
///
/// 消费方（路径守卫）拿到的待判路径必然已 canonicalize，而这里存的条目可能
/// 在解析时尚不存在（工作区被删），此时只能留普通路径。两侧统一经本函数
/// 兜底，保证比较同基准（Windows 下 canonical 带 `\\?\` 前缀）。
pub(crate) fn canonicalize_opt(p: PathBuf) -> PathBuf {
    p.canonicalize().unwrap_or(p)
}

/// 句柄内部值：一条事实的两种形态（写入时一次算出，之后只读）。
#[derive(Debug, Clone)]
struct Inner {
    /// 用户设定的原样路径（工具落点与用户可见文案）。
    raw: PathBuf,
    /// 比对用 canonical 形态（沙箱判定；不存在则等于 `raw`）。
    canonical: PathBuf,
}

impl Inner {
    fn of(raw: PathBuf) -> Self {
        let canonical = canonicalize_opt(raw.clone());
        Self { raw, canonical }
    }
}

/// 当前工作根：全局唯一的那份事实。
///
/// 内部 `Arc<RwLock<Inner>>`——**克隆共享同一份值**（各消费方持有的是句柄，
/// 不是快照）。
///
/// 刻意不提供 `Default`：空路径不是"没有工作目录"的合法表达
/// （`Path::starts_with("")` 恒真，拿空串当哨兵会让沙箱判定全线放行）。
#[derive(Debug, Clone)]
pub struct WorkspaceRoot {
    inner: Arc<RwLock<Inner>>,
}

impl WorkspaceRoot {
    /// 以给定路径建立句柄。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::of(path.into()))),
        }
    }

    /// 用户设定的路径**原样**（工具的落点与文案用它）。消费方每次使用时读，
    /// 不要缓存。
    pub fn current(&self) -> PathBuf {
        self.inner
            .read()
            .expect("工作根读锁中毒（持有者 panic）")
            .raw
            .clone()
    }

    /// 比对用 canonical 形态（沙箱判定用它；目录不存在时等于 [`Self::current`]）。
    pub fn canonical(&self) -> PathBuf {
        self.inner
            .read()
            .expect("工作根读锁中毒（持有者 panic）")
            .canonical
            .clone()
    }

    /// 就地把工作根换成新值（两种形态一并重算）。
    ///
    /// 写入点唯一：`set_settings`（在 busy 闸之内，故不会在对话处理中更换）。
    pub fn set(&self, path: impl Into<PathBuf>) {
        *self.inner.write().expect("工作根写锁中毒（持有者 panic）") = Inner::of(path.into());
    }
}

impl From<PathBuf> for WorkspaceRoot {
    /// 由路径建**独立**句柄：单测 / 独立使用。生产装配请传已有的共享句柄。
    fn from(path: PathBuf) -> Self {
        Self::new(path)
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-wsroot-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn clones_share_one_value_in_both_forms() {
        // 句柄语义的核心：克隆体共享同一份值，改一处全体可见。
        // 这是"策略与工具随设置热生效"的全部依据。
        let a = unique_temp_dir("share-a");
        let b = unique_temp_dir("share-b");
        let root = WorkspaceRoot::new(a.clone());
        let shadow = root.clone();

        assert_eq!(root.current(), a, "原样形态 = 用户设定值");
        assert_eq!(root.canonical(), a.canonicalize().unwrap());

        shadow.set(b.clone());

        assert_eq!(
            root.current(),
            b,
            "克隆体应共享同一份值（set 对两侧都可见）"
        );
        assert_eq!(
            root.canonical(),
            b.canonicalize().unwrap(),
            "canonical 形态同样随 set 更新"
        );

        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn missing_path_falls_back_to_raw_for_both_forms() {
        // 目录不存在（用户删掉了工作区）时两种形态都保留原样：
        // 不 panic、不静默换成别处。
        let missing = std::env::temp_dir().join(format!(
            "prts-wsroot-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = WorkspaceRoot::new(missing.clone());
        assert_eq!(root.current(), missing);
        assert_eq!(root.canonical(), missing, "canonicalize 失败应回退原样");
    }
}
