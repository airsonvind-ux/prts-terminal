//! exec —— 命令执行层（M3-A / M4 降权执行）
//!
//! - [`runner`]：执行内核（spawn / 超时 / 进程树回收 / 输出截断落盘）
//! - [`spawn`]：进程创建抽象（M4：身份与句柄层，std / Windows 受限令牌双实现）
//! - [`spawn_win32`]：Windows 受限令牌实现（restricted token + kill-on-close
//!   Job + workspace ACL 临时放行 + TEMP/缓存重定向 + PowerShell 探针）
//! - [`risk`]：CRIT 恒拦名单 + 命令语义族数据层（命令 → OpKind / 选项白名单）
//! - [`gate`]：自动化模式"地域性信任"放行门（evaluate 判定管道 + 子集 G 词法）

pub(crate) mod env;
pub mod gate;
pub mod risk;
pub mod runner;
mod spawn;
#[cfg(windows)]
mod spawn_win32;
