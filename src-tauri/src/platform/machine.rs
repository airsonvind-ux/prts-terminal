//! machine —— 本机信息探测（模拟器 / PC 端 / MAA 的**安装态**信号）
//!
//! 分层探测（见 `_本机信息-设计.md` §2.1 / §2.2）：
//!   1. 用户设置（MAA 路径等，最高优先）
//!   2. 注册表 Uninstall 枚举（**三个根都要扫**：HKLM-64 / HKLM-WOW6432Node / HKCU）
//!      ——键名是随机 GUID，只能按 `DisplayName` / `Publisher` 认；
//!      `InstallLocation` 可能为空，需回落剥 `UninstallString`。
//!   3. 盘根浅扫特征文件（兜底；便携版 MAA 注册表里没有，只能靠这层）
//!   4. 特征文件嗅探（确认目录真是它）
//!
//! 两路产出：① 值维度（存在 / 模拟器类型列表 / 官B服）交 `notices`；
//! ② 路径持久化到 `data_dir/machine.json`，供**启动存活检查**与工具层解析。
//!
//! 时机（§2.3）：首次启动全量；之后启动只做 `all_alive()` 存活检查，失活才升级全量。
//!
//! **探测只是尽力而为的提示，真值以用户设置为准。**

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// PC 端服务器（官 / B 服两套分发、互不复用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Server {
    Official,
    Bilibili,
}

impl Server {
    pub fn label(self) -> &'static str {
        match self {
            Server::Official => "官服",
            Server::Bilibili => "B服",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Emulator {
    /// 展示用类型名（如 `MuMu 12`）——值维度报的就是它。
    pub label: String,
    /// 安装目录（内部用，不入值维度）。
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PcClient {
    pub server: Server,
    /// 启动器目录（内部用）。
    pub launcher_dir: PathBuf,
    /// 游戏目录（含 `Arknights.exe`；内部用）。
    pub game_dir: PathBuf,
}

/// 本机信息探测结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfo {
    #[serde(default)]
    pub maa: Option<PathBuf>,
    #[serde(default)]
    pub emulators: Vec<Emulator>,
    #[serde(default)]
    pub pc_clients: Vec<PcClient>,
}

/// 模拟器识别表（关键字小写比对；`DisplayName` 命中即认）。
const EMULATORS: &[(&str, &[&str])] = &[
    ("MuMu 12", &["mumu"]),
    ("雷电 LDPlayer", &["雷电", "ldplayer", "dnplayer"]),
    ("BlueStacks", &["bluestacks"]),
    ("夜神 Nox", &["夜神", "nox"]),
    ("逍遥 XYAZ", &["逍遥", "memu"]),
    ("腾讯 Androws", &["androws"]),
];

/// PC 端启动器识别关键字（官 / B 服都注册为「鹰角启动器」，只能靠它定位安装目录）。
const LAUNCHER_KEYS: &[&str] = &["鹰角", "hypergryph"];

/// 便携版 MAA 的盘根浅扫标记。
const MAA_MARKER: &str = "MaaCore.dll";
/// PC 端游戏可执行文件标记（目录名两服不同，故不看名、看它）。
const PC_MARKER: &str = "Arknights.exe";
/// B 服平台组件（存在 → B 服）。
const BILI_MARKER: &str = "BLPlatform64";
/// 官服 Hypergryph SDK（存在 → 官服）。
const OFFICIAL_MARKER: &str = "hgsdk.dll";

impl MachineInfo {
    /// 三值维度文案：`(maa, emulator, game_pc)`。空 → `未检测到`。
    pub fn dim_values(&self) -> (String, String, String) {
        let maa = if self.maa.is_some() {
            "已安装"
        } else {
            "未检测到"
        };

        let emulator = if self.emulators.is_empty() {
            "未检测到".to_string()
        } else {
            let mut labels: Vec<&str> = self.emulators.iter().map(|e| e.label.as_str()).collect();
            labels.sort_unstable();
            labels.dedup();
            labels.join("；")
        };

        let mut servers: Vec<&str> = self.pc_clients.iter().map(|p| p.server.label()).collect();
        servers.sort_unstable();
        servers.dedup();
        let game_pc = if servers.is_empty() {
            "未检测到".to_string()
        } else {
            servers.join("；")
        };

        (maa.to_string(), emulator, game_pc)
    }

    /// 存活检查：所有已记路径（连同标记）是否仍在。
    ///
    /// 常见情形（什么都没变）→ 全健在，μs 级返回；一旦某路径失活
    /// （卸载 / 移动）→ 调用方升级为全量重探。
    pub fn all_alive(&self) -> bool {
        if let Some(p) = &self.maa {
            if !p.join(MAA_MARKER).is_file() {
                return false;
            }
        }
        for e in &self.emulators {
            if !e.path.is_dir() {
                return false;
            }
        }
        for p in &self.pc_clients {
            if !p.game_dir.join(PC_MARKER).is_file() {
                return false;
            }
        }
        true
    }

    /// 从 `dir/machine.json` 载入（缺 / 坏 → None）。
    pub fn load(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join("machine.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 写回 `dir/machine.json`（失败仅 eprintln 告警，不阻断）。
    pub fn save(&self, dir: &Path) {
        std::fs::create_dir_all(dir).ok();
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(dir.join("machine.json"), text) {
                    eprintln!("[machine] 写入失败：{e}");
                }
            }
            Err(e) => eprintln!("[machine] 序列化失败：{e}"),
        }
    }

    /// 全量探测（首次启动 / 手动 / 设置变更）。
    ///
    /// `maa_override`：用户在设置页填的 MAA 路径（空 / None = 不指定）。
    pub fn detect(maa_override: Option<&str>) -> Self {
        #[cfg(windows)]
        {
            let entries = registry::uninstall_entries();
            Self::detect_from(&entries, maa_override)
        }
        #[cfg(not(windows))]
        {
            let _ = maa_override;
            MachineInfo::default()
        }
    }

    /// 由注册表条目 + 兜底扫描推导（Windows 下调用；拆出便于阅读）。
    #[cfg(windows)]
    /// 用设置里的 MAA 目录覆盖探测结果。
    ///
    /// 只认目录里有 `MaaCore.dll`；无效路径保持原值（不把好的探测结果砸掉）。
    /// 与连接方式的 `auto_detect` 无关。
    pub fn apply_maa_override(&mut self, raw: Option<&str>) {
        let Some(raw) = raw else { return };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let dir = PathBuf::from(trimmed);
        if dir.join(MAA_MARKER).is_file() {
            self.maa = Some(dir);
        }
    }

    /// 读缓存（失活则重新探测）并应用覆盖——工具层与桥的统一入口。
    ///
    /// 单独抽出来是因为：`machine.json` 是缓存，用户刚填的路径必须能绕过它，
    /// 否则又要重启才能生效。
    pub fn load_resolved(dir: &Path, maa_override: Option<&str>) -> Option<Self> {
        let mut info = match Self::load(dir) {
            Some(info) if info.all_alive() => info,
            _ => {
                let info = Self::detect(None);
                info.save(dir);
                info
            }
        };
        info.apply_maa_override(maa_override);
        Some(info)
    }

    fn detect_from(entries: &[registry::Entry], maa_override: Option<&str>) -> Self {
        // ── 模拟器（层 2：注册表）──
        let mut emulators: Vec<Emulator> = Vec::new();
        for (label, keys) in EMULATORS {
            for e in entries {
                if !has_keyword(&e.display, keys) {
                    continue;
                }
                let path = e.install_dir();
                if !path.as_os_str().is_empty()
                    && path.is_dir()
                    && !emulators.iter().any(|x| x.label == *label)
                {
                    emulators.push(Emulator {
                        label: (*label).to_string(),
                        path,
                    });
                }
            }
        }

        // ── MAA（层 1 设置 → 层 2 注册表 → 层 3 盘根浅扫）──
        let maa = resolve_maa(entries, maa_override);

        // ── PC 端（层 2 启动器 → 层 4 games\ 判游戏 + 定服）──
        let mut pc_clients: Vec<PcClient> = Vec::new();
        for e in entries {
            if !has_keyword(&e.display, LAUNCHER_KEYS) {
                continue;
            }
            let launcher_dir = e.install_dir();
            if launcher_dir.as_os_str().is_empty() {
                continue;
            }
            for game_dir in arknights_games(&launcher_dir) {
                if let Some(server) = classify_server(&game_dir) {
                    if !pc_clients.iter().any(|p| p.game_dir == game_dir) {
                        pc_clients.push(PcClient {
                            server,
                            launcher_dir: launcher_dir.clone(),
                            game_dir,
                        });
                    }
                }
            }
        }

        MachineInfo {
            maa,
            emulators,
            pc_clients,
        }
    }
}

/// `DisplayName` / `Publisher` 是否含任一关键字（小写比对）。
fn has_keyword(hay: &str, keys: &[&str]) -> bool {
    let h = hay.to_lowercase();
    keys.iter().any(|k| h.contains(&k.to_lowercase()))
}

/// 定位 MAA：设置值（校验 `MaaCore.dll`）→ 注册表 → 盘根浅扫。
#[cfg(windows)]
fn resolve_maa(entries: &[registry::Entry], maa_override: Option<&str>) -> Option<PathBuf> {
    if let Some(raw) = maa_override {
        let t = raw.trim();
        if !t.is_empty() {
            let p = PathBuf::from(t);
            if p.join(MAA_MARKER).is_file() {
                return Some(p);
            }
        }
    }
    for e in entries {
        if has_keyword(&e.display, &["maa"]) {
            let dir = e.install_dir();
            if dir.join(MAA_MARKER).is_file() {
                return Some(dir);
            }
        }
    }
    // 兜底：已知位置快扫 + 各盘根一层。压缩包解压型没有注册表项，这一层真的会用到。
    // 候选可能不止一个（旧版本目录没删、下载目录里还留着安装包旁的一份），
    // 所以不取第一个，而是按 `MaaCore.dll` 的修改时间取最新——目录名里的版本号
    // 不可信：很多人直接把新版本覆盖进老目录，名字不变。
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut budget = SCAN_BUDGET;
    for root in maa_common_roots() {
        collect_marker_dirs(&root, 3, MAA_MARKER, &mut candidates, &mut budget);
    }
    for (dir, _marker) in scan_drive_roots(&[MAA_MARKER]) {
        candidates.push(dir);
    }
    candidates.sort_by_key(|dir| std::cmp::Reverse(marker_mtime(dir, MAA_MARKER)));
    candidates.into_iter().next()
}

/// 扫描预算：目录条目上限，防止把整个盘拖进来。
const SCAN_BUDGET: usize = 20_000;

/// MAA 的常见落脚点（相对用户目录的那几个是解压默认位置）。
fn maa_common_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        out.push(PathBuf::from(&v));
    }
    if let Some(v) = std::env::var_os("APPDATA") {
        out.push(PathBuf::from(v));
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let home = PathBuf::from(home);
        out.push(home.join("Desktop"));
        out.push(home.join("Downloads"));
        out.push(home.join("Documents"));
    }
    out
}

/// 递归找含标记文件的目录；命中后不再深入。`budget` 是剩余条目预算。
fn collect_marker_dirs(
    root: &Path,
    depth: u8,
    marker: &str,
    out: &mut Vec<PathBuf>,
    budget: &mut usize,
) {
    if depth == 0 || *budget == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if p.join(marker).is_file() {
            out.push(p);
            continue;
        }
        collect_marker_dirs(&p, depth - 1, marker, out, budget);
    }
}

fn marker_mtime(dir: &Path, marker: &str) -> std::time::SystemTime {
    std::fs::metadata(dir.join(marker))
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
}

/// `<launcher>/games/*` 下含 `Arknights.exe` 的目录（不看目录名——两服不同）。
fn arknights_games(launcher_dir: &Path) -> Vec<PathBuf> {
    let games = launcher_dir.join("games");
    let Ok(entries) = std::fs::read_dir(&games) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() && p.join(PC_MARKER).is_file() {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// 定服：`BLPlatform64` → B 服；`hgsdk.dll` → 官服；都无 → 未知（跳过）。
fn classify_server(game_dir: &Path) -> Option<Server> {
    if game_dir.join(BILI_MARKER).is_dir() {
        return Some(Server::Bilibili);
    }
    if game_dir.join(OFFICIAL_MARKER).is_file() {
        return Some(Server::Official);
    }
    None
}

/// 各盘根（C–H）**一层**浅扫：找含指定标记文件的目录。返回 `(目录, 标记)`。
#[cfg(windows)]
fn scan_drive_roots(markers: &[&str]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for letter in ['C', 'D', 'E', 'F', 'G', 'H'] {
        let root = format!("{letter}:\\");
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            for m in markers {
                if p.join(m).is_file() {
                    out.push((p.clone(), (*m).to_string()));
                }
            }
        }
    }
    out
}

/// 从 `UninstallString` 剥出安装目录（`"…\uninstall.exe"` → 其父目录）。
fn dir_from_uninstall(s: &str) -> PathBuf {
    let s = s.trim();
    if s.is_empty() {
        return PathBuf::new();
    }
    let exe = if let Some(rest) = s.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        // 裸路径可能含空格（如 `D:\Games\Arknights bilibili\Uninstall.exe`）——以
        // `.exe` 为界，不按空白切分（`to_ascii_lowercase` 保字节长度，索引对齐）。
        let lower = s.to_ascii_lowercase();
        let cut = lower.find(".exe").map(|i| i + 4).unwrap_or(s.len());
        &s[..cut]
    };
    Path::new(exe)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

// ═══════════════════════════════════════════════════════
// 注册表（Windows）——Uninstall 枚举
// ═══════════════════════════════════════════════════════

#[cfg(windows)]
mod registry {
    use std::path::PathBuf;

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    };

    /// 一条 Uninstall 条目（只取需要的字段）。
    pub struct Entry {
        pub display: String,
        pub install_location: String,
        pub uninstall_string: String,
    }

    impl Entry {
        /// 安装目录：`InstallLocation` 优先；空 → 剥 `UninstallString`。
        pub fn install_dir(&self) -> PathBuf {
            let loc = self.install_location.trim();
            if !loc.is_empty() {
                return PathBuf::from(loc);
            }
            super::dir_from_uninstall(&self.uninstall_string)
        }
    }

    /// 三个 Uninstall 根（缺一不可：实测启动器落在 HKLM-WOW6432Node）。
    fn roots() -> [(HKEY, &'static str); 3] {
        [
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_CURRENT_USER,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
        ]
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 打开的注册表键（Drop 时关闭）。
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn open(root: HKEY, sub: &str) -> Option<Key> {
        let w = wide(sub);
        let mut h = HKEY::default();
        let rc = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), Some(0), KEY_READ, &mut h) };
        if rc == ERROR_SUCCESS {
            Some(Key(h))
        } else {
            None
        }
    }

    fn subkey_names(k: &Key) -> Vec<String> {
        let mut out = Vec::new();
        let mut idx = 0u32;
        loop {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let rc = unsafe {
                RegEnumKeyExW(
                    k.0,
                    idx,
                    Some(PWSTR(buf.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if rc != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
            idx += 1;
        }
        out
    }

    /// 读字符串值（REG_SZ / REG_EXPAND_SZ；其余 → None）。
    ///
    /// 双调用：先问长度、再取内容（取值不省这一步）。
    fn value_string(k: &Key, name: &str) -> String {
        let n = wide(name);
        let mut ty = REG_VALUE_TYPE::default();
        let mut len = 0u32;
        let rc = unsafe {
            RegQueryValueExW(
                k.0,
                PCWSTR(n.as_ptr()),
                None,
                Some(&mut ty),
                None,
                Some(&mut len),
            )
        };
        if rc != ERROR_SUCCESS || len == 0 {
            return String::new();
        }
        if ty != REG_SZ && ty != REG_EXPAND_SZ {
            return String::new();
        }
        let mut buf = vec![0u8; len as usize];
        let rc = unsafe {
            RegQueryValueExW(
                k.0,
                PCWSTR(n.as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
        };
        if rc != ERROR_SUCCESS {
            return String::new();
        }
        // UTF-16LE → String（去掉尾部 NUL）
        let units: Vec<u16> = buf[..len as usize]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let s: String = String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_string();
        if ty == REG_EXPAND_SZ {
            expand_env(&s)
        } else {
            s
        }
    }

    /// `%VAR%` 展开（注册表值里偶见；未知变量保持原样）。
    fn expand_env(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            if let Some(end) = after.find('%') {
                let var = &after[..end];
                match std::env::var(var) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push('%');
                        out.push_str(var);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            } else {
                out.push('%');
                rest = after;
            }
        }
        out.push_str(rest);
        out
    }

    /// 全量枚举三个根下的 Uninstall 条目。
    pub fn uninstall_entries() -> Vec<Entry> {
        let mut out = Vec::new();
        for (root, sub) in roots() {
            let Some(key) = open(root, sub) else {
                continue;
            };
            for name in subkey_names(&key) {
                let Some(sub_key) = open(root, &format!("{sub}\\{name}")) else {
                    continue;
                };
                out.push(Entry {
                    display: value_string(&sub_key, "DisplayName"),
                    install_location: value_string(&sub_key, "InstallLocation"),
                    uninstall_string: value_string(&sub_key, "UninstallString"),
                });
            }
        }
        out
    }
}

#[cfg(not(windows))]
mod registry {
    pub struct Entry {
        pub display: String,
        pub install_location: String,
        pub uninstall_string: String,
    }
    impl Entry {
        pub fn install_dir(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }
    }
    pub fn uninstall_entries() -> Vec<Entry> {
        Vec::new()
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maa_override_requires_marker_file() {
        let dir = std::env::temp_dir().join(format!("prts-maa-override-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MAA_MARKER), b"x").unwrap();

        let mut info = MachineInfo::default();
        info.apply_maa_override(Some(dir.to_string_lossy().as_ref()));
        assert_eq!(
            info.maa.as_deref(),
            Some(dir.as_path()),
            "有标记文件就用它覆盖"
        );

        let mut untouched = MachineInfo::default();
        untouched.apply_maa_override(Some("Z:\\definitely-not-here"));
        assert!(
            untouched.maa.is_none(),
            "无效路径不得覆盖（包括不得砸掉已有结果）"
        );

        let mut blank = MachineInfo {
            maa: Some(PathBuf::from("X:\\maa")),
            ..Default::default()
        };
        blank.apply_maa_override(Some("   "));
        assert_eq!(
            blank.maa,
            Some(PathBuf::from("X:\\maa")),
            "空串 = 回自动，不动原值"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dim_values_empty_are_not_detected() {
        let m = MachineInfo::default();
        assert_eq!(
            m.dim_values(),
            ("未检测到".into(), "未检测到".into(), "未检测到".into())
        );
        assert!(m.all_alive(), "空信息视为存活（无路径可失效）");
    }

    #[test]
    fn dim_values_join_sorted_and_dedup() {
        let m = MachineInfo {
            maa: Some(PathBuf::from("X:\\maa")),
            emulators: vec![
                Emulator {
                    label: "MuMu 12".into(),
                    path: PathBuf::from("X:\\mumu"),
                },
                Emulator {
                    label: "雷电 LDPlayer".into(),
                    path: PathBuf::from("Y:\\ld"),
                },
                Emulator {
                    label: "MuMu 12".into(),
                    path: PathBuf::from("Z:\\mumu2"),
                },
            ],
            pc_clients: vec![
                PcClient {
                    server: Server::Bilibili,
                    launcher_dir: PathBuf::from("A"),
                    game_dir: PathBuf::from("A\\games\\g"),
                },
                PcClient {
                    server: Server::Official,
                    launcher_dir: PathBuf::from("B"),
                    game_dir: PathBuf::from("B\\games\\g"),
                },
            ],
        };
        let (maa, emulator, game_pc) = m.dim_values();
        assert_eq!(maa, "已安装");
        assert_eq!(
            emulator, "MuMu 12；雷电 LDPlayer",
            "同类型去重 + 排序（字节序）"
        );
        assert_eq!(game_pc, "B服；官服", "官/B 都装时并报");
    }

    #[test]
    fn all_alive_detects_missing_marker() {
        let dir = std::env::temp_dir().join(format!("prts-machine-{}", std::process::id()));
        let maa_dir = dir.join("maa");
        std::fs::create_dir_all(&maa_dir).unwrap();
        let m = MachineInfo {
            maa: Some(maa_dir.clone()),
            ..Default::default()
        };
        assert!(!m.all_alive(), "缺 MaaCore.dll → 不存活");
        std::fs::write(maa_dir.join("MaaCore.dll"), b"x").unwrap();
        assert!(m.all_alive(), "标记到位 → 存活");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_from_uninstall_handles_quoted_and_bare() {
        assert_eq!(
            dir_from_uninstall("\"D:\\Netease\\MuMuPlayer-12.0\\uninstall.exe\""),
            PathBuf::from("D:\\Netease\\MuMuPlayer-12.0")
        );
        assert_eq!(
            dir_from_uninstall("D:\\Games\\Arknights bilibili\\Uninstall.exe"),
            PathBuf::from("D:\\Games\\Arknights bilibili")
        );
        assert_eq!(dir_from_uninstall(""), PathBuf::new());
    }

    /// 真机诊断（`cargo test --lib detect_reports -- --ignored --nocapture`）：
    /// 打印本机探测结果。**只读注册表 + 文件系统，不触碰任何 PC 应用 / 游戏窗口 / 模拟器。**
    #[test]
    #[ignore = "真机诊断，手动运行"]
    fn detect_reports_this_machine() {
        let info = MachineInfo::detect(None);
        println!("maa       = {:?}", info.maa);
        println!("emulators = {:?}", info.emulators);
        println!("pc        = {:?}", info.pc_clients);
        println!("dims      = {:?}", info.dim_values());
    }
}
