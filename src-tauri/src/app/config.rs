//! config —— 运行时路径：数据根 + 随包资源（M1 地基）
//!
//! 设置页扩展定稿后职责收拢：workspace 根已并入 settings.json（`settings.rs`
//! workspace 段；空 = 回退默认工作区，见 [`default_workspace_path`]），
//! `PRTS_WORKSPACE` env 与
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

use std::path::{Path, PathBuf};

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
// 默认工作区
// ═══════════════════════════════════════════════════════

/// 默认工作区目录名（用户主目录下的可见目录）。
///
/// ASCII 名是有意的：该路径会成为 `run_command` 的 cwd 与受限令牌的 ACL
/// 派生根，中文名一旦进入子进程环境与 ACL 派生就多出一类编码风险
/// （本项目已有前车之鉴）。
pub const DEFAULT_WORKSPACE_DIRNAME: &str = "PRTS-WorkSpace";

/// 默认工作区路径：`<主目录>/PRTS-WorkSpace`。
///
/// **不放桌面**：OneDrive 接管桌面时会把桌面重定向到 `~/OneDrive/Desktop`，
/// 此时 `~/Desktop` 并不存在——在那里建目录，用户在桌面上看不到（静默失败）。
/// 主目录不参与重定向。纯路径解析，不建目录。
///
/// 与数据根的分工：数据根（`~/.prts`）放隐藏的运行时数据，本目录放用户可见
/// 的产物。两者不相互包含。
pub fn default_workspace_path() -> PathBuf {
    home_dir().join(DEFAULT_WORKSPACE_DIRNAME)
}

/// 确保给定工作区目录存在（`mkdir -p` 语义，已存在则幂等空操作）。
pub fn ensure_workspace_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// 确保默认工作区存在，返回其路径。
///
/// 仅对默认路径使用：用户自选路径**不**重建——目录消失应当显式暴露，
/// 而不是被静默重建（既有实现同规则，其 `restoreDefaultWorkspaceIfMissing`
/// 只对默认路径生效）。
pub fn ensure_default_workspace() -> std::io::Result<PathBuf> {
    let path = default_workspace_path();
    ensure_workspace_dir(&path)?;
    Ok(path)
}

// ═══════════════════════════════════════════════════════
// 随包资源（开发构建 / 发布构建各一条显式路径）
// ═══════════════════════════════════════════════════════

/// 数据库**种子**路径：随程序分发的只读初始副本。
///
/// 开发构建 = `src-tauri/resources/arknights.db`；发布构建 = 程序目录下的
/// `arknights.db`（`bundle.resources` 把它映射到安装目录根）。**不做存在性
/// 探测**：路径就是路径，缺失由启动前自检拦下（发布构建）。
pub fn db_seed_path() -> PathBuf {
    if is_dev() {
        resource_root().join("resources").join("arknights.db")
    } else {
        resource_root().join("arknights.db")
    }
}

/// 数据库**运行时副本**路径：数据根下的 `arknights.db`（可写）。
///
/// 数据库与 Policy 都读这一份；不存在或落后于程序版本时，由
/// [`crate::db_seed::sync_on_boot`] 从种子恢复（首启与后续启动同一条路径）。
pub fn resolve_db_path() -> PathBuf {
    data_dir().join("arknights.db")
}

/// 知识库目录（随包只读资源）。
///
/// 开发构建 = `src-tauri/content/knowledge`；发布构建 = 程序目录下 `resources/knowledge`。
pub fn bundled_knowledge_dir() -> PathBuf {
    if is_dev() {
        resource_root().join("content").join("knowledge")
    } else {
        resource_root().join("resources").join("knowledge")
    }
}

/// 内置技能源目录（随包只读资源；启动时同步进数据根运行根）。
///
/// 开发构建 = `src-tauri/content/skills`；发布构建 = 程序目录下 `resources/skills`。
pub fn bundled_skills_dir() -> PathBuf {
    if is_dev() {
        resource_root().join("content").join("skills")
    } else {
        resource_root().join("resources").join("skills")
    }
}

/// 使用说明文档目录（随包只读资源）。
///
/// 开发构建 = `src-tauri/content/docs`；发布构建 = 程序目录下 `resources/docs`。
pub fn bundled_docs_dir() -> PathBuf {
    if is_dev() {
        resource_root().join("content").join("docs")
    } else {
        resource_root().join("resources").join("docs")
    }
}

/// 许可证文件（随包资源；关于页展示全文用）。
///
/// 开发构建 = 仓库根 `LICENSE`（`resource_root()` 指向 `src-tauri/`，故上跳一级）；
/// 发布构建 = 程序目录下 `LICENSE`（`bundle.resources` 里 `"../LICENSE": "./LICENSE"`）。
/// 与 bundled_docs_dir 同款 dev/release 双解析；**不做存在性探测**：
/// 缺失由读取命令报错暴露。
pub fn license_path() -> PathBuf {
    if is_dev() {
        resource_root().join("..").join("LICENSE")
    } else {
        resource_root().join("LICENSE")
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
        assert!(
            !text.contains("com.tauri-app"),
            "不得再用 identifier 当目录名"
        );
        assert!(!text.contains("prts-workspace"), "旧兜底目录名不得复现");
        assert!(!text.contains("AppData"), "数据根不再落在 AppData");
    }

    #[test]
    fn license_path_targets_bundled_license() {
        assert_eq!(
            license_path().file_name().unwrap().to_string_lossy(),
            "LICENSE"
        );
        #[cfg(debug_assertions)]
        assert!(
            license_path().starts_with(resource_root().parent().unwrap()),
            "开发构建的 LICENSE 应取仓库根那份"
        );
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
        let db = db_seed_path();
        let kb = bundled_knowledge_dir();
        let skills = bundled_skills_dir();

        // 三者都在资源基准目录之下（同一构建只有一条路）
        for p in [&db, &kb, &skills] {
            assert!(
                p.starts_with(&root),
                "资源必须落在基准目录下：{}",
                p.display()
            );
        }

        // 运行时副本与种子是两份路径：前者在数据根，后者在资源目录
        let runtime = resolve_db_path();
        assert!(runtime.starts_with(data_dir()), "运行时副本必须在数据根下");
        assert_ne!(runtime, db, "种子与运行时副本不是同一份");

        if is_dev() {
            // 开发构建：源码树相对路径，且基准目录即源码树
            assert_eq!(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")));
            assert_eq!(db, root.join("resources").join("arknights.db"));
            assert_eq!(kb, root.join("content").join("knowledge"));
            assert_eq!(skills, root.join("content").join("skills"));
        } else {
            // 发布构建：程序目录相对路径
            assert_eq!(db, root.join("arknights.db"));
            assert_eq!(kb, root.join("resources").join("knowledge"));
            assert_eq!(skills, root.join("resources").join("skills"));
        }

        // 幂等：同一次构建下解析结果唯一（不随"谁存在"改变）
        assert_eq!(db_seed_path(), db);
        assert_eq!(resolve_db_path(), runtime);
        assert_eq!(bundled_knowledge_dir(), kb);
        assert_eq!(bundled_skills_dir(), skills);
    }

    #[test]
    fn default_workspace_lives_in_home_dir_not_desktop() {
        let p = default_workspace_path();
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            DEFAULT_WORKSPACE_DIRNAME,
            "末级目录名必须是默认工作区名"
        );
        assert!(p.starts_with(home_dir()), "默认工作区必须在主目录下");
        // 不落在桌面：OneDrive 重定向会让 ~/Desktop 指向不存在的路径
        assert!(
            !p.to_string_lossy().contains("Desktop"),
            "默认工作区不得依赖桌面：{}",
            p.display()
        );
        // 与数据根分离（隐藏数据 vs 可见产物）
        assert_ne!(p, data_dir(), "默认工作区与数据根不得重合");
        assert!(!p.starts_with(data_dir()), "默认工作区不得落在数据根内");
    }

    #[test]
    fn default_workspace_path_is_idempotent() {
        // 纯路径解析：重复解析结果一致（不随“谁存在”改变，不建目录）
        let a = default_workspace_path();
        let b = default_workspace_path();
        assert_eq!(a, b);
        assert!(!a.to_string_lossy().is_empty());
    }

    #[test]
    fn ensure_workspace_dir_is_idempotent() {
        // 用临时目录验证创建语义（不在真实主目录里建东西）
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-ws-ensure-{}-{nanos}", std::process::id()));
        let nested = dir.join("a").join("b");
        assert!(!nested.exists());
        ensure_workspace_dir(&nested).unwrap();
        assert!(nested.is_dir(), "应递归创建多级目录");
        // 幂等：再调一次不报错
        ensure_workspace_dir(&nested).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
