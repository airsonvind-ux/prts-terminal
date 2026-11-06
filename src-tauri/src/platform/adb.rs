//! adb —— adb 定位与设备发现（平台设施，**不是能力**）。
//!
//! 这里放的是「本机上 adb 在哪、它在跟谁说话」——纯查询与无副作用的进程调用，
//! 不碰插件数据、不越出本机，因此按 D33 的补充条款留在 `platform/`，crate 内
//! 可见，不注册成能力、不需要插件声明。
//!
//! 谁在用：
//! - `tools/capture.rs`：截图通道经 adb 取设备帧；
//! - `plugins/builtin/maa/connection.rs`：MAA 连接前要 adb 路径、候选地址与探活。
//!
//! **两处都只用这里的事实，不互相引用**（M2a 的解耦目标：`tools/` 里不再出现
//! `crate::maa::…`）。
//!
//! 表的外键是 `machine.rs` 里的模拟器标签，`plugins/builtin/maa/connection.rs`
//! 的 `TABLE` 必须与 [`PROFILES`] 同名同序；改一处要改两处（有测试守着）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::machine::MachineInfo;

/// 一种模拟器的 adb 事实：装在哪、可能连到哪些地址。
///
/// `config` / `extras` 是 MAA 侧的知识（连接配置名、截图增强），不放在这里——
/// 平台层不知道 MAA 的存在。
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    /// 与 `machine.rs` 的模拟器标签同名。
    pub label: &'static str,
    /// adb 可执行文件的候选相对路径（相对安装目录，按序取第一个存在的）。
    pub adb_rel: &'static [&'static str],
    /// 候选连接地址（地址或设备序列号，按序探活）。
    pub addresses: &'static [&'static str],
}

/// 已知模拟器的 adb 事实。
///
/// 取值对照 MAA 的实现（改表时请一并核对上游）：
/// `src/MaaWpfGui/Helper/WinAdapter.cs` 的 `_adbRelativePathDict`、
/// `src/MaaWpfGui/ViewModels/UserControl/Settings/ConnectSettingsUserControlModel.cs`
/// 的 `ConnectConfig` 与 `DefaultAddress`。
///
/// 上游的 adb 相对路径是相对**进程路径**写的（它扫运行中的模拟器进程），这里换成
/// 相对**安装目录**——PRTS 从注册表拿到的是安装目录（见 `machine.rs`）。
pub const PROFILES: &[Profile] = &[
    Profile {
        label: "MuMu 12",
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
    },
    Profile {
        label: "雷电 LDPlayer",
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
    },
    Profile {
        label: "BlueStacks",
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
    },
    Profile {
        label: "夜神 Nox",
        adb_rel: &["nox_adb.exe"],
        addresses: &["127.0.0.1:62001", "127.0.0.1:59865"],
    },
    Profile {
        label: "逍遥 XYAZ",
        adb_rel: &["adb.exe"],
        addresses: &["127.0.0.1:21503"],
    },
    Profile {
        label: "腾讯 Androws",
        // adb 在 `Application\<版本>\adb.exe` 下，版本号不定 → 交给目录扫。
        adb_rel: &[],
        addresses: &["127.0.0.1:5555"],
    },
];

/// 用户在设置里给的覆盖项（空 = 用自动值）。
///
/// 语义上属于 MAA 设置，但内容是纯 adb 事实，故落在平台层：`MaaSettings`
/// 只把它转出去，不自己定义一份。
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

/// 一个解析出来的 adb 目标（还没探活）。
#[derive(Debug, Clone)]
pub struct Target {
    /// 模拟器展示名（`machine.rs` 的标签；走覆盖时为「自定义」）。
    pub label: String,
    /// 模拟器安装目录。
    pub root: PathBuf,
    /// adb 可执行文件。
    pub adb_path: PathBuf,
    /// 待探活的候选地址（用户指定地址时只有一条）。
    pub addresses: Vec<String>,
}

/// 按标签取模拟器档案。
pub fn profile(label: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.label == label)
}

/// 在模拟器安装目录下找 adb：先按候选相对路径，再（仅无候选者，如应用宝）扫
/// `Application\<版本>\adb.exe`。
fn find_adb(root: &Path, profile: &Profile) -> Option<PathBuf> {
    for rel in profile.adb_rel {
        let candidate = root.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if profile.adb_rel.is_empty() {
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

/// 按本机信息解析出**所有**可用 adb 目标。
///
/// 多装几个模拟器就返回多条（由调用方探活挑在线的那个）；找不到 adb 的跳过。
/// 用户在设置里指定了 adb 路径时，直接按该路径解析（标签为「自定义」）。
pub fn resolve_targets(info: &MachineInfo, over: &Overrides) -> Vec<Target> {
    if let Some(adb) = non_empty(over.adb_path.as_deref()) {
        return vec![Target {
            label: "自定义".to_string(),
            root: PathBuf::from(adb)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            adb_path: PathBuf::from(adb),
            addresses: non_empty(over.address.as_deref())
                .map(|a| vec![a.to_string()])
                .unwrap_or_default(),
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
        let Some(profile) = profile(&emulator.label) else {
            continue;
        };
        let Some(adb_path) = find_adb(&emulator.path, profile) else {
            continue;
        };
        out.push(Target {
            label: emulator.label.clone(),
            root: emulator.path.clone(),
            adb_path,
            addresses: match non_empty(over.address.as_deref()) {
                Some(address) => vec![address.to_string()],
                None => profile.addresses.iter().map(|s| s.to_string()).collect(),
            },
        });
    }
    out
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// 造一个不弹控制台窗口的 adb 命令。
pub fn command(adb: &Path) -> Command {
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
/// [`devices`] 判定。这一步是必要的：全新起的 adb 还没认识模拟器时，
/// 直接列 `adb devices` 会是空的。
pub fn connect(adb: &Path, address: &str) -> Result<(), String> {
    adb_command(adb)
        .arg("connect")
        .arg(address)
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("执行 adb connect 失败（{}）：{e}", adb.display()))?;
    Ok(())
}

/// 列 `adb devices` 里处于 `device` 状态的序列号。
pub fn devices(adb: &Path) -> Result<Vec<String>, String> {
    let output = adb_command(adb)
        .arg("devices")
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| format!("执行 adb devices 失败（{}）：{e}", adb.display()))?;
    Ok(parse_devices(&String::from_utf8_lossy(&output.stdout)))
}

/// 解析 `adb devices` 的输出：只收 `device` 状态的序列号。
fn parse_devices(text: &str) -> Vec<String> {
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
    devices
}

fn adb_command(adb: &Path) -> Command {
    let mut command = command(adb);
    // 输出要读（`devices` 的 stdout），所以这里不能继承父进程的 stdout。
    command.stdout(Stdio::piped());
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Emulator;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prts-adb-{tag}-{}", std::process::id()));
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

        let resolved = resolve_targets(
            &machine_with("MuMu 12", root.clone()),
            &Overrides::default(),
        );
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].label, "MuMu 12");
        assert_eq!(resolved[0].adb_path, root.join("shell").join("adb.exe"));
        assert_eq!(
            resolved[0].addresses.first().map(String::as_str),
            Some("127.0.0.1:16384")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_emulator_without_adb() {
        let root = temp_root("empty");
        let resolved = resolve_targets(
            &machine_with("MuMu 12", root.clone()),
            &Overrides::default(),
        );
        assert!(resolved.is_empty(), "装目录里没有 adb 就不该给出目标");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_androws_adb_under_version_dir() {
        let root = temp_root("androws");
        let version_dir = root.join("Application").join("5.10.56.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("adb.exe"), b"x").unwrap();

        let resolved = resolve_targets(
            &machine_with("腾讯 Androws", root.clone()),
            &Overrides::default(),
        );
        assert_eq!(resolved.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn user_address_override_wins() {
        let root = temp_root("override");
        std::fs::create_dir_all(root.join("shell")).unwrap();
        std::fs::write(root.join("shell").join("adb.exe"), b"x").unwrap();

        let over = Overrides {
            emulator: None,
            adb_path: None,
            address: Some("127.0.0.1:9999".to_string()),
        };
        let resolved = resolve_targets(&machine_with("MuMu 12", root.clone()), &over);
        assert_eq!(resolved[0].addresses, vec!["127.0.0.1:9999".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_adb_path_wins_and_is_labelled_custom() {
        let root = temp_root("explicit");
        let adb = root.join("adb.exe");
        std::fs::write(&adb, b"x").unwrap();

        let over = Overrides {
            emulator: None,
            adb_path: Some(adb.to_string_lossy().into_owned()),
            address: Some("127.0.0.1:7000".to_string()),
        };
        let resolved = resolve_targets(&MachineInfo::default(), &over);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].label, "自定义");
        assert_eq!(resolved[0].adb_path, adb);
        assert_eq!(resolved[0].addresses, vec!["127.0.0.1:7000".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
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
                Emulator {
                    label: "MuMu 12".to_string(),
                    path: mumu.clone(),
                },
                Emulator {
                    label: "雷电 LDPlayer".to_string(),
                    path: ld.clone(),
                },
            ],
            ..Default::default()
        };

        let auto = resolve_targets(&info, &Overrides::default());
        assert_eq!(auto.len(), 2, "不指定时两家都给");

        let only = Overrides {
            emulator: Some("雷电 LDPlayer".to_string()),
            ..Default::default()
        };
        let picked = resolve_targets(&info, &only);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].label, "雷电 LDPlayer");

        let _ = std::fs::remove_dir_all(&mumu);
        let _ = std::fs::remove_dir_all(&ld);
    }

    #[test]
    fn parses_only_online_devices() {
        let text = "* daemon started successfully *\nList of devices attached\nemulator-5554\tdevice\n127.0.0.1:16384\toffline\n\n";
        assert_eq!(parse_devices(text), vec!["emulator-5554".to_string()]);
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }
}
