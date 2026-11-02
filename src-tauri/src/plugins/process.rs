//! process 插件的生命周期与 RPC 通道（M2b）。
//!
//! 一条子进程、一条 stdin/stdout 管道、一份分帧（[`super::transport`]）、一份
//! 请求槽位表（[`super::rpc`]）。读线程独占 stdout，**每读到一帧立即按类型分派**
//! （R3）：应答填槽位、`host.call` 交给工作线程、`log` 直接进 stderr。
//! 任何路径都不得在读线程里阻塞等待另一个请求——插件的 `host.call`
//! （`maa.bridge.ensure` 最长 30s）与宿主的 `tool.call` 必须能同时在路上。

use std::io::BufReader;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::manifest::{PluginEntry, SUPPORTED_PROTOCOL};
use super::rpc::{self, Pending, RpcError, RpcResult};
use super::transport::{write_frame, FrameReader};
use super::PluginError;
use crate::host::{HostError, PluginCtx};
use crate::tools::{Tool, ToolContext, ToolError, ToolOutput, ToolRegistry};

/// 握手超时（规格 §5）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
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
}

/// 握手结果：工具面**只此一处来源**（规格 §5）。
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
}

impl ProcessPlugin {
    /// 拉起进程并起读线程。握手要单独调 [`ProcessPlugin::initialize`]。
    pub fn spawn(
        id: &str,
        plugin_dir: &Path,
        entry: &PluginEntry,
        ctx: PluginCtx,
    ) -> Result<Arc<Self>, PluginError> {
        if let Some(transport) = entry.transport.as_deref() {
            if transport != "stdio" {
                return Err(PluginError::Process {
                    id: id.to_string(),
                    detail: format!("不支持的 transport：{transport}（M2 只实现 stdio）"),
                });
            }
        }

        let exe = plugin_dir.join(&entry.exe);
        if !exe.is_file() {
            return Err(PluginError::Process {
                id: id.to_string(),
                detail: format!(
                    "入口不存在：{}（检查 plugin.json 的 entry.exe 是否相对插件目录）",
                    exe.display()
                ),
            });
        }

        let mut command = Command::new(&exe);
        command
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
            detail: format!("起进程失败（{}）：{error}", exe.display()),
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
        let result = self
            .request(
                "initialize",
                json!({
                    "protocol": SUPPORTED_PROTOCOL,
                    "hostVersion": env!("CARGO_PKG_VERSION"),
                    "pluginId": self.id,
                }),
                HANDSHAKE_TIMEOUT,
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

    /// 通道断了：标记死亡、叫醒所有等待者、收掉进程。
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

/// 宿主把插件工具挂进注册表的适配器。
///
/// 模型看到的是带前缀的名字（`probe_echo`），管道上发的是插件自己的名字
/// （`echo`）——前缀是宿主的命名空间，插件不需要知道。
pub struct PluginTool {
    plugin: Arc<ProcessPlugin>,
    name: String,
    method: String,
    description: String,
    parameters: Value,
    timeout: Duration,
}

impl PluginTool {
    pub fn new(plugin: Arc<ProcessPlugin>, prefix: &str, spec: PluginToolSpec) -> Self {
        let name = format!("{prefix}_{}", spec.name);
        Self {
            plugin,
            name,
            method: spec.name,
            description: spec.description,
            parameters: spec.parameters,
            timeout: spec.timeout,
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

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let args: Value = if params.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(params).map_err(|error| ToolError::Parse(error.to_string()))?
        };

        let result = self
            .plugin
            .call_tool(&self.method, args, self.timeout)
            .map_err(|error| {
                ToolError::Execution(format!(
                    "插件 {} 的 {} 调用失败：{error}",
                    self.plugin.id(),
                    self.method
                ))
            })?;

        Ok(render_output(result))
    }
}

/// 插件返回值 → 结构化输出。字符串直接当展示文本，其余走 pretty JSON。
fn render_output(value: Value) -> ToolOutput {
    match &value {
        Value::String(text) => ToolOutput::json(value.clone(), text.clone()),
        _ => {
            let summary = serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| value.to_string());
            ToolOutput::json(value, summary)
        }
    }
}

/// 安装一个 process 插件：拉起 → 握手 → 按前缀注册工具。
///
/// 任何一步失败都要先收掉进程再返回——失败的插件不该留一个孤儿进程。
pub fn install(
    id: &str,
    plugin_dir: &Path,
    entry: &PluginEntry,
    ctx: PluginCtx,
    reg: &mut ToolRegistry,
) -> Result<(Vec<String>, Vec<String>), PluginError> {
    let plugin = ProcessPlugin::spawn(id, plugin_dir, entry, ctx)?;

    let handshake = match plugin.initialize() {
        Ok(handshake) => handshake,
        Err(error) => {
            plugin.kill_and_forget();
            return Err(error);
        }
    };

    let prefix = id.replace('-', "_");
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
        tools.push(Box::new(PluginTool::new(Arc::clone(&plugin), &prefix, spec)));
    }

    match super::register_plugin_tools(reg, id, tools) {
        Ok(names) => Ok((names, handshake.capabilities)),
        Err(error) => {
            plugin.kill_and_forget();
            Err(error)
        }
    }
}

impl ProcessPlugin {
    /// 安装失败时收掉进程，并把它从「运行中」表里摘掉。
    fn kill_and_forget(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.kill();
        forget(&self.id);
    }
}

/// 工具名契约：`^[a-z][a-z0-9_]{0,63}$`（D5，长度含首字符 1..=64）。
fn is_valid_tool_name(name: &str) -> bool {
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
        specs.push(PluginToolSpec {
            name,
            description,
            parameters,
            timeout,
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
    fn render_output_prefers_strings_as_summary() {
        let output = render_output(json!("你好"));
        assert_eq!(output.summary, "你好");
        let output = render_output(json!({ "a": 1 }));
        assert!(output.summary.contains("\"a\""));
    }

    /// 探针二进制的目录。`cargo test` 会先编译 bin，找不到就跳过（不装 flaky 用例）。
    fn probe_dir() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.parent()?.to_path_buf();
        let probe = dir.join(format!("probe_plugin{}", std::env::consts::EXE_SUFFIX));
        probe.is_file().then_some(dir)
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
        };
        let plugin = ProcessPlugin::spawn("probe", &dir, &entry, crate::host::test_plugin_ctx("probe"))
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
        assert_eq!(bad.code, crate::plugins::rpc::METHOD_NOT_FOUND);

        crate::plugins::process::shutdown_all();
        assert!(!plugin.is_alive());
    }
}
