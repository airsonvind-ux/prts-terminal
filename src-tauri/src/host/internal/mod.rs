//! `host` 的内部实现：**只有 `host` 及其后代能看见**（可见性纪律，D33）。
//!
//! 这里放「知道宿主怎么干活」的代码。它不是契约层——插件与工具都不该直接调用，
//! 唯一出口是 `host` 根上那几项 `pub(crate)` 再导出。

pub mod fs;
pub mod maa_bridge;
pub mod query;
