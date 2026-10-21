use super::prelude::*;

// ═══════════════════════════════════════════════════════
// 空间页 · 工作区查看（目录树 / 按名搜索 / 资源管理器）
// ═══════════════════════════════════════════════════════
//
// 定位：让用户能“舒服地查看当前的工作目录”。树按需逐层读取（点开哪级读哪级）；
// 搜索递归按名匹配并跳过隐藏项与依赖/构建目录。两侧口径有意不同：树照实列
// （＝真实目录，也与 list_dir 一致），搜索降噪（结果才有用）。

/// 树节点 DTO（list_workspace_dir 返回）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub name: String,
    pub is_dir: bool,
    /// 绝对路径（普通形式，非 canonical）
    pub path: String,
}

/// 搜索命中 DTO（search_workspace_files 返回）。
///
/// 为什么要带 `path` 与 `ancestorDirs`：前端不拼路径（与 FileLink 同口径）——
/// 命中后要在树里展开祖先并选中，展开链由后端给出，前端只做状态合并。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchHit {
    pub name: String,
    /// 绝对路径（点击打开 / 树选中定位）
    pub path: String,
    /// 相对工作区根，'/' 分隔（展示）
    pub relative_path: String,
    /// 相对根的父目录（根级命中 = 空串；展示 + 兜底）
    pub parent_subdir: String,
    /// 从根下一级到命中项所在目录的绝对路径链（展开用，根级命中为空）
    pub ancestor_dirs: Vec<String>,
    pub is_dir: bool,
}

/// 搜索跳过的目录名（隐藏项另按 '.' 前缀跳过）。
const WORKSPACE_SEARCH_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "__pycache__",
];

/// 搜索结果上限（照 Hanako 工作台的 WORKSPACE_SEARCH_LIMIT = 80）。
pub(super) const WORKSPACE_SEARCH_LIMIT: usize = 80;
/// 搜索递归深度上限（根 = 0）。
const WORKSPACE_SEARCH_MAX_DEPTH: usize = 12;
/// 搜索扫描条目预算（防超大目录树把界面拖住）。
const WORKSPACE_SEARCH_SCAN_BUDGET: usize = 20_000;

/// 解析工作区内的目标路径。
///
/// `requested` 为空 → 根自身；非空必须是绝对路径，且（canonical 化后）落在
/// 工作区根内——越界一律拒绝。返回值保持传入口径（非 canonical），供前端回用。
pub(super) fn resolve_workspace_target(
    root: &std::path::Path,
    requested: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let target = match requested.map(str::trim).filter(|p| !p.is_empty()) {
        None => root.to_path_buf(),
        Some(p) => {
            let pb = std::path::PathBuf::from(p);
            if !pb.is_absolute() {
                return Err(format!("需要绝对路径：{p}"));
            }
            pb
        }
    };
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("工作区根不可访问：{e}"))?;
    let canon_target = target
        .canonicalize()
        .map_err(|e| format!("路径不可访问：{e}"))?;
    if !canon_target.starts_with(&canon_root) {
        return Err("路径不在工作区内".to_string());
    }
    Ok(target)
}

/// 列一层目录：目录在前，各自按名排序（大小写不敏感，末位比原串保确定）。
pub(super) fn list_dir_entries(dir: &std::path::Path) -> Result<Vec<WorkspaceEntry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败：{e}"))?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(WorkspaceEntry {
            name,
            is_dir,
            path: entry.path().to_string_lossy().into_owned(),
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// 递归按名搜索（大小写不敏感子串）。
///
/// 跳过点开头项与依赖/构建目录；深度 ≤ MAX_DEPTH；扫描条目 ≤ BUDGET；
/// 命中数达上限即停。结果按相对路径排序（确定序）。
pub(super) fn search_workspace_entries(
    root: &std::path::Path,
    query: &str,
) -> Vec<WorkspaceSearchHit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<WorkspaceSearchHit> = Vec::new();
    let mut budget = WORKSPACE_SEARCH_SCAN_BUDGET;
    let mut stack: Vec<(std::path::PathBuf, String, Vec<String>, usize)> =
        vec![(root.to_path_buf(), String::new(), Vec::new(), 0)];
    'outer: while let Some((dir, rel, chain, depth)) = stack.pop() {
        if depth > WORKSPACE_SEARCH_MAX_DEPTH {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut subdirs: Vec<(std::path::PathBuf, String, Vec<String>, usize)> = Vec::new();
        for entry in rd.flatten() {
            if budget == 0 {
                break 'outer;
            }
            budget -= 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let child_abs = entry.path().to_string_lossy().into_owned();
            if name.to_lowercase().contains(&needle) {
                hits.push(WorkspaceSearchHit {
                    name: name.clone(),
                    path: child_abs.clone(),
                    relative_path: child_rel.clone(),
                    parent_subdir: rel.clone(),
                    ancestor_dirs: chain.clone(),
                    is_dir,
                });
                if hits.len() >= WORKSPACE_SEARCH_LIMIT {
                    break 'outer;
                }
            }
            if is_dir && !WORKSPACE_SEARCH_SKIP_DIRS.contains(&name.to_lowercase().as_str()) {
                let mut child_chain = chain.clone();
                child_chain.push(child_abs);
                subdirs.push((entry.path(), child_rel, child_chain, depth + 1));
            }
        }
        for item in subdirs.into_iter().rev() {
            stack.push(item);
        }
    }
    hits.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    hits
}

/// 列出工作区内一层目录（path 省略 = 根）。
#[tauri::command]
pub fn list_workspace_dir(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    path: Option<String>,
) -> Result<Vec<WorkspaceEntry>, String> {
    let root = {
        let st = store.lock().map_err(|e| format!("设置锁获取失败：{e}"))?;
        st.workspace_root()
    };
    let dir = resolve_workspace_target(&root, path.as_deref())?;
    list_dir_entries(&dir)
}

/// 在工作区内按名字递归搜索（跳过隐藏项与依赖/构建目录）。
#[tauri::command]
pub fn search_workspace_files(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    query: String,
) -> Result<Vec<WorkspaceSearchHit>, String> {
    let root = {
        let st = store.lock().map_err(|e| format!("设置锁获取失败：{e}"))?;
        st.workspace_root()
    };
    Ok(search_workspace_entries(&root, &query))
}

/// 用系统文件管理器打开工作区根。
///
/// 目录不能走 open_path（那里只放行文件），故单开一条命令；目标固定为工作区根、
/// 不经前端传参，边界最清楚。
#[tauri::command]
pub fn open_workspace_in_explorer(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
) -> Result<(), String> {
    let root = {
        let st = store.lock().map_err(|e| format!("设置锁获取失败：{e}"))?;
        st.workspace_root()
    };
    let dir = resolve_workspace_target(&root, None)?;
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(&dir).spawn();
    spawned.map_err(|e| format!("打开文件管理器失败：{e}"))?;
    Ok(())
}
