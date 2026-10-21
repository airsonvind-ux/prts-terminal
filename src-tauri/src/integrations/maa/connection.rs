//! connection —— 连接参数：把本机信息推导成 MAA 的 `(config, adb_path, address)`。
//!
//! 取值全部对照 MAA 的实现（改表时请一并核对上游，尤其大版本更新后）：
//!
//! - **连接配置名**与**候选地址**：`src/MaaWpfGui/ViewModels/UserControl/Settings/ConnectSettingsUserControlModel.cs`
//!   （`ConnectConfig` 枚举 + `DefaultAddress` 表）；
//! - **adb 相对路径**：`src/MaaWpfGui/Helper/WinAdapter.cs` 的 `_adbRelativePathDict`。
//!   上游是相对**进程路径**写的（它扫运行中的模拟器进程），本模块换成相对
//!   **安装目录**——PRTS 从注册表拿到的是安装目录（见 `machine.rs`）。
//!
//! 本层只做推导，不启进程、不发命令：真正的连接由管理器发起。
//!
//! **表的外键是 `machine.rs` 里的模拟器标签**，两处必须同名；改一处要改两处。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

use crate::machine::MachineInfo;

/// 一种模拟器的连接参数。
#[derive(Debug, Clone, Copy)]
pub struct EmulatorConn {
    /// 传给 MAA 的连接配置名（`AsstConnect` / 桥 `connect_adb` 的 `config`）。
    pub config: &'static str,
    /// adb 可执行文件的候选相对路径（相对安装目录，按序取第一个存在的）。
    pub adb_rel: &'static [&'static str],
    /// 候选连接地址（地址或设备序列号，按序探活）。
    pub addresses: &'static [&'static str],
    /// 是否支持截图增强（决定连接前是否配 extras）。
    pub extras: bool,
}

/// `machine.rs` 的模拟器标签 → 连接参数。
pub const TABLE: &[(&str, EmulatorConn)] = &[
    (
        "MuMu 12",
        EmulatorConn {
            config: "MuMuEmulator12",
            adb_rel: &["shell\\adb.exe", "vmonitor\\bin\\adb_server.exe", "adb.exe"],
            addresses: &[
                "127.0.0.1:16384",
                "127.0.0.1:16416",
                "127.0.0.1:16448",
                "127.0.0.1:16480",
                "127.0.0.1:16512",
                "127.0.0.1:16544",
                "127.0.0.1:16576",
            ],
            extras: true,
        },
    ),
    (
        "雷电 LDPlayer",
        EmulatorConn {
            config: "LDPlayer",
            adb_rel: &["adb.exe"],
            addresses: &[
                "emulator-5554",
                "emulator-5556",
                "emulator-5558",
                "emulator-5560",
                "127.0.0.1:5555",
                "127.0.0.1:5557",
                "127.0.0.1:5559",
                "127.0.0.1:5561",
            ],
            extras: true,
        },
    ),
    (
        "BlueStacks",
        EmulatorConn {
            config: "BlueStacks",
            adb_rel: &["HD-Adb.exe", "Engine\\ProgramFiles\\HD-Adb.exe"],
            addresses: &[
                "127.0.0.1:5555",
                "127.0.0.1:5556",
                "127.0.0.1:5565",
                "127.0.0.1:5575",
                "127.0.0.1:5585",
                "127.0.0.1:5595",
                "127.0.0.1:5554",
            ],
            extras: false,
        },
    ),
    (
        "夜神 Nox",
        EmulatorConn {
            config: "Nox",
            adb_rel: &["nox_adb.exe"],
            addresses: &["127.0.0.1:62001", "127.0.0.1:59865"],
            extras: false,
        },
    ),
    (
        "逍遥 XYAZ",
        EmulatorConn {
            config: "XYAZ",
            adb_rel: &["adb.exe"],
            addresses: &["127.0.0.1:21503"],
            extras: false,
        },
    ),
    (
        "腾讯 Androws",
        EmulatorConn {
            config: "Androws",
            // adb 在 `Application\<版本>\adb.exe` 下，版本号不定 → 交给目录扫。
            adb_rel: &[],
            addresses: &["127.0.0.1:5555"],
            extras: false,
        },
    ),
];

/// 用户在设置里给的覆盖项（空 = 用自动值）。
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    /// 指定模拟器（`machine.json` 里的标签）；None = 自动挑正在跑的那台。
    pub emulator: Option<String>,
    pub adb_path: Option<String>,
    pub address: Option<String>,
}

impl Overrides {
    /// 是否什么都不覆盖（全自动）。
    pub fn is_empty_like(&self) -> bool {
        self.emulator.is_none() && self.adb_path.is_none() && self.address.is_none()
    }
}

/// 一个解析好的连接目标。
#[derive(Debug, Clone)]
pub struct Resolved {
    /// 模拟器展示名（`machine.rs` 的标签；走覆盖时为「自定义」）。
    pub emulator: String,
    pub config: String,
    pub adb_path: PathBuf,
    /// 模拟器安装目录（截图增强的 extras 要用）。
    pub root: PathBuf,
    /// 待探活的候选地址（用户指定地址时只有一条）。
    pub addresses: Vec<String>,
    pub extras: bool,
}

/// 在模拟器安装目录下找 adb：先按候选相对路径，再（仅无候选者，如应用宝）扫
/// `Application\<版本>\adb.exe`。
fn find_adb(root: &Path, conn: &EmulatorConn) -> Option<PathBuf> {
    for rel in conn.adb_rel {
        let candidate = root.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if conn.adb_rel.is_empty() {
        if let Ok(entries) = std::fs::read_dir(root.join("Application")) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("adb.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// 按本机信息解析出**所有**可用连接目标。
///
/// 多装几个模拟器就返回多条（由调用方探活挑在线的那个）；找不到 adb 的跳过。
/// 用户在设置里指定了 adb 路径时，直接按该路径解析（配置名取覆盖值或 `General`）。
pub fn resolve_all(info: &MachineInfo, over: &Overrides) -> Vec<Resolved> {
    if let Some(adb) = non_empty(over.adb_path.as_deref()) {
        return vec![Resolved {
            emulator: "自定义".to_string(),
            // 手填 ADB 路径的情形：不认得是哪家，用通用方言。
            config: "General".to_string(),
            root: PathBuf::from(adb)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            adb_path: PathBuf::from(adb),
            addresses: non_empty(over.address.as_deref())
                .map(|a| vec![a.to_string()])
                .unwrap_or_default(),
            extras: false,
        }];
    }

    let mut out = Vec::new();
    for emulator in &info.emulators {
        // 用户指定了模拟器 → 只认这一家。
        if let Some(want) = over.emulator.as_deref() {
            if emulator.label != want {
                continue;
            }
        }
        let Some((_, conn)) = TABLE.iter().find(|(label, _)| *label == emulator.label) else {
            continue;
        };
        let Some(adb_path) = find_adb(&emulator.path, conn) else {
            continue;
        };
        out.push(Resolved {
            emulator: emulator.label.clone(),
            // 配置名由模拟器类型唯一决定，不可覆盖（见设计文档 §3）。
            config: conn.config.to_string(),
            root: emulator.path.clone(),
            adb_path,
            addresses: match non_empty(over.address.as_deref()) {
                Some(address) => vec![address.to_string()],
                None => conn.addresses.iter().map(|s| s.to_string()).collect(),
            },
            extras: conn.extras,
        });
    }
    out
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// 挑地址时的两种「没法自动决定」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickError {
    /// 候选里没有在线设备——模拟器没开。
    NoOnline,
    /// 多个候选同时在线（同一家开了多实例）——需要用户指定。
    Ambiguous(Vec<String>),
}

impl std::fmt::Display for PickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PickError::NoOnline => write!(f, "没有检测到运行中的模拟器实例"),
            PickError::Ambiguous(hits) => write!(
                f,
                "同一模拟器有多个实例在运行（{}），需要在设置里指定连接地址",
                hits.join("、")
            ),
        }
    }
}

/// 在候选地址里挑一个连上去。
///
/// - 只有一台在线 → 它；
/// - 一台都没有 → `NoOnline`；
/// - 多于一台 → `Ambiguous`。
///
/// **多台时不猜**：猜错就是在另一台实例上真跑任务，代价远大于让用户指定一次。
pub fn pick_address(candidates: &[String], online: &[String]) -> Result<String, PickError> {
    let hits: Vec<String> = candidates
        .iter()
        .filter(|c| online.iter().any(|d| d == *c))
        .cloned()
        .collect();
    match hits.len() {
        1 => Ok(hits.into_iter().next().unwrap()),
        0 => Err(PickError::NoOnline),
        _ => Err(PickError::Ambiguous(hits)),
    }
}

/// 造一个不弹控制台窗口的 adb 命令。
fn adb_command(adb: &Path) -> Command {
    let mut command = Command::new(adb);
    command.stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// 让 adb 先认识这个地址（`adb connect <addr>`）。
///
/// 连不上不当作错误——不在线的地址本来就连不上，是否在线由随后的
/// `adb_devices` 判定。这一步是必要的：全新起的 adb 还没认识模拟器时，
/// 直接列 `adb devices` 会是空的。
pub fn adb_connect(adb: &Path, address: &str) -> Result<(), String> {
    let output = adb_command(adb)
        .arg("connect")
        .arg(address)
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("执行 adb connect 失败（{}）：{e}", adb.display()))?;
    let _ = output;
    Ok(())
}

/// 列 `adb devices` 里处于 `device` 状态的序列号。
pub fn adb_devices(adb: &Path) -> Result<Vec<String>, String> {
    let output = adb_command(adb)
        .arg("devices")
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| format!("执行 adb devices 失败（{}）：{e}", adb.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut devices = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // 跳过表头与 adb 自己的状态行（`* daemon started successfully *`）。
        if line.is_empty() || line.starts_with('*') || line.starts_with("List of devices") {
            continue;
        }
        let mut parts = line.split_whitespace();
        if let (Some(serial), Some(state)) = (parts.next(), parts.next()) {
            if state == "device" {
                devices.push(serial.to_string());
            }
        }
    }
    Ok(devices)
}

/// 组截图增强的 extras（连接**前**交给桥的 `set_connection_extras`，值是字符串）。
///
/// `client_type` 必须与账号服一致，否则 MuMu 的显示 id 查错、增强失效。
pub fn extras_json(emulator_path: &Path, touch: bool, client_type: &str) -> String {
    json!({
        "path": emulator_path.to_string_lossy(),
        "touch": touch,
        "client_type": client_type,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Emulator;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prts-maa-conn-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn machine_with(label: &str, path: PathBuf) -> MachineInfo {
        MachineInfo {
            emulators: vec![Emulator {
                label: label.to_string(),
                path,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn resolves_mumu_from_install_dir() {
        let root = temp_root("mumu");
        std::fs::create_dir_all(root.join("shell")).unwrap();
        std::fs::write(root.join("shell").join("adb.exe"), b"x").unwrap();

        let resolved = resolve_all(&machine_with("MuMu 12", root.clone()), &Overrides::default());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].config, "MuMuEmulator12");
        assert_eq!(resolved[0].adb_path, root.join("shell").join("adb.exe"));
        assert!(resolved[0].extras, "MuMu 支持截图增强");
        assert_eq!(resolved[0].addresses.first().map(String::as_str), Some("127.0.0.1:16384"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_emulator_without_adb() {
        let root = temp_root("empty");
        let resolved = resolve_all(&machine_with("MuMu 12", root.clone()), &Overrides::default());
        assert!(resolved.is_empty(), "装目录里没有 adb 就不该给出目标");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_androws_adb_under_version_dir() {
        let root = temp_root("androws");
        let version_dir = root.join("Application").join("5.10.56.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("adb.exe"), b"x").unwrap();

        let resolved = resolve_all(
            &machine_with("腾讯 Androws", root.clone()),
            &Overrides::default(),
        );
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].config, "Androws");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn user_address_override_wins_but_config_stays_derived() {
        let root = temp_root("override");
        std::fs::create_dir_all(root.join("shell")).unwrap();
        std::fs::write(root.join("shell").join("adb.exe"), b"x").unwrap();

        let over = Overrides {
            emulator: None,
            adb_path: None,
            address: Some("127.0.0.1:9999".to_string()),
        };
        let resolved = resolve_all(&machine_with("MuMu 12", root.clone()), &over);
        assert_eq!(resolved[0].addresses, vec!["127.0.0.1:9999".to_string()]);
        assert_eq!(
            resolved[0].config, "MuMuEmulator12",
            "配置名跟着模拟器类型，用户改不了"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pick_address_needs_a_unique_online_instance() {
        let candidates: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_address(&candidates, &["b".to_string()]).unwrap(), "b");
        assert_eq!(
            pick_address(&candidates, &[]),
            Err(PickError::NoOnline),
            "一台都没在跑 → 不猜"
        );
        assert_eq!(
            pick_address(&candidates, &["a".to_string(), "c".to_string()]),
            Err(PickError::Ambiguous(vec!["a".to_string(), "c".to_string()])),
            "多开 → 不替用户选"
        );
        assert_eq!(pick_address(&[], &[]), Err(PickError::NoOnline));
    }

    #[test]
    fn picks_only_the_named_emulator() {
        let mumu = temp_root("pick-mumu");
        std::fs::create_dir_all(mumu.join("shell")).unwrap();
        std::fs::write(mumu.join("shell").join("adb.exe"), b"x").unwrap();
        let ld = temp_root("pick-ld");
        std::fs::write(ld.join("adb.exe"), b"x").unwrap();

        let info = MachineInfo {
            emulators: vec![
                Emulator { label: "MuMu 12".to_string(), path: mumu.clone() },
                Emulator { label: "雷电 LDPlayer".to_string(), path: ld.clone() },
            ],
            ..Default::default()
        };

        let auto = resolve_all(&info, &Overrides::default());
        assert_eq!(auto.len(), 2, "不指定时两家都给");

        let only = Overrides { emulator: Some("雷电 LDPlayer".to_string()), ..Default::default() };
        let picked = resolve_all(&info, &only);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].config, "LDPlayer");

        let _ = std::fs::remove_dir_all(&mumu);
        let _ = std::fs::remove_dir_all(&ld);
    }

    #[test]
    fn extras_json_carries_path_touch_and_client_type() {
        let raw = extras_json(Path::new("D:\\Netease\\MuMuPlayer-12.0"), true, "Bilibili");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["touch"], json!(true));
        assert_eq!(value["client_type"], json!("Bilibili"));
        assert!(value["path"].as_str().unwrap().contains("MuMuPlayer-12.0"));
    }
}
