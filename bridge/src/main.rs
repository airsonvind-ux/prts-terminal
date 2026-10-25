//! PRTS-MAA 桥的入口。
//!
//! 用法：`prts-maa-bridge --maa-dir <MAA 安装目录> [--user-dir <工作目录>] [--port <端口>]`
//!
//! 协议走**本机 TCP**（`--port`）；未给则回退到 stdin/stdout。协议**不走** stdout，
//! 因为第三方 DLL（MaaFramework 控制单元）会往 stdout 打日志并冲乱帧流（见 `ipc` 模块）。
//! MaaCore 不可用时（未装 MAA / 版本不兼容 / 依赖缺失）**不退出**，
//! 而是以 `available:false` 报告原因，并继续响应 `status` / `shutdown`，
//! 让 PRTS 能展示状态并正常关闭该功能。

mod engine;
mod events;
mod ffi;
mod ipc;
#[cfg(windows)]
mod stdio;
mod win32;

use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use ipc::Sink;

fn main() -> ExitCode {
    // 声明进程 DPI 感知（Per-Monitor V2），与 MAA 的 WPF 前端一致。
    // 不声明时，Windows 在非 100% 缩放（如 125%）下会虚拟化本进程的坐标空间：
    // MaaCore 拿到的尺寸/坐标与真实帧会差一个缩放系数（实测 1600×900 vs 1280×720，正好 1.25），
    // 于是按 1600×900 放大的模板拿去匹配 1280×720 的帧，识别与点击全偏。
    // 必须在任何 DPI 相关调用之前执行；失败不致命（例如已被外部设定），仅记录。
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        // windows-rs 不同小版本对该函数返回值有 BOOL / Result 两种签名，用 `let _ =` 兼容。
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let args = Args::parse(std::env::args().skip(1));

    let user_dir = args
        .user_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("prts-maa-bridge"));
    let _ = std::fs::create_dir_all(&user_dir);

    // 协议通道：本机 TCP（`--port <n>`），与 stdout/stderr 彻底隔离——第三方 DLL
    // （MaaFramework 控制单元）会往 stdout 打日志，实测会插进帧流。未给 `--port`
    // 时回退到 stdin/stdout。
    let (sink, input): (Arc<Sink>, Box<dyn Read + Send>) = match args.port {
        Some(port) => {
            #[cfg(windows)]
            {
                // TCP 模式下 stdout/stderr 只剩日志用途：重定向到文件，既不干扰协议、也不会阻塞。
                let log = user_dir.join("debug").join("bridge-stdout.log");
                if let Some(parent) = log.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = stdio::redirect_std_to_log(&log);
            }
            let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("监听 127.0.0.1:{port} 失败：{e}");
                    return ExitCode::from(3);
                }
            };
            // 等待父进程连上，给一个上限。父进程若在 connect 之前就没了（强杀、崩溃），
            // accept 会一直等下去——那会留下一个没人管、也永远收不到 EOF 的孤儿桥。
            // 超时即退（退出码 4，与“监听失败”的 3 区分开）。
            let stream = match accept_with_timeout(&listener, CONNECT_TIMEOUT) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    eprintln!(
                        "等待连接超时（{} 秒）：父进程未连上，退出",
                        CONNECT_TIMEOUT.as_secs()
                    );
                    return ExitCode::from(4);
                }
                Err(e) => {
                    eprintln!("接受连接失败：{e}");
                    return ExitCode::from(3);
                }
            };
            let _ = stream.set_nodelay(true);
            let write_end = match stream.try_clone() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("克隆连接失败：{e}");
                    return ExitCode::from(3);
                }
            };
            (
                Arc::new(Sink::from_writer(Box::new(write_end))),
                Box::new(stream),
            )
        }
        None => (Arc::new(Sink::new()), Box::new(std::io::stdin())),
    };

    let mut version = String::new();
    let mut startup_error: Option<String> = None;
    let mut instance: Option<engine::Instance> = None;

    match args.maa_dir.as_deref() {
        None => startup_error = Some("缺少 --maa-dir（用户 MAA 安装目录）".into()),
        Some(dir) => match ffi::MaaCore::load(&dir.join("MaaCore.dll")) {
            Ok(core) => {
                version = core.version();
                if let Err(e) = std::fs::create_dir_all(&user_dir) {
                    startup_error = Some(format!("创建 user dir 失败：{e}"));
                } else {
                    match engine::init(core, &user_dir, dir) {
                        Ok(()) => {
                            let (ctx, rx) = events::channel();
                            // 有意泄漏：回调上下文必须比实例活得久（进程退出即回收）。
                            let ctx: &'static events::CallbackCtx = Box::leak(Box::new(ctx));
                            match engine::Instance::create(
                                core,
                                Some(events::on_msg),
                                ctx as *const events::CallbackCtx as *mut c_void,
                            ) {
                                Some(inst) => {
                                    events::spawn_forwarder(rx, sink.clone());
                                    instance = Some(inst);
                                }
                                None => startup_error = Some("AsstCreateEx 返回空句柄".into()),
                            }
                        }
                        Err(e) => startup_error = Some(e),
                    }
                }
            }
            Err(e) => startup_error = Some(e.to_string()),
        },
    }

    let ready = match &startup_error {
        Some(e) => json!({ "event": "ready", "available": false, "error": e }),
        None => json!({
            "event": "ready",
            "available": true,
            "version": version,
            "maa_dir": args.maa_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
            "user_dir": user_dir.to_string_lossy(),
        }),
    };
    let _ = sink.emit(&ready);

    // 命令循环独占 `instance` 的借用；结束后再 drop，触发 AsstDestroy。
    let code = {
        let mut session = Session {
            sink: &sink,
            instance: instance.as_ref(),
            out_dir: user_dir.clone(),
            input,
        };
        session.run()
    };
    drop(instance);
    code
}

/// 截图落点：固定的 `out_dir/screenshots/`，文件名由桥生成。
fn screenshot_path(out_dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    out_dir
        .join("screenshots")
        .join(format!("screenshot_{nanos}.png"))
}

/// 桥被连上之前愿意等多久。
///
/// 父进程正常退出时，桥会从连接上读到 EOF 自行退出；但父进程若在 connect
/// **之前**就没了，就永远没有 EOF 可读——accept 会挂到天荒地老。这个上限只
/// 覆盖那一段，connect 本身是毫秒级的，30 秒宽得离谱，不会误杀慢机器。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 等待一个连接，最长 `timeout`。`Ok(None)` 表示等到超时也没人来。
///
/// 先切非阻塞再短眠轮询（`TcpListener::accept` 本身没有超时参数）。返回的
/// socket 已恢复成阻塞模式：非阻塞标志会被 accept 出来的连接继承，而命令
/// 循环依赖阻塞读，否则读会立刻返回 `WouldBlock`。监听套接字本身也一并复位。
fn accept_with_timeout(
    listener: &std::net::TcpListener,
    timeout: std::time::Duration,
) -> std::io::Result<Option<std::net::TcpStream>> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + timeout;
    let result = loop {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let _ = stream.set_nonblocking(false);
                break Ok(Some(stream));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    break Ok(None);
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => break Err(e),
        }
    };
    let _ = listener.set_nonblocking(false);
    result
}

struct Session<'a> {
    sink: &'a Sink,
    instance: Option<&'a engine::Instance>,
    /// 截图等产物的默认输出目录（PRTS 的数据目录）。
    out_dir: PathBuf,
    /// 命令/帧的读端（命名管道或 stdin）。
    input: Box<dyn Read + Send>,
}

impl Session<'_> {
    fn run(&mut self) -> ExitCode {
        loop {
            match ipc::read_frame(&mut *self.input) {
                Ok(None) => break, // EOF：父进程关闭 → 退出
                Ok(Some(buf)) => {
                    let cmd: Value = match serde_json::from_slice(&buf) {
                        Ok(v) => v,
                        Err(e) => {
                            self.emit_error(Value::Null, format!("非法 JSON 帧：{e}"));
                            continue;
                        }
                    };
                    if !self.dispatch(&cmd) {
                        break;
                    }
                }
                Err(e) => {
                    self.emit_error(Value::Null, format!("读帧失败：{e}"));
                    break;
                }
            }
        }
        ExitCode::SUCCESS
    }

    fn reply(&self, id: &Value, ok: bool, data: Value) {
        let _ = self.sink.emit(&json!({
            "event": "result", "id": id, "ok": ok, "data": data
        }));
    }

    fn emit_error(&self, id: Value, msg: String) {
        let _ = self
            .sink
            .emit(&json!({ "event": "error", "id": id, "error": msg }));
    }

    /// 返回 `false` 表示请求退出。
    fn dispatch(&mut self, cmd: &Value) -> bool {
        let name = cmd.get("cmd").and_then(Value::as_str).unwrap_or("");
        let id = cmd.get("id").cloned().unwrap_or(Value::Null);
        match name {
            "status" => {
                let data = match self.instance {
                    Some(inst) => json!({
                        "available": true,
                        "connected": inst.connected(),
                        "running": inst.running(),
                    }),
                    None => json!({ "available": false }),
                };
                self.reply(&id, true, data);
            }
            "task_add" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => {
                    let type_ = cmd.get("type").and_then(Value::as_str).unwrap_or("");
                    let params = cmd.get("params").cloned().unwrap_or_else(|| json!({}));
                    if type_.is_empty() {
                        self.reply(&id, false, json!({ "error": "缺少 task type" }));
                    } else {
                        match inst.append_task(type_, &params.to_string()) {
                            Ok(tid) => self.reply(&id, true, json!({ "task_id": tid })),
                            Err(e) => self.reply(&id, false, json!({ "error": e })),
                        }
                    }
                }
            },
            "run_start" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => match inst.start() {
                    Ok(()) => self.reply(&id, true, json!({})),
                    Err(e) => self.reply(&id, false, json!({ "error": e })),
                },
            },
            "run_stop" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => match inst.stop() {
                    Ok(()) => self.reply(&id, true, json!({})),
                    Err(e) => self.reply(&id, false, json!({ "error": e })),
                },
            },
            "connect_adb" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => {
                    let adb = cmd.get("adb_path").and_then(Value::as_str).unwrap_or("adb");
                    let addr = cmd.get("address").and_then(Value::as_str).unwrap_or("");
                    if addr.is_empty() {
                        self.reply(&id, false, json!({ "error": "缺少 address" }));
                    } else {
                        let config = cmd.get("config").and_then(Value::as_str);
                        match inst.connect_adb(adb, addr, config) {
                            Ok(call_id) => {
                                self.reply(&id, true, json!({ "async_call_id": call_id }))
                            }
                            Err(e) => self.reply(&id, false, json!({ "error": e })),
                        }
                    }
                }
            },
            "set_connection_extras" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => {
                    let name = cmd.get("name").and_then(Value::as_str).unwrap_or("");
                    let extras = cmd.get("extras").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        self.reply(&id, false, json!({ "error": "缺少 name" }));
                    } else {
                        match inst.set_connection_extras(name, extras) {
                            Ok(()) => self.reply(&id, true, json!({})),
                            Err(e) => self.reply(&id, false, json!({ "error": e })),
                        }
                    }
                }
            },
            "task_set" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => {
                    let tid = cmd.get("task_id").and_then(Value::as_i64).unwrap_or(0) as i32;
                    let params = cmd.get("params").cloned().unwrap_or_else(|| json!({}));
                    match inst.set_task_params(tid, &params.to_string()) {
                        Ok(()) => self.reply(&id, true, json!({})),
                        Err(e) => self.reply(&id, false, json!({ "error": e })),
                    }
                }
            },
            "task_list" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => match inst.tasks_snapshot() {
                    Ok(list) => {
                        let arr: Vec<Value> = list
                            .into_iter()
                            .map(|(tid, ty, params)| {
                                json!({ "task_id": tid, "type": ty, "params": params })
                            })
                            .collect();
                        self.reply(&id, true, json!({ "tasks": arr }));
                    }
                    Err(e) => self.reply(&id, false, json!({ "error": e })),
                },
            },
            "uuid" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => match inst.uuid() {
                    Ok(u) => self.reply(&id, true, json!({ "uuid": u })),
                    Err(e) => self.reply(&id, false, json!({ "error": e })),
                },
            },
            "click" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => {
                    let x = cmd.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
                    let y = cmd.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
                    match inst.click(x, y) {
                        Ok(call_id) => self.reply(&id, true, json!({ "async_call_id": call_id })),
                        Err(e) => self.reply(&id, false, json!({ "error": e })),
                    }
                }
            },
            "back_to_home" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => match inst.back_to_home() {
                    Ok(()) => self.reply(&id, true, json!({})),
                    Err(e) => self.reply(&id, false, json!({ "error": e })),
                },
            },
            "set_instance_option" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => {
                    let key = cmd.get("key").and_then(Value::as_i64).unwrap_or(-1) as i32;
                    let value = cmd.get("value").and_then(Value::as_str).unwrap_or("");
                    match inst.set_instance_option(key, value) {
                        Ok(()) => self.reply(&id, true, json!({})),
                        Err(e) => self.reply(&id, false, json!({ "error": e })),
                    }
                }
            },
            "screenshot" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => match inst.screenshot() {
                    Ok(bytes) => {
                        // 截图落点由桥自己决定。客户端给的 path 不参与定位，
                        // 否则任何连上桥的进程都能借它覆盖任意文件。
                        let path = screenshot_path(&self.out_dir);
                        let result = path
                            .parent()
                            .map(std::fs::create_dir_all)
                            .unwrap_or(Ok(()))
                            .and_then(|()| std::fs::write(&path, &bytes));
                        match result {
                            Ok(()) => self.reply(
                                &id,
                                true,
                                json!({ "path": path.to_string_lossy(), "bytes": bytes.len() }),
                            ),
                            Err(e) => self.reply(
                                &id,
                                false,
                                json!({ "error": format!("写文件失败：{e}") }),
                            ),
                        }
                    }
                    Err(e) => self.reply(&id, false, json!({ "error": e })),
                },
            },
            "find_window" => {
                let needle = cmd.get("title").and_then(Value::as_str).unwrap_or("");
                if needle.is_empty() {
                    self.reply(&id, false, json!({ "error": "缺少 title" }));
                } else {
                    match win32::find_by_title(needle) {
                        Some(w) => self.reply(
                            &id,
                            true,
                            json!({ "hwnd": w.hwnd, "title": w.title, "class": w.class, "pid": w.pid }),
                        ),
                        None => self.reply(&id, false, json!({ "error": "未找到匹配窗口" })),
                    }
                }
            }
            "list_windows" => {
                let list: Vec<Value> = win32::list_windows()
                    .into_iter()
                    .map(|w| {
                        json!({
                            "hwnd": w.hwnd, "title": w.title, "class": w.class, "pid": w.pid
                        })
                    })
                    .collect();
                self.reply(&id, true, json!({ "windows": list }));
            }
            "window_info" => {
                // 诊断：读窗口外框/客户区/DPI。hwnd 或 title 二选一。
                let hwnd = cmd.get("hwnd").and_then(Value::as_i64).unwrap_or(0);
                let hwnd = if hwnd != 0 {
                    hwnd
                } else {
                    let needle = cmd.get("title").and_then(Value::as_str).unwrap_or("");
                    match win32::find_by_title(needle) {
                        Some(w) => w.hwnd,
                        None => 0,
                    }
                };
                if hwnd == 0 {
                    self.reply(&id, false, json!({ "error": "未找到窗口" }));
                } else {
                    match win32::window_info(hwnd) {
                        Some(i) => self.reply(
                            &id,
                            true,
                            json!({
                                "hwnd": i.hwnd,
                                "window_rect": i.window_rect,
                                "client_size": i.client_size,
                                "dpi": i.dpi,
                            }),
                        ),
                        None => self.reply(&id, false, json!({ "error": "读取窗口信息失败" })),
                    }
                }
            }
            #[cfg(windows)]
            "connect_win32" => match self.instance {
                None => self.reply(&id, false, json!({ "error": "MaaCore 不可用" })),
                Some(inst) => {
                    let hwnd = cmd.get("hwnd").and_then(Value::as_i64).unwrap_or(0);
                    if hwnd == 0 {
                        self.reply(&id, false, json!({ "error": "缺少 hwnd" }));
                    } else {
                        // 默认取 MAA WPF GUI 的默认组合：
                        // 截图 PrintWindow(16) / 鼠标 PostMessageWithWindowPos(256) / 键盘 SendMessage(2)。
                        // 默认取一个截图方式**掩码**：控制单元会对掩码里启用的方式做速度
                        // 测试、自己挑一个能用的（单一方式很容易“没一个可用”而 attach 失败）。
                        // GDI|FramePool|DXGI_DesktopDup_Window|PrintWindow = 1|2|8|16。
                        // 不含 DXGI_DesktopDup(4，整个桌面) 与 ScreenDC(32，需前台)。
                        let screencap = cmd.get("screencap").and_then(Value::as_u64).unwrap_or(27);
                        let mouse = cmd.get("mouse").and_then(Value::as_u64).unwrap_or(256);
                        let keyboard = cmd.get("keyboard").and_then(Value::as_u64).unwrap_or(2);
                        match inst.attach_window(hwnd, screencap, mouse, keyboard) {
                            Ok(call_id) => {
                                self.reply(&id, true, json!({ "async_call_id": call_id }))
                            }
                            Err(e) => self.reply(&id, false, json!({ "error": e })),
                        }
                    }
                }
            },
            "shutdown" => {
                self.reply(&id, true, json!({}));
                return false;
            }
            other => self.reply(&id, false, json!({ "error": format!("未知命令：{other}") })),
        }
        true
    }
}

struct Args {
    maa_dir: Option<PathBuf>,
    user_dir: Option<PathBuf>,
    /// 本机 TCP 端口（`--port`）。协议走它；不给则回退 stdin/stdout。
    port: Option<u16>,
}
impl Args {
    fn parse<I: Iterator<Item = String>>(mut it: I) -> Self {
        let mut args = Args {
            maa_dir: None,
            user_dir: None,
            port: None,
        };
        while let Some(a) = it.next() {
            match a.as_str() {
                "--maa-dir" => args.maa_dir = it.next().map(PathBuf::from),
                "--user-dir" => args.user_dir = it.next().map(PathBuf::from),
                "--port" => args.port = it.next().and_then(|s| s.parse().ok()),
                _ => {}
            }
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn screenshot_path_is_generated_under_out_dir() {
        let out = std::env::temp_dir().join("prts-bridge-shot-test");
        let path = screenshot_path(&out);
        assert!(path.starts_with(out.join("screenshots")));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
    }
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    fn listener_and_addr() -> (TcpListener, std::net::SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        (listener, addr)
    }

    #[test]
    fn accept_times_out_when_nobody_connects() {
        let (listener, _) = listener_and_addr();
        let start = Instant::now();
        let got = accept_with_timeout(&listener, Duration::from_millis(150)).expect("accept");
        assert!(got.is_none(), "没人连上时应超时返回 None");
        assert!(start.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn accept_returns_connection_when_someone_connects() {
        let (listener, addr) = listener_and_addr();
        let handle = std::thread::spawn(move || TcpStream::connect(addr).expect("connect"));
        let got = accept_with_timeout(&listener, Duration::from_secs(5)).expect("accept");
        assert!(got.is_some(), "有人连上时应拿到连接");
        let _ = handle.join();
    }

    #[test]
    fn accepted_stream_is_blocking() {
        // 非阻塞标志会被 accept 出来的连接继承，而命令循环依赖阻塞读：
        // 拿到非阻塞连接的话，read_frame 会立刻返回 WouldBlock，桥刚连上就死。
        // std 没提供 is_nonblocking，只能验行为：对端不发数据时，读应当阻塞住。
        let (listener, addr) = listener_and_addr();
        let handle = std::thread::spawn(move || TcpStream::connect(addr).expect("connect"));
        let mut stream = accept_with_timeout(&listener, Duration::from_secs(5))
            .expect("accept")
            .expect("some");

        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 1];
            let _ = tx.send(stream.read(&mut buf).is_ok());
        });
        match rx.recv_timeout(Duration::from_millis(200)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {} // 阻塞住了，正是要的
            other => panic!("读不该立刻返回，实际 {other:?}"),
        }
        drop(handle.join()); // 关掉对端，阻塞的读随之返回
        let _ = reader.join();
    }
}
