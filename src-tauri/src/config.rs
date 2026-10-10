//! config —— 数据根定位 + 数据文件路径解析（M1 地基）
//!
//! 设置页扩展定稿后职责收拢：workspace 根已并入 settings.json（`settings.rs`
//! workspace 段；空 = 回退数据根），`PRTS_WORKSPACE` env 与
//! `resources/config.json` 链**已废弃**（dev.ps1 不再设置）。本模块仅保留：
//!
//! - [`data_dir`]：数据根（settings.json / notices.json / sessions/ / memory/ /
//!   skills/ / 附件 / 截图 / MAA 记录 / 数据库运行副本 / 密钥 的落点）。用户主目录
//!   下的 `.prts`；开发构建用 `.prts-dev`（编译期判定，开发与安装版互不干扰）。
//!   **与 identifier 解耦**：identifier 只负责系统集成，不参与目录名。
//!   纯路径解析，不建目录——需要目录存在的地方（SettingsStore::load /
//!   SessionStore / Notices）自行 create_dir_all。
//! - [`resolve_db_path`]：arknights.db 路径（dev resources/ → 打包 exe 同级）。

use std::path::PathBuf;

/// 数据根目录名（发布构建）。
pub const DATA_ROOT_NAME: &str = ".prts";
/// 数据根目录名（开发构建）。
pub const DATA_ROOT_NAME_DEV: &str = ".prts-dev";

/// 应用数据根目录：用户主目录下的 `.prts`（开发构建 = `.prts-dev`）。
///
/// 主目录不可解析时回退当前目录。纯路径解析，不建目录。
pub fn data_dir() -> PathBuf {
    home_dir().join(data_root_name())
}

/// 生效的数据根目录名（编译期判定）。
pub fn data_root_name() -> &'static str {
    if cfg!(debug_assertions) {
        DATA_ROOT_NAME_DEV
    } else {
        DATA_ROOT_NAME
    }
}

/// 用户主目录：USERPROFILE（Windows）→ HOME（其它平台）→ `.` 兜底。
fn home_dir() -> PathBuf {
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    PathBuf::from(".")
}

/// arknights.db 路径解析。
///
/// 优先级：开发目录（src-tauri/resources/）→ 可执行文件同级（打包后）。
/// Policy BLOCKED 名单调用此函数获取 db 文件路径。
pub fn resolve_db_path() -> PathBuf {
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arknights.db");
    if dev_path.exists() {
        return dev_path;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let prod_path = dir.join("resources/arknights.db");
            if prod_path.exists() {
                return prod_path;
            }
        }
    }
    dev_path
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_root_is_home_dot_prts_and_decoupled_from_identifier() {
        let root = data_dir();
        // 目录名 = 数据根名（开发构建为 .prts-dev）
        assert_eq!(
            root.file_name().unwrap().to_string_lossy(),
            data_root_name(),
            "目录名必须是数据根名"
        );
        // 落在用户主目录下
        assert!(root.starts_with(home_dir()), "数据根必须在主目录下");
        // 与 identifier / 旧落点解耦（改名不回头）
        let text = root.to_string_lossy();
        assert!(!text.contains("com.tauri-app"), "不得再用 identifier 当目录名");
        assert!(!text.contains("prts-workspace"), "旧兜底目录名不得复现");
        assert!(!text.contains("AppData"), "数据根不再落在 AppData");
    }

    #[test]
    fn dev_and_release_root_names_differ() {
        assert_ne!(DATA_ROOT_NAME, DATA_ROOT_NAME_DEV);
        let expected = if cfg!(debug_assertions) {
            DATA_ROOT_NAME_DEV
        } else {
            DATA_ROOT_NAME
        };
        assert_eq!(data_root_name(), expected);
    }
}
