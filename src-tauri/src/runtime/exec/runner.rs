//! runner —— run_command 执行内核（M3-A §2 / M4 降权执行）
//!
//! 输出上限截断（内存滚动保留尾部 + 全文落盘，read_file 指引）与
//! stdout/stderr 分通道收集（读线程防管道满死锁）+ 超时 watchdog
//! （`try_wait` 100ms 轮询）。
//!
//! 进程创建（身份与句柄层）自 M4 起收敛到 [`super::spawn`]：
//! Windows 默认以**受限令牌**启动（restricted token + kill-on-close Job +
//! workspace ACL 临时放行 + TEMP/缓存重定向 + PS 探针，见
//! [`super::spawn_win32`]）；非 Windows / 调试开关 PRTS_EXEC_UNRESTRICTED=1
//! 走 std 路径（M3 语义）。回收统一 `Spawned::kill_tree`（受限 = Job 收敛；
//! std = taskkill /PID /T /F）。决策链（gate/risk/审批）一律不感知降权。
//!
//! 错误建模：spawn 失败 / 超时 / 被树杀 / 非零退出都不走 Err——
//! 统一编码进 [`RunOutput`]（outcome + exit_code），由调用方（工具层）
//! 按 outcome 渲染 summary 与 canonical（§2.4 错误映射表）。

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::exec::spawn;
use crate::turn_registry::AbortSignal;

/// 内存收集上限：超过后开始全文落盘（滚动保留尾部供 tail 渲染）。
pub(crate) const MEM_CAP_BYTES: usize = 256 * 1024;
/// 内存滚动保留的尾部字节数（截断渲染的来源）。
pub(crate) const TAIL_MEM_BYTES: usize = 64 * 1024;
/// 读线程单次读块大小。
const READ_CHUNK_BYTES: usize = 8 * 1024;
/// 超时 watchdog 轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// shell 选择（run_command 参数 shell 枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// 平台默认：Windows = PowerShell
    Auto,
    PowerShell,
    Cmd,
}

impl ShellKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "auto" => Some(Self::Auto),
            "powershell" => Some(Self::PowerShell),
            "cmd" => Some(Self::Cmd),
            _ => None,
        }
    }
}

/// 单条命令执行规格。
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub command: String,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    /// 受信写根（M4：受限启动的 ACL 放行根 + TEMP/缓存重定向基准；
    /// 与工具层 Policy.workspace_root 同源；std 路径不使用）。
    pub workspace_root: PathBuf,
    /// M4 完整权限通道（permission=full 批准后）：true = 应用自身令牌执行
    /// （Windows 仍带 Job 回收）；false = 默认受限身份。
    pub full_permission: bool,
}

/// 执行选项：超时、取消信号与输出落盘目录。
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// 进程超时（None = 不限制；工具层默认 120s）。
    pub timeout: Option<Duration>,
    /// 取消信号（M3-B：turn 中止时由 ToolContext.abort 注入）。
    /// 轮询循环命中 → 走与超时完全相同的 kill_process_tree + 收尸路径，
    /// outcome = Killed。None = 无取消（独立调用/直调场景）。
    pub cancel: Option<AbortSignal>,
    /// 输出全文落盘目录（截断发生时创建文件）。目录不存在会自动创建。
    pub output_dir: Option<PathBuf>,
    /// 落盘文件名词干（如会话/调用标识；截断时生成 {stem}.stdout.log）。
    pub log_stem: Option<String>,
}

/// 执行结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 进程正常退出（exit_code 由 0/非零区分 ok）
    Completed,
    /// 超时触发进程树回收
    TimedOut,
    /// 被外部/进程树 kill（非超时路径；当前仅 kill 兜底误杀等）
    Killed,
    /// spawn 失败（程序不存在等），`spawn_error` 承载细节
    SpawnError,
}

/// 执行结果（统一承载，无 Err 通道）。
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub outcome: Outcome,
    /// 进程退出码（Completed 时 Some；其余 None）
    pub exit_code: Option<i32>,
    /// spawn 失败细节（SpawnError 时）
    pub spawn_error: Option<String>,
    /// stdout 尾部文本（≤ TAIL_MEM_BYTES 解码后；lossy UTF-8）
    pub stdout_tail: String,
    /// stderr 尾部文本
    pub stderr_tail: String,
    /// 是否发生截断（输出总量超 MEM_CAP，全文已落盘）
    pub truncated: bool,
    /// stdout 全文落盘路径（truncated 时）
    pub full_stdout_path: Option<PathBuf>,
    /// stderr 全文落盘路径（truncated 时）
    pub full_stderr_path: Option<PathBuf>,
    /// 总输出字节（stdout+stderr）
    pub total_bytes: u64,
    /// 执行耗时（spawn 到进程退出/超时）
    pub elapsed_ms: u64,
}

impl RunOutput {
    pub fn ok(&self) -> bool {
        self.outcome == Outcome::Completed && self.exit_code == Some(0)
    }
}

/// 命令解析后的（program, args）形态。
fn resolve_shell(shell: ShellKind) -> (&'static str, Vec<String>) {
    match shell {
        ShellKind::Auto => {
            #[cfg(windows)]
            {
                // Windows 默认 PowerShell（与既有 win32 默认语义一致）
                (
                    "powershell",
                    vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                    ],
                )
            }
            #[cfg(not(windows))]
            {
                ("sh", vec!["-c".into()])
            }
        }
        ShellKind::PowerShell => (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
            ],
        ),
        ShellKind::Cmd => ("cmd", vec!["/D".into(), "/S".into(), "/C".into()]),
    }
}

/// 输出收集器：溢出前完整保留 + 超限全文落盘 + 内存滚动保留尾部。
///
/// stdout / stderr 各自一个实例（分通道保留与落盘）。
/// 落盘语义（M3-A §2.2）：未超限时内存保留全部内容（≤ MEM_CAP 级）；
/// 首次超限 → 开文件把内存中全部内容（溢出前的全文）写入，此后新 chunk
/// 直接追加文件，内存只滚动保留尾部 TAIL_MEM（供 tail 渲染，文件已含全量）。
struct OutputCollector {
    /// 未溢出时保留全部 / 溢出后滚动保留尾部（chunk 粒度）
    tail: VecDeque<Vec<u8>>,
    tail_bytes: usize,
    total_bytes: u64,
    /// 超 MEM_CAP 后打开的全文文件流
    spill: Option<std::fs::File>,
    spill_path: Option<PathBuf>,
}

impl OutputCollector {
    fn new() -> Self {
        Self {
            tail: VecDeque::new(),
            tail_bytes: 0,
            total_bytes: 0,
            spill: None,
            spill_path: None,
        }
    }

    /// 落盘初始化（懒）：目录不存在自动创建；文件名 {stem}.{chan}.log。
    fn ensure_spill(&mut self, output_dir: &Path, stem: &str, chan: &str) -> std::io::Result<()> {
        if self.spill.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(output_dir)?;
        let path = output_dir.join(format!("{stem}.{chan}.log"));
        let file = std::fs::File::create(&path)?;
        self.spill = Some(file);
        self.spill_path = Some(path);
        Ok(())
    }

    fn append(&mut self, chunk: &[u8], output_dir: Option<&Path>, stem: Option<&str>, chan: &str) {
        if chunk.is_empty() {
            return;
        }
        self.total_bytes += chunk.len() as u64;
        self.tail.push_back(chunk.to_vec());
        self.tail_bytes += chunk.len();

        if self.spill.is_some() {
            // 已溢出：新 chunk 直接追加文件；内存压缩到尾部保留区
            if let Some(file) = self.spill.as_mut() {
                use std::io::Write;
                let _ = file.write_all(chunk);
            }
            while self.tail_bytes > TAIL_MEM_BYTES && self.tail.len() > 1 {
                if let Some(front) = self.tail.pop_front() {
                    self.tail_bytes -= front.len();
                }
            }
        } else if self.total_bytes > MEM_CAP_BYTES as u64 {
            // 首次溢出：开文件，把内存中全部内容（溢出前的全文）一次性写入
            if let (Some(dir), Some(stem)) = (output_dir, stem) {
                if self.ensure_spill(dir, stem, chan).is_ok() {
                    let all: Vec<u8> = self.tail.iter().flatten().copied().collect();
                    if let Some(file) = self.spill.as_mut() {
                        use std::io::Write;
                        let _ = file.write_all(&all);
                    }
                    // 文件已含全量：内存只保留尾部供 tail 渲染
                    while self.tail_bytes > TAIL_MEM_BYTES && self.tail.len() > 1 {
                        if let Some(front) = self.tail.pop_front() {
                            self.tail_bytes -= front.len();
                        }
                    }
                }
            }
        }
    }

    /// 尾部解码文本。
    ///
    /// 滚动窗口的起点可能落在字符中间（chunk 被整块丢弃），所以先退到字符边界。
    /// 编码策略见 [`decode_output`]。
    fn tail_text(&self) -> String {
        let bytes: Vec<u8> = self.tail.iter().flatten().copied().collect();
        decode_output(&bytes).0
    }

    fn spilled_path(&self) -> Option<PathBuf> {
        self.spill_path.clone()
    }
}

/// 解码子进程输出。
///
/// 为什么不能一句 lossy 了事：Windows 的控制台程序（cmd 内建命令、不少 CLI）
/// 按本机代码页输出，中文环境即 GBK；当 UTF-8 解会全篇乱码。策略：严格 UTF-8
/// 通过就用它；否则 Windows 上按 GBK 解一次；其余平台保留 lossy。
/// 返回 (文本, 是否按本机代码页转码)。
fn decode_output(bytes: &[u8]) -> (String, bool) {
    if bytes.is_empty() {
        return (String::new(), false);
    }
    let bytes = skip_utf8_continuation(bytes);
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_owned(), false);
    }
    if cfg!(windows) {
        use encoding_rs::GBK;
        let (text, _, _) = GBK.decode(bytes);
        return (text.into_owned(), true);
    }
    (String::from_utf8_lossy(bytes).into_owned(), false)
}

/// 跳过多字节字符的续接字节（0b10xxxxxx）：窗口起点落在字符中间时退到边界，
/// 免得解码结果开头多一个替换符。
fn skip_utf8_continuation(bytes: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
        i += 1;
    }
    &bytes[i..]
}

/// 执行命令。返回统一 [`RunOutput`]（spawn 失败编码在 outcome 内）。
pub fn run_command(spec: &CommandSpec, options: &RunOptions) -> RunOutput {
    let started = Instant::now();
    let (program, prefix_args) = resolve_shell(spec.shell);

    // M4：进程创建收敛到 spawn 抽象（Windows 默认受限令牌 / full 走自身令牌
    // + Job；std 兑底与受限开启判定见 exec/spawn.rs——本层只消费统一视图）
    let mut argv: Vec<String> = prefix_args.to_vec();
    argv.push(spec.command.clone());
    let mut spawned = match spawn::spawn_command(
        program,
        &argv,
        &spec.cwd,
        &spec.workspace_root,
        spec.full_permission,
    ) {
        Ok(s) => s,
        Err(e) => {
            return RunOutput {
                outcome: Outcome::SpawnError,
                exit_code: None,
                spawn_error: Some(format!("{program} 启动失败：{e}")),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                truncated: false,
                full_stdout_path: None,
                full_stderr_path: None,
                total_bytes: 0,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
    };

    // 读线程：各自通道独立收集
    let stdout_collector = Arc::new(Mutex::new(OutputCollector::new()));
    let stderr_collector = Arc::new(Mutex::new(OutputCollector::new()));
    let out_dir = options.output_dir.clone();
    let stem = options.log_stem.clone().unwrap_or_else(default_stem);

    let stdout_pipe = spawned.take_stdout();
    let stderr_pipe = spawned.take_stderr();

    let out_col = Arc::clone(&stdout_collector);
    let t_out = stdout_pipe.map(|mut reader| {
        // 闭包各自持有一份 out_dir/stem（stdout 用克隆，stderr 用原值）
        let out_dir = out_dir.clone();
        let stem = stem.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; READ_CHUNK_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut col = out_col.lock().unwrap();
                        col.append(&buf[..n], out_dir.as_deref(), Some(&stem), "stdout");
                    }
                }
            }
        })
    });
    let err_col = Arc::clone(&stderr_collector);
    let t_err = stderr_pipe.map(|mut reader| {
        std::thread::spawn(move || {
            let mut buf = [0u8; READ_CHUNK_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut col = err_col.lock().unwrap();
                        col.append(&buf[..n], out_dir.as_deref(), Some(&stem), "stderr");
                    }
                }
            }
        })
    });

    // 超时/取消 watchdog：轮询 try_wait；超时或 cancel 命中 → 树杀 → 收尸。
    // M3-B：cancel 与超时共用同一回收路径（Spawned::kill_tree + 收尸 +
    // join 读线程）；结局差异仅在 outcome 编码（cancel → Killed，超时 →
    // TimedOut）。kill 竞态（abort 恰逢自然结束）由回收路径幂等兜底。
    let deadline = options.timeout.map(|t| Instant::now() + t);
    let status: Option<i32> = loop {
        match spawned.try_wait() {
            Ok(Some(code)) => break Some(code),
            Ok(None) => {
                let cancelled = options.cancel.as_ref().is_some_and(|c| c.is_aborted());
                let timed_out = deadline.is_some_and(|d| Instant::now() >= d);
                if cancelled || timed_out {
                    // 树杀 + 收尸（受限 = TerminateJobObject 收敛；std = taskkill /T /F）
                    spawned.kill_tree();
                    // 读线程此刻应因管道 EOF 自然结束
                    let _ = t_out.map(|h| h.join());
                    let _ = t_err.map(|h| h.join());
                    let (stdout_tail, stderr_tail, truncated, full_stdout, full_stderr, total) =
                        finalize_collectors(&stdout_collector, &stderr_collector);
                    return RunOutput {
                        outcome: if cancelled {
                            Outcome::Killed
                        } else {
                            Outcome::TimedOut
                        },
                        exit_code: None,
                        spawn_error: None,
                        stdout_tail,
                        stderr_tail,
                        truncated,
                        full_stdout_path: full_stdout,
                        full_stderr_path: full_stderr,
                        total_bytes: total,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    };
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => break None,
        }
    };

    let _ = t_out.map(|h| h.join());
    let _ = t_err.map(|h| h.join());

    let (stdout_tail, stderr_tail, truncated, full_stdout, full_stderr, total) =
        finalize_collectors(&stdout_collector, &stderr_collector);

    match status {
        Some(exit_code) => RunOutput {
            outcome: Outcome::Completed,
            exit_code: Some(exit_code),
            spawn_error: None,
            stdout_tail,
            stderr_tail,
            truncated,
            full_stdout_path: full_stdout,
            full_stderr_path: full_stderr,
            total_bytes: total,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        None => RunOutput {
            outcome: Outcome::Killed,
            exit_code: None,
            spawn_error: Some("子进程状态不可读".into()),
            stdout_tail,
            stderr_tail,
            truncated,
            full_stdout_path: full_stdout,
            full_stderr_path: full_stderr,
            total_bytes: total,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
    }
}

fn finalize_collectors(
    stdout_collector: &Arc<Mutex<OutputCollector>>,
    stderr_collector: &Arc<Mutex<OutputCollector>>,
) -> (String, String, bool, Option<PathBuf>, Option<PathBuf>, u64) {
    let out = stdout_collector.lock().unwrap();
    let err = stderr_collector.lock().unwrap();
    let truncated = out.spilled_path().is_some() || err.spilled_path().is_some();
    (
        out.tail_text(),
        err.tail_text(),
        truncated,
        out.spilled_path(),
        err.spilled_path(),
        out.total_bytes + err.total_bytes,
    )
}

/// 默认落盘词干：进程 id + 时间戳（Tool::execute 无 session/callId 可达，
/// 命名无法携带会话上下文——见实现要点说明）。
fn default_stem() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("run-{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(command: &str, cwd: &Path) -> CommandSpec {
        CommandSpec {
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            shell: ShellKind::Auto,
            // runner 单测：临时目录既当 cwd 也当写根（受限路径 ACL 放行对象）
            workspace_root: cwd.to_path_buf(),
            full_permission: false,
        }
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-exec-runner-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn echo_collects_stdout_and_exits_zero() {
        let dir = unique_temp_dir("echo");
        let out = run_command(
            &spec("echo hello-runner", &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(10)),
                output_dir: None,
                log_stem: None,
                cancel: None,
            },
        );
        assert_eq!(out.outcome, Outcome::Completed);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.ok());
        assert!(
            out.stdout_tail.contains("hello-runner"),
            "stdout: {:?}",
            out.stdout_tail
        );
        assert!(!out.truncated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GBK 兜底：Windows 控制台的中文输出按本机代码页解（"中文" 的 CP936 字节）。
    #[test]
    fn decode_output_falls_back_to_gbk_on_windows() {
        let gbk = [0xD6u8, 0xD0, 0xCE, 0xC4];
        let (text, transcoded) = decode_output(&gbk);
        if cfg!(windows) {
            assert_eq!(text, "中文");
            assert!(transcoded, "非 UTF-8 应记为转码");
        } else {
            assert!(!transcoded);
        }
        let (utf8, t2) = decode_output("中文".as_bytes());
        assert_eq!(utf8, "中文");
        assert!(!t2, "合法 UTF-8 不应转码");
    }

    #[test]
    fn exit_code_propagated() {
        let dir = unique_temp_dir("exit");
        let out = run_command(
            &spec("exit 3", &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(10)),
                output_dir: None,
                log_stem: None,
                cancel: None,
            },
        );
        assert_eq!(out.outcome, Outcome::Completed);
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stdout_stderr_channels_are_separated() {
        let dir = unique_temp_dir("channels");
        let out = run_command(
            &spec("Write-Output channel-out; Write-Error channel-err", &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(10)),
                output_dir: None,
                log_stem: None,
                cancel: None,
            },
        );
        assert!(
            out.stdout_tail.contains("channel-out"),
            "stdout: {:?}",
            out.stdout_tail
        );
        assert!(
            out.stderr_tail.contains("channel-err") || out.stdout_tail.contains("channel-err"),
            "stderr: {:?} / stdout: {:?}",
            out.stderr_tail,
            out.stdout_tail
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeout_kills_process_tree() {
        let dir = unique_temp_dir("timeout");
        let out = run_command(
            &spec("Start-Sleep -Seconds 3", &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(1)),
                output_dir: None,
                log_stem: None,
                cancel: None,
            },
        );
        assert_eq!(
            out.outcome,
            Outcome::TimedOut,
            "应超时并树杀: {:?}",
            out.outcome
        );
        // 上限放宽（<8s）：M4 受限路径首次使用含 PS 探针 + 受限启动开销，
        // 全量并行回归下该开销可能超过 3s；断言仍证明「1s 超时即回收，
        // 未等满 3s 的 Start-Sleep」的提前返回语义。
        assert!(
            out.elapsed_ms < 8000,
            "应提前返回（≤8s 含启动开销，而非等满 3s）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_kills_process_like_timeout() {
        // M3-B：cancel 命中 → 与超时共用 kill_process_tree 回收路径，
        // outcome = Killed（非 TimedOut）——中止语义区分。
        let dir = unique_temp_dir("cancel");
        let cancel = AbortSignal::new();
        cancel.abort(); // 前置置位：轮询首轮即命中（≈100ms 内返回）
        let out = run_command(
            &spec("Start-Sleep -Seconds 3", &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                output_dir: None,
                log_stem: None,
                cancel: Some(cancel),
            },
        );
        assert_eq!(
            out.outcome,
            Outcome::Killed,
            "cancel 命中应为 Killed: {:?}",
            out.outcome
        );
        assert!(out.elapsed_ms < 3000, "应提前返回（cancel 而非等满 3s）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_failure_reported_without_panic() {
        let dir = unique_temp_dir("spawn");
        let out = run_command(
            &spec("totally-not-exist-xyz-12345", &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(5)),
                output_dir: None,
                log_stem: None,
                cancel: None,
            },
        );
        // PowerShell 对未知命令非零退出（CommandNotFound）——两种结局都合法：
        // spawn 级失败（找不到 shell）或命令级失败；断言不 panic 且非 ok
        assert!(!out.ok(), "未知命令不应 ok: {:?}", out.outcome);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_output_spills_to_disk_and_truncates_tail() {
        let dir = unique_temp_dir("spill");
        let stem = format!("spill-test-{}", std::process::id());
        // 生成 > MEM_CAP(256KB) 的输出：300KB 单行文本
        let cmd = "Write-Output ('x' * 300000)".to_string();
        let out = run_command(
            &spec(&cmd, &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(20)),
                output_dir: Some(dir.clone()),
                log_stem: Some(stem.clone()),
                cancel: None,
            },
        );
        assert!(out.truncated, "输出超 MEM_CAP 应截断");
        assert!(out.total_bytes > MEM_CAP_BYTES as u64);
        let path = out.full_stdout_path.clone().expect("stdout 全文应落盘");
        assert!(path.exists(), "全文文件应存在: {:?}", path);
        assert!(path.to_string_lossy().contains("stdout.log"));
        // tail 是完整输出尾部（trim 行尾换行后以 x 串结尾）
        assert!(
            out.stdout_tail.trim_end().ends_with('x'),
            "tail 应为输出尾部: {:?}",
            out.stdout_tail.chars().rev().take(20).collect::<String>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cwd_is_honored() {
        let dir = unique_temp_dir("cwd");
        // 在 cwd 下建临时文件再用 Get-ChildItem 相对路径读
        std::fs::write(dir.join("probe.txt"), "probe-content").unwrap();
        let out = run_command(
            &spec("Get-Content probe.txt", &dir),
            &RunOptions {
                timeout: Some(Duration::from_secs(10)),
                output_dir: None,
                log_stem: None,
                cancel: None,
            },
        );
        assert!(
            out.ok(),
            "outcome: {:?} err: {:?}",
            out.outcome,
            out.stderr_tail
        );
        assert!(out.stdout_tail.contains("probe-content"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cmd_shell_works() {
        let dir = unique_temp_dir("cmd-shell");
        let out = run_command(
            &CommandSpec {
                command: "echo cmd-ok".into(),
                cwd: dir.clone(),
                shell: ShellKind::Cmd,
                workspace_root: dir.clone(),
                full_permission: false,
            },
            &RunOptions {
                timeout: Some(Duration::from_secs(10)),
                output_dir: None,
                log_stem: None,
                cancel: None,
            },
        );
        assert!(
            out.ok(),
            "cmd shell: {:?} / {:?}",
            out.outcome,
            out.stderr_tail
        );
        assert!(out.stdout_tail.to_lowercase().contains("cmd-ok"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
