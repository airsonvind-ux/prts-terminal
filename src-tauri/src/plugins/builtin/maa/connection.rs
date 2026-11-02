//! connection —— 连接参数：把本机信息推导成 MAA 的 `(config, adb_path, address)`。
//!
//! adb 在哪、候选地址是什么，属于平台事实，自 M2a 起归
//! [`crate::platform::adb`]；本模块只补 MAA 侧的两样知识：**连接配置名**与
//! **截图增强开关**，然后拼成 [`Resolved`]。
//!
//! 取值全部对照 MAA 的实现（改表时请一并核对上游，尤其大版本更新后）：
//!
//! - **连接配置名**：`src/MaaWpfGui/ViewModels/UserControl/Settings/ConnectSettingsUserControlModel.cs`
//!   （`ConnectConfig` 枚举）；
//! - 候选地址与 adb 相对路径：见 `platform/adb.rs` 的 `PROFILES`。
//!
//! 本层只做推导，不启进程、不发命令：真正的连接由管理器发起。
//!
//! **表的外键是 `machine.rs` 里的模拟器标签**，与 `platform::adb::PROFILES`
//! 必须同名；改一处要改两处（有测试守着）。

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::machine::MachineInfo;
use crate::platform::adb;

/// 一种模拟器的 MAA 侧参数（adb 事实在 `platform::adb`）。
#[derive(Debug, Clone, Copy)]
struct EmulatorConn {
    /// 传给 MAA 的连接配置名（`AsstConnect` / 桥 `connect_adb` 的 `config`）。
    config: &'static str,
    /// 是否支持截图增强（决定连接前是否配 extras）。
    extras: bool,
}

/// `machine.rs` 的模拟器标签 → MAA 侧参数。
const TABLE: &[(&str, EmulatorConn)] = &[
    (
        "MuMu 12",
        EmulatorConn {
            config: "MuMuEmulator12",
            extras: true,
        },
    ),
    (
        "雷电 LDPlayer",
        EmulatorConn {
            config: "LDPlayer",
            extras: true,
        },
    ),
    (
        "BlueStacks",
        EmulatorConn {
            config: "BlueStacks",
            extras: false,
        },
    ),
    ("夜神 Nox", EmulatorConn { config: "Nox", extras: false }),
    ("逍遥 XYAZ", EmulatorConn { config: "XYAZ", extras: false }),
    (
        "腾讯 Androws",
        EmulatorConn {
            config: "Androws",
            extras: false,
        },
    ),
];

/// 用户在设置里给的覆盖项（定义在平台层，这里只转出去）。
pub use crate::platform::adb::Overrides;

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

/// 按本机信息解析出**所有**可用连接目标。
///
/// 多装几个模拟器就返回多条（由调用方探活挑在线的那个）；找不到 adb 的跳过。
/// 用户在设置里指定了 adb 路径时，直接按该路径解析（配置名取通用方言 `General`）。
pub fn resolve_all(info: &MachineInfo, over: &Overrides) -> Vec<Resolved> {
    adb::resolve_targets(info, over)
        .into_iter()
        .map(|target| {
            let conn = TABLE
                .iter()
                .find(|(label, _)| *label == target.label)
                .map(|(_, conn)| conn);
            Resolved {
                emulator: target.label,
                // 配置名由模拟器类型唯一决定，不可覆盖（见设计文档 §3）；
                // 手填 adb 路径的情形认不得是哪家，用通用方言。
                config: conn.map_or("General", |c| c.config).to_string(),
                adb_path: target.adb_path,
                root: target.root,
                addresses: target.addresses,
                extras: conn.is_some_and(|c| c.extras),
            }
        })
        .collect()
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

/// 让 adb 先认识这个地址（转发到平台层，保留旧入口以免管理器大改）。
pub fn adb_connect(adb: &Path, address: &str) -> Result<(), String> {
    crate::platform::adb::connect(adb, address)
}

/// 列 `adb devices` 里处于 `device` 状态的序列号（转发到平台层）。
pub fn adb_devices(adb: &Path) -> Result<Vec<String>, String> {
    crate::platform::adb::devices(adb)
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
    fn every_profile_has_maa_config_and_vice_versa() {
        for profile in adb::PROFILES {
            assert!(
                TABLE.iter().any(|(label, _)| *label == profile.label),
                "MAA 侧缺少模拟器「{}」的连接配置",
                profile.label
            );
        }
        for (label, _) in TABLE {
            assert!(
                adb::profile(label).is_some(),
                "平台层缺少模拟器「{label}」的 adb 事实"
            );
        }
    }

    #[test]
    fn resolves_config_and_extras_from_label() {
        let root = temp_root("config");
        std::fs::create_dir_all(root.join("shell")).unwrap();
        std::fs::write(root.join("shell").join("adb.exe"), b"x").unwrap();

        let resolved = resolve_all(&machine_with("MuMu 12", root.clone()), &Overrides::default());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].config, "MuMuEmulator12");
        assert!(resolved[0].extras, "MuMu 支持截图增强");
        assert_eq!(
            resolved[0].addresses.first().map(String::as_str),
            Some("127.0.0.1:16384")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn custom_adb_path_uses_general_config_and_no_extras() {
        let root = temp_root("custom");
        let adb = root.join("adb.exe");
        std::fs::write(&adb, b"x").unwrap();

        let over = Overrides {
            emulator: None,
            adb_path: Some(adb.to_string_lossy().into_owned()),
            address: None,
        };
        let resolved = resolve_all(&MachineInfo::default(), &over);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].emulator, "自定义");
        assert_eq!(resolved[0].config, "General");
        assert!(!resolved[0].extras);
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
    fn extras_json_carries_path_touch_and_client_type() {
        let raw = extras_json(Path::new("D:\\Netease\\MuMuPlayer-12.0"), true, "Bilibili");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["touch"], json!(true));
        assert_eq!(value["client_type"], json!("Bilibili"));
        assert!(value["path"].as_str().unwrap().contains("MuMuPlayer-12.0"));
    }
}
