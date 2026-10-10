//! db_seed —— 数据库种子同步（随包初始副本 → 数据根运行时副本）
//!
//! 数据库分两份：
//! - **种子**（[`crate::config::db_seed_path`]）：随程序分发的只读初始副本；
//! - **运行时副本**（[`crate::config::resolve_db_path`]）：数据根下的那一份，
//!   数据库与 Policy 都读它，将来随版本更新。
//!
//! 装配期调用 [`sync_on_boot`]，**首次运行与后续启动走同一条路径**（对照成熟
//! 项目"首启与自愈同一路径"的做法）：
//! - 运行时副本不存在 → 从种子复制；
//! - 存在但落后于程序版本 → 替换（旧的留成 `.prev`）；
//! - 不落后 → 不动。
//!
//! 替换判据用**应用版本号**（记在数据根 `.db-applied` 标记里），不用文件指纹：
//! 版本比大小能防"用户装回旧版把新库覆盖掉"。写入一律临时文件加原子改名。
//!
//! 失败不静默：同步失败意味着数据根写不进去（磁盘满 / 权限 / 被拦截），
//! 由调用方（lib.rs 装配期）走启动失败通道。

use std::path::{Path, PathBuf};

/// 同步结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// 运行时副本已存在且不落后于程序版本，未动。
    UpToDate,
    /// 运行时副本不存在，已从种子复制。
    Copied,
    /// 运行时副本落后于程序版本，已替换（旧的留成 `.prev`）。
    Replaced,
}

/// 装配期入口：按 config 解析路径并同步。
pub fn sync_on_boot() -> Result<SyncOutcome, String> {
    let seed = crate::config::db_seed_path();
    let runtime = crate::config::resolve_db_path();
    let marker = crate::config::data_dir().join(DB_APPLIED_MARKER);
    sync(&seed, &runtime, &marker, env!("CARGO_PKG_VERSION"))
}

/// 已应用的数据库版本标记（数据根；用户不可见）。
pub const DB_APPLIED_MARKER: &str = ".db-applied";

/// 同步本体（路径显式传入，便于单测）。
pub fn sync(
    seed: &Path,
    runtime: &Path,
    marker: &Path,
    app_version: &str,
) -> Result<SyncOutcome, String> {
    if !seed.is_file() {
        return Err(format!("数据库初始副本不存在：{}", seed.display()));
    }
    if let Some(dir) = runtime.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("创建数据根失败（{}）：{e}", dir.display()))?;
    }

    let applied = read_marker(marker);
    if runtime.is_file() && !version_gt(app_version, &applied) {
        return Ok(SyncOutcome::UpToDate);
    }

    let replacing = runtime.is_file();
    if replacing {
        // 旧的留底（best-effort：留不上不阻断，正式的失败点在下面的写入）。
        let prev = prev_path(runtime);
        if let Err(e) = std::fs::copy(runtime, &prev) {
            eprintln!("[db_seed] 旧库留底失败（忽略）：{e}");
        }
    }

    // 原子替换：临时文件 → rename。
    let tmp = tmp_path(runtime);
    std::fs::copy(seed, &tmp)
        .map_err(|e| format!("写临时文件失败（{}）：{e}", tmp.display()))?;
    std::fs::rename(&tmp, runtime)
        .map_err(|e| format!("替换运行时副本失败（{}）：{e}", runtime.display()))?;
    write_marker(marker, app_version)?;

    Ok(if replacing {
        SyncOutcome::Replaced
    } else {
        SyncOutcome::Copied
    })
}

/// 运行时副本的留底路径（`arknights.db` → `arknights.db.prev`）。
fn prev_path(runtime: &Path) -> PathBuf {
    sibling(runtime, "prev")
}

/// 写临时文件路径（`arknights.db` → `arknights.db.tmp`）。
fn tmp_path(runtime: &Path) -> PathBuf {
    sibling(runtime, "tmp")
}

/// 同目录下的变体路径：`<name>.<suffix>`。
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut out = path.to_path_buf();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    out.set_file_name(format!("{name}.{suffix}"));
    out
}

/// 读标记（缺失或不可读 = 空串，等价"从未应用过"）。
fn read_marker(marker: &Path) -> String {
    std::fs::read_to_string(marker)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 写标记。**写不上即返错**：它意味着数据根写不进去，是真问题，不掩盖。
fn write_marker(marker: &Path, version: &str) -> Result<(), String> {
    std::fs::write(marker, version)
        .map_err(|e| format!("写版本标记失败（{}）：{e}", marker.display()))
}

/// `a` 是否比 `b` 新。按点分段数值比较；任一段非数字则退化为字符串比较。
pub fn version_gt(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let (pa, pb): (Vec<&str>, Vec<&str>) = (a.split('.').collect(), b.split('.').collect());
    let numeric = pa.iter().chain(pb.iter()).all(|s| s.parse::<u64>().is_ok());
    if numeric {
        for i in 0..pa.len().max(pb.len()) {
            let x = pa.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            let y = pb.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            if x != y {
                return x > y;
            }
        }
        false
    } else {
        a > b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-dbseed-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup(tag: &str, seed_body: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let seed = dir.join("seed").join("arknights.db");
        std::fs::create_dir_all(seed.parent().unwrap()).unwrap();
        std::fs::write(&seed, seed_body).unwrap();
        let runtime = dir.join("root").join("arknights.db");
        let marker = dir.join("root").join(DB_APPLIED_MARKER);
        (dir, seed, runtime, marker)
    }

    #[test]
    fn copies_seed_when_runtime_absent() {
        let _g = LOCK.lock().unwrap();
        let (_d, seed, runtime, marker) = setup("copy", "seed-v1");
        let out = sync(&seed, &runtime, &marker, "1.0.0").unwrap();
        assert_eq!(out, SyncOutcome::Copied);
        assert_eq!(std::fs::read_to_string(&runtime).unwrap(), "seed-v1");
        assert_eq!(read_marker(&marker), "1.0.0");
    }

    #[test]
    fn up_to_date_keeps_runtime_untouched() {
        let _g = LOCK.lock().unwrap();
        let (_d, seed, runtime, marker) = setup("uptodate", "seed-v2");
        std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
        std::fs::write(&runtime, "runtime-用户数据").unwrap();
        std::fs::write(&marker, "1.0.0").unwrap();

        let out = sync(&seed, &runtime, &marker, "1.0.0").unwrap();
        assert_eq!(out, SyncOutcome::UpToDate);
        assert_eq!(std::fs::read_to_string(&runtime).unwrap(), "runtime-用户数据");
    }

    #[test]
    fn replaces_when_app_version_is_newer_and_keeps_prev() {
        let _g = LOCK.lock().unwrap();
        let (_d, seed, runtime, marker) = setup("replace", "seed-v2");
        std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
        std::fs::write(&runtime, "runtime-旧库").unwrap();
        std::fs::write(&marker, "1.0.0").unwrap();

        let out = sync(&seed, &runtime, &marker, "1.1.0").unwrap();
        assert_eq!(out, SyncOutcome::Replaced);
        assert_eq!(std::fs::read_to_string(&runtime).unwrap(), "seed-v2");
        assert_eq!(
            std::fs::read_to_string(prev_path(&runtime)).unwrap(),
            "runtime-旧库"
        );
        assert_eq!(read_marker(&marker), "1.1.0");
    }

    #[test]
    fn no_downgrade_when_app_version_is_older() {
        let _g = LOCK.lock().unwrap();
        let (_d, seed, runtime, marker) = setup("nodowngrade", "seed-老");
        std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
        std::fs::write(&runtime, "runtime-新库").unwrap();
        std::fs::write(&marker, "1.1.0").unwrap();

        // 用户装回旧版：不得用旧种子覆盖新库
        let out = sync(&seed, &runtime, &marker, "1.0.0").unwrap();
        assert_eq!(out, SyncOutcome::UpToDate);
        assert_eq!(std::fs::read_to_string(&runtime).unwrap(), "runtime-新库");
        assert_eq!(read_marker(&marker), "1.1.0");
    }

    #[test]
    fn missing_seed_is_an_error() {
        let _g = LOCK.lock().unwrap();
        let dir = temp_dir("noseed");
        let seed = dir.join("nope").join("arknights.db");
        let runtime = dir.join("root").join("arknights.db");
        let marker = dir.join("root").join(DB_APPLIED_MARKER);
        let err = sync(&seed, &runtime, &marker, "1.0.0").unwrap_err();
        assert!(err.contains("初始副本不存在"), "错误应说明种子缺失：{err}");
    }

    #[test]
    fn tmp_and_prev_names_are_siblings() {
        let runtime = PathBuf::from("C:/x/arknights.db");
        assert_eq!(tmp_path(&runtime), PathBuf::from("C:/x/arknights.db.tmp"));
        assert_eq!(prev_path(&runtime), PathBuf::from("C:/x/arknights.db.prev"));
    }

    #[test]
    fn version_compare() {
        assert!(version_gt("1.1.0", "1.0.0"));
        assert!(version_gt("0.2.0", "0.1.9"));
        assert!(version_gt("1.0.1", "1.0.0"));
        assert!(!version_gt("1.0.0", "1.0.0"));
        assert!(!version_gt("1.0.0", "1.1.0"));
        // 非数字段退化为字符串比较，不 panic
        assert!(version_gt("1.0.0-beta", "1.0.0-alpha"));
    }
}
