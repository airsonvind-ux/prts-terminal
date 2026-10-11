//! path_guard —— 路径守卫（M1 地基）：读写分离的安全路径解析
//!
//! 升级自 tools/fs.rs 的 `resolve_path`：原实现绝对/相对拼接 + canonicalize，
//! **无边界检查**（read_file 当前可读任意绝对路径）。本模块补上策略判定：
//!
//! - [`resolve_read`]：解析 → canonicalize → BLOCKED 检查 → 放行。
//!   默认放行全盘（`Policy::check_read` 仅查 BLOCKED），只堵台账/数据库。
//! - [`resolve_write`]：**目标不存在也允许**——父目录 canonicalize + join
//!   文件名 → canonicalize 之后做边界判定（工作区内 → 非 BLOCKED → 非
//!   protected → 非 READ_ONLY）。父目录必须存在（edit 局部手术用）。
//! - [`resolve_write_create`]：父目录**不存在也允许**（write_file 全量写入用）
//!   ——沿父链上溯最近存在的目录祖先 A，A canonicalize 为真实路径基准，
//!   目标 = A_canon.join(缺失段) 后做与 resolve_write 同一序列的边界判定。
//!   **只判不创建**：目录创建由工具层 create_dir_all 完成（守卫不写盘）。
//!
//! 写判定含 BLOCKED（M2 #B 修正）：默认工作区 = data_dir 时 db / 台账文件
//! 位于可写区内，不堵则写工具可改写运行中数据库——写与读同拒 BLOCKED。
//!
//! 边界检查一律发生在 canonicalize 之后（符号链接 / `..` 解析后的真实
//! 路径为准，防止 `workspace/../knowledge/x.md` 绕过前缀检查）。
//!
//! 守卫返回语义化错误（中文文案，供工具层映射为 ToolError）；
//! "文件不存在：{raw}" 等面向用户的细节文案由工具层包装。

use std::fmt;
use std::path::{Path, PathBuf};

use crate::policy::Policy;

/// 守卫拒绝 / 失败原因（语义化分类，供工具层映射为 ToolError 文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathGuardError {
    /// BLOCKED：名单内路径，连读都不行
    Blocked(String),
    /// READ_ONLY 区写入被拒
    ReadOnly(String),
    /// protected 区（工作区 .git）写入被拒
    Protected(String),
    /// 目标在工作区之外（resolve_write）
    OutsideWorkspace(String),
    /// 路径不存在 / IO 失败 / 无法解析
    Io(String),
}

impl fmt::Display for PathGuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked(m) => write!(f, "路径受保护，禁止读取：{m}"),
            Self::ReadOnly(m) => write!(f, "只读路径，禁止写入：{m}"),
            Self::Protected(m) => write!(f, "工作区保护路径，禁止写入：{m}"),
            Self::OutsideWorkspace(m) => write!(f, "目标不在工作区内：{m}"),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for PathGuardError {}

/// 读路径解析：绝对路径直接用 / 相对路径 join workspace_root →
/// canonicalize（失败即"不存在"）→ BLOCKED 检查 → 放行。
///
/// 工作根从 policy 取**当前值**（改设置即跟随；不重复传参）。返回值已
/// canonicalize。
pub fn resolve_read(policy: &Policy, raw: &str) -> Result<PathBuf, PathGuardError> {
    let raw_path = Path::new(raw);
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        policy.workspace_root().canonical().join(raw)
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|_| PathGuardError::Io(format!("文件不存在：{raw}")))?;
    if policy.is_blocked(&canonical) {
        return Err(PathGuardError::Blocked(format!("受保护路径（BLOCKED）：{raw}")));
    }
    Ok(canonical)
}

/// 写边界判定公共序列（resolve_write / resolve_write_create 共用，防判定漂移）。
///
/// 对象 = canonicalize 之后的真实路径。顺序即错误优先级：工作区内
/// （否则 OutsideWorkspace）→ 非 BLOCKED（M2 #B：写同读拒 BLOCKED，防改写
/// 运行中 db / 台账）→ 非 protected（.git）→ 非 READ_ONLY。
fn check_write_bounds(
    policy: &Policy,
    resolved: &Path,
    raw: &str,
) -> Result<(), PathGuardError> {
    if !policy.is_in_workspace(resolved) {
        return Err(PathGuardError::OutsideWorkspace(format!(
            "目标不在工作区内：{raw}"
        )));
    }
    if policy.is_blocked(resolved) {
        return Err(PathGuardError::Blocked(format!(
            "受保护路径（BLOCKED）：{raw}"
        )));
    }
    if policy.is_protected(resolved) {
        return Err(PathGuardError::Protected(format!(
            "工作区保护路径（.git）：{raw}"
        )));
    }
    if policy.is_read_only(resolved) {
        return Err(PathGuardError::ReadOnly(format!("只读路径：{raw}")));
    }
    Ok(())
}

/// 写路径解析：目标不存在也允许——
/// 父目录 canonicalize（必须存在）+ join 文件名 → canonicalize 之后的
/// 真实路径做边界判定（check_write_bounds：工作区内 → 非 BLOCKED →
/// 非 protected → 非 READ_ONLY）。
///
/// edit（局部手术）走本函数：文件必已存在，父目录必存在，不扩张建目录语义。
pub fn resolve_write(policy: &Policy, raw: &str) -> Result<PathBuf, PathGuardError> {
    let raw_path = Path::new(raw);
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        policy.workspace_root().canonical().join(raw)
    };
    let parent = candidate.parent().ok_or_else(|| {
        PathGuardError::Io(format!("无法解析路径：{raw}"))
    })?;
    let file_name = candidate.file_name().ok_or_else(|| {
        PathGuardError::Io(format!("路径缺少文件名：{raw}"))
    })?;
    // 父目录必须存在（可 canonicalize）；文件名不参与解析——目标不存在
    // 也允许（新建文件场景）
    let canonical_parent = parent.canonicalize().map_err(|_| {
        PathGuardError::Io(format!("目录不存在：{}", parent.display()))
    })?;
    let resolved = canonical_parent.join(file_name);

    check_write_bounds(policy, &resolved, raw)?;
    Ok(resolved)
}

/// 写路径解析（父目录自动创建版，write_file 全量写入用）：
/// 目标不存在、父目录不存在都允许——自 candidate 沿父链上溯，找最近
/// 存在的目录祖先 A（当前工作根必存在）；A canonicalize 后的真实路径
/// 为基准，目标 = A_canon.join(缺失段)（缺失段不含 `.`/`..`——其存在性已
/// 由上溯消解）；父链中段遇普通文件（如 `ws/a.txt/b.txt`）视为不可解析。
///
/// 边界判定对象 = canonicalize 后的完整目标路径，序列同 resolve_write
/// （check_write_bounds）。**只判不创建**：目录创建由工具层 create_dir_all
/// 完成（守卫不写盘）。
pub fn resolve_write_create(policy: &Policy, raw: &str) -> Result<PathBuf, PathGuardError> {
    let raw_path = Path::new(raw);
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        policy.workspace_root().canonical().join(raw)
    };

    // ── 1. 上溯找最近存在的目录祖先 A；沿途不存在组件（与「目标是既有
    //    文件」场景的目标文件名）压入待重建段，自底向上 ──
    let mut segments: Vec<std::ffi::OsString> = Vec::new();
    let mut current = candidate.clone();
    loop {
        match std::fs::metadata(&current) {
            Ok(meta) if meta.is_dir() => break, // A = current
            Ok(_) => {
                // 存在但非目录：仅当它就是目标自身（覆盖既有文件）时继续
                // 上溯父目录；否则父链中段是文件（ws/a.txt/b.txt）
                if current != candidate {
                    return Err(PathGuardError::Io(format!("父路径不是目录：{raw}")));
                }
                let name = current.file_name().ok_or_else(|| {
                    PathGuardError::Io(format!("路径缺少文件名：{raw}"))
                })?;
                segments.push(name.to_os_string());
                current = current.parent().ok_or_else(|| {
                    PathGuardError::Io(format!("无法解析路径：{raw}"))
                })?.to_path_buf();
            }
            Err(_) => {
                // 不存在：压入组件继续上溯
                let name = current.file_name().ok_or_else(|| {
                    PathGuardError::Io(format!("无法解析路径：{raw}"))
                })?;
                segments.push(name.to_os_string());
                match current.parent() {
                    Some(p) => current = p.to_path_buf(),
                    None => {
                        return Err(PathGuardError::Io(format!("无法解析路径：{raw}")))
                    }
                }
            }
        }
    }
    // segments 自底向上（[c.txt, b, a]）；反序后从 A 拼出完整目标
    let canonical_ancestor = current.canonicalize().map_err(|_| {
        PathGuardError::Io(format!("目录不存在：{}", current.display()))
    })?;
    let mut resolved = canonical_ancestor;
    for seg in segments.iter().rev() {
        resolved.push(seg);
    }

    // ── 2. 边界判定（对象 = canonicalize 后的真实目标路径）──
    check_write_bounds(policy, &resolved, raw)?;
    Ok(resolved)
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一临时目录（不引入 tempfile crate，沿用 fs.rs 夹具模式）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-pg-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 工作区夹具：ws/ 含 .git/、sub/ 与 a.txt；ro/ 为工作区内
    /// READ_ONLY 演示目录（注入 read_only 名单）。
    fn ws_fixture(tag: &str) -> PathBuf {
        let ws = unique_temp_dir(tag);
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::create_dir_all(ws.join("ro")).unwrap();
        std::fs::write(ws.join("a.txt"), "hello").unwrap();
        ws
    }

    fn make_policy(ws: &Path) -> Policy {
        let mut p = Policy::new(ws.to_path_buf());
        p.read_only.push(ws.join("ro"));
        p
    }

    /// 工作区内注入 blocked 文件的策略（#B 真分支：默认工作区 = data_dir
    /// 时 db / 台账即「工作区内 blocked 文件」形态，此处用注入复现）。
    fn make_blocked_policy(ws: &Path) -> Policy {
        let mut p = Policy::new(ws.to_path_buf());
        p.blocked.push(ws.join("secret.txt"));
        p
    }

    // ── resolve_read ──

    #[test]
    fn read_relative_path_resolves_from_workspace() {
        let ws = ws_fixture("rd-rel");
        let policy = make_policy(&ws);

        let out = resolve_read(&policy, "a.txt").unwrap();
        assert_eq!(out, ws.join("a.txt").canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_absolute_path_outside_workspace_allowed() {
        // 读默认放行全盘（仅查 BLOCKED）：工作区外普通文件可读
        let ws = ws_fixture("rd-abs");
        let policy = make_policy(&ws);
        let other = unique_temp_dir("rd-out");
        std::fs::write(other.join("x.txt"), "x").unwrap();

        let abs = other.join("x.txt");
        let out = resolve_read(&policy, abs.to_str().unwrap()).unwrap();
        assert_eq!(out, abs.canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_blocked_db_path_rejected() {
        let ws = ws_fixture("rd-blocked");
        let policy = make_policy(&ws);

        // dev 下 arknights.db 真实存在 → canonicalize 成功 → BLOCKED 命中
        let db = crate::config::db_seed_path();
        assert!(db.exists(), "dev 数据库种子应存在（测试前提）");
        let err = resolve_read(&policy, db.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(&err, PathGuardError::Blocked(_)),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_missing_file_io_error() {
        let ws = ws_fixture("rd-missing");
        let policy = make_policy(&ws);

        let err = resolve_read(&policy, "nope.txt").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::Io(m) if m.contains("文件不存在：nope.txt")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_traversal_canonicalized_before_blocked_check() {
        // `..` 穿越在 canonicalize 阶段被解析——守卫只看真实路径。
        // 相对穿越到 workspace 之外仍可读（读放行全盘，仅 BLOCKED 拒绝），
        // 验证边界检查发生在 canonicalize 之后（不误伤、不漏判）
        let ws = ws_fixture("rd-dotdot");
        let policy = make_policy(&ws);
        let other = unique_temp_dir("rd-target");
        std::fs::write(other.join("target.txt"), "t").unwrap();

        // ws 与 other 同处 temp_dir（unique_temp_dir 保证）→
        // ../{sibling}/target.txt 经 canonicalize 解析为 other 下真实路径
        let sibling_name = other.file_name().unwrap().to_string_lossy();
        let rel = format!("../{sibling_name}/target.txt");
        let out = resolve_read(&policy, &rel).unwrap();
        assert_eq!(out, other.join("target.txt").canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── resolve_write ──

    #[test]
    fn write_new_file_in_workspace_allowed() {
        // 目标不存在也允许（父目录存在即可）
        let ws = ws_fixture("wr-new");
        let policy = make_policy(&ws);
        let canon_ws = ws.canonicalize().unwrap();

        let out = resolve_write(&policy, "new.txt").unwrap();
        assert_eq!(out, canon_ws.join("new.txt"));
        assert!(!out.exists(), "守卫只解析不创建，目标仍应不存在");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_nested_new_file_allowed() {
        let ws = ws_fixture("wr-nested");
        let policy = make_policy(&ws);
        let canon_sub = ws.join("sub").canonicalize().unwrap();

        let out = resolve_write(&policy, "sub/new.txt").unwrap();
        assert_eq!(out, canon_sub.join("new.txt"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_outside_workspace_rejected() {
        let ws = ws_fixture("wr-outside");
        let policy = make_policy(&ws);
        let other = unique_temp_dir("wr-target");

        let abs = other.join("x.txt");
        let err = resolve_write(&policy, abs.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(&err, PathGuardError::OutsideWorkspace(_)),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_git_dir_rejected() {
        let ws = ws_fixture("wr-git");
        let policy = make_policy(&ws);

        // .git 下文件（写入对象库/HEAD 等）
        let err = resolve_write(&policy, ".git/config").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::Protected(_)),
            "got: {err}"
        );
        // .git 目录自身（resolved == .git → 前缀命中 protected）
        let err2 = resolve_write(&policy, ".git").unwrap_err();
        assert!(
            matches!(&err2, PathGuardError::Protected(_)),
            "got: {err2}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_read_only_dir_rejected() {
        let ws = ws_fixture("wr-ro");
        let policy = make_policy(&ws); // ws/ro 注入 read_only

        let err = resolve_write(&policy, "ro/x.txt").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::ReadOnly(_)),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── #B（M2）：写判定同读拒 BLOCKED ──

    #[test]
    fn write_blocked_file_rejected() {
        // blocked 文件（如默认工作区 = data_dir 时的 db / env-ledger.json）
        // 位于可写区内：写同读拒 BLOCKED，防改写运行中数据库/台账
        let ws = ws_fixture("wr-blocked");
        let policy = make_blocked_policy(&ws); // ws/secret.txt 注入 blocked
        std::fs::write(ws.join("secret.txt"), "secret").unwrap();

        let err = resolve_write(&policy, "secret.txt").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::Blocked(_)),
            "got: {err}"
        );

        // 同路径经 resolve_write_create（覆盖既有 blocked 文件）亦拒：
        // 写工具共用同一判定序列
        let err2 = resolve_write_create(&policy, "secret.txt").unwrap_err();
        assert!(
            matches!(&err2, PathGuardError::Blocked(_)),
            "got: {err2}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_workspace_normal_file_unaffected_by_blocked() {
        // blocked 检查只堵名单内文件，不误伤工作区普通路径
        let ws = ws_fixture("wr-blocked-ok");
        let policy = make_blocked_policy(&ws);
        let canon_ws = ws.canonicalize().unwrap();

        let out = resolve_write(&policy, "normal.txt").unwrap();
        assert_eq!(out, canon_ws.join("normal.txt"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_parent_dir_missing_io_error() {
        let ws = ws_fixture("wr-nodir");
        let policy = make_policy(&ws);

        let err = resolve_write(&policy, "no-such-dir/x.txt").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::Io(m) if m.contains("目录不存在")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_traversal_escape_rejected() {
        // `workspace/../xxx` 穿越：canonicalize 后落在工作区外 → 拒绝
        let ws = ws_fixture("wr-dotdot");
        let policy = make_policy(&ws);
        let other = unique_temp_dir("wr-target");
        // 确保目标目录真实存在（父目录 canonicalize 要求）
        let sibling_name = other.file_name().unwrap().to_string_lossy();
        let rel = format!("../{sibling_name}/escape.txt");

        let err = resolve_write(&policy, &rel).unwrap_err();
        assert!(
            matches!(&err, PathGuardError::OutsideWorkspace(_)),
            ".. 穿越到工作区外应拒绝，got: {err}"
        );

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── resolve_write_create（父目录自动创建版，write_file 用）──

    #[test]
    fn write_create_nested_missing_dirs_allowed() {
        // 多层缺目录：目标不存在也允许（最近存在祖先 A 之下的缺失段可重建）
        let ws = ws_fixture("wrc-nested");
        let policy = make_policy(&ws);
        let canon_sub = ws.join("sub").canonicalize().unwrap();

        let out = resolve_write_create(&policy, "sub/a/b/new.txt").unwrap();
        assert_eq!(out, canon_sub.join("a").join("b").join("new.txt"));
        assert!(!out.exists(), "守卫只解析不创建，目标仍应不存在");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_all_dirs_missing_under_root_allowed() {
        // 从工作区根起全部缺失（A = workspace_root 自身）
        let ws = ws_fixture("wrc-root");
        let policy = make_policy(&ws);
        let canon_ws = ws.canonicalize().unwrap();

        let out = resolve_write_create(&policy, "new/deep/file.txt").unwrap();
        assert_eq!(out, canon_ws.join("new").join("deep").join("file.txt"));

        // 工作区根下缺 1 层（父目录存在）——与 resolve_write 语义一致
        let out2 = resolve_write_create(&policy, "top.txt").unwrap();
        assert_eq!(out2, canon_ws.join("top.txt"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_existing_file_overwrite_allowed() {
        // 覆盖既有文件：目标自身是文件 → 当重建段上溯，A = 父目录
        let ws = ws_fixture("wrc-exist");
        let policy = make_policy(&ws);
        std::fs::write(ws.join("a.txt"), "hello").unwrap();
        let canon_file = ws.join("a.txt").canonicalize().unwrap();

        let out = resolve_write_create(&policy, "a.txt").unwrap();
        assert_eq!(out, canon_file);
        assert!(out.exists(), "既有文件覆盖场景：目标保持存在");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_existing_dir_target_resolves_itself() {
        // 目标是既有目录：守卫解析返回自身（判定通过），是否拒由工具层判
        let ws = ws_fixture("wrc-isdir");
        let policy = make_policy(&ws);
        let canon_sub = ws.join("sub").canonicalize().unwrap();

        let out = resolve_write_create(&policy, "sub").unwrap();
        assert_eq!(out, canon_sub);

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_outside_workspace_rejected() {
        let ws = ws_fixture("wrc-outside");
        let policy = make_policy(&ws);
        let other = unique_temp_dir("wrc-target");

        let abs = other.join("x.txt");
        let err = resolve_write_create(&policy, abs.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(&err, PathGuardError::OutsideWorkspace(_)),
            "got: {err}"
        );

        // 工作区外缺多层目录同样拒绝（A 在工作区外 → resolved 在工作区外）
        let abs2 = other.join("n1/n2/x.txt");
        let err2 = resolve_write_create(&policy, abs2.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(&err2, PathGuardError::OutsideWorkspace(_)),
            "got: {err2}"
        );

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_traversal_escape_rejected() {
        // `..` 穿越：上溯经真实路径（canonicalize 后）判定 → 工作区外拒绝
        let ws = ws_fixture("wrc-dotdot");
        let policy = make_policy(&ws);
        let other = unique_temp_dir("wrc-target");
        let sibling_name = other.file_name().unwrap().to_string_lossy();
        let rel = format!("../{sibling_name}/escape.txt");

        let err = resolve_write_create(&policy, &rel).unwrap_err();
        assert!(
            matches!(&err, PathGuardError::OutsideWorkspace(_)),
            ".. 穿越到工作区外应拒绝，got: {err}"
        );

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_read_only_prefix_rejected() {
        // RO 前缀下缺目录（新建链起点在只读区）：A = ro → resolved 命中 RO
        let ws = ws_fixture("wrc-ro");
        let policy = make_policy(&ws); // ws/ro 注入 read_only

        let err = resolve_write_create(&policy, "ro/n1/x.txt").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::ReadOnly(_)),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_git_dir_rejected() {
        let ws = ws_fixture("wrc-git");
        let policy = make_policy(&ws);

        // .git 下缺目录的新建链（写入对象库/HEAD 等）→ protected
        let err = resolve_write_create(&policy, ".git/objects/aa/x.txt").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::Protected(_)),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_create_parent_path_is_file_io_error() {
        // 祖先链中段是文件（ws/a.txt/b.txt）：a.txt 非目录不可作父路径
        let ws = ws_fixture("wrc-fileparent");
        let policy = make_policy(&ws);
        std::fs::write(ws.join("a.txt"), "hello").unwrap();

        let err = resolve_write_create(&policy, "a.txt/b.txt").unwrap_err();
        assert!(
            matches!(&err, PathGuardError::Io(m) if m.contains("父路径不是目录")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn guard_error_display_messages() {
        let e = PathGuardError::Blocked("x".into());
        assert!(e.to_string().contains("禁止读取"));
        let e = PathGuardError::ReadOnly("x".into());
        assert!(e.to_string().contains("只读"));
        let e = PathGuardError::Protected("x".into());
        assert!(e.to_string().contains("保护"));
        let e = PathGuardError::OutsideWorkspace("x".into());
        assert!(e.to_string().contains("工作区"));
        let e = PathGuardError::Io("文件不存在：x".into());
        assert!(e.to_string().contains("文件不存在"));
    }
}
