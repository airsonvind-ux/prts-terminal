//! config —— 运行时路径：数据根 + 随包资源（M1 地基）
//!
//! 设置页扩展定稿后职责收拢：workspace 根已并入 settings.json（`settings.rs`
//! workspace 段；空 = 回退数据根），`PRTS_WORKSPACE` env 与
//! `resources/config.json` 链**已废弃**（dev.ps1 不再设置）。本模块提供：
//!
//! - **数据根**（[`data_dir`]）：可写数据的唯一落点（settings.json /
//!   notices.json / sessions/ / memory/ / skills/ / 附件 / 截图 / MAA 记录 /
//!   数据库运行副本 / 密钥）。用户主目录下的 `.prts`；开发构建用 `.prts-dev`。
//!   **与 identifier 解耦**：identifier 只负责系统集成，不参与目录名。
//!   纯路径解析，不建目录——需要目录存在的地方（SettingsStore::load /
//!   SessionStore / Notices）自行 create_dir_all。
//! - **随包资源**（随程序分发的只读资源）：数据库种子、知识库、内置技能源。
//!   **按构建类型给唯一路径，不做存在性探测**——"哪条路存在走哪条"会让
//!   开发机上的安装版偷读源码目录，掩盖发布环境的问题（观察清单①的根因）。
//!   发布构建不含任何编译期绝对路径（`env!("CARGO_MANIFEST_DIR")` 只在开发
//!   构建编译）。
//!
//! 两个来源共用同一个编译期判定：[`is_dev`]。

use std::path::PathBuf;

// ═══════════════════════════════════════════════════════
// 构建类型（唯一判定点）
// ═══════════════════════════════════════════════════════

/// 是否开发构建。数据根与随包资源解析共用此判定。
pub fn is_dev() -> bool {
    cfg!(debug_assertions)
}

/// 资源基准目录：开发构建 = 源码树（`src-tauri/`）；发布构建 = 程序目录。
///
/// 发布构建不含编译期绝对路径（下列 `cfg` 门控：`env!` 只在开发构建编译）。
#[cfg(debug_assertions)]
pub fn resource_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 资源基准目录：发布构建 = 可执行文件所在目录。
#[cfg(not(debug_assertions))]
pub fn resource_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

// ═══════════════════════════════════════════════════════
// 数据根
// ═══════════════════════════════════════════════════════

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
    if is_dev() {
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

// ═══════════════════════════════════════════════════════
// 随包资源（开发构建 / 发布构建各一条显式路径）
// ═══════════════════════════════════════════════════════

/// arknights.db 路径（运行时读的那一份）。
///
/// 开发构建 = `src-tauri/resources/arknights.db`；
/// 发布构建 = 程序目录下的 `arknights.db`（`bundle.resources` 把它映射到安装
/// 目录根）。**不做存在性探测**：路径就是路径，缺失由启动自检上报。
///
/// 数据库落位改造后，运行时会改读数据根副本，本函数届时收敛为"随包种子"。
pub fn resolve_db_path() -> PathBuf {
    if is_dev() {
        resource_root().join("resources").join("arknights.db")
    } else {
        resource_root().join("arknights.db")
    }
}

/// 知识库目录（随包只读资源）。
///
/// 开发构建 = `src-tauri/src/knowledge`；发布构建 = 程序目录下 `resources/knowledge`。
pub fn bundled_knowledge_dir() -> PathBuf {
    if is_dev() {
        resource_root().join("src").join("knowledge")
    } else {
        resource_root().join("resources").join("knowledge")
    }
}

/// 内置技能源目录（随包只读资源；启动时同步进数据根运行根）。
///
/// 开发构建 = `src-tauri/src/skills`；发布构建 = 程序目录下 `resources/skills`。
pub fn bundled_skills_dir() -> PathBuf {
    if is_dev() {
        resource_root().join("src").join("skills")
    } else {
        resource_root().join("resources").join("skills")
    }
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
        let expected = if is_dev() {
            DATA_ROOT_NAME_DEV
        } else {
            DATA_ROOT_NAME
        };
        assert_eq!(data_root_name(), expected);
    }

    #[test]
    fn resource_paths_are_explicit_and_probe_free() {
        let root = resource_root();
        let db = resolve_db_path();
        let kb = bundled_knowledge_dir();
        let skills = bundled_skills_dir();

        // 三者都在资源基准目录之下（同一构建只有一条路）
        for p in [&db, &kb, &skills] {
            assert!(p.starts_with(&root), "资源必须落在基准目录下：{}", p.display());
        }

        if is_dev() {
            // 开发构建：源码树相对路径，且基准目录即源码树
            assert_eq!(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")));
            assert_eq!(db, root.join("resources").join("arknights.db"));
            assert_eq!(kb, root.join("src").join("knowledge"));
            assert_eq!(skills, root.join("src").join("skills"));
        } else {
            // 发布构建：程序目录相对路径
            assert_eq!(db, root.join("arknights.db"));
            assert_eq!(kb, root.join("resources").join("knowledge"));
            assert_eq!(skills, root.join("resources").join("skills"));
        }

        // 幂等：同一次构建下解析结果唯一（不随"谁存在"改变）
        assert_eq!(resolve_db_path(), db);
        assert_eq!(bundled_knowledge_dir(), kb);
        assert_eq!(bundled_skills_dir(), skills);
    }
}
