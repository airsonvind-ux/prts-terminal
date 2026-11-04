//! process 插件的生命周期与 RPC 通道（M2b）。
//!
//! 一条子进程、一条 stdin/stdout 管道、一份分帧（[`super::transport`]）、一份
//! 请求槽位表（[`super::rpc`]）。读线程独占 stdout，**每读到一帧立即按类型分派**
//! （R3）：应答填槽位、`host.call` 交给工作线程、`log` 直接进 stderr。
//! 任何路径都不得在读线程里阻塞等待另一个请求——插件的 `host.call`
//! （`maa.bridge.ensure` 最长 30s）与宿主的 `tool.call` 必须能同时在路上。

use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::manifest::{PluginEntry, SUPPORTED_PROTOCOL};
use super::registry::PluginRegistry;
use super::rpc::{self, Pending, RpcError, RpcResult};
use super::transport::{write_frame, FrameReader};
use super::PluginError;
use crate::host::{HostError, PluginCtx};
use crate::tools::{
    Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolView, ToolViewKind,
    MAX_VIEW_HTML_BYTES, MAX_VIEW_TITLE_CHARS, VIEW_HEIGHT_MAX, VIEW_HEIGHT_MIN,
};

/// 握手超时（规格 §5）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 调用超时后等插件自己对 `cancel` 的宽限（规格 §5）。
const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// 工具声明的超时缺省值（规格 §5）。
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);
/// 工具名契约（D5 / 规格 §5）。
const MAX_TOOL_NAME_LEN: usize = 64;

/// 启动中的进程插件。一个插件一条连接，连接没了就不再复用。
pub struct ProcessPlugin {
    id: String,
    ctx: PluginCtx,
    stdin: Mutex<ChildStdin>,
    pending: Arc<Pending>,
    child: Mutex<Child>,
    alive: Arc<AtomicBool>,
    /// 正常收场（`shutdown` / 安装失败清理）时置位：此时的进程退出不算故障。
    stopping: AtomicBool,
    ledger: Arc<Mutex<PluginRegistry>>,
}

/// 握手结果：工具面**只此一处来源**（规格 §5）。
#[derive(Debug)]
pub struct Handshake {
    /// 插件自报的能力，只作诊断，不参与 `granted`（N8）。
    pub capabilities: Vec<String>,
    pub tools: Vec<PluginToolSpec>,
}

/// 插件声明的一个工具（不带宿主加的插件前缀）。
#[derive(Debug)]
pub struct PluginToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub timeout: Duration,
    /// 调用文案里的动词短语；`None` = 未提供，走前端保底。
    pub action: Option<String>,
}

impl ProcessPlugin {
    /// 拉起进程并起读线程。握手要单独调 [`ProcessPlugin::initialize`]。
    pub fn spawn(
        id: &str,
        plugin_dir: &Path,
        entry: &PluginEntry,
        ctx: PluginCtx,
        ledger: Arc<Mutex<PluginRegistry>>,
    ) -> Result<Arc<Self>, PluginError> {
        if let Some(transport) = entry.transport.as_deref() {
            if transport != "stdio" {
                return Err(PluginError::Process {
                    id: id.to_string(),
                    detail: format!("不支持的 transport：{transport}（M2 只实现 stdio）"),
                });
            }
        }

        // 入口的两种形态（M4 §3.2），判据在 `plugins::resolve_program`：
        //  1. 插件目录里有这个文件（含 `hello.exe` 这种没有分隔符的名字）→ 用它；
        //  2. 目录里没有、且是不含分隔符的裸名 → 按 PATH 解析（解释型语言靠它，
        //     例如 {"exe": "python", "args": ["hello.py"]}）；
        //  3. 其余一律报「入口不存在」。
        // 与 `plugin_check` 共用同一份实现：校验器说「能起」的东西，宿主必须能起。
        let program = match super::resolve_program(plugin_dir, entry) {
            Ok(super::ProgramResolution::InDir(path)) => path,
            Ok(super::ProgramResolution::OnPath(name)) => PathBuf::from(name),
            Err(detail) => {
                return Err(PluginError::Process {
                    id: id.to_string(),
                    detail,
                })
            }
        };

        let mut command = Command::new(&program);
        command
            .args(&entry.args)
            .current_dir(plugin_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // 与 MAA 桥一致：不弹控制台窗口。
            command.creation_flags(0x0800_0000);
        }

        let mut child = command.spawn().map_err(|error| PluginError::Process {
            id: id.to_string(),
            detail: format!("起进程失败（{}）：{error}", program.display()),
        })?;

        let stdin = child.stdin.take().expect("stdin 已设为 piped");
        let stdout = child.stdout.take().expect("stdout 已设为 piped");
        let stderr = child.stderr.take().expect("stderr 已设为 piped");

        let plugin = Arc::new(ProcessPlugin {
            id: id.to_string(),
            ctx,
            stdin: Mutex::new(stdin),
            pending: Arc::new(Pending::new()),
            child: Mutex::new(child),
            alive: Arc::new(AtomicBool::new(true)),
            stopping: AtomicBool::new(false),
            ledger,
        });

        let reader = Arc::clone(&plugin);
        std::thread::Builder::new()
            .name(format!("plugin-{id}-stdout"))
            .spawn(move || reader.run_reader(stdout))
            .map_err(|error| PluginError::Process {
                id: id.to_string(),
                detail: format!("起读线程失败：{error}"),
            })?;

        let logger = Arc::clone(&plugin);
        std::thread::Builder::new()
            .name(format!("plugin-{id}-stderr"))
            .spawn(move || logger.run_stderr(stderr))
            .map_err(|error| PluginError::Process {
                id: id.to_string(),
                detail: format!("起 stderr 线程失败：{error}"),
            })?;

        remember(Arc::clone(&plugin));
        Ok(plugin)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// 握手：`initialize` → 校验协议版本 → 取工具面。
    pub fn initialize(&self) -> Result<Handshake, PluginError> {
        self.initialize_with_deadline(HANDSHAKE_TIMEOUT)
    }

    /// 同 [`ProcessPlugin::initialize`]，超时可调（测试用短超时，不装 10 秒的慢用例）。
    pub fn initialize_with_deadline(&self, deadline: Duration) -> Result<Handshake, PluginError> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocol": SUPPORTED_PROTOCOL,
                    "hostVersion": env!("CARGO_PKG_VERSION"),
                    "pluginId": self.id,
                }),
                deadline,
            )
            .map_err(|error| PluginError::Process {
                id: self.id.clone(),
                detail: format!("握手失败：{error}"),
            })?;

        let protocol = result["protocol"].as_u64().unwrap_or(0);
        if protocol != u64::from(SUPPORTED_PROTOCOL) {
            return Err(PluginError::Process {
                id: self.id.clone(),
                detail: format!(
                    "协议版本不匹配：插件说 {protocol}，本宿主支持 {SUPPORTED_PROTOCOL}"
                ),
            });
        }

        let capabilities = result["capabilities"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let tools = parse_tools(&self.id, &result["tools"])?;
        Ok(Handshake {
            capabilities,
            tools,
        })
    }

    /// 调一次插件的工具方法。
    pub fn call_tool(&self, method: &str, args: Value, timeout: Duration) -> RpcResult {
        let deadline_ms = timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        self.request(
            "tool.call",
            json!({ "name": method, "args": args, "deadlineMs": deadline_ms }),
            timeout,
        )
    }

    /// 让插件自己收场；不听话就杀掉。
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.request("shutdown", json!({}), Duration::from_secs(2));
        self.kill();
    }

    /// 是否还活着（进程未退出且通道未断）。
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// 发一个请求并等应答。
    fn request(&self, method: &str, params: Value, timeout: Duration) -> RpcResult {
        if !self.is_alive() {
            return Err(RpcError::new(
                rpc::PLUGIN_FAULT,
                format!("插件 {} 已经退出，无法调用 {method}", self.id),
            ));
        }

        let (id, receiver) = self.pending.register();
        let frame = json!({ "id": id, "method": method, "params": params });
        if let Err(error) = self.send(&frame) {
            // 槽位已经领了：摘掉它，别让它在表里留到进程结束。
            let _ = self.pending.resolve(id, Err(error.clone()));
            return Err(error);
        }

        match receiver.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(RecvTimeoutError::Timeout) => {
                let error = RpcError::new(
                    rpc::TIMEOUT,
                    format!(
                        "插件 {} 的 {method} 在 {} 秒内没有应答",
                        self.id,
                        timeout.as_secs_f32()
                    ),
                );
                // 工具调用超时先给插件一次收场机会（规格 §5）：发 cancel 通知，
                // 等满宽限期还没回话才判故障、杀进程。握手与 shutdown 没有中途
                // 取消的语义，直接算超时。
                if method == "tool.call" {
                    let cancel = json!({ "method": "cancel", "params": { "id": id } });
                    let _ = self.send(&cancel);
                    if let Ok(outcome) = receiver.recv_timeout(CANCEL_GRACE) {
                        let _ = self.pending.resolve(id, outcome.clone());
                        return outcome;
                    }
                    self.mark_dead(&format!(
                        "{method} 超时且 cancel 在 {} 秒内没有回音",
                        CANCEL_GRACE.as_secs()
                    ));
                }
                // 摘槽位（接收端已经走了，这里只是清理）。
                let _ = self.pending.resolve(id, Err(error.clone()));
                Err(error)
            }
            Err(RecvTimeoutError::Disconnected) => Err(RpcError::new(
                rpc::PLUGIN_FAULT,
                format!("插件 {} 的调用通道已断开", self.id),
            )),
        }
    }

    /// 写一帧到插件 stdin。
    fn send(&self, frame: &Value) -> Result<(), RpcError> {
        let mut stdin = self
            .stdin
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        write_frame(&mut *stdin, frame).map_err(|error| {
            RpcError::new(
                rpc::PLUGIN_FAULT,
                format!("与插件 {} 的通道写入失败：{error}", self.id),
            )
        })
    }

    /// 读线程：独占 stdout，按帧类型立即分派（R3）。
    fn run_reader(self: Arc<Self>, stdout: ChildStdout) {
        let mut reader = FrameReader::new(BufReader::new(stdout));
        loop {
            match reader.read_frame() {
                Ok(Some(frame)) => self.dispatch(frame),
                Ok(None) => {
                    self.mark_dead("插件关闭了 stdout");
                    return;
                }
                Err(error) => {
                    self.mark_dead(&error.to_string());
                    return;
                }
            }
        }
    }

    /// stderr 归宿主日志：逐行加前缀转写，不让它污染协议通道。
    fn run_stderr(self: Arc<Self>, stderr: std::process::ChildStderr) {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines() {
            match line {
                Ok(text) => {
                    if !text.trim().is_empty() {
                        eprintln!("[plugin {}] {text}", self.id);
                    }
                }
                Err(_) => return,
            }
        }
    }

    /// 一帧入站：应答 → 槽位；`host.call` → 工作线程；`log` → stderr。
    fn dispatch(self: &Arc<Self>, frame: Value) {
        let id = frame["id"].as_u64();
        let method = frame["method"].as_str();

        match (method, id) {
            // 应答：有 id、没有 method。
            (None, Some(id)) => {
                let outcome = if let Some(error) = frame.get("error") {
                    Err(RpcError::from_json(error))
                } else {
                    Ok(frame.get("result").cloned().unwrap_or(Value::Null))
                };
                self.pending.resolve(id, outcome);
            }
            (Some("host.call"), Some(id)) => {
                self.spawn_host_call(id, frame["params"].clone());
            }
            (Some("log"), _) => {
                let level = frame["params"]["level"].as_str().unwrap_or("info");
                let text = frame["params"]["text"].as_str().unwrap_or("");
                if !text.trim().is_empty() {
                    eprintln!("[plugin {}] {level}: {text}", self.id);
                }
            }
            // 宿主要求取消的通知：插件自己处理，宿主侧没有等待者。
            (Some("cancel"), _) => {}
            (Some(other), Some(id)) => {
                let reply = json!({
                    "id": id,
                    "error": RpcError::new(
                        rpc::METHOD_NOT_FOUND,
                        format!("宿主不认识方法 {other}"),
                    ).to_json(),
                });
                let _ = self.send(&reply);
            }
            (Some(other), None) => {
                eprintln!("[plugin {}] 收到没有 id 的请求帧：{other}", self.id);
            }
            (None, None) => {
                eprintln!("[plugin {}] 收到既没有 method 也没有 id 的帧", self.id);
            }
        }
    }

    /// 插件发起的 `host.call`：**不能在读线程里执行**（会与 tool.call 互等），
    /// 交一个短命工作线程，执行完把结果写回管道。
    fn spawn_host_call(self: &Arc<Self>, id: u64, params: Value) {
        let plugin = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("plugin-{}-host-call", self.id))
            .spawn(move || {
                let key = params["key"].as_str().unwrap_or_default();
                let args = params.get("args").cloned().unwrap_or(Value::Null);
                if key.is_empty() {
                    let reply = json!({
                        "id": id,
                        "error": RpcError::new(
                            rpc::INVALID_PARAMS,
                            "host.call 缺少 key",
                        ).to_json(),
                    });
                    let _ = plugin.send(&reply);
                    return;
                }

                let reply = match plugin.ctx.call(key, args) {
                    Ok(value) => json!({ "id": id, "result": value }),
                    Err(error) => json!({ "id": id, "error": host_error_to_rpc(&error).to_json() }),
                };
                let _ = plugin.send(&reply);
            })
            .ok();
    }

    /// 通道断了：标记死亡、叫醒所有等待者、收掉进程；不是正常收场就记故障。
    fn mark_dead(&self, reason: &str) {
        let first = self.alive.swap(false, Ordering::SeqCst);
        if first {
            eprintln!("[plugins] {} 通道失联：{reason}", self.id);
        }
        self.pending.fail_all(&RpcError::new(
            rpc::PLUGIN_FAULT,
            format!("插件 {} 已退出：{reason}", self.id),
        ));
        self.kill();
        if first && !self.stopping.load(Ordering::SeqCst) {
            let mut ledger = self
                .ledger
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            ledger.mark_faulted(&self.id, reason.to_string());
        }
    }

    /// 杀进程并等它退出（幂等）。
    fn kill(&self) {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for ProcessPlugin {
    fn drop(&mut self) {
        let _ = self.child.lock().map(|mut child| {
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

/// 一个 process 插件的生命周期句柄（D21：崩溃后不自动重启，下次调用试一次）。
///
/// 工具持有的是这个句柄而不是进程本身：进程没了，下一次调用照 D21 拉起一次；
/// 拉起也失败就把原因记进台账，之后的调用直接返回同一条原因，直到用户重启 PRTS。
pub struct Launcher {
    id: String,
    plugin_dir: std::path::PathBuf,
    entry: PluginEntry,
    ctx: PluginCtx,
    ledger: Arc<Mutex<PluginRegistry>>,
    current: Mutex<Option<Arc<ProcessPlugin>>>,
    /// 插件声明过的工具名（插件自己的名字）。重启后必须还是同一套工具。
    declared: Mutex<Vec<String>>,
    /// 重启失败过：不再重试（D21「仍失败则标记故障直到用户手动重启」）。
    restart_failed: Mutex<Option<String>>,
}

impl Launcher {
    pub fn new(
        id: &str,
        plugin_dir: &Path,
        entry: &PluginEntry,
        ctx: PluginCtx,
        ledger: Arc<Mutex<PluginRegistry>>,
    ) -> Arc<Self> {
        Arc::new(Launcher {
            id: id.to_string(),
            plugin_dir: plugin_dir.to_path_buf(),
            entry: entry.clone(),
            ctx,
            ledger,
            current: Mutex::new(None),
            declared: Mutex::new(Vec::new()),
            restart_failed: Mutex::new(None),
        })
    }

    /// 拿一个可用进程：活着就复用，死了按 D21 拉起一次。
    pub fn acquire(&self) -> Result<Arc<ProcessPlugin>, String> {
        let mut slot = self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(plugin) = slot.as_ref() {
            if plugin.is_alive() {
                return Ok(Arc::clone(plugin));
            }
        }

        if let Some(reason) = self
            .restart_failed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
        {
            return Err(format!(
                "插件 {} 已故障：{reason}（重启 PRTS 后恢复）",
                self.id
            ));
        }

        // 旧进程已经由读线程/超时路径收掉了，这里只需要一次干净的拉起。
        *slot = None;
        match self.launch() {
            Ok((plugin, _handshake)) => {
                *slot = Some(Arc::clone(&plugin));
                if let Ok(mut ledger) = self.ledger.lock() {
                    ledger.mark_loaded(&self.id);
                }
                Ok(plugin)
            }
            Err(error) => {
                eprintln!("[plugins] {} 重新拉起失败：{error}", self.id);
                if let Ok(mut ledger) = self.ledger.lock() {
                    ledger.mark_faulted(&self.id, format!("重新拉起失败：{error}"));
                }
                *self
                    .restart_failed
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(error.clone());
                Err(error)
            }
        }
    }

    /// 停用/卸载时收掉进程，并清掉「重启失败」的记忆。
    #[allow(dead_code)] // 提交 5 的 command 层接线后删除
    fn stop(&self) {
        let plugin = self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(plugin) = plugin {
            plugin.shutdown();
        }
        self.clear_failure();
    }

    #[allow(dead_code)] // 提交 5 的 command 层接线后删除
    fn clear_failure(&self) {
        *self
            .restart_failed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }

    /// 拉起并握手。重启时还会核对工具面（`declared` 非空时）。
    fn launch(&self) -> Result<(Arc<ProcessPlugin>, Handshake), String> {
        let plugin = ProcessPlugin::spawn(
            &self.id,
            &self.plugin_dir,
            &self.entry,
            self.ctx.clone(),
            Arc::clone(&self.ledger),
        )
        .map_err(|error| error.to_string())?;

        let handshake = match plugin.initialize() {
            Ok(handshake) => handshake,
            Err(error) => {
                plugin.kill_and_forget();
                return Err(error.to_string());
            }
        };

        // 重启后的工具面必须和注册时一致，否则注册表里的名字会指向不存在的工具。
        let declared = self
            .declared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if !declared.is_empty() {
            let mut names: Vec<&str> = handshake
                .tools
                .iter()
                .map(|spec| spec.name.as_str())
                .collect();
            names.sort_unstable();
            let mut expected: Vec<&str> = declared.iter().map(String::as_str).collect();
            expected.sort_unstable();
            if names != expected {
                plugin.kill_and_forget();
                return Err(format!(
                    "重启后声明的工具变了（注册时 {expected:?}，现在 {names:?}）"
                ));
            }
        }

        Ok((plugin, handshake))
    }

    /// 安装成功后把进程与工具面记下来。
    fn adopt(&self, plugin: Arc<ProcessPlugin>, declared: &[String]) {
        *self
            .declared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = declared.to_vec();
        *self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(plugin);
    }
}

/// 宿主把插件工具挂进注册表的适配器。
///
/// 模型看到的是带前缀的名字（`probe_echo`），管道上发的是插件自己的名字
/// （`echo`）——前缀是宿主的命名空间，插件不需要知道。
pub struct PluginTool {
    launcher: Arc<Launcher>,
    name: String,
    method: String,
    description: String,
    parameters: Value,
    timeout: Duration,
    action: Option<String>,
}

impl PluginTool {
    pub fn new(launcher: Arc<Launcher>, prefix: &str, spec: PluginToolSpec) -> Self {
        let name = format!("{prefix}_{}", spec.name);
        Self {
            launcher,
            name,
            method: spec.name,
            description: spec.description,
            parameters: spec.parameters,
            timeout: spec.timeout,
            action: spec.action,
        }
    }
}

impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    fn timeout(&self) -> Option<Duration> {
        Some(self.timeout)
    }

    fn action(&self) -> Option<&str> {
        self.action.as_deref()
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let args: Value = if params.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(params).map_err(|error| ToolError::Parse(error.to_string()))?
        };

        let plugin = self.launcher.acquire().map_err(ToolError::Execution)?;
        let result = plugin
            .call_tool(&self.method, args, self.timeout)
            .map_err(|error| {
                ToolError::Execution(format!(
                    "插件 {} 的 {} 调用失败：{error}",
                    self.launcher.id, self.method
                ))
            })?;

        render_output(result, &self.launcher.plugin_dir)
    }
}

/// 插件返回结果的契约问题（视图接口规格 §二）。
///
/// 独立成一类而不是直接拼字符串，是为了让 `plugin_check --call` 也能复用同一套
/// 判据与同一份文案——校验器和宿主必须对同一个返回值下同一个结论。
#[derive(Debug, Clone, PartialEq)]
pub enum ViewIssue {
    /// 对象形态却没有 `content`。
    MissingContent,
    /// `content` 是空串或只有空白。
    EmptyContent,
    /// `result` 既不是字符串也不是对象。
    NotAnObject,
    /// `kind` 不是 html / image / doc。
    UnknownKind(String),
    /// `view` 不是对象。
    ViewNotObject,
    /// 缺 `title` 或为空。
    MissingTitle,
    /// `title` 超过 60 字。
    TitleTooLong { chars: usize },
    /// html 视图缺 `html` 字段。
    MissingHtml,
    /// image/doc 缺 `path`。
    MissingPath,
    /// image/doc 的 `path` 越出插件目录。
    PathEscapes { path: String },
    /// image/doc 的 `path` 在插件目录里不存在。
    PathMissing { path: String },
    /// doc 缺 `mime`。
    MissingMime,
}

impl std::fmt::Display for ViewIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingContent => f.write_str(
                "对象结果缺少 content（字符串以外必须给 {content, view?}，content 不能省）",
            ),
            Self::EmptyContent => f.write_str("content 是空的：模型只看 content，视图不能代替文字"),
            Self::NotAnObject => {
                f.write_str("结果既不是字符串也不是对象（只支持这两种形态）")
            }
            Self::UnknownKind(kind) => {
                write!(f, "view.kind 只能是 html / image / doc，收到「{kind}」")
            }
            Self::ViewNotObject => f.write_str("view 必须是对象"),
            Self::MissingTitle => f.write_str("view.title 必填，且不能为空"),
            Self::TitleTooLong { chars } => {
                write!(f, "view.title 超过 {MAX_VIEW_TITLE_CHARS} 字（现在 {chars} 字）")
            }
            Self::MissingHtml => f.write_str("html 视图缺少 html 字段"),
            Self::MissingPath => f.write_str("image / doc 视图缺少 path"),
            Self::PathEscapes { path } => {
                write!(f, "view.path 越出插件目录：{path}")
            }
            Self::PathMissing { path } => write!(f, "view.path 指向的文件不存在：{path}"),
            Self::MissingMime => f.write_str("doc 视图缺少 mime"),
        }
    }
}

impl std::error::Error for ViewIssue {}

/// 一份合法结果拆出来的两部分：进上下文的文字、进画面的视图。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedResult {
    pub content: String,
    /// `None` = 没有视图，或 html 超限被降级。
    pub view: Option<ToolView>,
    /// html 超限降级：`content` 末尾已追加说明。
    pub degraded: bool,
}

/// 插件返回值 → 结构化输出。
///
/// 契约（视图接口规格 §二）：字符串等价于 `{content}`；对象必须给非空
/// `content`，`view` 可选。对象缺 `content` **判错而不是回落到 JSON**——
/// 否则整个对象会被 pretty 打印进上下文，`view` 就从后门进了模型视野。
pub fn render_output(value: Value, plugin_dir: &Path) -> Result<ToolOutput, ToolError> {
    let parsed = parse_result(&value, plugin_dir)
        .map_err(|issue| ToolError::Execution(format!("插件返回的结果不合法（-32602）：{issue}")))?;
    let mut output = ToolOutput::json(value, parsed.content);
    output.view = parsed.view;
    Ok(output)
}

/// 校验并拆解插件返回值（`plugin_check --call` 与宿主共用这一份判据）。
pub fn parse_result(value: &Value, plugin_dir: &Path) -> Result<ParsedResult, ViewIssue> {
    match value {
        Value::String(text) => Ok(ParsedResult {
            content: text.clone(),
            view: None,
            degraded: false,
        }),
        Value::Object(map) => {
            let content = match map.get("content") {
                Some(Value::String(text)) if !text.trim().is_empty() => text.clone(),
                Some(Value::String(_)) => return Err(ViewIssue::EmptyContent),
                _ => return Err(ViewIssue::MissingContent),
            };
            let view = match map.get("view") {
                None | Some(Value::Null) => None,
                Some(view) => match parse_view(view, &content, plugin_dir)? {
                    ParsedView::Keep(view) => Some(view),
                    ParsedView::Degraded(degraded) => {
                        return Ok(ParsedResult {
                            content: degraded,
                            view: None,
                            degraded: true,
                        })
                    }
                },
            };
            Ok(ParsedResult {
                content,
                view,
                degraded: false,
            })
        }
        _ => Err(ViewIssue::NotAnObject),
    }
}

/// 视图解析的两种归宿：照用、超限降级（丢弃视图，`content` 追加说明）。
enum ParsedView {
    Keep(ToolView),
    Degraded(String),
}

fn parse_view(view: &Value, content: &str, plugin_dir: &Path) -> Result<ParsedView, ViewIssue> {
    let object = view.as_object().ok_or(ViewIssue::ViewNotObject)?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or(ViewIssue::MissingTitle)?
        .to_string();
    let chars = title.chars().count();
    if chars > MAX_VIEW_TITLE_CHARS {
        return Err(ViewIssue::TitleTooLong { chars });
    }
    let height = object
        .get("height")
        .and_then(Value::as_f64)
        .map(|raw| raw.round().clamp(VIEW_HEIGHT_MIN as f64, VIEW_HEIGHT_MAX as f64) as u16);

    match kind {
        "html" => {
            let html = object
                .get("html")
                .and_then(Value::as_str)
                .ok_or(ViewIssue::MissingHtml)?;
            if html.len() > MAX_VIEW_HTML_BYTES {
                let mut degraded = content.to_string();
                degraded.push_str("\n\n视图内容超过上限，只保留文字");
                return Ok(ParsedView::Degraded(degraded));
            }
            Ok(ParsedView::Keep(ToolView {
                kind: ToolViewKind::Html,
                title,
                height,
                html: Some(html.to_string()),
                source: None,
                mime: None,
            }))
        }
        "image" => {
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .ok_or(ViewIssue::MissingPath)?;
            let source = resolve_inside(plugin_dir, path)?;
            Ok(ParsedView::Keep(ToolView {
                kind: ToolViewKind::Image,
                title,
                height,
                html: None,
                source: Some(source),
                mime: None,
            }))
        }
        "doc" => {
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .ok_or(ViewIssue::MissingPath)?;
            let mime = object
                .get("mime")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|mime| !mime.is_empty())
                .ok_or(ViewIssue::MissingMime)?
                .to_string();
            let source = resolve_inside(plugin_dir, path)?;
            Ok(ParsedView::Keep(ToolView {
                kind: ToolViewKind::Doc,
                title,
                height,
                html: None,
                source: Some(source),
                mime: Some(mime),
            }))
        }
        other => Err(ViewIssue::UnknownKind(other.to_string())),
    }
}

/// 把 view 里的相对路径解析成插件目录内的绝对路径（canonicalize 后比对前缀，
/// 与 `host::internal::fs::ensure_within` 同一套判据）。
fn resolve_inside(plugin_dir: &Path, relative: &str) -> Result<PathBuf, ViewIssue> {
    // 先做词法判断：绝对路径、`..`、盘符一律视为越界——即使目标恰好不存在，
    // 也该报「越界」而不是「不存在」，否则插件收到的报错会指错方向。
    let rel = Path::new(relative);
    if rel.is_absolute()
        || rel
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir | std::path::Component::Prefix(_)))
    {
        return Err(ViewIssue::PathEscapes {
            path: relative.to_string(),
        });
    }
    let candidate = plugin_dir.join(rel);
    let root = plugin_dir
        .canonicalize()
        .map_err(|_| ViewIssue::PathMissing {
            path: relative.to_string(),
        })?;
    let resolved = candidate.canonicalize().map_err(|_| ViewIssue::PathMissing {
        path: relative.to_string(),
    })?;
    if !resolved.starts_with(&root) {
        return Err(ViewIssue::PathEscapes {
            path: relative.to_string(),
        });
    }
    Ok(resolved)
}

/// 安装一个 process 插件：拉起 → 握手 → 按前缀注册工具。
///
/// 任何一步失败都要先收掉进程再返回——失败的插件不该留一个孤儿进程。
pub fn install(
    id: &str,
    plugin_dir: &Path,
    entry: &PluginEntry,
    ctx: PluginCtx,
    ledger: Arc<Mutex<PluginRegistry>>,
    reg: &ToolRegistry,
) -> Result<(Vec<String>, Vec<String>), PluginError> {
    let launcher = Launcher::new(id, plugin_dir, entry, ctx, ledger);
    let (plugin, handshake) = launcher.launch().map_err(|detail| PluginError::Process {
        id: id.to_string(),
        detail,
    })?;

    let prefix = id.replace('-', "_");
    let declared: Vec<String> = handshake.tools.iter().map(|spec| spec.name.clone()).collect();
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    for spec in handshake.tools {
        let full = format!("{prefix}_{}", spec.name);
        if !is_valid_tool_name(&full) {
            plugin.kill_and_forget();
            return Err(PluginError::Invalid {
                field: "tools.name",
                value: format!("{full}（工具名必须匹配 ^[a-z][a-z0-9_]{{0,63}}$）"),
            });
        }
        tools.push(Box::new(PluginTool::new(
            Arc::clone(&launcher),
            &prefix,
            spec,
        )));
    }

    match super::register_plugin_tools(reg, id, tools) {
        Ok(names) => {
            launcher.adopt(Arc::clone(&plugin), &declared);
            remember_launcher(id, Arc::clone(&launcher));
            Ok((names, handshake.capabilities))
        }
        Err(error) => {
            plugin.kill_and_forget();
            Err(error)
        }
    }
}

impl ProcessPlugin {
    /// 安装失败时收掉进程，并把它从「运行中」表里摘掉。
    fn kill_and_forget(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.alive.store(false, Ordering::SeqCst);
        self.kill();
        forget(&self.id);
    }
}

/// 工具名契约：`^[a-z][a-z0-9_]{0,63}$`（D5，长度含首字符 1..=64）。
pub fn is_valid_tool_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_TOOL_NAME_LEN {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// 解析 `initialize` 回报的工具面。
fn parse_tools(plugin_id: &str, value: &Value) -> Result<Vec<PluginToolSpec>, PluginError> {
    let items = value.as_array().ok_or_else(|| PluginError::Process {
        id: plugin_id.to_string(),
        detail: "握手回报的 tools 不是数组".to_string(),
    })?;

    let mut specs = Vec::with_capacity(items.len());
    for item in items {
        let name = item["name"].as_str().unwrap_or_default().trim().to_string();
        if name.is_empty() {
            return Err(PluginError::Process {
                id: plugin_id.to_string(),
                detail: format!("工具声明缺少 name：{item}"),
            });
        }
        let description = item["description"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let parameters = item.get("parameters").cloned().unwrap_or_else(|| json!({}));
        if !parameters.is_object() {
            return Err(PluginError::Process {
                id: plugin_id.to_string(),
                detail: format!("工具 {name} 的 parameters 不是 JSON 对象"),
            });
        }
        let timeout = item["timeoutMs"]
            .as_u64()
            .filter(|millis| *millis > 0)
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TOOL_TIMEOUT);
        // 文案不合格就整插件失败：模型看的是「PRTS正在{短语}...」，
        // 放过一条超长/带空白的短语，坏的是用户每一轮的对话卡片。
        let action = match item.get("action").and_then(Value::as_str) {
            Some(raw) => match super::parse_action(raw) {
                Ok(action) => action,
                Err(problem) => {
                    return Err(PluginError::Process {
                        id: plugin_id.to_string(),
                        detail: format!("工具 {name} 的 {problem}"),
                    })
                }
            },
            None => None,
        };
        specs.push(PluginToolSpec {
            name,
            description,
            parameters,
            timeout,
            action,
        });
    }
    Ok(specs)
}

/// `HostError` → 协议错误码。文案保持宿主原文（模型能读懂）。
fn host_error_to_rpc(error: &HostError) -> RpcError {
    match error {
        HostError::CapabilityDenied { .. } => {
            RpcError::new(rpc::CAPABILITY_DENIED, error.to_string())
        }
        HostError::InvalidArgs { .. } => {
            RpcError::new(rpc::INVALID_PARAMS, error.to_string())
        }
        HostError::UnknownService(_) => {
            RpcError::new(rpc::METHOD_NOT_FOUND, error.to_string())
        }
        HostError::Failed { .. } => RpcError::new(rpc::PLUGIN_FAULT, error.to_string()),
    }
}

// ═══════════════════════════════════════════════════════
// 运行中的进程插件（应用退出时统一收尸）
// ═══════════════════════════════════════════════════════

static RUNNING: OnceLock<Mutex<Vec<Arc<ProcessPlugin>>>> = OnceLock::new();

fn running() -> &'static Mutex<Vec<Arc<ProcessPlugin>>> {
    RUNNING.get_or_init(|| Mutex::new(Vec::new()))
}

// ═══════════════════════════════════════════════════════
// 按 id 管理句柄（M3 的启停 / 重试 / 卸载）
// ═══════════════════════════════════════════════════════

/// 已成功安装的 process 插件的句柄表。
///
/// 工具只持有自己的那个 `Arc<Launcher>`，宿主侧要在「停用」「卸载」时按 id
/// 找到并收掉进程，所以这里另存一份索引（不增加引用环：Launcher 不反向持有这里）。
static LAUNCHERS: OnceLock<Mutex<HashMap<String, Arc<Launcher>>>> = OnceLock::new();

fn launchers() -> &'static Mutex<HashMap<String, Arc<Launcher>>> {
    LAUNCHERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_launcher(id: &str, launcher: Arc<Launcher>) {
    launchers()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(id.to_string(), launcher);
}

/// 停用一个 process 插件：收掉进程、清掉「重启已失败」的记忆，并摘掉句柄。
///
/// 返回是否真的找到并收掉了一个句柄。停用不是故障，所以这里顺手清
/// `restart_failed`：下次启用是一次干净的重来。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
pub fn shutdown_plugin(id: &str) -> bool {
    let launcher = launchers()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(id);
    let Some(launcher) = launcher else {
        forget(id);
        return false;
    };
    launcher.stop();
    forget(id);
    true
}

/// 重试一个故障插件：清掉「拉起失败」的记忆，再按 D21 拉起一次。
///
/// 只给 `Faulted` 用（规格 §三「进程退出给重试」）；工具仍然注册在表里，
/// 拉起成功后调用路径照旧工作。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
pub fn retry(id: &str) -> Result<(), String> {
    let launcher = launchers()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(id)
        .cloned()
        .ok_or_else(|| format!("插件 {id} 没有可重试的进程句柄"))?;
    launcher.clear_failure();
    launcher.acquire().map(|_| ())
}

fn remember(plugin: Arc<ProcessPlugin>) {
    running()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(plugin);
}

fn forget(id: &str) {
    running()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .retain(|plugin| plugin.id() != id);
}

/// 收掉所有进程插件（M2b 的生命周期收尾；应用退出路径在提交 4a 接线）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn shutdown_all() {
    let plugins = std::mem::take(
        &mut *running()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()),
    );
    for plugin in plugins {
        plugin.shutdown();
    }
    launchers()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_follow_the_contract() {
        assert!(is_valid_tool_name("probe_echo"));
        assert!(is_valid_tool_name("a"));
        assert!(!is_valid_tool_name(""));
        assert!(!is_valid_tool_name("Probe_echo"));
        assert!(!is_valid_tool_name("probe-echo"));
        assert!(!is_valid_tool_name("_probe"));
        assert!(!is_valid_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN + 1)));
        assert!(is_valid_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN)));
    }

    #[test]
    fn prefix_replaces_hyphens() {
        assert_eq!("probe-two".replace('-', "_"), "probe_two");
    }

    #[test]
    fn parse_tools_requires_a_name_and_object_parameters() {
        let error = parse_tools("probe", &json!([{ "description": "x" }])).unwrap_err();
        assert!(matches!(error, PluginError::Process { .. }));

        let error =
            parse_tools("probe", &json!([{ "name": "echo", "parameters": [] }])).unwrap_err();
        assert!(matches!(error, PluginError::Process { .. }));

        let specs = parse_tools(
            "probe",
            &json!([{ "name": "echo", "description": "回声", "parameters": { "type": "object" } }]),
        )
        .unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "echo");
        assert_eq!(specs[0].timeout, DEFAULT_TOOL_TIMEOUT);
    }

    #[test]
    fn parse_tools_honours_timeout_ms() {
        let specs = parse_tools(
            "probe",
            &json!([{ "name": "echo", "parameters": {}, "timeoutMs": 1500 }]),
        )
        .unwrap();
        assert_eq!(specs[0].timeout, Duration::from_millis(1500));
    }

    #[test]
    fn parse_tools_carries_the_declared_action() {
        let specs = parse_tools(
            "probe",
            &json!([{ "name": "echo", "parameters": {}, "action": "清点图片" }]),
        )
        .unwrap();
        assert_eq!(specs[0].action.as_deref(), Some("清点图片"));

        // 缺失 / 空串 = 未提供，不报错。
        let specs = parse_tools(
            "probe",
            &json!([
                { "name": "a", "parameters": {} },
                { "name": "b", "parameters": {}, "action": "" }
            ]),
        )
        .unwrap();
        assert_eq!(specs[0].action, None);
        assert_eq!(specs[1].action, None);
    }

    #[test]
    fn parse_tools_rejects_a_bad_action_and_names_the_tool() {
        // 不合格的短语会让整个插件失败：报错要带工具名，作者才知道改哪一条。
        let error = parse_tools(
            "probe",
            &json!([{ "name": "echo", "parameters": {}, "action": "清点 图片" }]),
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("echo"), "要指出是哪个工具：{text}");
        assert!(text.contains("action 含空白或不可见字符"), "{text}");

        let long = "图".repeat(17);
        let error = parse_tools(
            "probe",
            &json!([{ "name": "echo", "parameters": {}, "action": long }]),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("action 超过 16 字（现在 17 字）"),
            "{error}"
        );
    }

    /// 视图接口规格 §二：字符串保持旧语义，对象必须给非空 content。
    #[test]
    fn render_output_requires_content_for_objects() {
        let dir = scratch_dir("render-basic");

        let output = render_output(json!("你好"), &dir).unwrap();
        assert_eq!(output.summary, "你好");
        assert!(output.view.is_none());

        let output = render_output(
            json!({
                "content": "正文",
                "view": { "kind": "html", "html": "<p>hi</p>", "title": "标题", "height": 148 }
            }),
            &dir,
        )
        .unwrap();
        assert_eq!(output.summary, "正文", "summary 只能是 content，不能是对象本身");
        let view = output.view.expect("应当带视图");
        assert_eq!(view.kind, ToolViewKind::Html);
        assert_eq!(view.title, "标题");
        assert_eq!(view.height_px(), 148);
        assert_eq!(view.html.as_deref(), Some("<p>hi</p>"));

        // 缺 content → 判错，绝不回落到 pretty JSON（那是 view 的后门）。
        let error = render_output(json!({ "a": 1 }), &dir).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("-32602"), "got: {text}");
        assert!(text.contains("缺少 content"), "got: {text}");
        assert!(!text.contains("\"a\""), "不能把对象本身塞进文案：{text}");

        let error = render_output(json!({ "content": "   " }), &dir).unwrap_err();
        assert!(error.to_string().contains("content 是空的"), "got: {error}");

        let error = render_output(json!([1, 2]), &dir).unwrap_err();
        assert!(error.to_string().contains("既不是字符串也不是对象"), "got: {error}");
    }

    #[test]
    fn render_output_validates_title_and_height() {
        let dir = scratch_dir("render-title");

        let error = render_output(json!({ "content": "x", "view": { "kind": "html", "html": "<p/>" } }), &dir)
            .unwrap_err();
        assert!(error.to_string().contains("view.title 必填"), "got: {error}");

        let long = "标".repeat(61);
        let error = render_output(
            json!({ "content": "x", "view": { "kind": "html", "html": "<p/>", "title": long } }),
            &dir,
        )
        .unwrap_err();
        assert!(error.to_string().contains("超过 60 字"), "got: {error}");

        let error = render_output(json!({ "content": "x", "view": { "kind": "png", "title": "t" } }), &dir)
            .unwrap_err();
        assert!(error.to_string().contains("html / image / doc"), "got: {error}");

        // 高度越界夹取到 40–160，缺省 120。
        let low = render_output(
            json!({ "content": "x", "view": { "kind": "html", "html": "<p/>", "title": "t", "height": 10 } }),
            &dir,
        )
        .unwrap();
        assert_eq!(low.view.unwrap().height_px(), VIEW_HEIGHT_MIN);
        let high = render_output(
            json!({ "content": "x", "view": { "kind": "html", "html": "<p/>", "title": "t", "height": 999 } }),
            &dir,
        )
        .unwrap();
        assert_eq!(high.view.unwrap().height_px(), VIEW_HEIGHT_MAX);
        let none = render_output(
            json!({ "content": "x", "view": { "kind": "html", "html": "<p/>", "title": "t" } }),
            &dir,
        )
        .unwrap();
        assert_eq!(none.view.unwrap().height_px(), 120);
    }

    /// html 超限不判错：丢视图、留文字，并追加一句给人看的说明。
    #[test]
    fn oversized_html_degrades_to_text() {
        let dir = scratch_dir("render-big");
        let html = "a".repeat(MAX_VIEW_HTML_BYTES + 1);
        let output = render_output(
            json!({ "content": "正文", "view": { "kind": "html", "html": html, "title": "t" } }),
            &dir,
        )
        .unwrap();
        assert!(output.view.is_none());
        assert!(output.summary.starts_with("正文"));
        assert!(output.summary.contains("视图内容超过上限，只保留文字"));
    }

    #[test]
    fn image_and_doc_paths_stay_inside_the_plugin_dir() {
        let dir = scratch_dir("render-path");
        let image = dir.join("shot.png");
        std::fs::write(&image, b"png").unwrap();

        let output = render_output(
            json!({ "content": "看图", "view": { "kind": "image", "path": "shot.png", "title": "截图" } }),
            &dir,
        )
        .unwrap();
        let view = output.view.unwrap();
        assert_eq!(view.kind, ToolViewKind::Image);
        assert_eq!(view.source.as_deref(), Some(image.canonicalize().unwrap().as_path()));

        let error = render_output(
            json!({ "content": "越界", "view": { "kind": "image", "path": "../secret.txt", "title": "t" } }),
            &dir,
        )
        .unwrap_err();
        assert!(error.to_string().contains("越出插件目录"), "got: {error}");

        let error = render_output(
            json!({ "content": "没有", "view": { "kind": "image", "path": "nope.png", "title": "t" } }),
            &dir,
        )
        .unwrap_err();
        assert!(error.to_string().contains("不存在"), "got: {error}");

        let error = render_output(
            json!({ "content": "文档", "view": { "kind": "doc", "path": "shot.png", "title": "t" } }),
            &dir,
        )
        .unwrap_err();
        assert!(error.to_string().contains("缺少 mime"), "got: {error}");

        let output = render_output(
            json!({ "content": "文档", "view": { "kind": "doc", "path": "shot.png", "mime": "image/png", "title": "t" } }),
            &dir,
        )
        .unwrap();
        let view = output.view.unwrap();
        assert_eq!(view.kind, ToolViewKind::Doc);
        assert_eq!(view.mime.as_deref(), Some("image/png"));
    }

    /// 每个用例一个独立目录，避免并行测试互相踩文件。
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 探针二进制的目录。`cargo test` 会先编译 bin，找不到就跳过（不装 flaky 用例）。
    fn probe_dir() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.parent()?.to_path_buf();
        let probe = dir.join(format!("probe_plugin{}", std::env::consts::EXE_SUFFIX));
        probe.is_file().then_some(dir)
    }

    /// 把探针复制成带故障模式后缀的名字，返回（目录，入口文件名）。
    ///
    /// 模式按 exe 文件名判别（见 `bin/probe_plugin.rs` 的模块文档）：测试不动
    /// 探针协议，只换一个启动名。
    fn probe_with_mode(mode: &str) -> Option<(std::path::PathBuf, String)> {
        let dir = probe_dir()?;
        let source = dir.join(format!("probe_plugin{}", std::env::consts::EXE_SUFFIX));
        let target_dir = std::env::temp_dir().join(format!("prts-probe-{mode}"));
        std::fs::create_dir_all(&target_dir).ok()?;
        let name = format!("probe_{mode}{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(&source, target_dir.join(&name)).ok()?;
        Some((target_dir, name))
    }

    /// 一条 `Loaded` 的台账记录：故障路径要往里写 `Faulted`。
    fn ledger_with(id: &str) -> Arc<Mutex<PluginRegistry>> {
        let text = format!(
            r#"{{"manifestVersion":1,"id":"{id}","name":"探针","version":"0.1.1",
                 "kind":"process","entry":{{"exe":"probe_plugin.exe"}},
                 "activation":[],"capabilities":[],"contributes":{{}}}}"#
        );
        let manifest = crate::plugins::manifest::parse_manifest(&text).expect("样本 manifest 应当能解析");
        let mut registry = PluginRegistry::new();
        registry.insert(crate::plugins::registry::PluginRecord {
            manifest,
            state: crate::plugins::registry::PluginState::Loaded,
            tools: Vec::new(),
            reported_capabilities: Vec::new(),
            dir: std::path::PathBuf::from("."),
        });
        Arc::new(Mutex::new(registry))
    }

    /// 等台账出现 `Faulted`（读线程是异步写台账的）。
    fn wait_faulted(ledger: &Arc<Mutex<PluginRegistry>>, id: &str) -> bool {
        for _ in 0..100 {
            {
                let guard = ledger.lock().unwrap_or_else(|poison| poison.into_inner());
                if let Some(record) = guard.get(id) {
                    if matches!(record.state, crate::plugins::registry::PluginState::Faulted(_)) {
                        return true;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn talks_to_the_probe_over_content_length_frames() {
        let Some(dir) = probe_dir() else {
            eprintln!("跳过：probe_plugin 还没构建");
            return;
        };
        let entry = PluginEntry {
            exe: format!("probe_plugin{}", std::env::consts::EXE_SUFFIX),
            transport: Some("stdio".to_string()),
            args: Vec::new(),
        };
        let plugin = ProcessPlugin::spawn(
            "probe",
            &dir,
            &entry,
            crate::host::test_plugin_ctx("probe"),
            ledger_with("probe"),
        )
        .expect("探针应当能起来");

        let handshake = plugin.initialize().expect("握手应当成功");
        assert_eq!(handshake.tools.len(), 1);
        assert_eq!(handshake.tools[0].name, "echo");
        assert!(handshake.capabilities.is_empty());

        let out = plugin
            .call_tool("echo", json!({ "text": "hi" }), Duration::from_secs(5))
            .expect("echo 应当有应答");
        assert_eq!(out["echo"]["text"], "hi");

        let bad = plugin
            .call_tool("nope", json!({}), Duration::from_secs(5))
            .unwrap_err();
        // 未知工具名是「参数错误」（M4 起），-32601 只留给未知方法。
        assert_eq!(bad.code, crate::plugins::rpc::INVALID_PARAMS);

        // 只收自己这一个：tests 并行跑，`shutdown_all` 会误杀别人的进程。
        plugin.shutdown();
        assert!(!plugin.is_alive());
    }

    /// 停用/卸载要能从「插件 id」收掉进程（§六 / S6），而不是只会全杀。
    #[test]
    fn shutting_down_by_id_kills_the_installed_process() {
        let Some(dir) = probe_dir() else {
            eprintln!("跳过：probe_plugin 还没构建");
            return;
        };
        let entry = PluginEntry {
            exe: format!("probe_plugin{}", std::env::consts::EXE_SUFFIX),
            transport: Some("stdio".to_string()),
            args: Vec::new(),
        };
        let reg = ToolRegistry::new();
        let ledger = ledger_with("probe_kill");
        let (tools, _) = install(
            "probe_kill",
            &dir,
            &entry,
            crate::host::test_plugin_ctx("probe_kill"),
            Arc::clone(&ledger),
            &reg,
        )
        .expect("探针应当能装上");
        assert_eq!(tools, vec!["probe_kill_echo".to_string()]);
        assert!(reg.get("probe_kill_echo").is_some());

        assert!(shutdown_plugin("probe_kill"), "第一次应找得到句柄");
        assert!(!shutdown_plugin("probe_kill"), "收过之后就没有句柄了");
    }

    /// 故障一：握手超时（探针收到 `initialize` 不回话）。
    #[test]
    fn silent_probe_times_out_the_handshake() {
        let Some((dir, exe)) = probe_with_mode("silent") else {
            eprintln!("跳过：probe_plugin 还没构建");
            return;
        };
        let entry = PluginEntry {
            exe,
            transport: Some("stdio".to_string()),
            args: Vec::new(),
        };
        let plugin = ProcessPlugin::spawn(
            "probe-silent",
            &dir,
            &entry,
            crate::host::test_plugin_ctx("probe-silent"),
            ledger_with("probe-silent"),
        )
        .expect("探针应当能起来");

        let error = plugin
            .initialize_with_deadline(Duration::from_millis(300))
            .expect_err("不回 initialize 应当握手超时");
        assert!(
            error.to_string().contains("握手失败"),
            "错误文案要指出握手失败：{error}"
        );
        plugin.kill_and_forget();
    }

    /// 故障二：协议版本不匹配（探针回 `protocol: 2`）。
    #[test]
    fn mismatched_probe_fails_the_handshake() {
        let Some((dir, exe)) = probe_with_mode("mismatch") else {
            eprintln!("跳过：probe_plugin 还没构建");
            return;
        };
        let entry = PluginEntry {
            exe,
            transport: Some("stdio".to_string()),
            args: Vec::new(),
        };
        let plugin = ProcessPlugin::spawn(
            "probe-mismatch",
            &dir,
            &entry,
            crate::host::test_plugin_ctx("probe-mismatch"),
            ledger_with("probe-mismatch"),
        )
        .expect("探针应当能起来");

        let error = plugin.initialize().expect_err("protocol 2 应当被拒绝");
        assert!(
            error.to_string().contains("协议版本不匹配"),
            "错误文案要指出协议版本：{error}"
        );
        plugin.kill_and_forget();
    }

    /// 故障三：进程崩溃（探针收到第一次 `tool.call` 直接退出）。
    #[test]
    fn crashing_probe_is_marked_faulted() {
        let Some((dir, exe)) = probe_with_mode("crash") else {
            eprintln!("跳过：probe_plugin 还没构建");
            return;
        };
        let entry = PluginEntry {
            exe,
            transport: Some("stdio".to_string()),
            args: Vec::new(),
        };
        let ledger = ledger_with("probe-crash");
        let plugin = ProcessPlugin::spawn(
            "probe-crash",
            &dir,
            &entry,
            crate::host::test_plugin_ctx("probe-crash"),
            Arc::clone(&ledger),
        )
        .expect("探针应当能起来");
        plugin.initialize().expect("崩溃模式握手仍应成功");

        let error = plugin
            .call_tool("echo", json!({}), Duration::from_secs(5))
            .expect_err("进程已退出，调用应当失败");
        assert_eq!(error.code, crate::plugins::rpc::PLUGIN_FAULT);
        assert!(
            wait_faulted(&ledger, "probe-crash"),
            "崩溃的插件必须在台账里记 Faulted"
        );
        assert!(!plugin.is_alive());
    }

    /// 故障四：调用超时且不理 `cancel`（宿主等满宽限期后杀进程并记故障）。
    #[test]
    fn hanging_probe_is_killed_after_the_cancel_grace() {
        let Some((dir, exe)) = probe_with_mode("hang") else {
            eprintln!("跳过：probe_plugin 还没构建");
            return;
        };
        let entry = PluginEntry {
            exe,
            transport: Some("stdio".to_string()),
            args: Vec::new(),
        };
        let ledger = ledger_with("probe-hang");
        let plugin = ProcessPlugin::spawn(
            "probe-hang",
            &dir,
            &entry,
            crate::host::test_plugin_ctx("probe-hang"),
            Arc::clone(&ledger),
        )
        .expect("探针应当能起来");
        plugin.initialize().expect("挂起模式握手仍应成功");

        let error = plugin
            .call_tool("echo", json!({}), Duration::from_millis(200))
            .expect_err("不回话的调用应当超时");
        assert_eq!(error.code, crate::plugins::rpc::TIMEOUT);
        assert!(
            wait_faulted(&ledger, "probe-hang"),
            "超时且不理 cancel 的插件必须记 Faulted"
        );
        assert!(
            !plugin.is_alive(),
            "宽限期过后宿主应当把进程杀掉"
        );
    }
}
