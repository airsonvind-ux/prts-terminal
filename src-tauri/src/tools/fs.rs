//! fs —— 通用文件系统工具（read_file / list_dir）
//!
//! 路径解析收口于 `path_guard`（M1 沙箱地基，批 2 接入）：
//! 绝对路径直接用 / 相对路径基于工作区拼接，canonicalize 后做策略判定
//! —— 读走 `resolve_read`（默认全盘放行，仅 BLOCKED 拒读：notices.json /
//! env-ledger.json 数据源、arknights.db）；写路径（M2 批 1：write_file / edit 在 fs_write.rs）
//! 走 path_guard 写解析（仅工作区内，排除 BLOCKED / READ_ONLY / protected，
//! 见 M2 #B）：既有文件手术（edit）走 `resolve_write`，父目录自动创建
//! （write_file）走 `resolve_write_create`。守卫层给语义化拒绝，
//! 本模块负责映射为面向用户的 ToolError 文案（守卫 Io 文案与工具层
//! 文案去重：Io 统一映射回"不存在"，其余语义按变体分类，不双重包装）。
//!
//! 文件大小上限与写工具共用 `MAX_FILE_BYTES`（fs_write.rs 定义，读写同值
//! 防上限漂移——保证写工具产出的文件必能被本工具读回）。

use std::io::{BufRead, BufReader};
use std::sync::Arc;

use crate::path_guard;
use crate::policy::Policy;
use crate::tools::fs_write::MAX_FILE_BYTES;
use crate::tools::display::file_link;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

// ═══════════════════════════════════════════════════════
// ReadFileTool —— 读取 UTF-8 文本文件（带行号窗口）
// ═══════════════════════════════════════════════════════

/// 读取 UTF-8 文本文件，返回带行号的内容窗口。
///
/// 大文件用 offset/limit 分页续读；单行超长自动截断；
/// 结果含 totalLines 表示文件总行数。
pub struct ReadFileTool {
    /// 沙箱策略：路径守卫（workspace_root 与 BLOCKED/READ_ONLY/protected
    /// 名单内含；构造时注入，非 ToolContext 执行时通道）
    policy: Arc<Policy>,
}

impl ReadFileTool {
    pub fn new(policy: Arc<Policy>) -> Self {
        Self { policy }
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "读取 UTF-8 文本文件，返回带行号的内容窗口。大文件用 offset/limit 分页续读；结果含 totalLines 表示文件总行数。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "文件路径（绝对路径，或相对工作目录的相对路径）"
                },
                "offset": {
                    "type": "number",
                    "description": "1-based 起始行，默认 1，须为正整数"
                },
                "limit": {
                    "type": "number",
                    "description": "返回行数，默认 1000，上限 2000，超出报错"
                }
            },
            "required": ["path"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let Some(raw) = v
            .get("path")
            .and_then(|p| p.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolDisplay::default();
        };
        ToolDisplay::link(file_link(&self.policy, raw))
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        let raw = v["path"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 path 必须是字符串".into()))?;

        // ── 1. 解析 offset / limit（可选参数，带默认值）──
        let offset = match v.get("offset") {
            None | Some(serde_json::Value::Null) => 1usize,
            Some(n) => {
                let n = n.as_u64().ok_or_else(|| {
                    ToolError::Parse("参数 offset 必须是正整数（1-based 起始行）".into())
                })?;
                if n == 0 {
                    return Err(ToolError::Parse("参数 offset 必须是正整数（1-based 起始行）".into()));
                }
                n as usize
            }
        };
        let limit = match v.get("limit") {
            None | Some(serde_json::Value::Null) => 1000usize,
            Some(n) => {
                let n = n.as_u64().ok_or_else(|| {
                    ToolError::Parse("参数 limit 必须是正整数".into())
                })?;
                if n == 0 {
                    return Err(ToolError::Parse("参数 limit 必须是正整数".into()));
                }
                if n > 2000 {
                    return Err(ToolError::Parse("参数 limit 超出上限 2000".into()));
                }
                n as usize
            }
        };

        // ── 2. 路径解析（守卫：BLOCKED 拒读） + 存在性 / 普通文件校验 ──
        // 守卫 Io（不存在）→ 保持旧文案"文件不存在：{raw}"；Blocked →
        // 面向用户说明受保护；其余变体（写路径拒绝）读路径理论上不出现，
        // 防御性映射。守卫内部消息不回填，避免双重包装。
        let resolved = path_guard::resolve_read(&self.policy, raw).map_err(|e| match e {
            path_guard::PathGuardError::Io(_) => {
                ToolError::Execution(format!("文件不存在：{raw}"))
            }
            path_guard::PathGuardError::Blocked(_) => {
                ToolError::Execution(format!("文件不可读（受保护路径）：{raw}"))
            }
            _ => ToolError::Execution(format!("文件不可读：{raw}")),
        })?;

        let meta = std::fs::metadata(&resolved)
            .map_err(|_| ToolError::Execution(format!("文件不存在：{raw}")))?;
        if !meta.is_file() {
            return Err(ToolError::Execution(format!("不是普通文件：{raw}")));
        }

        // ── 3. 文件大小守卫：超过 10MB 拒绝读取 ──
        // （在逐行读取之前检查，避免大文件无谓 IO；上限与写工具共用
        // MAX_FILE_BYTES——fs_write.rs 定义，读写同值防漂移）
        if meta.len() > MAX_FILE_BYTES {
            let mb = meta.len() as f64 / (1024.0 * 1024.0);
            return Err(ToolError::Execution(format!(
                "文件过大（{mb:.1}MB），超过读取上限 10MB"
            )));
        }

        // ── 4. 逐行读取，只保留窗口内行 ──
        // read_line 读入 String 天然要求 UTF-8（内部 String::from_utf8 语义），
        // 数据非 UTF-8 时返回 InvalidData 错误 → 映射为"不是 UTF-8 文本文件"
        let file = std::fs::File::open(&resolved)
            .map_err(|e| ToolError::Execution(format!("打开文件失败：{e}")))?;
        let mut reader = BufReader::new(file);

        let mut total_lines: usize = 0;
        let mut window: Vec<String> = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    total_lines += 1;
                    if total_lines >= offset && window.len() < limit {
                        // 去换行符（\n 与 \r\n）
                        let mut text = line
                            .trim_end_matches(['\n', '\r'])
                            .to_string();
                        // ── 5. 单行截断：超过 2000 字符 → 截断并追加标记 ──
                        if text.chars().count() > 2000 {
                            text = text.chars().take(2000).collect::<String>();
                            text.push_str(" …[行过长已截断]");
                        }
                        window.push(text);
                    }
                }
                Err(e) => {
                    // read_line 读入 String 对非 UTF-8 数据返回 InvalidData（编码问题）；
                    // 其余 I/O 错误（磁盘/权限等）如实透传
                    if e.kind() == std::io::ErrorKind::InvalidData {
                        return Err(ToolError::Execution("不是 UTF-8 文本文件".into()));
                    }
                    return Err(ToolError::Execution(format!("读取文件失败：{e}")));
                }
            }
        }

        // ── 6. 组装输出 ──
        let mut summary = String::new();
        for (i, text) in window.iter().enumerate() {
            summary.push_str(&format!("{:>4} | {text}\n", offset + i));
        }
        // 窗口是否被 limit 截断且还有剩余
        let truncated = total_lines > (offset - 1) + window.len();

        summary.push_str(&format!(
            "（共 {total_lines} 行，offset={offset}，已返回 {} 行）",
            window.len()
        ));
        if truncated {
            summary.push_str("（可增加 offset 续读）");
        }

        let display_path = resolved.to_string_lossy().to_string();
        let lines: Vec<serde_json::Value> = window
            .iter()
            .enumerate()
            .map(|(i, t)| serde_json::json!({ "number": offset + i, "text": t }))
            .collect();
        let canonical = serde_json::json!({
            "path": display_path,
            "offset": offset,
            "lines": lines,
            "totalLines": total_lines,
            "truncated": truncated,
        });

        Ok(ToolOutput::json(canonical, summary))
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }
}

// ═══════════════════════════════════════════════════════
// ListDirTool —— 列出目录直接子项（一层，不递归）
// ═══════════════════════════════════════════════════════

/// 列出目录的直接子项（一层，不递归）。目录名带 `/` 后缀。
/// path 省略时列出工作目录。
///
/// M2 批 2 恢复内置注册（工作区文件能力；知识库导航仍由 list_knowledge
/// 承担——两者职责不同：list_dir 是文件名导航，list_knowledge 是语义导航）。
pub struct ListDirTool {
    /// 沙箱策略：路径守卫（同 ReadFileTool，构造时注入）
    policy: Arc<Policy>,
}

impl ListDirTool {
    pub fn new(policy: Arc<Policy>) -> Self {
        Self { policy }
    }
}

impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "列出目录的直接子项（一层，不递归）。目录名带 / 后缀。path 省略时列出工作目录。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目录路径（绝对路径，或相对工作目录的相对路径）；省略时列出工作目录"
                }
            }
        })
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        let raw = match v.get("path") {
            None | Some(serde_json::Value::Null) => None,
            Some(p) => Some(
                p.as_str()
                    .ok_or_else(|| ToolError::Parse("参数 path 必须是字符串".into()))?,
            ),
        };
        let label = raw.unwrap_or("（工作目录）");

        // ── 1. 路径解析：缺省 → 工作区根；否则守卫 resolve_read ──
        let resolved = match raw {
            None => self.policy.workspace_root.clone(),
            Some(raw) => path_guard::resolve_read(&self.policy, raw).map_err(|e| match e {
                path_guard::PathGuardError::Io(_) => {
                    ToolError::Execution(format!("目录不存在：{raw}"))
                }
                path_guard::PathGuardError::Blocked(_) => {
                    ToolError::Execution(format!("目录不可读（受保护路径）：{raw}"))
                }
                _ => ToolError::Execution(format!("目录不可读：{raw}")),
            })?,
        };

        // ── 2. 目录校验 ──
        let meta = std::fs::metadata(&resolved)
            .map_err(|_| ToolError::Execution(format!("目录不存在：{label}")))?;
        if !meta.is_dir() {
            return Err(ToolError::Execution(format!("不是目录：{label}")));
        }

        // ── 3. 列直接子项，按名称排序 ──
        let mut entries: Vec<(String, bool)> = Vec::new();
        let rd = std::fs::read_dir(&resolved)
            .map_err(|e| ToolError::Execution(format!("读取目录失败：{e}")))?;
        for entry in rd {
            let entry = entry.map_err(|e| ToolError::Execution(format!("读取目录失败：{e}")))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            entries.push((name, is_dir));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // ── 4. 组装输出 ──
        let summary = if entries.is_empty() {
            "（空目录）".to_string()
        } else {
            entries
                .iter()
                .map(|(n, d)| {
                    if *d {
                        format!("{n}/")
                    } else {
                        n.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let entries_json: Vec<serde_json::Value> = entries
            .iter()
            .map(|(n, d)| serde_json::json!({ "name": n, "isDir": d }))
            .collect();
        let canonical = serde_json::json!({
            "path": resolved.to_string_lossy(),
            "entries": entries_json,
        });

        Ok(ToolOutput::json(canonical, summary))
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 创建唯一临时目录（不引入 tempfile crate）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-fs-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 以临时目录为工作区构造 read_file 工具（沙箱策略注入）。
    fn read_tool(root: &Path) -> ReadFileTool {
        ReadFileTool::new(Arc::new(Policy::new(root.to_path_buf())))
    }

    #[test]
    fn display_resolves_relative_path_to_absolute_file_link() {
        let dir = unique_temp_dir("display");
        std::fs::write(dir.join("note.md"), "hello").unwrap();
        let tool = read_tool(&dir);

        let d = tool.display(&ToolContext::new(), r#"{"path":"note.md"}"#);
        let json = serde_json::to_value(&d).unwrap();
        // 相对路径 → 工作区拼接 + canonicalize（绝对路径）
        assert_eq!(json["link"]["kind"], "file");
        assert_eq!(json["link"]["label"], "note.md");
        let target = json["link"]["target"].as_str().unwrap();
        assert!(std::path::Path::new(target).is_absolute(), "got {target}");
        assert!(target.ends_with("note.md"), "got {target}");

        // 缺 path / 空 path → 空切片（不报错）
        assert!(tool.display(&ToolContext::new(), "{}").is_empty());
        assert!(tool
            .display(&ToolContext::new(), r#"{"path":"  "}"#)
            .is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 以临时目录为工作区构造 list_dir 工具（沙箱策略注入）。
    fn list_tool(root: &Path) -> ListDirTool {
        ListDirTool::new(Arc::new(Policy::new(root.to_path_buf())))
    }

    /// 搭建测试目录（工作区内）：sub/ 与 empty/ 两个子目录，a.txt（3 行）、
    /// long.txt（2500 字符超长行）、bin.bin（非 UTF-8）、
    /// big.txt（11MB 全 'a' 文本，超过读取上限 10MB）。
    fn fixture(root: &Path) {
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("a.txt"), "line1\nline2\nline3\n").unwrap();
        std::fs::write(root.join("long.txt"), format!("{}\n", "x".repeat(2500))).unwrap();
        std::fs::write(root.join("bin.bin"), b"\xff\xfe\x00binary").unwrap();
        // 11MB 全 'a'（合法 UTF-8）→ 命中大小守卫而非编码分支
        let mut big = vec![b'a'; 11 * 1024 * 1024];
        big.push(b'\n');
        std::fs::write(root.join("big.txt"), big).unwrap();
    }

    /// 知识库目录内第一个 .md 文件（READ_ONLY 区可读性验证用）。
    fn first_kb_file() -> PathBuf {
        let kb = crate::knowledge::index::knowledge_dir();
        let mut found = None;
        for entry in std::fs::read_dir(&kb).expect("知识库目录应可读") {
            let p = entry.expect("read_dir 条目应可读").path();
            if p.is_file() && p.extension().map(|e| e == "md").unwrap_or(false) {
                found = Some(p);
                break;
            }
        }
        found.unwrap_or_else(|| panic!("知识库目录应含 .md 文件：{}", kb.display()))
    }

    // ── read_file ──

    #[test]
    fn read_file_relative_path_resolves_from_workspace() {
        let root = unique_temp_dir("rel");
        fixture(&root);
        let tool = read_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"path":"a.txt"}"#)
            .unwrap();
        assert!(out.summary.contains("   1 | line1"));
        assert!(out.summary.contains("   2 | line2"));
        assert!(out.summary.contains("（共 3 行，offset=1，已返回 3 行）"));
        // canonical：path 为 canonicalize 后的绝对路径（守卫返回同基准）
        let canon_path = root.join("a.txt").canonicalize().unwrap();
        assert_eq!(out.canonical["path"], canon_path.to_string_lossy().to_string());
        assert_eq!(out.canonical["totalLines"], 3);
        assert_eq!(out.canonical["lines"].as_array().unwrap().len(), 3);
        assert_eq!(out.canonical["truncated"], false);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_absolute_path() {
        let root = unique_temp_dir("abs");
        fixture(&root);
        let tool = read_tool(&root);

        let abs = root.join("a.txt");
        let out = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": abs.to_string_lossy() }).to_string(),
            )
            .unwrap();
        assert!(out.summary.contains("   1 | line1"));
        assert_eq!(out.canonical["totalLines"], 3);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_absolute_outside_workspace_allowed() {
        // 读默认全盘放行（仅 BLOCKED 拒读）：工作区外绝对路径可读（不回归）
        let root = unique_temp_dir("abs-out");
        let tool = read_tool(&root);
        let other = unique_temp_dir("abs-target");
        std::fs::write(other.join("x.txt"), "工作区外的文件").unwrap();

        let abs = other.join("x.txt");
        let out = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": abs.to_string_lossy() }).to_string(),
            )
            .unwrap();
        assert!(out.summary.contains("1 | 工作区外的文件"));

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_read_only_kb_dir_allowed() {
        // READ_ONLY 区（知识库）只限写、不限读：目录内真实条目绝对路径可读
        let root = unique_temp_dir("kb-read");
        let tool = read_tool(&root);

        let kb_file = first_kb_file();
        let out = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": kb_file.to_string_lossy() }).to_string(),
            )
            .unwrap();
        let total = out.canonical["totalLines"].as_u64().expect("应返回 totalLines");
        assert!(total > 0, "知识库条目应有内容：{}", kb_file.display());
        assert!(out.summary.contains("（共"), "应成功读取知识库条目");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_blocked_db_path_rejected() {
        // BLOCKED（M1）：数据库文件连读都不行——绝对路径直读应被守卫拒绝
        let root = unique_temp_dir("blocked");
        let tool = read_tool(&root);

        let db = crate::config::resolve_db_path();
        assert!(db.exists(), "dev 数据库应存在（测试前提）");
        let err = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": db.to_string_lossy() }).to_string(),
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("受保护路径")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_offset_limit_window() {
        let root = unique_temp_dir("window");
        fixture(&root);
        let tool = read_tool(&root);

        // offset=2 limit=100 → 第 2、3 行
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"a.txt","offset":2,"limit":100}"#,
            )
            .unwrap();
        assert!(!out.summary.contains("   1 | line1"));
        assert!(out.summary.contains("   2 | line2"));
        assert!(out.summary.contains("   3 | line3"));
        assert!(out.summary.contains("（共 3 行，offset=2，已返回 2 行）"));
        let lines = out.canonical["lines"].as_array().unwrap();
        assert_eq!(lines[0]["number"], 2);
        assert_eq!(lines[1]["number"], 3);
        assert_eq!(out.canonical["truncated"], false);

        // limit 截断：offset=1 limit=2 → 窗口满且还有剩余 → truncated
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"a.txt","offset":1,"limit":2}"#,
            )
            .unwrap();
        assert!(out.summary.contains("   1 | line1"));
        assert!(out.summary.contains("   2 | line2"));
        assert!(!out.summary.contains("   3 | line3"));
        assert!(out.summary.contains("（共 3 行，offset=1，已返回 2 行）"));
        assert!(out.summary.contains("（可增加 offset 续读）"));
        assert_eq!(out.canonical["truncated"], true);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_offset_beyond_eof_returns_empty_window() {
        let root = unique_temp_dir("eof");
        fixture(&root);
        let tool = read_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"a.txt","offset":10,"limit":100}"#,
            )
            .unwrap();
        assert!(out.summary.contains("（共 3 行，offset=10，已返回 0 行）"));
        assert!(!out.summary.contains("| line"));
        assert_eq!(out.canonical["lines"].as_array().unwrap().len(), 0);
        assert_eq!(out.canonical["totalLines"], 3);
        assert_eq!(out.canonical["truncated"], false);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_rejects_directory() {
        let root = unique_temp_dir("isdir");
        fixture(&root);
        let tool = read_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"sub"}"#)
            .unwrap_err();
        assert!(matches!(&err, ToolError::Execution(m) if m.contains("不是普通文件：sub")), "got: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_missing_file_error() {
        let root = unique_temp_dir("missing");
        fixture(&root);
        let tool = read_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"nope.txt"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("文件不存在：nope.txt")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_long_line_truncated() {
        let root = unique_temp_dir("long");
        fixture(&root);
        let tool = read_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"path":"long.txt"}"#)
            .unwrap();
        let text = out.canonical["lines"][0]["text"].as_str().unwrap();
        assert_eq!(text.len(), 2000 + " …[行过长已截断]".len());
        assert!(text.ends_with(" …[行过长已截断]"));
        assert!(out.summary.contains(" …[行过长已截断]"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_rejects_over_10mb() {
        let root = unique_temp_dir("big");
        fixture(&root);
        let tool = read_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"big.txt"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m)
                if m.contains("文件过大") && m.contains("超过读取上限 10MB")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_rejects_non_utf8() {
        let root = unique_temp_dir("utf8");
        fixture(&root);
        let tool = read_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"bin.bin"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("不是 UTF-8 文本文件")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_file_rejects_bad_offset_limit_params() {
        let root = unique_temp_dir("params");
        fixture(&root);
        let tool = read_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"a.txt","offset":0}"#)
            .unwrap_err();
        assert!(matches!(&err, ToolError::Parse(m) if m.contains("正整数")), "got: {err}");

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"a.txt","limit":5000}"#)
            .unwrap_err();
        assert!(matches!(&err, ToolError::Parse(m) if m.contains("上限 2000")), "got: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── list_dir ──

    #[test]
    fn list_dir_sorted_with_dir_suffix() {
        let root = unique_temp_dir("list");
        fixture(&root);
        let tool = list_tool(&root);

        let out = tool.execute(&ToolContext::new(), "{}").unwrap();
        let names: Vec<&str> = out.summary.lines().collect();
        assert_eq!(
            names,
            vec!["a.txt", "big.txt", "bin.bin", "empty/", "long.txt", "sub/"]
        );

        let entries = out.canonical["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0]["name"], "a.txt");
        assert_eq!(entries[0]["isDir"], false);
        assert_eq!(entries[1]["name"], "big.txt");
        assert_eq!(entries[1]["isDir"], false);
        assert_eq!(entries[2]["name"], "bin.bin");
        assert_eq!(entries[2]["isDir"], false);
        assert_eq!(entries[3]["name"], "empty");
        assert_eq!(entries[3]["isDir"], true);
        assert_eq!(entries[4]["name"], "long.txt");
        assert_eq!(entries[4]["isDir"], false);
        assert_eq!(entries[5]["name"], "sub");
        assert_eq!(entries[5]["isDir"], true);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_dir_subdirectory_and_empty_dir() {
        let root = unique_temp_dir("sub");
        fixture(&root);
        let tool = list_tool(&root);

        // 相对路径列子目录（fixture 中 sub 为空目录）
        let out = tool
            .execute(&ToolContext::new(), r#"{"path":"sub"}"#)
            .unwrap();
        assert_eq!(out.summary, "（空目录）");

        // 空目录
        let out = tool
            .execute(&ToolContext::new(), r#"{"path":"empty"}"#)
            .unwrap();
        assert_eq!(out.summary, "（空目录）");
        assert_eq!(out.canonical["entries"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_dir_missing_and_not_dir_errors() {
        let root = unique_temp_dir("err");
        fixture(&root);
        let tool = list_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"nope"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("目录不存在：nope")),
            "got: {err}"
        );

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"a.txt"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("不是目录：a.txt")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
