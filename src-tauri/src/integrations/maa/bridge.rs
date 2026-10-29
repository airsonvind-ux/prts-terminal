//! bridge —— 与 PRTS 自带的 MAA 桥进程通信。
//!
//! 协议（对照 `bridge/src/ipc.rs` 与 `bridge/src/main.rs`）：
//!
//! - 帧：`[4 字节大端 u32 长度][UTF-8 JSON]`，走本机 TCP 字节流；
//! - 命令：`{"cmd": "<名>", "id": <任意 JSON>, ...参数}`；
//! - 应答：`{"event": "result", "id": ..., "ok": bool, "data": {...}}`，
//!   或 `{"event": "error", "id": ..., "error": "..."}`；
//! - 其余帧（`ready` / `maa` / `recognition`）都是事件，与应答**共用一条流**。
//!
//! **必须有个常驻读线程。** 事件与应答在同一条连接上，而任务可以「发完就走」
//! （`wait=false`）——那时若没人读，桥的事件帧会堆满发送缓冲，把桥堵死。
//! 所以 `Bridge` 一建好就起一个读线程，不间断排空连接：
//!
//! - `id` 配得上待回答表 → 交给那条命令的等待者；
//! - 其余（`maa` / `recognition`）→ 进事件缓冲，由调用方 `drain_events` 取走。
//!
//! 桥先 `bind` 再 `accept` 再加载 MaaCore，最后才发 `ready`；
//! MaaCore 不可用时它**不退出**，而以 `available:false` 报告原因，
//! 并继续响应 `status` / `shutdown`。

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::maa::records::{results_dir, Kept, LastRun, RunRecord};

/// 单帧上限（防御畸形长度字段，别让一个整数把内存吃光）。
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// 启动到握手完成的时限（含 MaaCore 加载，慢盘上也要留够）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// 连接桥的重试间隔（桥的监听可能还没就绪）。
const CONNECT_RETRY: Duration = Duration::from_millis(100);
/// 事件缓冲上限：只保最近这些帧，老的自然淘汰（UI 看进度用，不做审计）。
const MAX_BUFFERED_EVENTS: usize = 4096;
/// 事件缓冲的字节上限。识别帧单条可达十万字——只按条数封顶挡不住增长。
const MAX_BUFFERED_BYTES: usize = 32 * 1024 * 1024;

/// 桥的失败。每条都带可展示的文案——错误要能被用户看见。
#[derive(Debug, Clone)]
pub enum BridgeError {
    /// 找不到桥可执行文件（附找过的路径）。
    ExeMissing(Vec<PathBuf>),
    /// 启动桥、连接、握手阶段失败。
    Start(String),
    /// 传输层出错（含对端断开）。
    Io(String),
    /// 命令超时。
    Timeout { cmd: String, secs: u64 },
    /// 桥回了错误（`ok:false` 或 `event:error`）。
    Refused { cmd: String, message: String },
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::ExeMissing(tried) => write!(
                f,
                "MAA 运行组件缺失，重新安装 PRTS 可恢复（找不到 prts-maa-bridge.exe）；找过：{}",
                tried
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("、")
            ),
            BridgeError::Start(msg) => write!(f, "{msg}"),
            BridgeError::Io(msg) => write!(f, "与桥通信失败：{msg}"),
            BridgeError::Timeout { cmd, secs } => write!(f, "{cmd} 在 {secs}s 内没有应答"),
            BridgeError::Refused { cmd, message } => write!(f, "{cmd} 被桥拒绝：{message}"),
        }
    }
}

impl std::error::Error for BridgeError {}

/// 命令名（仅用于错误文案）。
fn cmd_name(cmd: &Value) -> String {
    cmd.get("cmd")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

/// 定位桥可执行文件：**按构建类型给唯一路径**（开发构建读仓库构建产物，
/// 发布构建读程序目录），不做跨环境探测。
///
/// 桥是 PRTS 的组件，不是用户配置项——这里**没有**「让用户填路径」的分支。
pub fn resolve_exe() -> Result<PathBuf, BridgeError> {
    let name = if cfg!(windows) {
        "prts-maa-bridge.exe"
    } else {
        "prts-maa-bridge"
    };
    let mut tried: Vec<PathBuf> = Vec::new();

    let candidates: Vec<PathBuf> = if crate::config::is_dev() {
        // 开发构建：仓库内 bridge crate 的构建产物（debug 优先，再 release）。
        ["debug", "release"]
            .iter()
            .map(|profile| {
                crate::config::resource_root()
                    .join("..")
                    .join("bridge")
                    .join("target")
                    .join(profile)
                    .join(name)
            })
            .collect()
    } else {
        // 发布构建：桥随 PRTS 分发，落程序目录（sidecar 形态）或其 resources 下。
        let root = crate::config::resource_root();
        vec![root.join(name), root.join("resources").join(name)]
    };

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }

    Err(BridgeError::ExeMissing(tried))
}

/// 取一个空闲端口：`bind(0)` 拿系统分配的端口再放开。
///
/// 桥随后绑同一端口——存在极小的竞争窗口，可接受（绑不上桥会以退出码 3 收场）。
pub fn free_port() -> Result<u16, BridgeError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| BridgeError::Start(format!("取空闲端口失败：{e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| BridgeError::Start(format!("读本地地址失败：{e}")))?
        .port();
    drop(listener);
    Ok(port)
}

/// 写一帧。`&TcpStream` 即可写（std 对引用实现了 `Write`），无需独占。
pub fn write_frame(stream: &TcpStream, value: &Value) -> Result<(), BridgeError> {
    let bytes = serde_json::to_vec(value).map_err(|e| BridgeError::Io(e.to_string()))?;
    if bytes.len() > MAX_FRAME {
        return Err(BridgeError::Io("帧过大".to_string()));
    }
    let mut stream = stream;
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|()| stream.write_all(&bytes))
        .and_then(|()| stream.flush())
        .map_err(|e| BridgeError::Io(format!("写帧失败：{e}")))
}

/// 读一帧（阻塞，规格见 `Bridge` 的线程约定）。
pub fn read_frame(stream: &TcpStream) -> Result<Value, BridgeError> {
    let mut stream = stream;
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .map_err(|e| BridgeError::Io(format!("读帧头失败：{e}")))?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(BridgeError::Io(format!("帧长度异常：{len}")));
    }
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .map_err(|e| BridgeError::Io(format!("读帧体失败：{e}")))?;
    serde_json::from_slice(&buf).map_err(|e| BridgeError::Io(format!("帧不是合法 JSON：{e}")))
}

/// 把桥进程放进 kill-on-close Job：父进程崩溃、被强杀或卸载器直接结束时，
/// Windows 在最后一个 Job 句柄关闭时一并结束桥及其子进程。
#[cfg(windows)]
fn attach_kill_on_close_job(child: &Child) -> Option<OwnedHandle> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_err()
        {
            let _ = CloseHandle(job);
            return None;
        }

        if AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
            let _ = CloseHandle(job);
            return None;
        }

        Some(OwnedHandle::from_raw_handle(job.0))
    }
}

/// 确认桥子进程仍存活；端口抢占时真桥会因 bind 失败退出，
/// 父进程不能把抢占者的 ready 当成自己的桥。
fn ensure_child_running(child: &mut Child, stage: &str) -> Result<(), BridgeError> {
    match child.try_wait() {
        Ok(Some(status)) => Err(BridgeError::Start(format!(
            "桥进程在{stage}已退出（{status}），拒绝该连接"
        ))),
        Ok(None) => Ok(()),
        Err(e) => Err(BridgeError::Start(format!(
            "检查桥进程状态失败（{stage}）：{e}"
        ))),
    }
}

/// 待回答表：命令 id → （命令名，应答回执）。
type Pending = Arc<Mutex<HashMap<u64, (String, Sender<Result<Value, BridgeError>>)>>>;

/// 事件缓冲：条数与字节两个上限都在这里维护，做到 O(1)。
#[derive(Default)]
struct EventBuffer {
    frames: VecDeque<(Value, usize)>,
    bytes: usize,
}

impl EventBuffer {
    fn push(&mut self, frame: Value) {
        let size = frame.to_string().len();
        self.frames.push_back((frame, size));
        self.bytes += size;
        // 两个上限任一超了就淘汰最旧的；至少留一帧，别把最后一条也丢了。
        while self.frames.len() > MAX_BUFFERED_EVENTS
            || (self.bytes > MAX_BUFFERED_BYTES && self.frames.len() > 1)
        {
            if let Some((_, n)) = self.frames.pop_front() {
                self.bytes = self.bytes.saturating_sub(n);
            }
        }
    }

    fn drain(&mut self) -> Vec<Value> {
        self.bytes = 0;
        self.frames.drain(..).map(|(frame, _)| frame).collect()
    }

    /// 最近 `limit` 帧的拷贝，**不取走**。
    fn peek(&self, limit: usize) -> Vec<Value> {
        let skip = self.frames.len().saturating_sub(limit);
        self.frames
            .iter()
            .skip(skip)
            .map(|(frame, _)| frame.clone())
            .collect()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.frames.len()
    }
}

/// 桥进程 + 连接 + 常驻读线程。
///
/// 桥**只接受一条连接**，断了只能重起；`Drop` 会杀掉子进程，不留孤儿。
pub struct Bridge {
    child: Child,
    /// Windows Job：父进程崩溃或被强杀时，随句柄关闭一起结束桥。
    /// 字段本身只用于持有句柄，不需要读取。
    #[cfg(windows)]
    #[allow(dead_code)]
    job: Option<OwnedHandle>,
    stream: TcpStream,
    pending: Pending,
    events: Arc<Mutex<EventBuffer>>,
    /// 本次运行的识别帧：帧一到就记，落盘不依赖有人在等。
    records: Arc<Mutex<RunRecord>>,
    /// 「最近一次」的留痕，给状态查询用。
    last_run: Arc<Mutex<LastRun>>,
    alive: Arc<AtomicBool>,
    next_id: AtomicU64,
    /// 握手帧（`{"event":"ready","available":...,"version":...,"maa_dir":...,"user_dir":...}`）。
    pub ready: Value,
    /// 测试注入：让命令立即失败，不依赖真实桥。
    #[cfg(test)]
    fail_commands: bool,
}

impl Bridge {
    /// 起桥并完成握手：spawn → 连 TCP → 读 `ready` → 起读线程。
    ///
    /// `available:false`（未装 MAA、版本不兼容、依赖缺失）**不当作错误**——
    /// 那是有意义的运行态，由调用方决定怎么呈现。
    pub fn spawn(
        exe: &Path,
        maa_dir: &Path,
        user_dir: &Path,
        port: u16,
    ) -> Result<Self, BridgeError> {
        // cwd 必须先存在：Windows 下给子进程一个不存在的 current_dir，spawn 直接报
        // ERROR_DIRECTORY(267)，而错误文案指向「启动桥失败」，极易被误判成桥本身有问题
        // （跟模拟器、跟 MaaCore 都无关）。这个目录平时只有写任务结果时才顺带建出来，
        // 所以新装 / 清过数据根的用户会在「测试连接」这一步就被挡住（2026-10-11 实测）。
        std::fs::create_dir_all(user_dir).map_err(|e| {
            BridgeError::Start(format!("建桥工作目录失败（{}）：{e}", user_dir.display()))
        })?;
        let mut command = Command::new(exe);
        // 工作目录定在 user_dir：MaaCore 有一部分调试产出是**按相对路径**写的
        // （如战场识别存图 `debug/map/`，见 BattleHelper::save_map），不跟着它自己的
        // UserDir 走。不给 cwd 就继承调用方（开发时 = src-tauri/，打包后 = 启动目录），
        // 图会掉进项目目录或安装目录里。
        command
            .current_dir(user_dir)
            .arg("--maa-dir")
            .arg(maa_dir)
            .arg("--port")
            .arg(port.to_string())
            .arg("--user-dir")
            .arg(user_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // 不给控制台窗口：桥是后台进程，弹窗属于事故。
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command
            .spawn()
            .map_err(|e| BridgeError::Start(format!("启动桥失败（{}）：{e}", exe.display())))?;

        let addr: SocketAddr = format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| BridgeError::Start(format!("端口解析失败：{e}")))?;

        // 桥先 bind 再 accept：连接要重试到监听就绪。
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let stream = loop {
            match TcpStream::connect_timeout(&addr, CONNECT_RETRY) {
                Ok(s) => break s,
                Err(e) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(BridgeError::Start(format!("连接桥失败（{addr}）：{e}")));
                    }
                    std::thread::sleep(CONNECT_RETRY);
                }
            }
        };
        let _ = stream.set_nodelay(true);

        // 端口可能在 free_port 释放后被别人抢走：真桥 bind 失败会退出，
        // 而父进程仍可能连到抢占者的监听。先确认真桥还活着，再读 ready。
        ensure_child_running(&mut child, "连接后")?;

        // 握手：这次读要限时（避免桥卡在加载 MaaCore 时无限等）。
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let _ = stream.set_read_timeout(Some(remaining));
        let ready =
            read_frame(&stream).map_err(|e| BridgeError::Start(format!("等待桥握手失败：{e}")))?;
        if ready.get("event").and_then(Value::as_str) != Some("ready") {
            let _ = child.kill();
            let _ = child.wait();
            return Err(BridgeError::Start(format!("桥没有先发 ready：{ready}")));
        }
        // 抢占者可能伪造 ready；真桥若已因 bind 失败退出，这个 ready 不可信。
        ensure_child_running(&mut child, "握手后")?;
        // 读线程要长期阻塞等待，清掉握手用的读超时（socket 选项在克隆间共享）。
        let _ = stream.set_read_timeout(None);

        #[cfg(windows)]
        let job = attach_kill_on_close_job(&child);

        let bridge = Bridge {
            child,
            #[cfg(windows)]
            job,
            stream,
            pending: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(EventBuffer::default())),
            records: Arc::new(Mutex::new(RunRecord::default())),
            last_run: Arc::new(Mutex::new(LastRun::default())),
            alive: Arc::new(AtomicBool::new(true)),
            next_id: AtomicU64::new(1),
            ready,
            #[cfg(test)]
            fail_commands: false,
        };
        bridge.start_reader()?;
        Ok(bridge)
    }

    /// 起常驻读线程。失败（克隆连接失败 / 线程建不起来）时桥不可用。
    fn start_reader(&self) -> Result<(), BridgeError> {
        let read_stream = self
            .stream
            .try_clone()
            .map_err(|e| BridgeError::Start(format!("克隆连接失败：{e}")))?;
        let pending = Arc::clone(&self.pending);
        let events = Arc::clone(&self.events);
        let records = Arc::clone(&self.records);
        let last_run = Arc::clone(&self.last_run);
        let alive = Arc::clone(&self.alive);

        std::thread::Builder::new()
            .name("maa-bridge-reader".to_string())
            .spawn(move || {
                while let Ok(frame) = read_frame(&read_stream) {
                    route_frame(&frame, &pending, &events, &records, &last_run);
                }
                alive.store(false, Ordering::SeqCst);
                // 让所有等待中的命令立刻失败，而不是各自等到超时。
                if let Ok(mut map) = pending.lock() {
                    for (_, (name, tx)) in map.drain() {
                        let _ = tx.send(Err(BridgeError::Io(format!(
                            "桥已断开（{name} 未收到应答）"
                        ))));
                    }
                }
            })
            .map(|_| ())
            .map_err(|e| BridgeError::Start(format!("起读线程失败：{e}")))
    }

    /// MaaCore 是否可用（握手给的结论）。
    pub fn available(&self) -> bool {
        self.ready
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// 桥进程是否还在（读线程断开时置 false）。
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// 取走已缓冲的事件帧（`maa` / `recognition`），取空为止。
    pub fn drain_events(&self) -> Vec<Value> {
        self.events
            .lock()
            .map(|mut buffer| buffer.drain())
            .unwrap_or_default()
    }

    /// 看缓冲里最近的 `limit` 帧，**不取走**。
    ///
    /// 给 `status` 用：`drain_events` 会把待读的帧吃掉（那些帧属于下一次
    /// `run` 的等待结果），而 `status` 只该看。
    pub fn peek_events(&self, limit: usize) -> Vec<Value> {
        self.events
            .lock()
            .map(|buffer| buffer.peek(limit))
            .unwrap_or_default()
    }

    /// 开一次新运行。上一次没收尾的先收掉——完成信号可能根本没来（桥断、被中止）。
    pub fn begin_run(&self, task_ids: Vec<i64>) {
        if let Ok(mut record) = self.records.lock() {
            if record.in_flight() {
                let _ = record.finish(&results_dir());
            }
            record.begin(task_ids);
        }
    }

    /// 收尾当前运行，避免失败派发留下「永远在跑」的假象。
    ///
    /// 只用于 `begin_run` 之后、任务真正起跑之前的失败路径；已经在跑的任务
    /// 不能用它中止（那会绕开正常的任务停止流程）。
    pub fn abort_run(&self) {
        if let Ok(mut record) = self.records.lock() {
            if record.in_flight() {
                let _ = record.finish(&results_dir());
            }
        }
    }

    /// 测试用桥：不启动真实进程、不连接 MAA；命令按需立即失败。
    #[cfg(test)]
    pub(crate) fn test_stub(fail_commands: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("测试监听端口");
        let addr = listener.local_addr().expect("测试监听地址");
        let stream = TcpStream::connect(addr).expect("测试连接");
        let (server, _) = listener.accept().expect("测试接受连接");
        if fail_commands {
            drop(server);
        } else {
            // 成功路径的测试桥：逐帧回一条成功 result，让 command()
            // 能真实验证「命令成功后」的分支，而不是写帧即断。
            std::thread::spawn(move || {
                while let Ok(frame) = read_frame(&server) {
                    let id = frame.get("id").cloned().unwrap_or(json!(0));
                    let reply = json!({
                        "event": "result",
                        "id": id,
                        "ok": true,
                        "data": {}
                    });
                    if write_frame(&server, &reply).is_err() {
                        break;
                    }
                }
            });
        }

        let child = if cfg!(windows) {
            Command::new("cmd")
                .args(["/C", "exit", "0"])
                .spawn()
                .expect("测试子进程")
        } else {
            Command::new("true").spawn().expect("测试子进程")
        };

        Self {
            child,
            #[cfg(windows)]
            job: None,
            stream,
            pending: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(EventBuffer::default())),
            records: Arc::new(Mutex::new(RunRecord::default())),
            last_run: Arc::new(Mutex::new(LastRun::default())),
            alive: Arc::new(AtomicBool::new(true)),
            next_id: AtomicU64::new(1),
            ready: json!({ "event": "ready", "available": true }),
            fail_commands,
        }
    }

    /// 补齐本次运行的任务 id（`task_add` 之后才知道）。
    pub fn set_run_task_ids(&self, task_ids: Vec<i64>) {
        if let Ok(mut record) = self.records.lock() {
            record.set_task_ids(task_ids);
        }
    }

    /// 本次运行已经写下的结果文件（尚未收尾时为空）。
    pub fn run_files(&self) -> Vec<Kept> {
        self.records
            .lock()
            .map(|record| record.kept().to_vec())
            .unwrap_or_default()
    }

    /// 本次运行是否还没收尾（`AllTasksCompleted` 未到）。
    ///
    /// 「有没有任务在跑」问这个，别问 MaaCore 的 `AsstRunning`——那只是说
    /// 助手已启动、工作线程活着，队列空着时同样是 true。
    pub fn run_in_flight(&self) -> bool {
        self.records
            .lock()
            .map(|record| record.in_flight())
            .unwrap_or(false)
    }

    /// 「最近一次」的留痕。
    pub fn last_run(&self) -> LastRun {
        self.last_run
            .lock()
            .map(|lr| lr.clone())
            .unwrap_or_default()
    }

    /// 发一条命令并等它的应答（事件由读线程照常收进缓冲，不影响这里）。
    pub fn command(&self, mut cmd: Value, timeout: Duration) -> Result<Value, BridgeError> {
        #[cfg(test)]
        if self.fail_commands {
            return Err(BridgeError::Io("测试注入：桥命令失败".to_string()));
        }
        let name = cmd_name(&cmd);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if let Value::Object(ref mut map) = cmd {
            map.insert("id".to_string(), json!(id));
        }

        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| BridgeError::Io("桥状态损坏".to_string()))?
            .insert(id, (name.clone(), tx));

        if let Err(e) = write_frame(&self.stream, &cmd) {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&id);
            }
            return Err(e);
        }

        match rx.recv_timeout(timeout) {
            Ok(Ok(data)) => Ok(data),
            Ok(Err(e)) => Err(e),
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&id);
                }
                Err(BridgeError::Timeout {
                    cmd: name,
                    secs: timeout.as_secs(),
                })
            }
            Err(RecvTimeoutError::Disconnected) => Err(BridgeError::Io("桥已断开".to_string())),
        }
    }

    /// 请求桥退出。不理会应答（也可能已经断了），最后兜底强杀。
    pub fn shutdown(mut self) {
        let _ = write_frame(&self.stream, &json!({ "cmd": "shutdown" }));
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 一帧的归属：`id` 配得上待回答表就回给命令，否则进事件缓冲；
/// 识别帧另外记进本次运行（落盘不依赖有人在等）。
fn route_frame(
    frame: &Value,
    pending: &Pending,
    events: &Arc<Mutex<EventBuffer>>,
    records: &Arc<Mutex<RunRecord>>,
    last_run: &Arc<Mutex<LastRun>>,
) {
    let is_reply = matches!(
        frame.get("event").and_then(Value::as_str),
        Some("result") | Some("error")
    );
    if is_reply {
        if let Some(id) = frame.get("id").and_then(Value::as_u64) {
            let waiter = pending.lock().ok().and_then(|mut map| map.remove(&id));
            if let Some((name, tx)) = waiter {
                let _ = tx.send(parse_reply(&name, frame));
                return;
            }
        }
        // 没有等待者（命令已超时后迟到的应答）——丢掉，别混进事件流。
        return;
    }

    if frame.get("event").and_then(Value::as_str) == Some("recognition") {
        if let Ok(mut record) = records.lock() {
            record.observe(frame);
        }
    }
    let run_finished = frame.get("code").and_then(Value::as_i64)
        == Some(crate::maa::records::CODE_ALL_TASKS_COMPLETED);

    if let Ok(mut buffer) = events.lock() {
        buffer.push(frame.clone());
    }

    // 整条队列跑完 → 落盘。放在事件侧，`wait=0` 的任务同样留下结果文件。
    if run_finished {
        finish_run(records, last_run);
    }
}

/// 收尾本次运行：写结果文件，并留下「最近一次」的留痕。
///
/// **跑在读线程里**——绝不能碰管理器那把锁；只锁 `records` / `last_run`，
/// 文件都很小，写完就放。
fn finish_run(records: &Arc<Mutex<RunRecord>>, last_run: &Arc<Mutex<LastRun>>) {
    let Ok(mut record) = records.lock() else {
        return;
    };
    let started_at_ms = record.started_at_ms();
    let task_ids = record.task_ids().to_vec();
    match record.finish(&results_dir()) {
        Ok(()) => {
            if let Ok(mut lr) = last_run.lock() {
                *lr = LastRun {
                    started_at_ms,
                    ended_at_ms: record.ended_at_ms(),
                    task_ids,
                    files: record.kept().to_vec(),
                    completed: true,
                    error: None,
                    from_disk: false,
                };
            }
        }
        Err(e) => {
            // 落盘失败不该打断事件流，但要留下痕迹。
            if let Ok(mut lr) = last_run.lock() {
                lr.error = Some(e);
            }
        }
    }
}

/// 把应答帧翻译成结果。
fn parse_reply(name: &str, frame: &Value) -> Result<Value, BridgeError> {
    let refused = |message: String| BridgeError::Refused {
        cmd: name.to_string(),
        message,
    };
    if frame.get("event").and_then(Value::as_str) == Some("error") {
        return Err(refused(
            frame
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("未说明原因")
                .to_string(),
        ));
    }
    if frame.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(frame.get("data").cloned().unwrap_or(Value::Null));
    }
    Err(refused(
        frame
            .get("data")
            .and_then(|d| d.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("未说明原因")
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一对互连的 socket（客户端 / 服务端）。
    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn peek_returns_the_tail_without_draining() {
        let mut buffer = EventBuffer::default();
        for i in 0..5 {
            buffer.push(json!({ "n": i }));
        }
        let tail = buffer.peek(2);
        assert_eq!(tail, vec![json!({ "n": 3 }), json!({ "n": 4 })]);
        // 看一眼不该把事情改变：条数不变，drain 拿到的还是全部五条。
        assert_eq!(buffer.len(), 5);
        assert_eq!(buffer.drain().len(), 5);
    }

    #[test]
    fn peek_more_than_buffered_returns_everything() {
        let mut buffer = EventBuffer::default();
        buffer.push(json!({ "n": 1 }));
        assert_eq!(buffer.peek(50), vec![json!({ "n": 1 })]);
        assert_eq!(buffer.peek(0).len(), 0);
    }

    #[test]
    fn frame_round_trip() {
        let (client, server) = socket_pair();
        let value = json!({
            "cmd": "status",
            "id": 7,
            "中文": "值",
            "nested": { "list": [1, 2, 3] }
        });
        write_frame(&client, &value).unwrap();
        assert_eq!(read_frame(&server).unwrap(), value);
    }

    #[test]
    fn read_frame_rejects_oversized_length() {
        let (mut client, server) = socket_pair();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert!(read_frame(&server).is_err(), "畸形长度必须被挡下");
    }

    #[test]
    fn free_port_is_bindable() {
        let port = free_port().unwrap();
        assert!(port > 0);
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        drop(listener);
    }

    #[test]
    fn route_frame_replies_to_waiter_and_buffers_events() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let events: Arc<Mutex<EventBuffer>> = Arc::new(Mutex::new(EventBuffer::default()));
        let records: Arc<Mutex<RunRecord>> = Arc::new(Mutex::new(RunRecord::default()));
        let last_run: Arc<Mutex<LastRun>> = Arc::new(Mutex::new(LastRun::default()));

        let (tx, rx) = mpsc::channel();
        pending
            .lock()
            .unwrap()
            .insert(42, ("status".to_string(), tx));

        route_frame(
            &json!({ "event": "result", "id": 42, "ok": true, "data": { "connected": true } }),
            &pending,
            &events,
            &records,
            &last_run,
        );
        assert_eq!(rx.recv().unwrap().unwrap()["connected"], json!(true));

        route_frame(
            &json!({ "event": "maa", "code": 20003, "name": "x" }),
            &pending,
            &events,
            &records,
            &last_run,
        );
        assert_eq!(events.lock().unwrap().len(), 1, "非应答帧进事件缓冲");

        // 识别帧同时记进本次运行——落盘不依赖有人在等。
        route_frame(
            &json!({ "event": "recognition", "kind": "StageDrops", "details": { "stars": 3 } }),
            &pending,
            &events,
            &records,
            &last_run,
        );
        assert_eq!(records.lock().unwrap().kind_frames("StageDrops"), 1);

        // 迟到的应答（没有等待者）不该混进事件流。
        route_frame(
            &json!({ "event": "result", "id": 999, "ok": true, "data": {} }),
            &pending,
            &events,
            &records,
            &last_run,
        );
        assert_eq!(events.lock().unwrap().len(), 2);
    }

    #[test]
    fn parse_reply_reads_error_from_both_shapes() {
        let refused = parse_reply(
            "task_add",
            &json!({ "event": "result", "ok": false, "data": { "error": "invalid type: X" } }),
        );
        assert!(
            matches!(refused, Err(BridgeError::Refused { ref message, .. }) if message.contains("invalid type"))
        );

        let errored = parse_reply("status", &json!({ "event": "error", "error": "读帧失败" }));
        assert!(
            matches!(errored, Err(BridgeError::Refused { ref message, .. }) if message.contains("读帧失败"))
        );

        let ok = parse_reply(
            "status",
            &json!({ "event": "result", "ok": true, "data": { "available": true } }),
        );
        assert_eq!(ok.unwrap()["available"], json!(true));
    }

    #[test]
    fn event_buffer_drops_oldest_when_full() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let events: Arc<Mutex<EventBuffer>> = Arc::new(Mutex::new(EventBuffer::default()));
        let records: Arc<Mutex<RunRecord>> = Arc::new(Mutex::new(RunRecord::default()));
        let last_run: Arc<Mutex<LastRun>> = Arc::new(Mutex::new(LastRun::default()));
        for n in 0..(MAX_BUFFERED_EVENTS + 10) {
            route_frame(
                &json!({ "event": "maa", "seq": n }),
                &pending,
                &events,
                &records,
                &last_run,
            );
        }
        let mut buffer = events.lock().unwrap();
        assert_eq!(buffer.len(), MAX_BUFFERED_EVENTS, "缓冲有上限");
        let drained = buffer.drain();
        assert_ne!(drained[0]["seq"], json!(0), "老的先丢");
    }

    #[test]
    fn event_buffer_also_caps_total_bytes() {
        let mut buffer = EventBuffer::default();
        let big = "x".repeat(1024 * 1024); // 每帧约 1MB
        for _ in 0..(MAX_BUFFERED_BYTES / (1024 * 1024) + 5) {
            buffer.push(json!({ "event": "recognition", "payload": big }));
        }
        assert!(
            buffer.bytes <= MAX_BUFFERED_BYTES,
            "字节上限生效（现在 {} 字节）",
            buffer.bytes
        );
    }
}
