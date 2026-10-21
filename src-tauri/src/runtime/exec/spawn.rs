//! spawn —— 进程创建抽象层（M4 降权执行 · 子步 1）
//!
//! run_command 的「身份与句柄层」在此切换（决策链 gate/risk/审批/超时/cancel
//! 一律不动，本层只负责把子进程以什么身份、挂在什么句柄上拉起来）：
//!
//! - **std 路径**（非 Windows；或 Windows 上调试开关
//!   `PRTS_EXEC_UNRESTRICTED=1`）：`std::process::Command` 原路径，子进程以
//!   应用自身令牌运行（M3 及以前的语义），回收仍走 `taskkill /T /F`。
//! - **受限路径**（Windows 默认，见 [`crate::exec::spawn_win32`]）：restricted
//!   token（DISABLE_MAX_PRIVILEGE|LUA_TOKEN|WRITE_RESTRICTED + 限制 SID）+
//!   kill-on-close Job + workspace ACL 临时放行 + TEMP/缓存重定向 + PS 探针。
//! - **完整权限路径**（Windows，permission=full 批准后）：应用自身令牌 +
//!   同样保留 Job kill-on-close / 超时 / cancel 回收（见 spawn_win32::spawn_full）。
//!
//! [`Spawned`] 是对两种进程的统一视图：`try_wait` 轮询退出码、
//! `kill_tree` 进程树回收（std = taskkill；受限 = TerminateJobObject +
//! 收敛等待）、stdout/stderr 以 `Box<dyn Read + Send>` 交给 runner 的读线程
//! （std 路径直接取 `Child` 管道；受限路径由本层 CreatePipe 后包成 `File`）。

use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

/// Windows 受限路径的调试开关（非用户设置；仅开发/排障用）。
/// 设 `PRTS_EXEC_UNRESTRICTED=1` 时 Windows 也走 std 路径（M3 语义）。
fn restricted_enabled() -> bool {
    cfg!(windows) && std::env::var("PRTS_EXEC_UNRESTRICTED").as_deref() != Ok("1")
}

/// 已拉起的一个子进程（std 或受限实现），统一视图。
pub(crate) struct Spawned {
    inner: SpawnedInner,
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
}

enum SpawnedInner {
    Std {
        child: std::process::Child,
    },
    #[cfg(windows)]
    Win(crate::exec::spawn_win32::RestrictedChild),
}

impl Spawned {
    /// 取 stdout 读端（runner 移交给读线程；只可取一次）。
    pub(crate) fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stdout.take()
    }

    /// 取 stderr 读端。
    pub(crate) fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stderr.take()
    }

    /// 非阻塞轮询：`Ok(Some(code))` = 已退出；`Ok(None)` = 仍在运行。
    /// `Err` = 状态不可读（沿用旧 runner 语义 → outcome Killed 兜底）。
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<i32>> {
        match &mut self.inner {
            SpawnedInner::Std { child } => match child.try_wait() {
                Ok(Some(status)) => Ok(Some(status.code().unwrap_or(-1))),
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            },
            #[cfg(windows)]
            SpawnedInner::Win(c) => c.try_wait(),
        }
    }

    /// 进程树回收（阻塞至主进程退出）：
    /// - std：taskkill /PID /T /F + wait 收尸（M3 语义原样）；
    /// - 受限：TerminateJobObject + 收敛等待（Job kill-on-close 兜底）。
    /// 供超时 / cancel watchdog 调用；回收后不再读取退出码。
    pub(crate) fn kill_tree(&mut self) {
        match &mut self.inner {
            SpawnedInner::Std { child } => {
                taskkill_tree(child.id());
                let _ = child.wait();
            }
            #[cfg(windows)]
            SpawnedInner::Win(c) => c.kill_tree(),
        }
    }
}

/// 进程树回收（std 路径；与旧 runner::kill_process_tree 同语义）。
fn taskkill_tree(pid: u32) {
    let _ = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 进程创建入口（runner 调用）。
///
/// - `program`：可执行文件名（powershell / cmd / sh）
/// - `args`：前缀参数 + 命令串（完整 argv）
/// - `cwd`：子进程工作目录
/// - `workspace_root`：受信写根（受限路径的 ACL 放行根 + TEMP/缓存落点基准；
///   std 与 full 路径不使用）
/// - `full`：M4 完整权限通道（permission=full 批准后）——Windows 上走
///   自身令牌 + Job 回收（见 [`crate::exec::spawn_win32::spawn_full`]），
///   非 Windows 走 std（平台无降权概念）
///
/// 失败返回 `Err`（含平台细节），调用方按 SpawnError 编码（fail-closed，
/// 受限创建失败不自动降级到全令牌/无 Job 执行）。
pub(crate) fn spawn_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    workspace_root: &Path,
    full: bool,
) -> io::Result<Spawned> {
    if full {
        #[cfg(windows)]
        {
            let mut child = crate::exec::spawn_win32::spawn_full(program, args, cwd, workspace_root)?;
            let stdout = child.stdout_file().map(|f| -> Box<dyn Read + Send> { Box::new(f) });
            let stderr = child.stderr_file().map(|f| -> Box<dyn Read + Send> { Box::new(f) });
            return Ok(Spawned {
                inner: SpawnedInner::Win(child),
                stdout,
                stderr,
            });
        }
        #[cfg(not(windows))]
        {
            return spawn_std(program, args, cwd);
        }
    }
    if restricted_enabled() {
        #[cfg(windows)]
        {
            let mut child = crate::exec::spawn_win32::spawn_restricted(program, args, cwd, workspace_root)?;
            let stdout = child.stdout_file().map(|f| -> Box<dyn Read + Send> { Box::new(f) });
            let stderr = child.stderr_file().map(|f| -> Box<dyn Read + Send> { Box::new(f) });
            return Ok(Spawned {
                inner: SpawnedInner::Win(child),
                stdout,
                stderr,
            });
        }
        #[cfg(not(windows))]
        {
            // 不可达（restricted_enabled 恒 false）；防御性回落 std 路径
        }
    }
    spawn_std(program, args, cwd)
}

/// std 路径（非 Windows 编译路径也在此，保证跨平台可编译）。
fn spawn_std(program: &str, args: &[String], cwd: &Path) -> io::Result<Spawned> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .map(|s| -> Box<dyn Read + Send> { Box::new(s) });
    let stderr = child
        .stderr
        .take()
        .map(|s| -> Box<dyn Read + Send> { Box::new(s) });
    Ok(Spawned {
        inner: SpawnedInner::Std { child },
        stdout,
        stderr,
    })
}

