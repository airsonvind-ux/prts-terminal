//! manager —— 桥的生命周期与连接（进程级单例）。
//!
//! 管理器把前面两层串成一条路：
//!
//! 1. **懒启动**：真要用 MAA 时才读 `machine.json`——没有 MAA 就直接说清楚，
//!    不起桥；桥是 PRTS 自带的 sidecar，路径自己算，不劳用户。
//! 2. **连哪一台**（见 `_maa-接入模块-设计.md` §3）：只认**运行中**的模拟器。
//!    恰好一台就连；一台没在跑就报清楚；多台在跑**不猜**，让用户去设置里指定。
//! 3. **截图增强**：MuMu / 雷电连之前先配 extras，失败由 MAA 自行降级。
//! 4. **崩溃**：进程退或连接断 → 标记，**不静默自动重启**；下一次调用重新起一次
//!    （这就是「手动重试」），不做循环重连。
//!
//! 状态：`available` / `connected` 以桥的 `status` 为准；`running` 以运行记录为准
//! （桥的 `AsstRunning` 只表示助手已启动，队列空着时也返回 true）。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config;
use crate::machine::MachineInfo;

use super::bridge::{self, Bridge};
use super::connection::{self, Overrides, PickError, Resolved};
use super::settings::MaaSettings;

/// 单条普通命令的时限。
const QUICK_TIMEOUT: Duration = Duration::from_secs(20);
/// 连接命令的时限（MaaCore 内部还要跑 adb）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
/// 任务类命令（入队等）的时限。
const TASK_TIMEOUT: Duration = Duration::from_secs(60);
/// 等「连接真正完成」的上限。
const CONNECT_SETTLE_TIMEOUT: Duration = Duration::from_secs(90);

/// MaaCore 回调码：整条队列跑完。
const CODE_ALL_TASKS_COMPLETED: i64 = crate::maa::records::CODE_ALL_TASKS_COMPLETED;
/// MaaCore 回调码：异步调用的回执（连接完成的真信号在这里）。
const CODE_ASYNC_CALL_INFO: i64 = 4;

/// 已有任务在跑时的派发拒绝文案（工具层原样交给模型与用户）。
///
/// 2026-10-08：MAA 只有一个实例、一条队列，同一时刻只该有一个派发者。
const DISPATCH_BUSY_MSG: &str = "MAA 正在执行另一个对话派发的任务，本次派发被拒绝：\
     同一时刻只允许一个派发者。等它跑完，或先中止它（action=\"stop\"）；\
     要一次安排多个任务，请把它们放进同一次 tasks 数组（MAA 依次执行）。";

/// 连上的那个模拟器。
#[derive(Debug, Clone)]
pub struct ConnectedInfo {
    pub emulator: String,
    pub address: String,
    pub config: String,
    pub extras: bool,
    /// 连接测速后**实际生效**的截图方式（如 `MumuExtras`）——MAA 静默降级时
    /// 也会如实写在这里。
    pub screencap: Option<String>,
    /// 连接期间桥发来的事件——截图方式之类的实况就在里面，供上层如实呈现。
    pub events: Vec<Value>,
}

/// `submit` 的结果。
#[derive(Debug, Clone)]
pub struct SubmitOutcome {
    pub task_ids: Vec<i64>,
    pub connected: ConnectedInfo,
    /// 等待时收下的回执事件（`wait=0` 时为空）。
    pub events: Vec<Value>,
    /// 是否等到整条队列跑完；false = 到点仍在跑。
    pub completed: bool,
    /// 本次生效的等待上限（秒）。
    pub waited_secs: u64,
    /// 从派发到返回实际过了多少毫秒。
    pub elapsed_ms: u128,
}

struct Manager {
    bridge: Option<Bridge>,
    connected: Option<ConnectedInfo>,
    last_error: Option<String>,
    /// 当前生效的 MAA 设置（装配时与每次变更后由命令层推入）。
    settings: MaaSettings,
    /// 设置改过、旧连接因此作废（给设置页看，不是给逻辑看）。
    stale: bool,
    /// 正在连接中（起桥 + 连模拟器）。一次只放一个人进去：并发进来只会
    /// 排队等同一把锁，不如直接回一句「正在连接中」，别让按第二遍的人干等。
    connecting: bool,
}

static MANAGER: OnceLock<Mutex<Manager>> = OnceLock::new();

fn manager() -> &'static Mutex<Manager> {
    MANAGER.get_or_init(|| {
        Mutex::new(Manager {
            bridge: None,
            connected: None,
            last_error: None,
            settings: MaaSettings::default(),
            stale: false,
            connecting: false,
        })
    })
}

/// 取管理器锁。
///
/// **锁中毒（持锁期间 panic）也照用**：这里共享的只是普通数据，没有“半更新”
/// 不可读的语义要求；把中毒一路放大，只会让设置页此后每次都失败——
/// 一次异常变成永久失效，正是最难查的那种。（返回 Result 只为兼容既有调用点。）
fn lock() -> Result<MutexGuard<'static, Manager>, String> {
    Ok(manager().lock().unwrap_or_else(|e| e.into_inner()))
}

/// 确保桥在、且已连上模拟器。
fn ensure(m: &mut Manager) -> Result<(), String> {
    // 桥在但已断 → 丢掉重来一次。这就是「手动重试」，不做循环重连。
    let dead = m.bridge.as_ref().map(|b| !b.is_alive()).unwrap_or(false);
    if dead {
        m.bridge = None;
        m.connected = None;
    }
    if m.bridge.is_none() {
        if let Err(e) = m.start() {
            m.last_error = Some(e.clone());
            return Err(e);
        }
    }
    m.last_error = None;
    Ok(())
}

/// 更新 MAA 设置。
///
/// **任何一项变化都让现有连接作废**（下次使用时按新设置重建）：这段设置全部与
/// “怎么连、连哪个”有关，拿着旧连接继续跑就是隐性失效。
pub fn set_settings(next: MaaSettings) {
    let Ok(mut m) = lock() else {
        return;
    };
    if m.settings == next {
        return;
    }
    // 有过连接 → 这次改动把它作废了，给设置页留下“设置已更改”的痕迹。
    let had_connection = m.connected.take().is_some();
    m.settings = next;
    if let Some(bridge) = m.bridge.take() {
        bridge.shutdown();
    }
    m.stale = had_connection;
}

/// 把任务派给 MAA。
///
/// `tasks` 是一整条队列，按序执行；`wait_secs` 是最多等多少秒（`0` = 派发即返回）。
/// **等待期间不长期持有管理器锁**——设置页的状态查询不能被冻住。
pub fn submit(tasks: &[Value], wait_secs: u64) -> Result<SubmitOutcome, String> {
    let tasks = normalize_tasks(tasks)?;
    let started = Instant::now();

    let (task_ids, connected) = {
        let mut m = lock()?;
        ensure(&mut m)?;
        // 派发互斥（2026-10-08）：MAA 只有一个实例、一条队列，同一时刻只该有一个派发者。
        // 已有任务在跑（含 wait=0 派出去、出轮后仍在跑的长任务）→ 拒绝并说明原因。
        // 判据放在这把锁内，避免“检查与排队”之间被人插进来；ensure 在前，桥断了会
        // 先丢掉重建，旧记录随之消失，不会拿一条死桥的残留记录把人堵在外面。
        //
        // 为何不是“追加给它”：运行记录（结果文件 / 最近一次）、事件缓冲与等待结果
        // 都是单槽的——两处派发会让“谁在等、超时算谁的、这份结果是谁的”全部串味。
        // 要一次安排多项，放进同一次调用的 tasks 数组（MAA 依次执行）。
        if m.bridge
            .as_ref()
            .map(|b| b.run_in_flight())
            .unwrap_or(false)
        {
            return Err(DISPATCH_BUSY_MSG.to_string());
        }
        let connected = m
            .connected
            .clone()
            .ok_or_else(|| "桥已启动，但没有连上模拟器".to_string())?;

        let bridge = m.bridge.as_ref().ok_or("桥不可用")?;
        // 事件侧的落盘需要「本次运行」的上下文：先开一次运行，再排队。
        bridge.begin_run(Vec::new());

        let mut task_ids = Vec::new();
        for task in &tasks {
            let type_ = task.get("type").and_then(Value::as_str).unwrap_or("");
            let params = task.get("params").cloned().unwrap_or_else(|| json!({}));
            // 服务器（官/B）由 PRTS 补到用得上它的任务上——用户不必填，也不该填。
            let params = with_client_type(type_, params, m.settings.server.client_type());
            match add_task(bridge, type_, &params) {
                Ok(id) => task_ids.push(id),
                Err(error) => {
                    // begin_run 已经置了 in-flight；入队失败后必须收尾，
                    // 否则下一次 submit 会被这次失败留下"正在跑"的假记录挡住。
                    bridge.abort_run();
                    return Err(error);
                }
            }
        }
        bridge.set_run_task_ids(task_ids.clone());

        // 队列空闲才起跑；已在跑就只是追加给它。
        let running = match bridge.command(json!({ "cmd": "status" }), QUICK_TIMEOUT) {
            Ok(status) => status
                .get("running")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            Err(error) => {
                bridge.abort_run();
                return Err(error.to_string());
            }
        };
        if !running {
            if let Err(error) = bridge.command(json!({ "cmd": "run_start" }), QUICK_TIMEOUT) {
                bridge.abort_run();
                return Err(error.to_string());
            }
        }
        (task_ids, connected)
    };

    let (completed, events) = if wait_secs == 0 {
        (false, Vec::new())
    } else {
        wait_for_queue(wait_secs)?
    };
    Ok(SubmitOutcome {
        task_ids,
        connected,
        events,
        completed,
        waited_secs: wait_secs,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// 是否有任务正在跑（桥在、且有未收尾的运行）。
///
/// 给设置写入用：MAA 设置一变就作废连接（`set_settings` 会关桥），正在跑的任务
/// 会被腰斩。只看活着的桥——死桥的残留记录不算。
pub fn is_running() -> bool {
    lock()
        .map(|m| {
            m.bridge
                .as_ref()
                .map(|b| b.is_alive() && b.run_in_flight())
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// 各任务类型的默认等待上限（秒）。`0` = 派发即返回。
///
/// 数值来自真机实测（OperBox 42s、Depot 5s、StartUp 25s，见
/// `_maa-接入模块-设计.md`）：识别型给足慢机器的余量；长任务的正常状态
/// 就是长时间运行，给它任何正数都只是把 PRTS 按在原地，所以默认不等。
pub fn default_wait_secs(type_: &str) -> u64 {
    match type_ {
        "OperBox" => 180,
        "Depot" => 60,
        "StartUp" => 90,
        "CloseDown" | "SwitchTheme" => 60,
        // Award 只是领奖，加了额外内容才会逐渐变久；SingleStep 是实验性的逐步操作，
        // 由调用方自己决定等不等。
        "Award" => 60,
        // 其余（Fight / Recruit / Infrast / Mall / Roguelike / Copilot /
        // SSSCopilot / ParadoxCopilot / Reclamation / Custom / SingleStep /
        // VideoRecognition）默认不等。
        _ => 0,
    }
}

/// 整条队列的默认等待：任一项是 0 就 0（队列顺序执行，长任务注定拖住整条），
/// 否则取各项之和——顺序执行下，和就是「整条跑完」的界。
pub fn queue_default_wait_secs(tasks: &[Value]) -> u64 {
    let mut total = 0u64;
    for task in tasks {
        let type_ = task.get("type").and_then(Value::as_str).unwrap_or("");
        let secs = default_wait_secs(type_);
        if secs == 0 {
            return 0;
        }
        total = total.saturating_add(secs);
    }
    total
}

/// 当前队列（`task_list`）。**不起桥**：没有桥就回 `Ok(None)`。
pub fn queue() -> Result<Option<Value>, String> {
    let m = lock()?;
    let Some(bridge) = m.bridge.as_ref() else {
        return Ok(None);
    };
    bridge
        .command(json!({ "cmd": "task_list" }), QUICK_TIMEOUT)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// 本次运行已写下的结果文件（未收尾时为空）。
pub fn run_files() -> Vec<crate::maa::records::Kept> {
    let Ok(m) = lock() else {
        return Vec::new();
    };
    m.bridge
        .as_ref()
        .map(|bridge| bridge.run_files())
        .unwrap_or_default()
}

/// 「最近一次」运行的留痕。
///
/// 先看桥的笔记；桥被重建过（改设置 / 崩了 / 关过）笔记就没了——这时回落到
/// 磁盘上最新的 `latest-<类型>.jsonl`。文件一直都在，`status` 不该说“没跑过”。
pub fn last_run() -> crate::maa::records::LastRun {
    let memory = lock()
        .ok()
        .and_then(|m| m.bridge.as_ref().map(|bridge| bridge.last_run()))
        .unwrap_or_default();
    if !memory.files.is_empty() || memory.ended_at_ms.is_some() {
        return memory;
    }
    match crate::maa::records::latest_pointer_on_disk() {
        Some((kind, path, at, lines, chain)) => crate::maa::records::LastRun {
            ended_at_ms: Some(crate::maa::records::ms_of(at)),
            files: vec![crate::maa::records::Kept {
                kind,
                chain,
                path,
                frames: lines,
                seen: lines,
            }],
            completed: false,
            from_disk: true,
            ..Default::default()
        },
        None => memory,
    }
}

/// 中止当前任务。
pub fn stop() -> Result<(), String> {
    let m = lock()?;
    let Some(bridge) = m.bridge.as_ref() else {
        return Err("MAA 还没有启动，没有可中止的任务".to_string());
    };
    bridge
        .command(json!({ "cmd": "run_stop" }), QUICK_TIMEOUT)
        .map_err(|e| e.to_string())?;
    // AsstStop 成功不等于会再收到 AllTasksCompleted：MaaCore 停止时
    // 只发 TaskChainStopped。运行记录必须在这里收尾，否则设置页会一直
    // 把这次已中止的任务当成仍在运行。
    bridge.abort_run();
    Ok(())
}

/// 设置页用的只读状态视图。
///
/// **不触发连接、不起桥、不改任何状态**：只看已经存在的东西。要看最新结果，
/// 走 `status`（那才会真去连一次）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct StatusView {
    /// 本机装了 MAA（`machine.json` 的结论）。
    pub maa_installed: bool,
    /// 桥已启动且 MaaCore 可用。
    pub bridge_available: bool,
    pub connected: bool,
    pub running: bool,
    /// 设置改过 → 已有连接已作废。
    pub stale: bool,
    pub last_error: Option<String>,
    /// 已连上的模拟器 / 地址 / 是否启用了截图增强。
    pub emulator: Option<String>,
    pub address: Option<String>,
    pub extras: bool,
    /// 实际生效的截图方式（如 `MumuExtras`）。
    pub screencap: Option<String>,
    /// 本机探测到的模拟器标签（下拉用）。
    pub emulators: Vec<String>,
    /// 每个已装模拟器推导出的连接参数（页面用来显示“自动时实际在用”的值）。
    pub resolved: Vec<ResolvedView>,
}

/// 一个模拟器的推导结果（只读展示用；`address` 取候选表第一个）。
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedView {
    pub emulator: String,
    pub config: String,
    pub adb_path: String,
    pub address: String,
    pub extras: bool,
    pub root: String,
}

pub fn status_view() -> StatusView {
    let Ok(m) = lock() else {
        return StatusView::default();
    };

    let machine = MachineInfo::load(&config::data_dir());
    let maa_installed = machine
        .as_ref()
        .map(|info| info.maa.is_some())
        .unwrap_or(false);
    let emulators: Vec<String> = machine
        .as_ref()
        .map(|info| info.emulators.iter().map(|e| e.label.clone()).collect())
        .unwrap_or_default();
    let resolved: Vec<ResolvedView> = machine
        .as_ref()
        .map(|info| {
            connection::resolve_all(info, &Overrides::default())
                .into_iter()
                .map(|target| ResolvedView {
                    emulator: target.emulator,
                    config: target.config,
                    adb_path: target.adb_path.to_string_lossy().into_owned(),
                    address: target.addresses.first().cloned().unwrap_or_default(),
                    extras: target.extras,
                    root: target.root.to_string_lossy().into_owned(),
                })
                .collect()
        })
        .unwrap_or_default();

    let Some(bridge) = m.bridge.as_ref() else {
        return StatusView {
            maa_installed,
            last_error: m.last_error.clone(),
            stale: m.stale,
            emulators,
            resolved,
            ..Default::default()
        };
    };

    let bridge_available = bridge.available();
    let connected = match bridge.command(json!({ "cmd": "status" }), QUICK_TIMEOUT) {
        Ok(v) => v.get("connected").and_then(Value::as_bool).unwrap_or(false),
        Err(_) => false,
    };
    // MaaCore 的 `AsstRunning` 只表示「助手已启动、工作线程活着」——队列空着也是 true，
    // 不能当“在跑”。真正的运行状态以运行记录为准：`begin_run` 起，`AllTasksCompleted` 止。
    let running = bridge.run_in_flight();
    let info = m.connected.as_ref();
    StatusView {
        maa_installed,
        bridge_available,
        connected,
        running,
        stale: m.stale,
        last_error: m.last_error.clone(),
        emulator: info.map(|c| c.emulator.clone()),
        address: info.map(|c| c.address.clone()),
        extras: info.map(|c| c.extras).unwrap_or(false),
        screencap: info.and_then(|c| c.screencap.clone()),
        emulators,
        resolved,
    }
}

/// 「测试连接」的真入口：桥在但**没连上**时，真的重连一次。
///
/// 与 `status` 的差别就是里那句 `connect`：连接只在桥启动时做一次，失败之后
/// 桥还活着，光靠 `ensure` 会一路跳过连接——于是“再点一次”只会空查一遍，
/// 点了像没点。
///
/// 连接期间占用管理器锁（起桥 + 连模拟器几十秒），调用方必须不占主线程
/// （命令层走 async + spawn_blocking）；并发进来时第二个直接得到
/// 「正在连接中」，不排到同一条队尾去。
pub fn test_connection() -> Result<(), String> {
    // 领连接权：领不到就说明另一次连接还在跑。
    {
        let mut m = lock()?;
        if m.connecting {
            return Err("正在连接中".to_string());
        }
        m.connecting = true;
    }
    let result = (|| -> Result<(), String> {
        let mut m = lock()?;
        ensure(&mut m)?;
        if m.connected.is_some() {
            // 已经连着：不必推倒重来，直接把状态回给设置页。
            return Ok(());
        }
        m.connect()
    })();
    // 成与不成都要把连接权还回去，否则失败一次就再也按不动了。
    if let Ok(mut m) = lock() {
        m.connecting = false;
    }
    result
}

/// 桥侧状态（`available` / `connected` / `running`）。
///
/// **只保证「桥在」+ 问一句状态**：桥活着但模拟器没连上时它不会重连——
/// 要真连一次走 `test_connection`。名字容易让人以为它会连，别误解。
pub fn status() -> Result<Value, String> {
    let mut m = lock()?;
    ensure(&mut m)?;
    let bridge = m.bridge.as_ref().ok_or("桥不可用")?;
    bridge
        .command(json!({ "cmd": "status" }), QUICK_TIMEOUT)
        .map_err(|e| e.to_string())
}

/// 取走缓冲里的事件（`maa` / `recognition`）。
pub fn drain_events() -> Vec<Value> {
    lock()
        .map(|m| {
            m.bridge
                .as_ref()
                .map(|b| b.drain_events())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// 看缓冲里最近的事件帧（**不取走**）。给 `status` 用。
pub fn peek_events(limit: usize) -> Vec<Value> {
    lock()
        .map(|m| {
            m.bridge
                .as_ref()
                .map(|b| b.peek_events(limit))
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// 当前连着的模拟器（没有则 None）。
pub fn connected() -> Option<ConnectedInfo> {
    lock().ok().and_then(|m| m.connected.clone())
}

/// 上一次失败的原因（成功一次后清空）。
pub fn last_error() -> Option<String> {
    lock().ok().and_then(|m| m.last_error.clone())
}

/// 关掉桥（应用退出时调用）。之后再调用会重新起一个。
pub fn shutdown() {
    let Ok(mut m) = lock() else { return };
    if let Some(bridge) = m.bridge.take() {
        bridge.shutdown();
    }
    m.connected = None;
}

impl Manager {
    fn start(&mut self) -> Result<(), String> {
        let data_dir = config::data_dir();
        let info = MachineInfo::load(&data_dir).ok_or_else(|| {
            "还没有本机信息（machine.json）。请重启 PRTS，或在设置里确认 MAA 安装位置。".to_string()
        })?;
        let Some(maa_dir) = info.maa.clone() else {
            return Err("本机没有检测到 MAA。装好 MAA 再试，或在设置里指定 MAA 路径。".to_string());
        };

        let exe = bridge::resolve_exe().map_err(|e| e.to_string())?;
        let port = bridge::free_port().map_err(|e| e.to_string())?;
        let user_dir = data_dir.join("maa");
        let bridge = Bridge::spawn(&exe, &maa_dir, &user_dir, port).map_err(|e| e.to_string())?;

        if !bridge.available() {
            let reason = bridge
                .ready
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("原因未知")
                .to_string();
            // 桥本身没崩，只是 MaaCore 用不了：留着它（状态可查、能正常关闭），但报错。
            self.bridge = Some(bridge);
            return Err(format!("MAA 不可用：{reason}"));
        }

        self.bridge = Some(bridge);
        self.connect()
    }

    fn connect(&mut self) -> Result<(), String> {
        let mut bridge = self.bridge.take().ok_or("桥没有启动")?;
        let result = self.connect_with(&mut bridge);
        // 成败都把桥放回去：失败也留着，便于看状态、看原因、重试。
        self.bridge = Some(bridge);
        result
    }

    fn connect_with(&mut self, bridge: &mut Bridge) -> Result<(), String> {
        let data_dir = config::data_dir();
        let info = MachineInfo::load(&data_dir).ok_or("读不到 machine.json")?;
        let overrides = self.settings.overrides();
        let installed: Vec<String> = info.emulators.iter().map(|e| e.label.clone()).collect();

        // 指定了模拟器却没检测到 → 明说，不悄悄换一台（那会在别的机器上真跑任务）。
        if let Some(want) = overrides.emulator.as_deref() {
            if !installed.iter().any(|label| label == want) {
                return Err(format!(
                    "设置里指定的模拟器「{want}」没有检测到——请确认它已安装，或把模拟器改回自动。"
                ));
            }
        }

        let targets = connection::resolve_all(&info, &overrides);

        if targets.is_empty() {
            let listed = if installed.is_empty() {
                "本机没有检测到已安装的模拟器".to_string()
            } else {
                format!("本机检测到：{}，但都没找到可用的 adb", installed.join("、"))
            };
            return Err(format!("{listed}。请在设置里确认模拟器与 ADB 路径。"));
        }

        // 探活：先让 adb 认识每个候选地址，再列设备（全新 adb 否则看不到设备）。
        let mut online: Vec<(Resolved, String)> = Vec::new();
        for target in &targets {
            for address in &target.addresses {
                let _ = connection::adb_connect(&target.adb_path, address);
            }
            let devices = connection::adb_devices(&target.adb_path).unwrap_or_default();
            match connection::pick_address(&target.addresses, &devices) {
                Ok(address) => online.push((target.clone(), address)),
                Err(PickError::Ambiguous(hits)) => {
                    return Err(format!(
                        "{} 有多个实例在运行（{}）——请在设置里填写连接地址。",
                        target.emulator,
                        hits.join("、")
                    ));
                }
                Err(PickError::NoOnline) => {}
            }
        }
        let (target, address) = choose_target(online, &installed)?;

        // 截图增强：必须在 connect 之前配 extras，且 config 要传字符串。
        // 生效条件 = 模拟器支持 且 设置里开着（默认开）。
        let use_extras = target.extras && self.settings.screenshot_extras;
        if use_extras {
            // 模拟器路径：用户填了就用它，否则用本机探测到的安装目录。
            let root = {
                let raw = self.settings.emulator_path.trim();
                if raw.is_empty() {
                    target.root.clone()
                } else {
                    std::path::PathBuf::from(raw)
                }
            };
            let extras = connection::extras_json(
                &root,
                self.settings.touch_extras,
                self.settings.server.client_type(),
            );
            bridge
                .command(
                    json!({ "cmd": "set_connection_extras", "name": &target.config, "extras": extras }),
                    QUICK_TIMEOUT,
                )
                .map_err(|e| e.to_string())?;
        }

        bridge
            .command(
                json!({
                    "cmd": "connect_adb",
                    "adb_path": target.adb_path.to_string_lossy(),
                    "address": &address,
                    "config": &target.config,
                }),
                CONNECT_TIMEOUT,
            )
            .map_err(|e| e.to_string())?;

        let (events, screencap) = wait_for_connect(bridge)?;
        self.stale = false;
        self.connected = Some(ConnectedInfo {
            emulator: target.emulator,
            address,
            config: target.config,
            extras: use_extras,
            screencap,
            events,
        });
        Ok(())
    }
}

/// 从一帧里认出「连接完成」信号：None = 不是；Some(Ok) = 成功；Some(Err) = 失败。
///
/// 实测帧（MuMu 12 / MAA v6.18.0，2026-10-06 真机）：
/// `{"code":4,"event":"maa","name":"AsyncCallInfo",
///   "details":{"async_call_id":1,"details":{"cost":2404,"ret":true},"what":"Connect"}}`
///
/// **`ret` 在 `details.details.ret`**（嵌套一层）——写浅一层就会把失败判成成功。
fn parse_connect_settle(frame: &Value) -> Option<Result<(), String>> {
    if frame.get("code").and_then(Value::as_i64) != Some(CODE_ASYNC_CALL_INFO) {
        return None;
    }
    let details = frame.get("details")?;
    if details.get("what").and_then(Value::as_str) != Some("Connect") {
        return None;
    }
    let ret = details
        .get("details")
        .and_then(|d| d.get("ret"))
        .cloned()
        .unwrap_or(Value::Null);
    let failed = matches!(&ret, Value::Bool(false))
        || matches!(ret.as_str(), Some("false") | Some("False") | Some("0"));
    if failed {
        Some(Err(format!("连模拟器失败：{ret}")))
    } else {
        Some(Ok(()))
    }
}

/// 从一帧里取**实际生效**的截图方式。实测帧：
/// `{"code":2,"event":"maa","name":"ConnectionInfo","details":{"what":"FastestWayToScreencap",
///   "details":{"alternatives":[…],"cost":14,"method":"MumuExtras"}}}`
fn parse_screencap(frame: &Value) -> Option<String> {
    let details = frame.get("details")?;
    if details.get("what").and_then(Value::as_str) != Some("FastestWayToScreencap") {
        return None;
    }
    details
        .get("details")
        .and_then(|d| d.get("method"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 连接完成要等**真信号**：`AsyncCallInfo`（码 4）里 `what=Connect` 的那条。
///
/// `ConnectionInfo` 的 `Connected` 早于截图测速 / 触控探测，据此判断会过早
/// （已由真机帧证实）。返回：期间的所有事件 + 实际生效的截图方式。
fn wait_for_connect(bridge: &Bridge) -> Result<(Vec<Value>, Option<String>), String> {
    let deadline = Instant::now() + CONNECT_SETTLE_TIMEOUT;
    let mut seen = Vec::new();
    let mut screencap = None;
    loop {
        for frame in bridge.drain_events() {
            if let Some(method) = parse_screencap(&frame) {
                screencap = Some(method);
            }
            let settle = parse_connect_settle(&frame);
            seen.push(frame);
            match settle {
                Some(Ok(())) => return Ok((seen, screencap)),
                Some(Err(msg)) => return Err(msg),
                None => {}
            }
        }
        if Instant::now() >= deadline {
            // 超时未必是“真没连上”——也可能是这条判定没认出来。把刚才看到的
            // 几帧带出来，下一次就不用盲猜。
            let tail: Vec<String> = seen.iter().rev().take(5).map(|f| f.to_string()).collect();
            return Err(format!(
                "等待连接完成超时（90s）——模拟器可能没启动、adb 不通，或离场信号不在预期位置。最近事件：{}",
                tail.join(" | ")
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 等整条队列跑完（码 3）。到点不报错——任务还在跑，那是派发之后的正常态。
///
/// **不长期持锁**：每一轮只锁一小会儿把事件取走，然后放锁去睡。
fn wait_for_queue(secs: u64) -> Result<(bool, Vec<Value>), String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut seen = Vec::new();
    loop {
        {
            let m = lock()?;
            let Some(bridge) = m.bridge.as_ref() else {
                // 桥没了，没什么可等。
                return Ok((false, seen));
            };
            for frame in bridge.drain_events() {
                let done =
                    frame.get("code").and_then(Value::as_i64) == Some(CODE_ALL_TASKS_COMPLETED);
                seen.push(frame);
                if done {
                    return Ok((true, seen));
                }
            }
        }
        if Instant::now() >= deadline {
            return Ok((false, seen));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 用得上 `client_type` 的任务类型（对照官方 task 文档）：
/// `StartUp` / `CloseDown` 按它选客户端，`Fight` 用它做崩溃重启。
/// 其余类型不塞——多一个字段就是自找 `invalid params`。
const CLIENT_TYPE_TASKS: &[&str] = &["StartUp", "CloseDown", "Fight"];

/// 给用得上 `client_type` 的任务补上用户设置的服务器。
///
/// 用户不需要知道这个字段，更不该自己填——它是服务器设置的产物。
pub(crate) fn with_client_type(type_: &str, params: Value, client_type: &str) -> Value {
    if !CLIENT_TYPE_TASKS.contains(&type_) {
        return params;
    }
    match params {
        Value::Object(mut map) => {
            map.insert("client_type".to_string(), json!(client_type));
            Value::Object(map)
        }
        _ => json!({ "client_type": client_type }),
    }
}

/// 拼「任务被拒绝」的文案：日志里有具体原因就用它，否则退回传进来的退路。
fn append_error_message(type_: &str, log_reason: Option<String>, fallback: String) -> String {
    match log_reason {
        Some(reason) => format!("任务 {type_} 被 MAA 拒绝：{reason}"),
        None => fallback,
    }
}

/// 入队一个任务。
///
/// 失败有**两种样子**：桥直接报错（`AsstAppendTask` 返回 0，桥只报「没被接受」），
/// 或回一个 `task_id = 0` 的哑错误。两种都去日志里找更具体的原因——只有日志分得出
/// 「类型不存在」还是「参数不全」。日志是 append-only，先记长度、只读新增那段。
fn add_task(bridge: &Bridge, type_: &str, params: &Value) -> Result<i64, String> {
    let log_path = asst_log_path();
    let before = file_len(&log_path);
    let reason_from_log = || parse_append_error(&read_log_since(&log_path, before));

    let reply = bridge.command(
        json!({ "cmd": "task_add", "type": type_, "params": params }),
        TASK_TIMEOUT,
    );

    let task_id = match reply {
        Ok(data) => data.get("task_id").and_then(Value::as_i64).unwrap_or(0),
        Err(e) => {
            return Err(append_error_message(
                type_,
                reason_from_log(),
                e.to_string(),
            ));
        }
    };
    if task_id != 0 {
        return Ok(task_id);
    }
    Err(append_error_message(
        type_,
        reason_from_log(),
        format!(
            "任务 {type_} 被 MAA 拒绝（日志里没找到原因，可查 {}）",
            log_path.display()
        ),
    ))
}

/// MaaCore 的日志路径（桥把 user_dir 指向 PRTS 的数据目录）。
fn asst_log_path() -> PathBuf {
    config::data_dir()
        .join("maa")
        .join("debug")
        .join("asst.log")
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// 读 `path` 里自 `offset` 起的内容（最多回看 256KB，防日志过大）。
fn read_log_since(path: &Path, offset: u64) -> String {
    const CAP: u64 = 256 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = offset.min(len).max(len.saturating_sub(CAP));
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut text = String::new();
    let _ = file.take(CAP).read_to_string(&mut text);
    text
}

/// 从日志片段里取最后一条入队失败的原因。
///
/// 引擎把结果写成 `… | task_id: N` / `… | invalid type: X` /
/// `… | invalid params: {…}`，取最后一条（最新的那条）。
pub fn parse_append_error(log: &str) -> Option<String> {
    let mut found = None;
    for line in log.lines() {
        if let Some(at) = line.find("invalid type") {
            let rest = line[at + "invalid type".len()..]
                .trim_start_matches([':', '：', ' '])
                .trim();
            found = Some(if rest.is_empty() {
                "MAA 不认识这个任务类型".to_string()
            } else {
                format!("MAA 不认识任务类型 `{rest}`")
            });
        } else if let Some(at) = line.find("invalid params") {
            let rest = line[at + "invalid params".len()..]
                .trim_start_matches([':', '：', ' '])
                .trim();
            found = Some(if rest.is_empty() {
                "任务类型存在，但参数不全或不合法".to_string()
            } else {
                format!("任务类型存在，但参数不全或不合法：{rest}")
            });
        }
    }
    found
}

/// 在「在线的模拟器」里选一个：没有就报没在跑，一个就用它，多个不猜。
pub fn choose_target(
    mut online: Vec<(Resolved, String)>,
    installed: &[String],
) -> Result<(Resolved, String), String> {
    match online.len() {
        0 => {
            let listed = if installed.is_empty() {
                "本机没有检测到已安装的模拟器".to_string()
            } else {
                format!("已装：{}", installed.join("、"))
            };
            Err(format!("没有运行中的模拟器（{listed}）。请先启动模拟器。"))
        }
        1 => Ok(online.pop().expect("len==1")),
        _ => {
            let names: Vec<String> = online
                .iter()
                .map(|(target, address)| format!("{}（{}）", target.emulator, address))
                .collect();
            Err(format!(
                "同时有多个模拟器在运行：{}。请在设置里指定要连哪一个。",
                names.join("、")
            ))
        }
    }
}

/// 校验并规范化 `tasks`：每项必须是 `{type: 非空字符串, params?: 对象}`。
fn normalize_tasks(tasks: &[Value]) -> Result<Vec<Value>, String> {
    if tasks.is_empty() {
        return Err("tasks 为空：至少给一个任务".to_string());
    }
    let mut out = Vec::with_capacity(tasks.len());
    for (index, task) in tasks.iter().enumerate() {
        let type_ = task
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if type_.is_empty() {
            return Err(format!("第 {} 项缺少 type", index + 1));
        }
        let params = task.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Err(format!("第 {} 项（{type_}）的 params 不是对象", index + 1));
        }
        let mut normalized = json!({ "type": type_ });
        if params.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            normalized["params"] = params;
        }
        out.push(normalized);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(emulator: &str, address: &str) -> (Resolved, String) {
        (
            Resolved {
                emulator: emulator.to_string(),
                config: "General".to_string(),
                adb_path: PathBuf::from("adb.exe"),
                root: PathBuf::from("root"),
                addresses: vec![address.to_string()],
                extras: false,
            },
            address.to_string(),
        )
    }

    #[test]
    fn connect_settle_is_parsed_from_the_real_frame() {
        // 实测帧（MuMu 12 / MAA v6.18.0，2026-10-06 真机，逐字拄录）
        let ok = json!({
            "code": 4, "event": "maa", "name": "AsyncCallInfo",
            "details": { "async_call_id": 1, "details": { "cost": 2404, "ret": true }, "what": "Connect" }
        });
        assert_eq!(parse_connect_settle(&ok), Some(Ok(())));

        let bad = json!({
            "code": 4, "event": "maa", "name": "AsyncCallInfo",
            "details": { "async_call_id": 2, "details": { "cost": 10, "ret": false }, "what": "Connect" }
        });
        assert!(
            matches!(parse_connect_settle(&bad), Some(Err(ref m)) if m.contains("失败")),
            "连接失败必须被认出来（ret 在 details.details 里，写浅一层就漏）"
        );

        // 别的 AsyncCallInfo、别的码，都不算数
        assert_eq!(
            parse_connect_settle(
                &json!({"code": 4, "details": {"what": "Click", "details": {"ret": true}}})
            ),
            None
        );
        assert_eq!(
            parse_connect_settle(&json!({"code": 2, "details": {"what": "Connect"}})),
            None
        );
    }

    #[test]
    fn screencap_method_is_parsed_from_the_real_frame() {
        // 实测帧（同上；MumuExtras 14ms 战胜 Encode 700ms）
        let frame = json!({
            "code": 2, "event": "maa", "name": "ConnectionInfo",
            "details": {
                "what": "FastestWayToScreencap",
                "details": {
                    "alternatives": [
                        {"cost": "312", "method": "RawByNc"},
                        {"cost": "14", "method": "MumuExtras"}
                    ],
                    "cost": 14,
                    "method": "MumuExtras"
                }
            }
        });
        assert_eq!(parse_screencap(&frame).as_deref(), Some("MumuExtras"));
        assert_eq!(
            parse_screencap(&json!({"code": 2, "details": {"what": "Connected"}})),
            None
        );
    }

    #[test]
    fn wait_defaults_follow_task_type() {
        assert_eq!(default_wait_secs("OperBox"), 180);
        assert_eq!(default_wait_secs("Depot"), 60);
        assert_eq!(default_wait_secs("StartUp"), 90);
        assert_eq!(default_wait_secs("Award"), 60, "领奖就一分钟");
        assert_eq!(default_wait_secs("SingleStep"), 0, "实验性功能：默认派发");
        assert_eq!(default_wait_secs("Fight"), 0, "长任务默认不等");
        assert_eq!(default_wait_secs("不认识的类型"), 0);
    }

    #[test]
    fn queue_default_wait_is_sum_unless_any_task_waits_zero() {
        let short = vec![
            json!({ "type": "StartUp" }),
            json!({ "type": "OperBox" }),
            json!({ "type": "Depot" }),
        ];
        assert_eq!(
            queue_default_wait_secs(&short),
            90 + 180 + 60,
            "顺序执行，和就是界"
        );

        let mixed = vec![json!({ "type": "StartUp" }), json!({ "type": "Fight" })];
        assert_eq!(queue_default_wait_secs(&mixed), 0, "含长任务 → 整条不等");

        assert_eq!(
            queue_default_wait_secs(&[json!({ "type": "Roguelike" })]),
            0
        );
    }

    #[test]
    fn client_type_is_injected_only_where_maa_uses_it() {
        let ct = "Bilibili";

        let injected = with_client_type("StartUp", json!({"start_game_enabled": true}), ct);
        assert_eq!(injected["client_type"], json!("Bilibili"));
        assert_eq!(injected["start_game_enabled"], json!(true), "原字段不动");

        // 空 params 也要补出对象
        assert_eq!(
            with_client_type("CloseDown", json!({}), ct)["client_type"],
            json!("Bilibili")
        );
        // Fight 也用它（崩溃重启）
        assert_eq!(
            with_client_type("Fight", json!({"stage": "1-7"}), ct)["client_type"],
            json!("Bilibili")
        );
        // 官方文档里用不到它的类型：不塞
        assert!(with_client_type("Recruit", json!({"select": [4]}), ct)
            .get("client_type")
            .is_none());
        assert!(with_client_type("Depot", json!({}), ct)
            .get("client_type")
            .is_none());
    }

    #[test]
    fn append_error_message_prefers_the_log_reason() {
        assert_eq!(
            append_error_message(
                "OperProgress",
                Some("MAA 不认识任务类型 `OperProgress`".to_string()),
                "桥的原话".to_string()
            ),
            "任务 OperProgress 被 MAA 拒绝：MAA 不认识任务类型 `OperProgress`"
        );
        assert_eq!(
            append_error_message("Fight", None, "桥的原话".to_string()),
            "桥的原话",
            "日志没线索时退回原话（不吞错）"
        );
    }

    #[test]
    fn parse_append_error_reads_last_reason() {
        let log = "\
[INFO] 某行无关
[ERR] append_task | invalid type: OperProgress
[ERR] append_task | invalid type: Nope
";
        let reason = parse_append_error(log).unwrap();
        assert!(reason.contains("Nope"), "取最后一条：{reason}");

        let params =
            parse_append_error("[ERR] append_task | invalid params: {\"stage\":null}\n").unwrap();
        assert!(params.contains("参数不全"), "{params}");

        assert!(parse_append_error("[INFO] append_task | task_id: 3\n").is_none());
    }

    #[test]
    fn choose_target_refuses_to_guess() {
        let installed = vec!["MuMu 12".to_string(), "雷电 LDPlayer".to_string()];

        let none = choose_target(vec![], &installed).unwrap_err();
        assert!(
            none.contains("没有运行中的模拟器") && none.contains("MuMu 12"),
            "{none}"
        );

        let (target, address) =
            choose_target(vec![resolved("MuMu 12", "127.0.0.1:16384")], &installed).unwrap();
        assert_eq!(target.emulator, "MuMu 12");
        assert_eq!(address, "127.0.0.1:16384");

        let many = choose_target(
            vec![
                resolved("MuMu 12", "127.0.0.1:16384"),
                resolved("雷电 LDPlayer", "emulator-5554"),
            ],
            &installed,
        )
        .unwrap_err();
        assert!(many.contains("多个模拟器在运行"), "{many}");
    }

    #[test]
    fn normalize_tasks_validates_shape() {
        assert!(normalize_tasks(&[]).unwrap_err().contains("为空"));
        assert!(normalize_tasks(&[json!({ "params": {} })])
            .unwrap_err()
            .contains("缺少 type"));
        assert!(normalize_tasks(&[json!({ "type": "Fight", "params": [] })])
            .unwrap_err()
            .contains("不是对象"));

        let ok = normalize_tasks(&[
            json!({ "type": "StartUp" }),
            json!({ "type": "Fight", "params": { "stage": "1-7" } }),
        ])
        .unwrap();
        assert_eq!(ok[0], json!({ "type": "StartUp" }), "空 params 不落进结果");
        assert_eq!(ok[1]["params"]["stage"], json!("1-7"));
    }

    /// B 服测试配置。
    fn bilibili_settings() -> MaaSettings {
        MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..MaaSettings::default()
        }
    }

    /// 真机验证（第四步）：给「默认等待时长」取校准数据。
    ///
    /// 一条队列跑 `[StartUp, OperBox, Depot]`（**会启动并操作游戏**），记录：
    /// 每个任务（taskid）的起止时刻、任务内的最大静默、收尾事件的 code + name，
    /// 以及 Depot 是否与 OperBox 一样是“增量 + done”。
    /// 运行：`cargo test --lib maa::manager::tests::wait_budget_timing -- --ignored --nocapture`
    #[test]
    #[ignore = "真机验证：耗时校准（会启动并操作游戏；期间勿操作模拟器）"]
    fn wait_budget_timing() {
        crate::maa::manager::set_settings(bilibili_settings());

        let t0 = Instant::now();
        let outcome = crate::maa::manager::submit(
            &[
                json!({ "type": "StartUp", "params": { "start_game_enabled": true } }),
                json!({ "type": "OperBox" }),
                json!({ "type": "Depot" }),
            ],
            0,
        )
        .expect("派发队列");
        println!(
            "task_ids = {:?}（StartUp / OperBox / Depot）",
            outcome.task_ids
        );

        #[derive(Debug)]
        struct Seen {
            at: f64,
            event: String,
            code: Option<i64>,
            name: String,
            taskid: Option<i64>,
            kind: Option<String>,
            size: usize,
        }
        let mut seen: Vec<Seen> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            for frame in crate::maa::manager::drain_events() {
                seen.push(Seen {
                    at: t0.elapsed().as_secs_f64(),
                    event: frame
                        .get("event")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                    code: frame.get("code").and_then(Value::as_i64),
                    name: frame
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                    taskid: frame.get("taskid").and_then(Value::as_i64),
                    kind: frame
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string()),
                    size: frame.to_string().chars().count(),
                });
            }
            let running = {
                let m = lock().unwrap();
                m.bridge
                    .as_ref()
                    .and_then(|b| {
                        b.command(json!({ "cmd": "status" }), Duration::from_secs(10))
                            .ok()
                    })
                    .and_then(|v| v.get("running").and_then(Value::as_bool))
                    .unwrap_or(false)
            };
            let terminal = seen
                .iter()
                .any(|s| s.name == "AllTasksCompleted" || s.name.ends_with("Error"));
            let quiet = seen
                .last()
                .map(|s| t0.elapsed().as_secs_f64() - s.at)
                .unwrap_or(0.0);
            if !running && (terminal || quiet > 20.0) {
                break;
            }
            if Instant::now() >= deadline {
                println!("⚠️ 600s 仍未完成 — 中止");
                let _ = crate::maa::manager::stop();
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let total = t0.elapsed().as_secs_f64();
        println!("\n总耗时 {:.1}s，事件 {} 条", total, seen.len());

        let mut ids: Vec<i64> = seen.iter().filter_map(|s| s.taskid).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let span: Vec<&Seen> = seen.iter().filter(|s| s.taskid == Some(id)).collect();
            if let (Some(first), Some(last)) = (span.first(), span.last()) {
                let mut gap = 0.0f64;
                for w in span.windows(2) {
                    gap = gap.max(w[1].at - w[0].at);
                }
                println!(
                    "task {id}: {:.1}s → {:.1}s（共 {:.1}s，{} 条，内部最大静默 {:.1}s）",
                    first.at,
                    last.at,
                    last.at - first.at,
                    span.len(),
                    gap
                );
            }
        }

        for s in seen.iter().filter(|s| {
            s.event == "maa"
                && (s.name == "AllTasksCompleted"
                    || s.name.ends_with("Error")
                    || s.name.ends_with("Stopped"))
        }) {
            println!("收尾：{} code={:?} @{:.1}s", s.name, s.code, s.at);
        }

        // 识别结果：每个 kind 的帧数、终帧的 done 与体量。
        let mut kinds: Vec<String> = seen.iter().filter_map(|s| s.kind.clone()).collect();
        kinds.sort();
        kinds.dedup();
        for kind in kinds {
            let frames: Vec<&Seen> = seen
                .iter()
                .filter(|s| s.kind.as_deref() == Some(kind.as_str()))
                .collect();
            if let Some(last) = frames.last() {
                println!(
                    "\n{kind}: {} 帧，终帧 {} 字符（@{:.1}s）",
                    frames.len(),
                    last.size,
                    last.at
                );
            }
        }

        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证（第三步）：识别类任务的完整闭环。
    ///
    /// **只读识别**（OperBox）：会真的操作模拟器（导航 / 滑动 / 识别），约 1-2 分钟。
    /// 验的是：任务能不能跑完、`recognition` 事件的**实际结构**、以及它的体量。
    /// 运行：`cargo test --lib maa::manager::tests::operbox_round_trip -- --ignored --nocapture`
    #[test]
    #[ignore = "真机验证：OperBox 闭环（需先启动模拟器；期间勿操作模拟器）"]
    fn operbox_round_trip() {
        crate::maa::manager::set_settings(bilibili_settings());

        // wait=false：先派出去，然后自己轮询等它跑完（不把锁攥在手里）。
        let outcome =
            crate::maa::manager::submit(&[json!({ "type": "OperBox" })], 0).expect("派发 OperBox");
        println!("task_ids = {:?}", outcome.task_ids);
        println!(
            "connected = {} @ {}",
            outcome.connected.emulator, outcome.connected.address
        );

        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            let running = {
                let m = lock().unwrap();
                m.bridge
                    .as_ref()
                    .and_then(|b| {
                        b.command(json!({ "cmd": "status" }), Duration::from_secs(10))
                            .ok()
                    })
                    .and_then(|v| v.get("running").and_then(Value::as_bool))
                    .unwrap_or(false)
            };
            if !running {
                break;
            }
            if Instant::now() >= deadline {
                println!("⚠️ 240s 仍未跑完 —— 中止");
                let _ = crate::maa::manager::stop();
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }

        let events = crate::maa::manager::drain_events();
        println!("事件 {} 条，按类型：", events.len());
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for frame in &events {
            let key = format!(
                "{}/{}",
                frame.get("event").and_then(Value::as_str).unwrap_or("?"),
                frame.get("name").and_then(Value::as_str).unwrap_or("?")
            );
            *counts.entry(key).or_default() += 1;
        }
        for (key, n) in &counts {
            println!("  {key} × {n}");
        }

        // recognition 帧逐条看：`done` 什么时候翻、每帧多少干员。
        println!("recognition 帧逐条：");
        for frame in events
            .iter()
            .filter(|f| f.get("event").and_then(Value::as_str) == Some("recognition"))
        {
            let details = frame.get("details");
            let own = details
                .and_then(|d| d.get("own_opers"))
                .and_then(|v| v.as_array())
                .map(|a| a.len());
            let all = details
                .and_then(|d| d.get("all_opers"))
                .and_then(|v| v.as_array())
                .map(|a| a.len());
            println!(
                "  kind={:?} done={:?} own={:?} all={:?} chars={}",
                frame.get("kind"),
                details.and_then(|d| d.get("done")),
                own,
                all,
                frame.to_string().chars().count()
            );
        }

        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 派发失败不能留下 in-flight，否则下一次 submit 会被自己的残影挡住。
    ///
    /// 用测试桥注入「命令必失败」，不依赖真实 MAA 或模拟器。
    #[test]
    fn failed_dispatch_does_not_leave_run_in_flight() {
        let bridge = Bridge::test_stub(true);
        {
            let mut m = lock().expect("管理器锁");
            m.bridge = Some(bridge);
            m.connected = Some(ConnectedInfo {
                emulator: "test".to_string(),
                address: "127.0.0.1:0".to_string(),
                config: "test".to_string(),
                extras: false,
                screencap: None,
                events: Vec::new(),
            });
            m.last_error = None;
        }

        let first = submit(&[json!({ "type": "Depot" })], 0).expect_err("add_task 应失败");
        assert!(!first.is_empty());
        assert!(!is_running(), "失败派发不能留下未收尾的运行");

        let second = submit(&[json!({ "type": "Depot" })], 0).expect_err("仍应失败");
        assert_ne!(second, DISPATCH_BUSY_MSG, "第二次不能撞上一次的残留");

        // 清掉全局单例状态，别把测试桥留给后续测试。
        let taken = {
            let mut m = lock().expect("管理器锁");
            (m.bridge.take(), m.connected.take())
        };
        drop(taken);
    }

    /// 中止成功必须立刻收掉运行记录。
    ///
    /// MaaCore 停止时不会补发 AllTasksCompleted；只发 TaskChainStopped。
    /// 若 stop 不主动收尾，设置页会一直显示「MAA 正在运行」。
    #[test]
    fn stop_clears_in_flight_run() {
        let bridge = Bridge::test_stub(false);
        bridge.begin_run(vec![1]);
        {
            let mut m = lock().expect("管理器锁");
            m.bridge = Some(bridge);
            m.connected = Some(ConnectedInfo {
                emulator: "test".to_string(),
                address: "127.0.0.1:0".to_string(),
                config: "test".to_string(),
                extras: false,
                screencap: None,
                events: Vec::new(),
            });
            m.last_error = None;
        }
        assert!(is_running(), "begin_run 之后应视为在跑");

        stop().expect("run_stop 成功后应能收尾");
        assert!(!is_running(), "中止后不能继续显示为运行中");

        let taken = {
            let mut m = lock().expect("管理器锁");
            (m.bridge.take(), m.connected.take())
        };
        drop(taken);
    }

    /// 真机验证（第二步）：哑错误翻译。
    ///
    /// 派一个人为不存在的任务类型（`OperProgress` 在本机 MAA v6.18.0 里没有），
    /// 看能不能从 asst.log 里把原因捞出来翻成人话。**只入队、不执行**。
    #[test]
    #[ignore = "真机验证：哑错误翻译（需先启动模拟器）"]
    fn append_error_is_translated() {
        crate::maa::manager::set_settings(bilibili_settings());

        let log_path = asst_log_path();
        let before = file_len(&log_path);

        match crate::maa::manager::submit(&[json!({ "type": "OperProgress" })], 0) {
            Ok(_) => panic!("OperProgress 在本机 v6.18.0 里不存在，不该入队成功"),
            Err(e) => println!("翻译结果 = {e}"),
        }

        println!("--- asst.log 新增片段 ---");
        print!("{}", read_log_since(&log_path, before));
        println!("--- 结束 ---");

        // 顺带验一件：带 client_type 的任务，MAA 收不收。
        // 只入队、**不运行**（不调 run_start），桥一关队列就随之消失。
        {
            let m = lock().unwrap();
            let bridge = m.bridge.as_ref().expect("刚才连过，桥应该在");
            let params = with_client_type(
                "StartUp",
                json!({ "start_game_enabled": false }),
                "Bilibili",
            );
            match bridge.command(
                json!({ "cmd": "task_add", "type": "StartUp", "params": params }),
                Duration::from_secs(20),
            ) {
                Ok(data) => println!("带 client_type=Bilibili 的 StartUp 入队 = {data}"),
                Err(e) => println!("带 client_type=Bilibili 的 StartUp 被拒：{e}"),
            }
        }

        // 收尾：万一队列里真进了什么东西，停掉它。
        let _ = crate::maa::manager::stop();
        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证（第一步）：连接流程 + 真信号形状 + 实际截图方式。
    ///
    /// **只连接、只看，不派任何任务。** 需要模拟器已启动。
    /// 运行：`cargo test --lib maa::manager::tests::connect_and_introspect -- --ignored --nocapture`
    #[test]
    #[ignore = "真机验证：连模拟器（需先启动模拟器）"]
    fn connect_and_introspect() {
        crate::maa::manager::set_settings(bilibili_settings());

        match crate::maa::manager::status() {
            Ok(v) => println!("bridge status = {v}"),
            Err(e) => panic!("连接没走通：{e}"),
        }

        match crate::maa::manager::connected() {
            Some(info) => {
                println!("emulator = {} @ {}", info.emulator, info.address);
                println!(
                    "config   = {}（extras 请求 = {}）",
                    info.config, info.extras
                );
                println!("连接期事件 {} 条：", info.events.len());
                for (i, frame) in info.events.iter().take(60).enumerate() {
                    println!("  [{i}] {frame}");
                }
            }
            None => println!("（没有 connected 记录——连接可能没走完）"),
        }

        println!("view = {:?}", crate::maa::manager::status_view());
        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：起桥 → 握手 → 连发两条命令 → 关桥。
    ///
    /// **只加载 MaaCore，不连模拟器、不碰游戏窗口。**
    /// 运行：`cargo test --lib maa::manager::tests::bridge_smoke -- --ignored --nocapture`
    #[test]
    #[ignore = "真机验证，手动运行"]
    fn bridge_smoke() {
        let data_dir = crate::config::data_dir();
        let info = MachineInfo::load(&data_dir).expect("需要 machine.json（先跑一次 PRTS 生成）");
        let maa_dir = info.maa.clone().expect("本机需要已安装 MAA");
        let exe = bridge::resolve_exe().expect("要能定位桥可执行文件");
        let port = bridge::free_port().expect("取空闲端口");
        let user_dir = data_dir.join("maa");

        let bridge = Bridge::spawn(&exe, &maa_dir, &user_dir, port).expect("起桥");
        println!("ready    = {}", bridge.ready);
        assert!(bridge.is_alive(), "起完之后桥该是活的");

        // 连发两条：验证读线程 + 待回答表在多次命令之间不串。
        for round in 1..=2 {
            let status = bridge
                .command(json!({ "cmd": "status" }), Duration::from_secs(20))
                .unwrap_or_else(|e| panic!("第 {round} 次 status 失败：{e}"));
            println!("status({round}) = {status}");
            assert!(status.get("available").is_some(), "status 该带 available");
        }

        println!("缓冲事件 = {} 条", bridge.drain_events().len());
        bridge.shutdown();
        println!("已请求桥退出");
    }

    /// 真机验证：派发互斥——已有任务在跑时，第二次 submit 必须被拒。
    ///
    /// **不连模拟器、不碰游戏**：管理器里放一个真桥，造一个「未收尾的运行」，
    /// 只验闸的位置与判据（submit 的锁内、ensure 之后）。
    /// 运行：`cargo test --lib maa::manager::tests::dispatch_mutex -- --ignored --nocapture`
    #[test]
    #[ignore = "真机验证，手动运行"]
    fn dispatch_mutex() {
        let data_dir = crate::config::data_dir();
        let info = MachineInfo::load(&data_dir).expect("需要 machine.json（先跑一次 PRTS 生成）");
        let maa_dir = info.maa.clone().expect("本机需要已安装 MAA");
        let exe = bridge::resolve_exe().expect("要能定位桥可执行文件");
        let port = bridge::free_port().expect("取空闲端口");
        let user_dir = data_dir.join("maa");

        let bridge = Bridge::spawn(&exe, &maa_dir, &user_dir, port).expect("起桥");
        // 真跑任务才会置「未收尾」；这里直接造出来，第二个 submit 就该被拒。
        bridge.begin_run(vec![1]);
        assert!(bridge.run_in_flight(), "begin_run 之后应视为在跑");

        {
            let mut m = lock().expect("管理器锁");
            m.bridge = Some(bridge);
            // connected 留空：ensure 只在「没有桥」时才起桥连接，
            // 而闸又排在连接检查之前，所以这样就能单独验闸。
            m.connected = None;
        }

        let err = submit(&[json!({ "type": "Depot" })], 0).expect_err("在跑时就该拒");
        println!("拒绝理由 = {err}");
        assert_eq!(err, DISPATCH_BUSY_MSG);

        // 收尾：清空管理器状态并关桥，不留孤儿进程
        let taken = {
            let mut m = lock().expect("管理器锁");
            (m.bridge.take(), m.connected.take())
        };
        if let Some(b) = taken.0 {
            b.shutdown();
        }
    }
}
