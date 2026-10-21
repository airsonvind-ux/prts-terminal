//! settings —— MAA 设置段（落 `settings.json` 的 `maa` 段，运行期由
//! `manager` 持有）。
//!
//! 这一段里的字段**全部与"怎么连、连哪个"有关**，所以任何一项变化都要让现有
//! 连接作废（见 `manager::set_settings`）——绝不拿着旧连接继续跑。

use serde::{Deserialize, Serialize};

use super::connection::Overrides;

/// 账号服务器。喂给任务的 `client_type`，也喂给 MuMu 截图增强的 extras。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Server {
    Official,
    Bilibili,
}

impl Default for Server {
    fn default() -> Self {
        Self::Official
    }
}

impl Server {
    /// MAA 侧要的取值（`client_type` / MuMu extras 的 `client_type`）。
    pub fn client_type(self) -> &'static str {
        match self {
            Self::Official => "Official",
            Self::Bilibili => "Bilibili",
        }
    }

    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Official => "官服",
            Self::Bilibili => "B服",
        }
    }
}

/// MAA 设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaaSettings {
    /// 账号服务器（用户手填——工具不会自己判断）。
    #[serde(default)]
    pub server: Server,
    /// 自动检测连接（默认开）：开着时忽略下面的三项覆盖，按本机情况自动判定。
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    /// 截图增强（MuMu / 雷电）。不可用时 MAA 自行降级为普通截图，只是慢。
    #[serde(default = "default_true")]
    pub screenshot_extras: bool,
    /// 触控增强（MuMu / 雷电）。与模拟器的「应用保活」有冲突。
    #[serde(default)]
    pub touch_extras: bool,
    /// 指定模拟器（取值 = `machine.json` 里的标签；空 = 自动）。
    #[serde(default)]
    pub emulator: String,
    /// 模拟器路径覆盖（空 = 用本机探测到的安装目录）。截图增强要用它。
    #[serde(default)]
    pub emulator_path: String,
    /// ADB 路径覆盖（空 = 自动判定）。
    #[serde(default)]
    pub adb_path: String,
    /// 连接地址覆盖（空 = 自动判定）。
    #[serde(default)]
    pub address: String,
}

fn default_true() -> bool {
    true
}

impl Default for MaaSettings {
    fn default() -> Self {
        Self {
            server: Server::default(),
            auto_detect: true,
            screenshot_extras: true,
            touch_extras: false,
            emulator: String::new(),
            emulator_path: String::new(),
            adb_path: String::new(),
            address: String::new(),
        }
    }
}

impl MaaSettings {
    /// 连接覆盖项：自动检测开着 → 一律不覆盖；否则空串（或全空白）= 不覆盖。
    pub fn overrides(&self) -> Overrides {
        if self.auto_detect {
            return Overrides::default();
        }
        let pick = |raw: &str| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        Overrides {
            emulator: pick(&self.emulator),
            adb_path: pick(&self.adb_path),
            address: pick(&self.address),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_agreed_design() {
        let s = MaaSettings::default();
        assert_eq!(s.server, Server::Official);
        assert!(s.screenshot_extras, "截图增强默认开");
        assert!(!s.touch_extras, "触控增强默认关");
        assert!(s.overrides().is_empty_like(), "默认全部自动");
    }

    #[test]
    fn serde_round_trip_is_kebab_case() {
        let v = serde_json::to_value(Server::Bilibili).unwrap();
        assert_eq!(v, serde_json::json!("bilibili"));
        let back: Server = serde_json::from_value(serde_json::json!("official")).unwrap();
        assert_eq!(back, Server::Official);

        // 缺字段全走 default（旧文件 / 新增段兼容）
        let s: MaaSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(s, MaaSettings::default());
        assert!(s.screenshot_extras);
        assert!(s.auto_detect);
    }

    #[test]
    fn auto_detect_ignores_overrides() {
        let mut s = MaaSettings::default();
        assert!(s.auto_detect, "自动检测默认开");
        s.address = "127.0.0.1:9999".to_string();
        assert!(
            s.overrides().is_empty_like(),
            "自动检测开着时，就算填了也不覆盖"
        );

        s.auto_detect = false;
        let o = s.overrides();
        assert_eq!(o.address.as_deref(), Some("127.0.0.1:9999"));
    }

    #[test]
    fn overrides_treat_blank_as_auto() {
        let mut s = MaaSettings::default();
        s.auto_detect = false;
        s.adb_path = " D:\\mumu\\shell\\adb.exe ".to_string();
        s.emulator = "MuMu 12".to_string();
        let o = s.overrides();
        assert_eq!(o.emulator.as_deref(), Some("MuMu 12"));
        assert_eq!(
            o.adb_path.as_deref(),
            Some("D:\\mumu\\shell\\adb.exe"),
            "首尾空白去掉"
        );
        assert_eq!(
            MaaSettings {
                auto_detect: false,
                adb_path: "   ".to_string(),
                ..MaaSettings::default()
            }
            .overrides()
            .adb_path,
            None,
            "全空白 = 不覆盖"
        );
    }

    #[test]
    fn server_maps_to_maa_values() {
        assert_eq!(Server::Official.client_type(), "Official");
        assert_eq!(Server::Bilibili.client_type(), "Bilibili");
        assert_eq!(Server::Official.label(), "官服");
        assert_eq!(Server::Bilibili.label(), "B服");
    }
}
