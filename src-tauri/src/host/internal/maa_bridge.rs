//! MAA 桥的**进程托管**（M2a 从 `plugins/builtin/maa/bridge.rs` 搬来）。
//!
//! 这里只管进程：定位可执行文件、挑空闲端口、spawn、挂 Job Object、看生死、收尸。
//! **不连接、不握手、不读 `ready`**——那些留在插件侧的 TCP 客户端里。
//!
//! # 禁令：宿主不得为了探活而连接
//!
//! 桥只 `accept` 一次（`bridge/src/main.rs` 先 bind，再 `accept_with_timeout`）：
//! 谁先连上，谁就拿走了这条桥**唯一**的连接机会。宿主若自己连一下看它活没活，
//! 插件随后再连就只能等到桥的连接超时，然后桥以退出码 4 退出，用户看到的是
//! 「MAA 起不来」——而且看不出是宿主自己把机会用掉了。
//!
//! 所以这里的就绪判定**只看子进程**（`Child::try_wait`），永远不碰 socket。
//! 这条写在这里而不是只写在规格里：文档会被后来的人跳过，注释不会（D41）。

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config;
use crate::host::{HostError, HostService, HostServiceInfo};
use crate::machine::MachineInfo;

/// 退出时等桥自己收场的上限；超时就强杀（与原实现一致：20 × 50ms）。
const SHUTDOWN_WAIT: Duration = Duration::from_millis(1_000);
const SHUTDOWN_POLL: Duration = Duration::from_millis(50);

/// 桥进程的状态。
///
/// 只有四种，且**全部来自子进程本身**：宿主不探活，所以「能连上」不在这里表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaaBridgeState {
    /// 刚 spawn，还不知道能不能起来。
    Starting,
    /// 子进程还活着。
    Running,
    /// 子进程已经退出（含从没启动过）。
    Stopped,
    /// 起不来：可执行文件缺失、MAA 目录不存在、端口挑不到、spawn 失败。
    Unavailable,
}

/// 一次状态查询的结果。
#[derive(Debug, Clone)]
pub struct MaaBridgeStatus {
    pub state: MaaBridgeState,
    /// 桥监听的端口；没起过就是 `None`。
    pub port: Option<u16>,
    /// `Unavailable` / `Stopped` 时的可展示原因（直接给用户看）。
    pub error: Option<String>,
}

impl MaaBridgeStatus {
    fn unavailable(error: impl Into<String>) -> Self {
        Self {
            state: MaaBridgeState::Unavailable,
            port: None,
            error: Some(error.into()),
        }
    }

    fn starting(port: u16) -> Self {
        Self {
            state: MaaBridgeState::Starting,
            port: Some(port),
            error: None,
        }
    }
}

/// 一条被托管的桥进程。
struct Supervisor {
    child: Child,
    /// Windows Job：父进程崩溃、被强杀或卸载器直接结束时，随句柄关闭一起结束桥。
    /// 字段本身只用于持有句柄，不需要读取。
    #[cfg(windows)]
    #[allow(dead_code)]
    job: Option<std::os::windows::io::OwnedHandle>,
    port: u16,
}

static SUPERVISOR: OnceLock<Mutex<Option<Supervisor>>> = OnceLock::new();

fn supervisor() -> &'static Mutex<Option<Supervisor>> {
    SUPERVISOR.get_or_init(|| Mutex::new(None))
}

/// 锁中毒也要能用：托管状态里没有「必须一起回滚」的不变量。
fn lock() -> MutexGuard<'static, Option<Supervisor>> {
    supervisor()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 桥可执行文件是否就位。
///
/// **只解析路径**：不启动、不连接（release 启动自检用）。
// 开发构建没有调用方：自检那段是 `cfg(not(debug_assertions))` 的。
#[cfg_attr(debug_assertions, allow(dead_code))]
pub fn ready() -> bool {
    resolve_exe().is_ok()
}

/// 定位随包分发的桥可执行文件。
///
/// 桥是 PRTS 的组件，不是用户配置项——这里**没有**「让用户填路径」的分支。
fn resolve_exe() -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "prts-maa-bridge.exe"
    } else {
        "prts-maa-bridge"
    };
    let root = config::resource_root();

    let candidates: Vec<PathBuf> = if config::is_dev() {
        // 开发构建：仓库内 bridge crate 的构建产物（debug 优先，再 release）。
        ["debug", "release"]
            .iter()
            .map(|profile| {
                root.join("..")
                    .join("bridge")
                    .join("target")
                    .join(profile)
                    .join(name)
            })
            .collect()
    } else {
        // 发布构建：桥随 PRTS 分发，落程序目录（sidecar 形态）或其 resources 下。
        vec![root.join(name), root.join("resources").join(name)]
    };

    let mut tried: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }

    let list = tried
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("；");
    Err(format!(
        "MAA 运行组件缺失，重新安装 PRTS 可恢复（找不到 {name}）；找过：{list}"
    ))
}

/// 取一个空闲端口：`bind(0)` 拿系统分配的端口再放开。
///
/// 桥随后绑同一端口——存在极小的竞争窗口，可接受（绑不上桥会以退出码 3 收场，
/// 而那个退出码经 [`status`] 会变成一条能看懂的报错）。
fn free_port() -> Result<u16, String> {
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("取空闲端口失败：{e}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| format!("读本地地址失败：{e}"))
}

/// 确保桥进程在跑，并把端口交出去。
///
/// **spawn 之后立即返回**，不等握手、不连接：握手在插件侧（D41 / 规格第 17 条）。
/// 已经在跑就复用（状态 `Running`），已经退出就收尸重起。
pub fn ensure(maa_dir: &Path, user_dir: &Path) -> MaaBridgeStatus {
    let mut slot = lock();

    if let Some(sup) = slot.as_mut() {
        match sup.child.try_wait() {
            Ok(None) => {
                return MaaBridgeStatus {
                    state: MaaBridgeState::Running,
                    port: Some(sup.port),
                    error: None,
                };
            }
            // 已经退出（或查不出来）：收尸，往下重新起一个。
            Ok(Some(_)) | Err(_) => *slot = None,
        }
    }

    if !maa_dir.is_dir() {
        return MaaBridgeStatus::unavailable(format!(
            "MAA 目录不存在（{}）：请在设置里重新检测 MAA 安装位置",
            maa_dir.display()
        ));
    }
    let exe = match resolve_exe() {
        Ok(exe) => exe,
        Err(error) => return MaaBridgeStatus::unavailable(error),
    };
    let port = match free_port() {
        Ok(port) => port,
        Err(error) => return MaaBridgeStatus::unavailable(error),
    };

    // cwd 必须先存在：Windows 下给子进程一个不存在的 current_dir，spawn 直接报
    // ERROR_DIRECTORY(267)，错误文案却指向「启动桥失败」，极易被误判成桥本身有问题
    // （跟模拟器、跟 MaaCore 都无关）。这个目录平时只有写任务结果时才顺带建出来，
    // 所以新装 / 清过数据根的用户会在「测试连接」这一步就被挡住（2026-10-11 实测）。
    if let Err(e) = std::fs::create_dir_all(user_dir) {
        return MaaBridgeStatus::unavailable(format!(
            "建桥工作目录失败（{}）：{e}",
            user_dir.display()
        ));
    }

    let mut command = Command::new(&exe);
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

    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return MaaBridgeStatus::unavailable(format!("启动桥失败（{}）：{e}", exe.display()))
        }
    };
    #[cfg(windows)]
    let job = attach_kill_on_close_job(&child);

    *slot = Some(Supervisor {
        child,
        #[cfg(windows)]
        job,
        port,
    });
    MaaBridgeStatus::starting(port)
}

/// 查桥进程的状态。**不连接、不探活**——只看子进程。
pub fn status() -> MaaBridgeStatus {
    let mut slot = lock();
    let Some(sup) = slot.as_mut() else {
        return MaaBridgeStatus {
            state: MaaBridgeState::Stopped,
            port: None,
            error: None,
        };
    };

    let (report, exited) = match sup.child.try_wait() {
        Ok(None) => (
            MaaBridgeStatus {
                state: MaaBridgeState::Running,
                port: Some(sup.port),
                error: None,
            },
            false,
        ),
        Ok(Some(status)) => (
            MaaBridgeStatus {
                state: MaaBridgeState::Stopped,
                port: Some(sup.port),
                error: Some(exit_message(&status)),
            },
            true,
        ),
        Err(e) => (
            MaaBridgeStatus {
                state: MaaBridgeState::Stopped,
                port: Some(sup.port),
                error: Some(format!("检查桥进程状态失败：{e}")),
            },
            true,
        ),
    };
    if exited {
        *slot = None;
    }
    report
}

/// 结束被托管的桥：先给 1 秒让它自己收场，超时强杀。
///
/// 幂等——没有托管进程时什么都不做。
pub fn shutdown() -> MaaBridgeStatus {
    let Some(mut sup) = lock().take() else {
        return MaaBridgeStatus {
            state: MaaBridgeState::Stopped,
            port: None,
            error: None,
        };
    };

    let polls = SHUTDOWN_WAIT.as_millis() / SHUTDOWN_POLL.as_millis();
    for _ in 0..polls {
        if matches!(sup.child.try_wait(), Ok(Some(_))) {
            return MaaBridgeStatus {
                state: MaaBridgeState::Stopped,
                port: Some(sup.port),
                error: None,
            };
        }
        std::thread::sleep(SHUTDOWN_POLL);
    }
    let _ = sup.child.kill();
    let _ = sup.child.wait();
    MaaBridgeStatus {
        state: MaaBridgeState::Stopped,
        port: Some(sup.port),
        error: None,
    }
}

/// 退出码翻译成人话：这几种是用户真会遇到的，每条都给下一步动作。
fn exit_message(status: &ExitStatus) -> String {
    match status.code() {
        Some(3) => "桥进程已退出（退出码 3）：端口监听失败，可能被别的程序占用——重试一次，\
             仍不行就重启 PRTS"
            .to_string(),
        Some(4) => "桥进程已退出（退出码 4）：等不到连接，自己收场了——重试一次".to_string(),
        Some(code) => format!("桥进程已退出（退出码 {code}）"),
        None => "桥进程已退出（被信号终止）".to_string(),
    }
}

/// 把桥进程放进 kill-on-close Job：父进程崩溃、被强杀或卸载器直接结束时，
/// Windows 在最后一个 Job 句柄关闭时一并结束桥及其子进程。
#[cfg(windows)]
fn attach_kill_on_close_job(child: &Child) -> Option<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

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

/// `maa.bridge` 能力的静态描述。
///
/// `stateful: true`——它托管的是跨越多次调用的子进程；`concurrency_safe: false`
/// 是因为 `ensure` 与 `shutdown` 对同一个进程是互斥的（内部另有锁，这里如实声明）。
static MAA_BRIDGE_INFO: HostServiceInfo = HostServiceInfo {
    key: "maa.bridge",
    summary: "MAA 桥进程托管：起桥（只给端口，不连接）、查状态、收尸",
    params_doc: "{ op: ensure|status|shutdown }（ensure 不收参数，宿主自己读 MAA 配置）",
    returns_doc: "{ state: starting|running|stopped|unavailable, port?, error? }",
    stateful: true,
    requires_user_grant: false,
    timeout: Some(Duration::from_secs(30)),
    concurrency_safe: false,
    since: 1,
};

/// `maa.bridge` 的服务实现：进程归宿主，连接归插件。
///
/// 三个 op 都**不碰 socket**：`ensure` 只 spawn 并立即返回端口，握手与就绪判定
/// 全在插件侧（桥只 accept 一次，宿主先连上去会吃掉插件唯一的连接机会，D41）。
pub struct MaaBridgeService {
    /// 数据根：`MachineInfo` 与桥的工作目录都从这里推。
    data_dir: PathBuf,
}

impl MaaBridgeService {
    /// 构造；`data_dir` 与宿主其它部分共用同一个根。
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

impl HostService for MaaBridgeService {
    fn info(&self) -> &'static HostServiceInfo {
        &MAA_BRIDGE_INFO
    }

    fn call(&self, _caller: &str, args: Value) -> Result<Value, HostError> {
        let op = args
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| HostError::invalid_args("maa.bridge", "缺少 op 字段"))?;

        let status = match op {
            "ensure" => {
                let maa_override = crate::storage::settings::SettingsStore::load(&self.data_dir)
                    .settings
                    .maa
                    .maa_dir_override()
                    .map(str::to_string);
                let Some(info) =
                    MachineInfo::load_resolved(&self.data_dir, maa_override.as_deref())
                else {
                    return Err(HostError::failed(
                        "maa.bridge",
                        "还没有本机信息（machine.json）：请重启 PRTS，或在设置里确认 MAA 安装位置",
                    ));
                };
                let Some(maa_dir) = info.maa.clone() else {
                    return Err(HostError::failed(
                        "maa.bridge",
                        "本机没有检测到 MAA：装好 MAA 再试，或在设置里指定 MAA 路径",
                    ));
                };
                ensure(&maa_dir, &self.data_dir.join("maa"))
            }
            "status" => status(),
            "shutdown" => shutdown(),
            other => {
                return Err(HostError::invalid_args(
                    "maa.bridge",
                    format!("未知 op：{other}（可用：ensure/status/shutdown）"),
                ))
            }
        };

        Ok(status_json(&status))
    }
}

/// 状态 → 协议里的形状（规格 §4：`{state, port, error}`）。
fn status_json(status: &MaaBridgeStatus) -> Value {
    let state = match status.state {
        MaaBridgeState::Starting => "starting",
        MaaBridgeState::Running => "running",
        MaaBridgeState::Stopped => "stopped",
        MaaBridgeState::Unavailable => "unavailable",
    };
    json!({
        "state": state,
        "port": status.port,
        "error": status.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试桩：把 `status()` 换成一个固定应答，验证服务的参数解析与序列化。
    struct FixedStatus(MaaBridgeStatus);

    impl HostService for FixedStatus {
        fn info(&self) -> &'static HostServiceInfo {
            &MAA_BRIDGE_INFO
        }

        fn call(&self, _caller: &str, args: Value) -> Result<Value, HostError> {
            match args.get("op").and_then(Value::as_str) {
                Some("status") => Ok(status_json(&self.0)),
                Some(other) => Err(HostError::invalid_args(
                    "maa.bridge",
                    format!("未知 op：{other}"),
                )),
                None => Err(HostError::invalid_args("maa.bridge", "缺少 op 字段")),
            }
        }
    }

    #[test]
    fn status_json_keeps_the_contract_shape() {
        let running = status_json(&MaaBridgeStatus {
            state: MaaBridgeState::Running,
            port: Some(43715),
            error: None,
        });
        assert_eq!(running["state"], "running");
        assert_eq!(running["port"], 43715);
        assert_eq!(running["error"], Value::Null);

        let unavailable = status_json(&MaaBridgeStatus::unavailable("没有桥"));
        assert_eq!(unavailable["state"], "unavailable");
        assert_eq!(unavailable["port"], Value::Null);
        assert_eq!(unavailable["error"], "没有桥");
    }

    #[test]
    fn service_rejects_unknown_ops_and_parses_ensure() {
        let service = FixedStatus(MaaBridgeStatus::starting(4000));
        let err = service
            .call("maa", serde_json::json!({"op": "nope"}))
            .unwrap_err();
        assert!(matches!(err, HostError::InvalidArgs { .. }), "{err:?}");
        let err = service.call("maa", serde_json::json!({})).unwrap_err();
        assert!(matches!(err, HostError::InvalidArgs { .. }), "{err:?}");

        let ok = service
            .call("maa", serde_json::json!({"op": "status"}))
            .unwrap();
        assert_eq!(ok["state"], "starting");
    }

    #[test]
    fn free_port_is_bindable() {
        let port = free_port().unwrap();
        assert!(port > 0);
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        drop(listener);
    }

    #[test]
    fn ensure_reports_unavailable_when_maa_dir_is_missing() {
        let missing = std::env::temp_dir().join("prts-m2a-no-such-maa-dir");
        let _ = std::fs::remove_dir_all(&missing);

        let status = ensure(&missing, &std::env::temp_dir());

        assert_eq!(status.state, MaaBridgeState::Unavailable);
        assert_eq!(status.port, None);
        assert!(
            status
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("MAA 目录不存在"),
            "错误文案要点名是 MAA 目录：{:?}",
            status.error
        );
    }
}
