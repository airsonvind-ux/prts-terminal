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
//! - 存在但数据版本落后于种子 → 替换（旧的留成 `.prev`）；
//! - 不落后 → 不动。
//!
//! 替换判据优先用数据库 `meta.dataVersion`；旧库没有 `meta` 时退回数据根的
//! `.db-applied` 标记。**不再用应用版本号判断数据库新旧**：应用版本只负责
//! 更新器排序，数据版本才描述库内数据。写入一律临时文件加原子改名。
//!
//! 失败不静默：同步失败意味着数据根写不进去（磁盘满 / 权限 / 被拦截），
//! 由调用方（lib.rs 装配期）走启动失败通道。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// 数据库工件元信息。字段名与 `meta` 表列名、`DATA_MANIFEST.json` 对齐。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbMeta {
    pub data_version: String,
    pub generated_at: String,
    pub source_commit: String,
    pub schema_version: u32,
}

/// 同步结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// 运行时副本已存在且不落后于种子数据版本，未动。
    UpToDate,
    /// 运行时副本不存在，已从种子复制。
    Copied,
    /// 运行时副本落后于种子数据版本，已替换（旧的留成 `.prev`）。
    Replaced,
}

/// 装配期入口：按 config 解析路径并同步。
pub fn sync_on_boot() -> Result<SyncOutcome, String> {
    let seed = crate::config::db_seed_path();
    let runtime = crate::config::resolve_db_path();
    let marker = crate::config::data_dir().join(DB_APPLIED_MARKER);
    let seed_meta = read_meta(&seed)?.ok_or_else(|| {
        format!(
            "数据库初始副本缺少 meta 表（需要 dataVersion/schemaVersion）：{}",
            seed.display()
        )
    })?;
    sync(&seed, &runtime, &marker, &seed_meta)
}

/// 已应用的数据版本标记（数据根；用户不可见）。
///
/// 旧版应用写的是应用版本，新版写 `dataVersion`；两者都是点分数字，可直接比较。
pub const DB_APPLIED_MARKER: &str = ".db-applied";

/// 同步本体（路径与种子元信息显式传入，便于单测）。
pub fn sync(
    seed: &Path,
    runtime: &Path,
    marker: &Path,
    seed_meta: &DbMeta,
) -> Result<SyncOutcome, String> {
    if !seed.is_file() {
        return Err(format!("数据库初始副本不存在：{}", seed.display()));
    }
    let seed_len = std::fs::metadata(seed)
        .map_err(|e| format!("读取数据库初始副本失败（{}）：{e}", seed.display()))?
        .len();
    if seed_len == 0 {
        return Err(format!("数据库初始副本为空：{}", seed.display()));
    }
    if let Some(dir) = runtime.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("创建数据根失败（{}）：{e}", dir.display()))?;
    }

    if runtime.is_file() {
        match read_meta(runtime)? {
            Some(runtime_meta) => {
                if runtime_meta.schema_version > seed_meta.schema_version {
                    return Err(format!(
                        "运行时数据库结构版本 {} 高于当前应用支持的 {}；请安装对应的新版本，或手动恢复 {}",
                        runtime_meta.schema_version,
                        seed_meta.schema_version,
                        prev_path(runtime).display()
                    ));
                }
                if runtime_meta.schema_version == seed_meta.schema_version
                    && !version_gt(&seed_meta.data_version, &runtime_meta.data_version)
                {
                    return Ok(SyncOutcome::UpToDate);
                }
            }
            None => {
                // 旧库没有 meta：用数据根标记判断。标记缺失等价“从未应用过”，
                // 此时替换一次，把旧库带进可追溯状态。
                let applied = read_marker(marker);
                if !version_gt(&seed_meta.data_version, &applied) {
                    return Ok(SyncOutcome::UpToDate);
                }
            }
        }
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
    std::fs::copy(seed, &tmp).map_err(|e| format!("写临时文件失败（{}）：{e}", tmp.display()))?;
    std::fs::rename(&tmp, runtime)
        .map_err(|e| format!("替换运行时副本失败（{}）：{e}", runtime.display()))?;
    write_marker(marker, &seed_meta.data_version)?;

    Ok(if replacing {
        SyncOutcome::Replaced
    } else {
        SyncOutcome::Copied
    })
}

/// 读取数据库 `meta` 表。
///
/// - 文件不存在或没有 `meta` 表 → `Ok(None)`（旧库兼容路径）；
/// - `meta` 表存在但为空或字段非法 → `Err`（不能静默当成旧库）。
pub fn read_meta(path: &Path) -> Result<Option<DbMeta>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("无法打开数据库 {}：{e}", path.display()))?;

    let has_meta: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("读取数据库表清单失败（{}）：{e}", path.display()))?;
    if has_meta == 0 {
        return Ok(None);
    }

    let row = conn
        .query_row(
            "SELECT dataVersion, generatedAt, sourceCommit, schemaVersion FROM meta LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("读取数据库 meta 失败（{}）：{e}", path.display()))?;

    let Some((data_version, generated_at, source_commit, schema_version)) = row else {
        return Err(format!("数据库 meta 表为空：{}", path.display()));
    };
    if data_version.trim().is_empty() {
        return Err(format!("数据库 meta.dataVersion 为空：{}", path.display()));
    }
    if generated_at.trim().is_empty() {
        return Err(format!("数据库 meta.generatedAt 为空：{}", path.display()));
    }
    if source_commit.trim().is_empty() {
        return Err(format!("数据库 meta.sourceCommit 为空：{}", path.display()));
    }
    let schema_version = u32::try_from(schema_version)
        .map_err(|_| format!("数据库 meta.schemaVersion 非法：{schema_version}"))?;
    if schema_version == 0 {
        return Err(format!(
            "数据库 meta.schemaVersion 不能为 0：{}",
            path.display()
        ));
    }

    Ok(Some(DbMeta {
        data_version,
        generated_at,
        source_commit,
        schema_version,
    }))
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
        let dir =
            std::env::temp_dir().join(format!("prts-dbseed-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn meta(data_version: &str, schema_version: u32) -> DbMeta {
        DbMeta {
            data_version: data_version.to_string(),
            generated_at: "2026-10-18T00:00:00Z".to_string(),
            source_commit: "test".to_string(),
            schema_version,
        }
    }

    fn create_db(path: &Path, meta: Option<&DbMeta>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        if let Some(meta) = meta {
            conn.execute_batch(
                "CREATE TABLE meta (
                    dataVersion TEXT NOT NULL,
                    generatedAt TEXT NOT NULL,
                    sourceCommit TEXT NOT NULL,
                    schemaVersion INTEGER NOT NULL
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO meta (dataVersion, generatedAt, sourceCommit, schemaVersion)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    meta.data_version,
                    meta.generated_at,
                    meta.source_commit,
                    meta.schema_version
                ],
            )
            .unwrap();
        }
    }

    fn setup(tag: &str, seed_meta: &DbMeta) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let seed = dir.join("seed").join("arknights.db");
        create_db(&seed, Some(seed_meta));
        let runtime = dir.join("root").join("arknights.db");
        let marker = dir.join("root").join(DB_APPLIED_MARKER);
        (dir, seed, runtime, marker)
    }

    #[test]
    fn copies_seed_when_runtime_absent() {
        let _g = LOCK.lock().unwrap();
        let seed_meta = meta("2026.10.18", 1);
        let (_d, seed, runtime, marker) = setup("copy", &seed_meta);
        let out = sync(&seed, &runtime, &marker, &seed_meta).unwrap();
        assert_eq!(out, SyncOutcome::Copied);
        assert_eq!(read_meta(&runtime).unwrap(), Some(seed_meta));
        assert_eq!(read_marker(&marker), "2026.10.18");
    }

    #[test]
    fn up_to_date_keeps_runtime_untouched() {
        let _g = LOCK.lock().unwrap();
        let seed_meta = meta("2026.10.18", 1);
        let (_d, seed, runtime, marker) = setup("uptodate", &seed_meta);
        create_db(&runtime, Some(&seed_meta));
        std::fs::write(&marker, "2026.10.18").unwrap();

        let out = sync(&seed, &runtime, &marker, &seed_meta).unwrap();
        assert_eq!(out, SyncOutcome::UpToDate);
        assert_eq!(read_meta(&runtime).unwrap(), Some(seed_meta));
    }

    #[test]
    fn replaces_when_data_version_is_newer_and_keeps_prev() {
        let _g = LOCK.lock().unwrap();
        let seed_meta = meta("2026.10.18", 1);
        let old_meta = meta("2026.10.17", 1);
        let (_d, seed, runtime, marker) = setup("replace", &seed_meta);
        create_db(&runtime, Some(&old_meta));
        std::fs::write(&marker, "2026.10.17").unwrap();

        let out = sync(&seed, &runtime, &marker, &seed_meta).unwrap();
        assert_eq!(out, SyncOutcome::Replaced);
        assert_eq!(read_meta(&runtime).unwrap(), Some(seed_meta));
        assert_eq!(read_meta(&prev_path(&runtime)).unwrap(), Some(old_meta));
        assert_eq!(read_marker(&marker), "2026.10.18");
    }

    #[test]
    fn replaces_legacy_runtime_without_meta_once() {
        let _g = LOCK.lock().unwrap();
        let seed_meta = meta("2026.10.18", 1);
        let (_d, seed, runtime, marker) = setup("legacy", &seed_meta);
        create_db(&runtime, None);
        std::fs::write(&marker, "0.1.0").unwrap();

        let out = sync(&seed, &runtime, &marker, &seed_meta).unwrap();
        assert_eq!(out, SyncOutcome::Replaced);
        assert_eq!(read_meta(&runtime).unwrap(), Some(seed_meta));
        assert_eq!(read_marker(&marker), "2026.10.18");
    }

    #[test]
    fn no_downgrade_when_data_version_is_older() {
        let _g = LOCK.lock().unwrap();
        let seed_meta = meta("2026.10.17", 1);
        let runtime_meta = meta("2026.10.18", 1);
        let (_d, seed, runtime, marker) = setup("nodowngrade", &seed_meta);
        create_db(&runtime, Some(&runtime_meta));
        std::fs::write(&marker, "2026.10.18").unwrap();

        // 用户装回旧版：不得用旧种子覆盖新库
        let out = sync(&seed, &runtime, &marker, &seed_meta).unwrap();
        assert_eq!(out, SyncOutcome::UpToDate);
        assert_eq!(read_meta(&runtime).unwrap(), Some(runtime_meta));
        assert_eq!(read_marker(&marker), "2026.10.18");
    }

    #[test]
    fn newer_schema_replaces_older_schema() {
        let _g = LOCK.lock().unwrap();
        let seed_meta = meta("2026.10.18", 2);
        let old_meta = meta("2026.10.18", 1);
        let (_d, seed, runtime, marker) = setup("schema-up", &seed_meta);
        create_db(&runtime, Some(&old_meta));

        let out = sync(&seed, &runtime, &marker, &seed_meta).unwrap();
        assert_eq!(out, SyncOutcome::Replaced);
        assert_eq!(read_meta(&runtime).unwrap(), Some(seed_meta));
    }

    #[test]
    fn rejects_runtime_schema_newer_than_app() {
        let _g = LOCK.lock().unwrap();
        let seed_meta = meta("2026.10.18", 1);
        let runtime_meta = meta("2026.10.18", 2);
        let (_d, seed, runtime, marker) = setup("schema-down", &seed_meta);
        create_db(&runtime, Some(&runtime_meta));

        let err = sync(&seed, &runtime, &marker, &seed_meta).unwrap_err();
        assert!(err.contains("结构版本"), "错误应说明结构版本：{err}");
        assert_eq!(read_meta(&runtime).unwrap(), Some(runtime_meta));
    }

    #[test]
    fn missing_seed_is_an_error() {
        let _g = LOCK.lock().unwrap();
        let dir = temp_dir("noseed");
        let seed = dir.join("nope").join("arknights.db");
        let runtime = dir.join("root").join("arknights.db");
        let marker = dir.join("root").join(DB_APPLIED_MARKER);
        let err = sync(&seed, &runtime, &marker, &meta("2026.10.18", 1)).unwrap_err();
        assert!(err.contains("初始副本不存在"), "错误应说明种子缺失：{err}");
    }

    #[test]
    fn empty_seed_is_an_error() {
        let _g = LOCK.lock().unwrap();
        let dir = temp_dir("emptyseed");
        let seed = dir.join("seed").join("arknights.db");
        std::fs::create_dir_all(seed.parent().unwrap()).unwrap();
        std::fs::write(&seed, "").unwrap();
        let runtime = dir.join("root").join("arknights.db");
        let marker = dir.join("root").join(DB_APPLIED_MARKER);
        let err = sync(&seed, &runtime, &marker, &meta("2026.10.18", 1)).unwrap_err();
        assert!(err.contains("初始副本为空"), "错误应说明种子为空：{err}");
    }

    #[test]
    fn missing_meta_table_is_none() {
        let _g = LOCK.lock().unwrap();
        let dir = temp_dir("nometa");
        let db = dir.join("arknights.db");
        create_db(&db, None);
        assert_eq!(read_meta(&db).unwrap(), None);
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
        assert!(version_gt("2026.10.18", "2026.10.17"));
        assert!(version_gt("2026.10.18.1", "2026.10.18"));
        assert!(!version_gt("1.0.0", "1.0.0"));
        assert!(!version_gt("1.0.0", "1.1.0"));
        // 非数字段退化为字符串比较，不 panic
        assert!(version_gt("1.0.0-beta", "1.0.0-alpha"));
    }
}
