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
//!
//! **进程归宿主，连接归这里**（M2a）：宿主只负责把桥拉起来并把端口交出来
//! （`crate::host::maa_bridge_ensure`），定位可执行文件、挑端口、spawn、Job
//! Object 也随之搬去了 `host::internal::maa_bridge`。
//! 桥只 `accept` 一次，所以**不能**有第二个人先连上去探活——那会把插件唯一的
//! 连接机会用掉（D41）。

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
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
    /// 启动桥、连接、握手阶段失败（含宿主报来的「组件缺失」「MAA 目录不存在」）。
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

/// 确认宿主托管的桥进程仍存活。
///
/// 端口可能在宿主挑好之后被抢走：真桥 bind 失败会退出，而我们仍可能连到抢占者
/// 的监听。进程生死只有宿主知道（宿主不探活），所以这个判断只能在这里做。
fn ensure_host_running(stage: &str) -> Result<(), BridgeError> {
    let status = super::manager::ctx()
        .map_err(BridgeError::Start)?
        .call("maa.bridge", serde_json::json!({ "op": "status" }))
        .map_err(|error| BridgeError::Start(error.to_string()))?;
    if status["state"] == serde_json::json!("running") {
        return Ok(());
    }
    Err(BridgeError::Start(match status["error"].as_str() {
        Some(error) => format!("桥进程在{stage}已退出（{error}），拒绝该连接"),
        None => format!("桥进程在{stage}已退出，拒绝该连接"),
    }))
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

/// 一条到桥的连接 + 常驻读线程。
///
/// 桥**只接受一条连接**，断了只能重起；进程本身归宿主托管
/// （`crate::host::maa_bridge_ensure`），`Drop` 会让宿主把它收掉，不留孤儿。
pub struct Bridge {
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
    /// 连上宿主已经起好的桥，并完成握手：connect → 读 `ready` → 起读线程。
    ///
    /// **进程不在这里起**（宿主托管），因此也不能探活——桥只 accept 一次，谁先
    /// 连上谁就把它占走（D41）。就绪与否只能靠这次连接自己判断。
    ///
    /// `available:false`（未装 MAA、版本不兼容、依赖缺失）**不当作错误**——
    /// 那是有意义的运行态，由调用方决定怎么呈现。
    pub fn connect(port: u16) -> Result<Self, BridgeError> {
        // 桥只 accept 一次：这次连接失败，宿主托管的进程会在自己的等待上限内
        // 收场（退出码 4），下次 `ensure` 会把它收尸重起——插件不需要、也不能
        // 替宿主杀进程（M2a：进程只经 `maa.bridge` 能力管）。
        Self::connect_inner(port)
    }

    fn connect_inner(port: u16) -> Result<Self, BridgeError> {
        let addr: SocketAddr = format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| BridgeError::Start(format!("端口解析失败：{e}")))?;

        // 桥先 bind 再 accept：连接要重试到监听就绪。宿主 spawn 后立即返回，
        // 所以最初几百毫秒里连不上是正常的。
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let stream = loop {
            match TcpStream::connect_timeout(&addr, CONNECT_RETRY) {
                Ok(stream) => break stream,
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(BridgeError::Start(format!("连接桥失败（{addr}）：{e}")));
                    }
                    std::thread::sleep(CONNECT_RETRY);
                }
            }
        };
        let _ = stream.set_nodelay(true);

        // 端口可能在宿主挑好之后被抢走：真桥 bind 失败会退出，而这里仍可能连到
        // 抢占者的监听。先确认真桥还活着，再读 ready。
        ensure_host_running("连接后")?;

        // 握手：这次读要限时（避免桥卡在加载 MaaCore 时无限等）。
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let _ = stream.set_read_timeout(Some(remaining));
        let ready =
            read_frame(&stream).map_err(|e| BridgeError::Start(format!("等待桥握手失败：{e}")))?;
        if ready.get("event").and_then(Value::as_str) != Some("ready") {
            return Err(BridgeError::Start(format!("桥没有先发 ready：{ready}")));
        }
        // 抢占者可能伪造 ready；真桥若已因 bind 失败退出，这个 ready 不可信。
        ensure_host_running("握手后")?;
        // 读线程要长期阻塞等待，清掉握手用的读超时（socket 选项在克隆间共享）。
        let _ = stream.set_read_timeout(None);

        let bridge = Bridge {
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
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("测试监听端口");
        let addr = listener.local_addr().expect("测试监听地址");
        let stream = TcpStream::connect(addr).expect("测试连接");
        let (server, _) = listener.accept().expect("测试接受连接");
        drop(server);

        Self {
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

    /// 请求桥退出：发一帧 `shutdown`（让它把结果文件写完），然后断开。
    ///
    /// 进程不归这里管，所以这里没有 `kill`——收尸逻辑在 `host::internal::maa_bridge`：
    /// 桥收到 `shutdown` 会自己退，宿主在下次 `ensure`/`shutdown` 时收尾；
    /// 应用退出则由 Job Object 兜底，不会留孤儿。
    pub fn shutdown(self) {
        let _ = write_frame(&self.stream, &json!({ "cmd": "shutdown" }));
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // 连接没了就是「这次不用了」：桥会在自己的等待上限内收场，
        // 宿主负责收尸。这里不能直接杀进程（进程只经 `maa.bridge` 能力管）。
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
    use std::net::TcpListener;

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
