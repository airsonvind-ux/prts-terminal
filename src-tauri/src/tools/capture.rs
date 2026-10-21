//! capture_game_screen —— 截取《明日方舟》当前的游戏画面。
//!
//! **与 MAA 完全独立**：不走 MAA、不起桥、不需要管理员权限。两条通道按目标选路：
//!
//! - **模拟器**：`adb exec-out screencap -p`。读的是设备内部帧，所以模拟器窗口
//!   最小化或被遮挡时都能截；必须用 `exec-out`（`adb shell` 会把 PNG 的换行字节
//!   按 Windows 规则改写，咬坏图片）。
//! - **PC 端**：Windows Graphics Capture。从合成器层取画面，与目标进程的权限级别
//!   无关（实测：连读进程名都被拒的受保护进程，捕获照样成功）。窗口最小化时
//!   DWM 不合成、WGC 不送帧，所以先做 `IsIconic` 预检，再留超时兜底。
//!
//! 落点：`<data>/screenshots/`（唯一名 + `latest.png` 展示别名 + 保留 N 张）。
//! 与 MAA 的截图（`<data>/maa/screenshots/`）在目录上就是分开的。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config;
use crate::maa::connection;
use crate::maa::settings::MaaSettings;
use crate::machine::MachineInfo;

use super::{DisplayLink, Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 展示别名文件名（卡片链接指向它）。
const LATEST_FILE: &str = "latest.png";
/// 保留的截图份数（唯一名文件；`latest.png` 不计、永不删）。
const KEPT: usize = 100;
/// PNG 签名。
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
/// PC 端游戏窗口标题（认窗口按标题，不读进程名——受保护进程会被拒）。
const PC_WINDOW_TITLE: &str = "明日方舟";
/// 找设备的**总**预算：超了就收手，直接给结论。
///
/// 设备没注册时每条 `adb connect` 都要等一次 TCP 超时，十多条凑起来就是十几秒——
/// 一次「看一眼」不该为一句结论等那么久。
const DISCOVERY_BUDGET: Duration = Duration::from_millis(2500);
/// 「列设备 + 空则重试」这一段的时间上限。
///
/// 为什么重试：`adb devices` 是**一次快照**。adb 服务冷启动（PRTS 刚开后第一次
/// 调用）或模拟器刚起来时，第一列常常是空的，而设备几百毫秒后就注册上了。
const LIST_BUDGET: Duration = Duration::from_millis(1200);
/// 重试列设备之间的间隔。
const LIST_RETRY_GAP: Duration = Duration::from_millis(300);
/// 单条 connect 的等待上限（不超总预算）。
const CONNECT_EACH: Duration = Duration::from_millis(1200);

/// 截图落在哪一端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Emulator,
    Pc,
}

pub struct CaptureGameScreen;

impl Tool for CaptureGameScreen {
    fn name(&self) -> &str {
        "capture_game_screen"
    }

    fn description(&self) -> &str {
        concat!(
            "截取《明日方舟》当前的游戏画面，返回一张图片。\n",
            "\n",
            "模拟器与 PC 端均适用；PC 端要求游戏窗口未最小化。PC 端通常一秒内返回；",
            "模拟器端要经 adb 取设备帧，慢一些，设备还没挂到 adb 上时首次调用会再多花几秒。\n",
            "\n参数：\n",
            "  target：可选，\"emulator\" | \"pc\"；省略时自动选择，两端都在时需指定。"
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "enum": ["emulator", "pc"],
                    "description": "省略时自动选择。"
                }
            }
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        // 切片 = 目标名（给了 target 才写）+ latest.png 链接（三态都在）。
        let label = serde_json::from_str::<Value>(params)
            .ok()
            .and_then(|v| v.get("target").and_then(Value::as_str).map(str::to_string))
            .and_then(|t| match t.as_str() {
                "emulator" => Some("模拟器".to_string()),
                "pc" => Some("PC 端".to_string()),
                _ => None,
            });
        let latest = latest_path();
        let mut display = ToolDisplay::link(DisplayLink::file(
            latest.to_string_lossy().into_owned(),
            LATEST_FILE,
        ));
        if let Some(label) = label {
            display.text = label;
        }
        display
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let requested = parse_target(params)?;
        let out = unique_path().map_err(ToolError::Execution)?;

        // 自动选择时才去问世界（显式指定就只探那一端）。
        let mut emulator: Option<Result<(PathBuf, String), String>> = None;
        let mut pc_window: Option<Result<isize, String>> = None;
        let target = match requested {
            Some(target) => target,
            None => {
                emulator = Some(find_emulator());
                pc_window = Some(find_pc_window());
                decide(
                    None,
                    emulator.as_ref().is_some_and(Result::is_ok),
                    pc_window.as_ref().is_some_and(Result::is_ok),
                )
                .map_err(ToolError::Execution)?
            }
        };

        match target {
            Target::Emulator => {
                let found = match emulator {
                    Some(found) => found,
                    None => find_emulator(),
                };
                let (adb, serial) = found.map_err(ToolError::Execution)?;
                capture_emulator(&adb, &serial, &out).map_err(ToolError::Execution)?;
            }
            Target::Pc => {
                let found = match pc_window {
                    Some(found) => found,
                    None => find_pc_window(),
                };
                let hwnd = found.map_err(ToolError::Execution)?;
                capture_pc(hwnd, &out).map_err(ToolError::Execution)?;
            }
        }

        finish(&out).map_err(ToolError::Execution)
    }

    /// 截图会向磁盘写唯一文件并更新 `latest.png`，串行执行。
    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

// ── 参数与目标解析（纯逻辑，单测覆盖） ──

/// 解析 `target` 参数；缺省 = 自动。
fn parse_target(params: &str) -> Result<Option<Target>, ToolError> {
    let trimmed = params.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| ToolError::Parse(e.to_string()))?;
    match value.get("target").and_then(Value::as_str) {
        None => Ok(None),
        Some("emulator") => Ok(Some(Target::Emulator)),
        Some("pc") => Ok(Some(Target::Pc)),
        Some(other) => Err(ToolError::Parse(format!(
            "target 只接受 \"emulator\" 或 \"pc\"，收到：{other}"
        ))),
    }
}

/// 定哪一端：显式请求优先；自动时要求候选唯一（两端都在 → 报清楚让调用方指定）。
fn decide(requested: Option<Target>, emulator_ok: bool, pc_ok: bool) -> Result<Target, String> {
    if let Some(target) = requested {
        return Ok(target);
    }
    match (emulator_ok, pc_ok) {
        (true, false) => Ok(Target::Emulator),
        (false, true) => Ok(Target::Pc),
        (true, true) => {
            Err("模拟器与 PC 端都在运行，请指定 target：\"emulator\" 或 \"pc\"。".to_string())
        }
        (false, false) => {
            Err("没有找到可以截取的画面：没有检测到模拟器设备，PC 端游戏也未启动。".to_string())
        }
    }
}

// ── 落点 ──

/// 截图落点目录：`<data>/screenshots/`。
fn shots_dir() -> PathBuf {
    config::data_dir().join("screenshots")
}

fn latest_path() -> PathBuf {
    shots_dir().join(LATEST_FILE)
}

/// 本次截图的不可变落点（附件引用它，`latest.png` 只是给人看的展示别名）。
fn unique_path() -> Result<PathBuf, String> {
    let dir = shots_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建截图目录失败（{}）：{e}", dir.display()))?;
    Ok(dir.join(crate::attachments::unique_name("screenshot", "png")))
}

/// 截图写好后的交付：贴 `latest.png` 别名 → 裁剪旧图 → 交图给模型。
fn finish(shot: &Path) -> Result<ToolOutput, String> {
    let latest = latest_path();
    let _ = std::fs::copy(shot, &latest);
    if let Some(dir) = latest.parent() {
        prune(dir, KEPT);
    }
    let mut output = ToolOutput::text(format!("已截取当前画面（{}）。", latest.display()));
    // 图交给模型：走附件通路（引用不可变的唯一文件）。
    output.images = vec![shot.to_string_lossy().into_owned()];
    Ok(output)
}

/// 裁剪旧截图：只数唯一名文件，保留最新 `keep` 张，其余按最旧先删。
///
/// `latest.png` 与其它文件一概不动。删除时打一行日志（不静默）。
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut shots: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("screenshot_") || !name.ends_with(".png") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        shots.push((modified, entry.path()));
    }
    if shots.len() <= keep {
        return;
    }
    shots.sort_by_key(|(time, _)| *time);
    let overflow = shots.len() - keep;
    for (_, path) in shots.iter().take(overflow) {
        if std::fs::remove_file(path).is_ok() {
            eprintln!("screenshot_prune: 删除旧截图 {}", path.display());
        }
    }
}

// ── 模拟器通道 ──

/// 找到可用的 adb 与唯一在线设备。
///
/// 复用 MAA 那套「本机信息 + 设置覆盖」解析出 adb 路径，但**不**用地址候选表：
/// 取帧只需要设备序列号，`adb devices` 现查最稳（实测这台机器上 MuMu 的序列号是
/// `emulator-5554`，并不在候选地址表里）。
fn find_emulator() -> Result<(PathBuf, String), String> {
    let dir = config::data_dir();
    let info = MachineInfo::load(&dir).unwrap_or_else(|| MachineInfo::detect(None));
    let resolved = connection::resolve_all(&info, &load_settings().overrides());
    if resolved.is_empty() {
        return Err("没有找到可用的 adb：模拟器未安装，或 adb 路径未配置。".to_string());
    }

    // ① 先列设备，空就短暂重试。`adb devices` 是一次快照：adb 服务冷启动（PRTS 刚
    //    开后的第一次调用）或模拟器刚起来时，第一列往往是空的，随后才注册上。
    let list_deadline = Instant::now() + LIST_BUDGET;
    let deadline = Instant::now() + DISCOVERY_BUDGET;
    loop {
        for target in &resolved {
            if let Some(serial) = pick_device(connection::adb_devices(&target.adb_path))? {
                return Ok((target.adb_path.clone(), serial));
            }
        }
        if Instant::now() >= list_deadline {
            break;
        }
        std::thread::sleep(LIST_RETRY_GAP);
    }

    // ② 仍没有 → 在总预算内对候选地址补一次 connect（有些模拟器要靠它才注册），再列。
    for target in &resolved {
        for address in &target.addresses {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = connect_with_timeout(&target.adb_path, address, CONNECT_EACH.min(left));
        }
    }
    for target in &resolved {
        if let Some(serial) = pick_device(connection::adb_devices(&target.adb_path))? {
            return Ok((target.adb_path.clone(), serial));
        }
    }

    Err("没有检测到模拟器设备：模拟器可能未运行，或尚未连接上。".to_string())
}

/// 从一次 `adb devices` 结果里挑设备：0 台 → None；1 台 → 它；多台 → 明确报错。
fn pick_device(listed: Result<Vec<String>, String>) -> Result<Option<String>, String> {
    let devices = listed.unwrap_or_default();
    match devices.len() {
        0 => Ok(None),
        1 => Ok(Some(devices[0].clone())),
        n => Err(format!("检测到 {n} 台在线设备，请只保留一台运行后重试。")),
    }
}

/// 读 `<data>/settings.json` 的 `maa` 段（与设置页落盘同一份）。
fn load_settings() -> MaaSettings {
    let path = config::data_dir().join("settings.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("maa").cloned())
        .and_then(|maa| serde_json::from_value::<MaaSettings>(maa).ok())
        .unwrap_or_default()
}

/// 有上限的 `adb connect`：超时就把子进程杀掉。
///
/// 为什么要自己造一个：`connection::adb_connect` 没有超时，而它服务于 MAA 连接
/// （那里慢一点可以接受，也不该为一个截图去改它）。截图要立刻回话，所以给它预算。
fn connect_with_timeout(adb: &Path, address: &str, timeout: Duration) -> Result<(), String> {
    let mut command = std::process::Command::new(adb);
    command
        .args(["connect", address])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("启动 adb connect 失败：{e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("adb connect 失败：{address}"))
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("adb connect 超时：{address}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("等待 adb connect 失败：{e}")),
        }
    }
}

/// 截一帧设备画面。
fn capture_emulator(adb: &Path, serial: &str, out: &Path) -> Result<(), String> {
    let mut command = std::process::Command::new(adb);
    command
        .arg("-s")
        .arg(serial)
        .arg("exec-out")
        .arg("screencap")
        .arg("-p")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().map_err(|e| format!("启动 adb 失败：{e}"))?;
    let mut file = std::fs::File::create(out).map_err(|e| format!("创建截图文件失败：{e}"))?;
    {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "adb 输出不可读。".to_string())?;
        std::io::copy(&mut stdout, &mut file).map_err(|e| format!("读取 adb 输出失败：{e}"))?;
    }
    let status = child.wait().map_err(|e| format!("等 adb 结束失败：{e}"))?;
    if !status.success() {
        return Err("模拟器取帧失败：adb 返回了失败状态。".to_string());
    }
    verify_png(out)
}

/// 校验落盘的是不是 PNG（防「取到了别的东西却当图交出去」）。
fn verify_png(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取截图失败：{e}"))?;
    if bytes.len() < PNG_MAGIC.len() || bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
        return Err("取到的不是有效图片。".to_string());
    }
    Ok(())
}

// ── PC 端通道 ──

/// 找 PC 端游戏窗口。找不到 = 游戏没启动。
#[cfg(windows)]
fn find_pc_window() -> Result<isize, String> {
    let hwnd = pc::find_window()?;
    Ok(hwnd.0 as isize)
}

#[cfg(not(windows))]
fn find_pc_window() -> Result<isize, String> {
    Err("PC 端截图仅在 Windows 上可用。".to_string())
}

/// 截 PC 端游戏窗口一帧（含最小化预检与标题栏裁剪）。
#[cfg(windows)]
fn capture_pc(hwnd: isize, out: &Path) -> Result<(), String> {
    if pc::is_minimized(hwnd) {
        return Err("PC 端游戏窗口已最小化，请还原后重试。".to_string());
    }
    let title_bar = pc::title_bar_height().unwrap_or(0);
    pc::capture_frame(out)?;
    if title_bar > 0 {
        crop_top(out, title_bar)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn capture_pc(_hwnd: isize, _out: &Path) -> Result<(), String> {
    Err("PC 端截图仅在 Windows 上可用。".to_string())
}

/// 裁掉顶部 `top` 像素（窗口化时帧里含标题栏；无边框全屏时 `top = 0`，不裁）。
fn crop_top(path: &Path, top: u32) -> Result<(), String> {
    let image = image::open(path).map_err(|e| format!("读取截图失败：{e}"))?;
    let (width, height) = (image.width(), image.height());
    if top == 0 || top >= height {
        return Ok(());
    }
    image
        .crop_imm(0, top, width, height - top)
        .save(path)
        .map_err(|e| format!("写出截图失败：{e}"))
}

#[cfg(windows)]
mod pc {
    use std::path::Path;
    use std::sync::mpsc::{self, Sender};
    use std::time::Duration;

    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic};
    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::encoder::ImageFormat;
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };
    use windows_capture::window::Window;

    /// PC 端取帧上限：窗口最小化时 WGC 不送帧，没有上限会一直等（实测确认）。
    const CAPTURE_TIMEOUT: Duration = Duration::from_secs(6);

    /// 按标题 + 类名找窗口（不读进程名：受保护进程会被拒）。
    pub fn find_window() -> Result<HWND, String> {
        let hwnd = unsafe { FindWindowW(w!("UnityWndClass"), w!("明日方舟")) }.unwrap_or_default();
        if hwnd.is_invalid() {
            return Err("PC 端游戏未启动。".to_string());
        }
        Ok(hwnd)
    }

    pub fn is_minimized(hwnd: isize) -> bool {
        unsafe { IsIconic(HWND(hwnd as *mut _)) }.as_bool()
    }

    /// 标题栏高度（像素）；取不到就当 0（不裁）。
    pub fn title_bar_height() -> Option<u32> {
        let window = Window::from_name(super::PC_WINDOW_TITLE).ok()?;
        window.title_bar_height().ok()
    }

    /// 抓一帧并写 PNG。看门狗：超时即判失败（不挂住）。
    pub fn capture_frame(out: &Path) -> Result<(), String> {
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let out = out.to_string_lossy().into_owned();

        std::thread::spawn(move || {
            let window = match Window::from_name(super::PC_WINDOW_TITLE) {
                Ok(window) => window,
                Err(_) => {
                    let _ = tx.send(Err("PC 端游戏未启动。".to_string()));
                    return;
                }
            };
            let settings = Settings::new(
                window,
                CursorCaptureSettings::Default,
                DrawBorderSettings::Default,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Rgba8,
                (out, tx.clone()),
            );
            if Shot::start(settings).is_err() {
                let _ = tx.send(Err("PC 端取帧失败：无法开始捕获。".to_string()));
            }
        });

        match rx.recv_timeout(CAPTURE_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err("PC 端取帧超时，请确认游戏窗口可见。".to_string()),
        }
    }

    struct Shot {
        out: String,
        done: Sender<Result<(), String>>,
    }

    impl GraphicsCaptureApiHandler for Shot {
        type Flags = (String, Sender<Result<(), String>>);
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self {
                out: ctx.flags.0.clone(),
                done: ctx.flags.1.clone(),
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            let result = frame
                .save_as_image(&self.out, ImageFormat::Png)
                .map_err(|e| e.to_string());
            let _ = self.done.send(result);
            control.stop();
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            let _ = self
                .done
                .send(Err("PC 端取帧失败：窗口已关闭。".to_string()));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_maps_and_rejects() {
        assert_eq!(parse_target("").unwrap(), None);
        assert_eq!(parse_target("{}").unwrap(), None);
        assert_eq!(
            parse_target(r#"{"target":"pc"}"#).unwrap(),
            Some(Target::Pc)
        );
        assert_eq!(
            parse_target(r#"{"target":"emulator"}"#).unwrap(),
            Some(Target::Emulator)
        );
        assert!(parse_target(r#"{"target":"pc端"}"#).is_err());
        assert!(parse_target("不是 json").is_err());
    }

    #[test]
    fn decide_requires_unique_candidate_when_auto() {
        assert_eq!(decide(None, true, false).unwrap(), Target::Emulator);
        assert_eq!(decide(None, false, true).unwrap(), Target::Pc);
        let both = decide(None, true, true).unwrap_err();
        assert!(both.contains("都在运行"), "{both}");
        let none = decide(None, false, false).unwrap_err();
        assert!(none.contains("没有找到可以截取的画面"), "{none}");
    }

    #[test]
    fn decide_honours_explicit_request() {
        assert_eq!(decide(Some(Target::Pc), true, true).unwrap(), Target::Pc);
        assert_eq!(
            decide(Some(Target::Emulator), false, false).unwrap(),
            Target::Emulator
        );
    }

    #[test]
    fn pick_device_only_accepts_a_single_one() {
        assert_eq!(pick_device(Ok(vec![])).unwrap(), None);
        assert_eq!(
            pick_device(Ok(vec!["emulator-5554".to_string()])).unwrap(),
            Some("emulator-5554".to_string())
        );
        let many = pick_device(Ok(vec!["a".into(), "b".into()])).unwrap_err();
        assert!(many.contains("2 台"), "{many}");
        // 列不出来（例如 adb 报错）按「没有设备」处理，交给上层给结论。
        assert_eq!(pick_device(Err("boom".into())).unwrap(), None);
    }

    #[test]
    fn shots_land_next_to_a_screenshots_dir() {
        let latest = latest_path();
        assert_eq!(latest.file_name().unwrap(), LATEST_FILE);
        assert_eq!(latest.parent().unwrap().file_name().unwrap(), "screenshots");
    }

    #[test]
    fn verify_png_accepts_real_png_and_rejects_junk() {
        let dir = std::env::temp_dir().join(format!("prts-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.png");
        std::fs::write(&good, PNG_MAGIC).unwrap();
        assert!(verify_png(&good).is_ok());

        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"not a png at all").unwrap();
        assert!(verify_png(&bad).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crop_top_keeps_the_bottom_part() {
        let dir = std::env::temp_dir().join(format!("prts-crop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frame.png");
        let img = image::RgbaImage::new(8, 10);
        img.save(&path).unwrap();

        crop_top(&path, 4).unwrap();
        let after = image::open(&path).unwrap();
        assert_eq!(after.width(), 8);
        assert_eq!(after.height(), 6);

        // top = 0（无边框全屏）不动画面。
        crop_top(&path, 0).unwrap();
        assert_eq!(image::open(&path).unwrap().height(), 6);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_newest_and_leaves_the_alias() {
        let dir = std::env::temp_dir().join(format!("prts-cap-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LATEST_FILE), b"alias").unwrap();
        std::fs::write(dir.join("unrelated.txt"), b"keep").unwrap();
        for i in 0..5u32 {
            std::fs::write(dir.join(format!("screenshot_{i}.png")), b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        prune(&dir, 3);

        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("screenshot_"))
            .collect();
        assert_eq!(left.len(), 3, "留最新 3 张：{left:?}");
        assert!(left.contains(&"screenshot_4.png".to_string()));
        assert!(left.contains(&"screenshot_2.png".to_string()));
        assert!(dir.join(LATEST_FILE).exists(), "别名不动");
        assert!(dir.join("unrelated.txt").exists(), "非截图文件不动");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 实机验证（默认忽略；需要真实模拟器 / PC 端在跑） ──

    /// 模拟器通道：adb 取一帧。
    #[test]
    #[ignore]
    fn live_capture_emulator() {
        let output = CaptureGameScreen
            .execute(&ToolContext::new(), r#"{"target":"emulator"}"#)
            .expect("模拟器取帧应成功");
        println!("captured: {:?}", output.images);
        assert!(!output.images.is_empty(), "应有一张图交给模型");
    }

    /// PC 端通道：WGC 取一帧（含标题栏裁剪）。
    #[test]
    #[ignore]
    fn live_capture_pc() {
        let output = CaptureGameScreen
            .execute(&ToolContext::new(), r#"{"target":"pc"}"#)
            .expect("PC 端取帧应成功");
        println!("captured: {:?}", output.images);
        assert!(!output.images.is_empty(), "应有一张图交给模型");
    }
}
