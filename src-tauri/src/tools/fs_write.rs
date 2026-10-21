//! fs_write —— 通用文件系统写工具（write_file / edit）
//!
//! 路径解析收口于 `path_guard`（M2 批 1 接线）：
//! - write_file（全量语义）走 `resolve_write_create`——父目录不存在自动
//!   创建（最近存在祖先 canonicalize 后做边界判定，只判不创建）；
//! - edit（局部手术）走 `resolve_read`（存在 + BLOCKED 拒读）→ 普通文件
//!   校验 → `resolve_write`（可写判定，父目录必存在——不扩张建目录语义）。
//!
//! 写判定（M2 #B）：仅工作区内，排除 BLOCKED（写同读拒——默认工作区 =
//! data_dir 时 db / 台账位于可写区）/ READ_ONLY / protected。守卫层给
//! 语义化拒绝，本模块负责映射为面向用户的 ToolError 文案（守卫 Io 文案
//! 与工具层文案去重：内部消息不回填，避免双重包装，规范同 fs.rs）。
//!
//! 覆盖 / 改写前校验「可读回」（≤ MAX_FILE_BYTES 且 UTF-8）：与 read_file
//! 语义对称——LLM 无法读回验证的对象不允许写工具无感知触碰。

use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use crate::path_guard;
use crate::policy::Policy;
use crate::tools::display::file_link;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 文件大小上限（读写共用，防上限漂移）：fs.rs read_file 引用本常量，
/// write_file / edit 与本文件内校验同一上限——保证写出的文件必能被
/// read_file 读回（> 10MB 读不回，写 > 10MB 是无意义产物）。
pub(crate) const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

// ═══════════════════════════════════════════════════════
// 写工具共享辅助
// ═══════════════════════════════════════════════════════

/// 字节数人性化显示：< 1KB → "890B"；< 1MB → "1.2KB"；否则 "3.4MB"。
fn fmt_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n}B")
    } else if n < 1024 * 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{:.1}MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// summary 展示路径：工作区内 → 相对路径（`/` 分隔，与 grep/list_dir 输出
/// 同口径）；strip 失败（理论不可达，防御）回退绝对路径。
fn display_path(policy: &Policy, abs: &Path) -> String {
    match abs.strip_prefix(policy.workspace_root().canonical()) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => abs.to_string_lossy().to_string(),
    }
}

/// 守卫错误 → 工具层文案（写路径专用；守卫内部消息不回填防双重包装）。
/// 与 fs.rs read_file 的 match 风格同构，动词随写语义。
fn map_write_guard_err(raw: &str, e: path_guard::PathGuardError) -> ToolError {
    match e {
        path_guard::PathGuardError::Io(_) => {
            ToolError::Execution(format!("路径不可写：{raw}"))
        }
        path_guard::PathGuardError::Blocked(_) => {
            ToolError::Execution(format!("文件不可写（受保护路径）：{raw}"))
        }
        path_guard::PathGuardError::ReadOnly(_) => {
            ToolError::Execution(format!("只读路径，禁止写入：{raw}"))
        }
        path_guard::PathGuardError::Protected(_) => {
            ToolError::Execution(format!("工作区保护路径（.git），禁止写入：{raw}"))
        }
        path_guard::PathGuardError::HardLink(_) => {
            ToolError::Execution(format!("文件存在多个硬链接，禁止写入：{raw}"))
        }
        path_guard::PathGuardError::OutsideWorkspace(_) => {
            ToolError::Execution(format!("目标不在工作区内：{raw}"))
        }
    }
}

/// 文本 byte 偏移 → 1-based 行号（按 `\n` 计数，UTF-8 安全——偏移为
/// str::match_indices 返回的合法字符边界）。
fn line_of(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx].bytes().filter(|&b| b == b'\n').count() + 1
}

/// 跨行数（含尾换行后的空行，与 edit 的 lineEnd 口径一致）：
/// `split('\n').count()`——"a\nb" → 2、"a\n" → 2、"a" → 1。
fn row_count(s: &str) -> usize {
    s.split('\n').count()
}

/// summary 旧/新内容展示片段：
/// - 单行且 ≤ 80 字符 → 整行回显；
/// - 否则截断为单行片段（首段 ≤ 60 字符）并附规模标注「…（{n} 字符，
///   跨 {m} 行）」；
/// - 超过 200 字符不整段回显（验证靠行号 + read_file），只给规模标注。
fn display_text(s: &str) -> String {
    let chars = s.chars().count();
    let rows = row_count(s);
    if rows == 1 && chars <= 80 {
        return s.to_string();
    }
    if chars > 200 {
        return format!("…（{chars} 字符，跨 {rows} 行）");
    }
    let first = s.split('\n').next().unwrap_or("");
    let head: String = if first.chars().count() > 60 {
        // 截断首段（统一省略号由末尾标注承载，避免双省略号）
        first.chars().take(60).collect()
    } else {
        first.to_string()
    };
    format!("{head}…（{chars} 字符，跨 {rows} 行）")
}

// ═══════════════════════════════════════════════════════
// WriteFileTool —— 全量写入（创建 / 整覆，父目录自动创建）
// ═══════════════════════════════════════════════════════

/// 向工作区写入 UTF-8 文本文件（全量语义）。
pub struct WriteFileTool {
    /// 沙箱策略：路径守卫（同 ReadFileTool，构造时注入）
    policy: Arc<Policy>,
}

impl WriteFileTool {
    pub fn new(policy: Arc<Policy>) -> Self {
        Self { policy }
    }
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "向工作目录写入 UTF-8 文本文件：不存在则创建（父目录自动创建），存在则整体覆盖。仅限工作区内，大小上限 10MB。修改既有文件局部用 edit。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件路径：绝对或相对工作目录的相对路径"
                },
                "content": {
                    "type": "string",
                    "description": "完整文件内容（UTF-8，按原样写入，不自动追加换行）。空串合法（创建空文件）"
                }
            },
            "required": ["path", "content"]
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
        let content = v["content"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 content 必须是字符串".into()))?;

        // ── 1. 路径解析（父目录自动创建版守卫）──
        let resolved = path_guard::resolve_write_create(&self.policy, raw)
            .map_err(|e| map_write_guard_err(raw, e))?;

        // ── 2. 内容大小上限（对称于读取上限；先于读旧内容，省大文件 IO）──
        let new_bytes = content.len();
        if new_bytes as u64 > MAX_FILE_BYTES {
            return Err(ToolError::Execution(format!(
                "内容过大（{:.1}MB），超过写入上限 10MB",
                new_bytes as f64 / (1024.0 * 1024.0)
            )));
        }

        // ── 3. 目标存在分支：覆盖前「可读回」校验 ──
        let mut replaced: Option<(usize, usize)> = None; // (bytes, lines)
        match std::fs::metadata(&resolved) {
            Ok(meta) => {
                if meta.is_dir() {
                    return Err(ToolError::Execution(format!("目标是目录：{raw}")));
                }
                if meta.len() > MAX_FILE_BYTES {
                    return Err(ToolError::Execution(format!(
                        "目标文件过大（{:.1}MB），无法读回校验，拒绝覆盖；请确认后手动处理",
                        meta.len() as f64 / (1024.0 * 1024.0)
                    )));
                }
                let old_bytes = std::fs::read(&resolved)
                    .map_err(|e| ToolError::Execution(format!("读取文件失败：{e}")))?;
                let old_text = String::from_utf8(old_bytes).map_err(|_| {
                    ToolError::Execution(
                        "目标文件不是 UTF-8 文本，拒绝覆盖（与 read_file 语义对称）".into(),
                    )
                })?;
                replaced = Some((old_text.len(), old_text.lines().count()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ToolError::Execution(format!("无法访问目标：{e}"))),
        }
        let created = replaced.is_none();

        // ── 4. 父目录自动创建（守卫只判不创建，创建在工具层） + 写盘 ──
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::Execution(format!("创建目录失败：{e}")))?;
        }
        std::fs::write(&resolved, content)
            .map_err(|e| ToolError::Execution(format!("写入文件失败：{e}")))?;

        // ── 5. 组装输出 ──
        let new_lines = content.lines().count();
        let display = display_path(&self.policy, &resolved);
        let summary = match replaced {
            Some((old_bytes, old_lines)) => format!(
                "已覆盖 {display}（原 {old_lines} 行 / {} → 现 {new_lines} 行 / {}，UTF-8）",
                fmt_bytes(old_bytes),
                fmt_bytes(new_bytes)
            ),
            None => format!(
                "已创建 {display}（{new_lines} 行 / {}，UTF-8）",
                fmt_bytes(new_bytes)
            ),
        };
        let canonical = serde_json::json!({
            "path": resolved.to_string_lossy(),
            "created": created,
            "bytes": new_bytes,
            "lines": new_lines,
            "replaced": replaced.map(|(b, l)| serde_json::json!({ "bytes": b, "lines": l })),
        });

        Ok(ToolOutput::json(canonical, summary))
    }

    fn is_concurrency_safe(&self) -> bool {
        // 写工具：串行执行（与 cache 工具同哲学——写时序必须确定）
        false
    }
}

// ═══════════════════════════════════════════════════════
// EditTool —— 局部精确替换（单次替换，唯一性强制）
// ═══════════════════════════════════════════════════════

/// 对既有文件做一处精确替换（局部手术）。old_string 必须与当前内容
/// 精确匹配且全文件唯一（契约引导：先 read_file 再 edit）。
pub struct EditTool {
    /// 沙箱策略：路径守卫（构造时注入）
    policy: Arc<Policy>,
}

impl EditTool {
    pub fn new(policy: Arc<Policy>) -> Self {
        Self { policy }
    }
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "对工作区内既有文本文件做一处精确替换。old_string 必须与当前内容逐字符匹配且全文件唯一，未命中或匹配多处均报错（不确定内容先 read_file 再编辑）。一次只替换一处，多处修改多次调用。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件路径（绝对或相对工作目录）"
                },
                "old_string": {
                    "type": "string",
                    "minLength": 1,
                    "description": "待替换文本，须与当前内容精确匹配且全文件唯一；可跨行（换行写作 \n）"
                },
                "new_string": {
                    "type": "string",
                    "description": "替换后文本，按原样写入。空串 = 删除 old_string 匹配内容"
                }
            },
            "required": ["path", "old_string", "new_string"]
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
        let old_string = v["old_string"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 old_string 必须是字符串".into()))?;
        let new_string = v["new_string"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 new_string 必须是字符串".into()))?;

        // ── 1. 路径解析：读（存在 + BLOCKED 拒读）→ 普通文件 → 写（可写性）──
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
        path_guard::resolve_write(&self.policy, raw).map_err(|e| map_write_guard_err(raw, e))?;

        // ── 2. 全量读入（大小守卫 + UTF-8）──
        if meta.len() > MAX_FILE_BYTES {
            return Err(ToolError::Execution(format!(
                "文件过大（{:.1}MB），超过处理上限 10MB",
                meta.len() as f64 / (1024.0 * 1024.0)
            )));
        }
        let text = std::fs::read_to_string(&resolved).map_err(|e| {
            // read_to_string 对非 UTF-8 数据返回 InvalidData（编码问题）；
            // 其余 I/O 错误（磁盘/权限等）如实透传
            if e.kind() == std::io::ErrorKind::InvalidData {
                ToolError::Execution("不是 UTF-8 文本文件".into())
            } else {
                ToolError::Execution(format!("读取文件失败：{e}"))
            }
        })?;

        // ── 3. 匹配计数（byte 级 UTF-8 安全：match_indices 返回字符边界）──
        let mut effective_old: Cow<'_, str> = Cow::Borrowed(old_string);
        let mut hits: Vec<usize> = text
            .match_indices(effective_old.as_ref())
            .map(|(i, _)| i)
            .collect();
        let mut normalized_crlf = false;
        if hits.is_empty()
            && text.contains("\r\n")
            && old_string.contains('\n')
            && !old_string.contains('\r')
        {
            // CRLF 归一候选：read_file 展示无行尾，LLM 无法构造 \r\n 多行文本。
            // 仅在归一候选唯一命中时采纳（0/多命中维持原样判定，不引入
            // 规格外文案）；归一只影响匹配，不改写区域外行尾。
            let cand = old_string.replace('\n', "\r\n");
            let cand_hits: Vec<usize> = text.match_indices(&cand).map(|(i, _)| i).collect();
            if cand_hits.len() == 1 {
                effective_old = Cow::Owned(cand);
                hits = cand_hits;
                normalized_crlf = true;
            }
        }

        // ── 4. 未找到 / 不唯一：报错（逼先读后改）──
        match hits.len() {
            0 => {
                return Err(ToolError::Execution(
                    "未找到待替换文本：old_string 与文件当前内容不匹配。请先 read_file 查看精确内容（含大小写/空格/换行）再编辑".into(),
                ));
            }
            n if n > 1 => {
                let shown: Vec<String> = hits
                    .iter()
                    .take(3)
                    .map(|&i| line_of(&text, i).to_string())
                    .collect();
                let suffix = if n > 3 { "…" } else { "" };
                return Err(ToolError::Execution(format!(
                    "old_string 在文件中出现 {n} 处（行 {}），不唯一：请扩充上下文使 old_string 唯一命中，或改用 write_file 整体重写",
                    shown.join("、") + suffix
                )));
            }
            _ => {}
        }

        // ── 5. 替换 / noop（old == new 不落盘）──
        let idx = hits[0];
        let start_line = line_of(&text, idx);
        let end_line = start_line + effective_old.bytes().filter(|&b| b == b'\n').count();
        let span = end_line - start_line + 1;
        let display = display_path(&self.policy, &resolved);

        let noop = old_string == new_string;
        let new_text = if noop {
            text.clone()
        } else {
            text.replacen(effective_old.as_ref(), new_string, 1)
        };
        if !noop && new_text.len() as u64 > MAX_FILE_BYTES {
            return Err(ToolError::Execution(
                "替换后内容将超过 10MB，操作已取消".into(),
            ));
        }
        if !noop {
            // 父目录必已存在（目标为既有文件），不需要 create_dir_all
            std::fs::write(&resolved, &new_text)
                .map_err(|e| ToolError::Execution(format!("写入文件失败：{e}")))?;
        }

        // ── 6. 组装输出 ──
        let summary = if noop {
            format!("未修改 {display}：old_string 与 new_string 相同（位置: 第 {start_line} 行）")
        } else {
            let pos = if span == 1 {
                format!("第 {start_line} 行")
            } else {
                format!("第 {start_line}–{end_line} 行")
            };
            let mut s = format!("已编辑 {display}\n  位置: {pos}（{span} 行区域）\n");
            s.push_str(&format!("  旧: {}\n", display_text(old_string)));
            s.push_str(&format!("  新: {}", display_text(new_string)));
            if normalized_crlf {
                s.push_str("\n（行尾 CRLF 已自动归一匹配）");
            }
            s
        };
        let canonical = serde_json::json!({
            "path": resolved.to_string_lossy(),
            "line": start_line,
            "lineEnd": end_line,
            "normalizedCrlf": normalized_crlf,
            "noop": noop,
            "bytes": new_text.len(),
            "lines": new_text.lines().count(),
        });

        Ok(ToolOutput::json(canonical, summary))
    }

    fn is_concurrency_safe(&self) -> bool {
        // 写工具：串行执行（多次 edit 调用按序作用于文件，结果确定）
        false
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 创建唯一临时目录（不引入 tempfile crate，沿用 fs.rs 夹具模式）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-wr-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 以临时目录为工作区构造 write_file 工具（沙箱策略注入）。
    fn write_tool(root: &Path) -> WriteFileTool {
        WriteFileTool::new(Arc::new(Policy::new(root.to_path_buf())))
    }

    /// 以临时目录为工作区构造 edit 工具（沙箱策略注入）。
    fn edit_tool(root: &Path) -> EditTool {
        EditTool::new(Arc::new(Policy::new(root.to_path_buf())))
    }

    /// 工作区内注入 READ_ONLY 目录的策略（守卫拒绝路径测试用）。
    fn make_policy(root: &Path) -> Arc<Policy> {
        let mut p = Policy::new(root.to_path_buf());
        p.read_only.push(root.join("ro"));
        Arc::new(p)
    }

    /// 工作区内注入 blocked 文件的策略（写同读拒 BLOCKED 测试用）。
    fn make_blocked_policy(root: &Path) -> Arc<Policy> {
        let mut p = Policy::new(root.to_path_buf());
        p.blocked.push(root.join("secret.txt"));
        Arc::new(p)
    }

    /// 搭建基础工作区：sub/、ro/（RO 演示，由 make_policy 注入）、empty/、
    /// a.txt（3 行）、crlf.txt（CRLF 行尾 3 行）、bin.bin（非 UTF-8）、
    /// big.txt（11MB，超过 10MB 上限）。
    fn fixture(root: &Path) {
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("ro")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("a.txt"), "line1\nline2\nline3\n").unwrap();
        std::fs::write(root.join("crlf.txt"), "line1\r\nline2\r\nline3\r\n").unwrap();
        std::fs::write(root.join("bin.bin"), b"\xff\xfe\x00binary").unwrap();
        let mut big = vec![b'a'; 11 * 1024 * 1024];
        big.push(b'\n');
        std::fs::write(root.join("big.txt"), big).unwrap();
    }

    /// 读回文件内容（断言用）。
    fn read_back(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }

    /// 以 root 为工作区构造 read_file 工具（写后可读回对称验证）。
    fn read_tool(root: &Path) -> crate::tools::fs::ReadFileTool {
        crate::tools::fs::ReadFileTool::new(Arc::new(Policy::new(root.to_path_buf())))
    }

    // ── write_file：正常 ──

    #[test]
    fn write_creates_new_file_at_workspace_root() {
        let root = unique_temp_dir("w-new");
        let tool = write_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"path":"a.txt","content":"new\n"}"#)
            .unwrap();
        assert_eq!(out.summary, "已创建 a.txt（1 行 / 4B，UTF-8）");
        assert_eq!(out.canonical["created"], true);
        assert_eq!(out.canonical["bytes"], 4);
        assert_eq!(out.canonical["lines"], 1);
        assert!(out.canonical["replaced"].is_null());
        // canonical.path 为 canonicalize 后的绝对路径（守卫返回同基准）
        let canon_path = root.join("a.txt").canonicalize().unwrap();
        assert_eq!(out.canonical["path"], canon_path.to_string_lossy().to_string());
        assert_eq!(read_back(&root, "a.txt"), "new\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_creates_nested_dirs_automatically() {
        let root = unique_temp_dir("w-nested");
        let tool = write_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"scripts/run.ps1","content":"Write-Output hi"}"#,
            )
            .unwrap();
        assert!(out.summary.starts_with("已创建 scripts/run.ps1"));
        assert!(root.join("scripts").is_dir(), "父目录应自动创建");
        assert_eq!(read_back(&root, "scripts/run.ps1"), "Write-Output hi");
        // summary 展示路径为工作区相对路径
        assert!(!out.summary.contains(root.to_string_lossy().as_ref()));

        // 多级缺目录
        tool.execute(
            &ToolContext::new(),
            r#"{"path":"logs/2026/09/run.jsonl","content":"{}\n"}"#,
        )
        .unwrap();
        assert!(root.join("logs/2026/09").is_dir());
        assert_eq!(read_back(&root, "logs/2026/09/run.jsonl"), "{}\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_overwrites_existing_file_with_replaced_stats() {
        let root = unique_temp_dir("w-over");
        fixture(&root);
        let tool = write_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"path":"a.txt","content":"new\n"}"#)
            .unwrap();
        assert_eq!(out.summary, "已覆盖 a.txt（原 3 行 / 18B → 现 1 行 / 4B，UTF-8）");
        assert_eq!(out.canonical["created"], false);
        assert_eq!(out.canonical["replaced"]["bytes"], 18);
        assert_eq!(out.canonical["replaced"]["lines"], 3);
        assert_eq!(read_back(&root, "a.txt"), "new\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_empty_content_creates_empty_file() {
        let root = unique_temp_dir("w-empty");
        let tool = write_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"path":"empty.txt","content":""}"#)
            .unwrap();
        assert_eq!(out.summary, "已创建 empty.txt（0 行 / 0B，UTF-8）");
        assert_eq!(out.canonical["lines"], 0);
        assert_eq!(std::fs::metadata(root.join("empty.txt")).unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_chinese_filename_and_content_roundtrip() {
        let root = unique_temp_dir("w-cn");
        let tool = write_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({
                    "path": "报告/干员分析.md",
                    "content": "# 源石虫分析\n阻挡数 1\n"
                })
                .to_string(),
            )
            .unwrap();
        assert!(out.summary.starts_with("已创建 报告/干员分析.md"));
        let content = read_back(&root, "报告/干员分析.md");
        assert!(content.contains("源石虫分析"));
        assert!(content.contains("阻挡数 1"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_relative_and_absolute_paths_equivalent() {
        // 同一目标经相对 / 绝对两种写法解析一致（绝对覆盖相对写的内容）
        let root = unique_temp_dir("w-relabs");
        let tool = write_tool(&root);

        tool.execute(&ToolContext::new(), r#"{"path":"x.txt","content":"first"}"#)
            .unwrap();
        let abs = root.join("x.txt").canonicalize().unwrap();
        let out = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": abs.to_string_lossy(), "content": "second" })
                    .to_string(),
            )
            .unwrap();
        assert!(out.summary.starts_with("已覆盖 x.txt"));
        assert_eq!(read_back(&root, "x.txt"), "second");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_10mb_exactly_ok_and_readable_back() {
        // 恰 10MB：等于上限可写，且 read_file 能读回（不报"文件过大"）
        let root = unique_temp_dir("w-10mb");
        let tool = write_tool(&root);

        let content = "x".repeat(MAX_FILE_BYTES as usize);
        let out = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": "big_ok.txt", "content": content }).to_string(),
            )
            .unwrap();
        assert_eq!(out.canonical["bytes"], MAX_FILE_BYTES);
        assert_eq!(
            std::fs::metadata(root.join("big_ok.txt")).unwrap().len(),
            MAX_FILE_BYTES
        );
        // 写后 read_file 可读回（10MB 上限读放行）
        let out2 = read_tool(&root)
            .execute(&ToolContext::new(), r#"{"path":"big_ok.txt","offset":1,"limit":1}"#)
            .unwrap();
        assert!(out2.summary.contains("共 1 行"));

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── write_file：边界 / 防护 ──

    #[test]
    fn write_over_10mb_content_rejected() {
        let root = unique_temp_dir("w-overmb");
        let tool = write_tool(&root);

        let content = "x".repeat(MAX_FILE_BYTES as usize + 1);
        let err = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": "huge.txt", "content": content }).to_string(),
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("超过写入上限 10MB")),
            "got: {err}"
        );
        assert!(!root.join("huge.txt").exists(), "超限内容不应落盘");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_to_directory_target_rejected() {
        let root = unique_temp_dir("w-isdir");
        fixture(&root);
        let tool = write_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"sub","content":"x"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("目标是目录：sub")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_overwrite_big_file_rejected() {
        // 覆盖 >10MB 目标：无法读回校验 → 拒绝覆盖（护栏）
        let root = unique_temp_dir("w-bigover");
        fixture(&root);
        let tool = write_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"big.txt","content":"new"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("无法读回校验，拒绝覆盖")),
            "got: {err}"
        );
        // 原文件未被改动
        assert_eq!(
            std::fs::metadata(root.join("big.txt")).unwrap().len(),
            11 * 1024 * 1024 + 1
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_overwrite_non_utf8_rejected() {
        // 覆盖非 UTF-8 目标：语义外对象拒覆盖（与 read_file 对称）
        let root = unique_temp_dir("w-binover");
        fixture(&root);
        let tool = write_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"bin.bin","content":"text"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("不是 UTF-8 文本，拒绝覆盖")),
            "got: {err}"
        );
        // 原文件未被改动（字节级比对；非 UTF-8 不能 read_to_string）
        assert_eq!(
            std::fs::read(root.join("bin.bin")).unwrap(),
            b"\xff\xfe\x00binary"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── write_file：守卫拒绝路径 ──

    #[test]
    fn write_read_only_dir_rejected() {
        let root = unique_temp_dir("w-ro");
        fixture(&root);
        let tool = WriteFileTool::new(make_policy(&root));

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"ro/x.txt","content":"x"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("只读路径，禁止写入")),
            "got: {err}"
        );
        // RO 前缀下缺目录的新建链同样拒绝
        let err2 = tool
            .execute(&ToolContext::new(), r#"{"path":"ro/n1/x.txt","content":"x"}"#)
            .unwrap_err();
        assert!(
            matches!(&err2, ToolError::Execution(m) if m.contains("只读路径，禁止写入")),
            "got: {err2}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_git_dir_rejected() {
        let root = unique_temp_dir("w-git");
        fixture(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let tool = write_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":".git/config","content":"x"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("工作区保护路径（.git）")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_outside_workspace_rejected() {
        let root = unique_temp_dir("w-outside");
        let tool = write_tool(&root);
        let other = unique_temp_dir("w-target");

        let abs = other.join("x.txt");
        let err = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": abs.to_string_lossy(), "content": "x" }).to_string(),
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("目标不在工作区内")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_blocked_file_rejected() {
        // #B：工作区内 blocked 文件（默认工作区 = data_dir 时 db 形态）拒写
        let root = unique_temp_dir("w-blocked");
        std::fs::write(root.join("secret.txt"), "secret").unwrap();
        let tool = WriteFileTool::new(make_blocked_policy(&root));

        let err = tool
            .execute(&ToolContext::new(), r#"{"path":"secret.txt","content":"x"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("文件不可写（受保护路径）")),
            "got: {err}"
        );
        assert_eq!(read_back(&root, "secret.txt"), "secret");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_missing_param_is_parse_error() {
        let root = unique_temp_dir("w-param");
        let tool = write_tool(&root);

        // 缺 content
        let err = tool.execute(&ToolContext::new(), r#"{"path":"a.txt"}"#).unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("参数 content 必须是字符串")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_is_concurrency_safe_false() {
        let root = unique_temp_dir("w-conc");
        let tool = write_tool(&root);
        assert!(!tool.is_concurrency_safe(), "写工具必须串行");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── edit：正常 ──

    #[test]
    fn edit_replaces_unique_single_line_with_line_number() {
        let root = unique_temp_dir("e-single");
        fixture(&root);
        let tool = edit_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"a.txt","old_string":"line2","new_string":"LINE2"}"#,
            )
            .unwrap();
        assert_eq!(out.canonical["line"], 2);
        assert_eq!(out.canonical["lineEnd"], 2);
        assert_eq!(out.canonical["normalizedCrlf"], false);
        assert_eq!(out.canonical["noop"], false);
        assert_eq!(out.canonical["lines"], 3);
        assert_eq!(
            out.summary,
            "已编辑 a.txt\n  位置: 第 2 行（1 行区域）\n  旧: line2\n  新: LINE2"
        );
        assert_eq!(read_back(&root, "a.txt"), "line1\nLINE2\nline3\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_multiline_replace_reports_line_range() {
        let root = unique_temp_dir("e-multi");
        std::fs::write(root.join("m.txt"), "aaa\nbbb\nccc\nddd\n").unwrap();
        let tool = edit_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"m.txt","old_string":"bbb\nccc","new_string":"BBB"}"#,
            )
            .unwrap();
        assert_eq!(out.canonical["line"], 2);
        assert_eq!(out.canonical["lineEnd"], 3);
        assert!(out.summary.contains("位置: 第 2–3 行（2 行区域）"), "got: {}", out.summary);
        assert_eq!(read_back(&root, "m.txt"), "aaa\nBBB\nddd\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_new_string_empty_deletes_old_text() {
        let root = unique_temp_dir("e-del");
        std::fs::write(root.join("d.txt"), "keep | drop | keep\n").unwrap();
        let tool = edit_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"d.txt","old_string":" | drop","new_string":""}"#,
            )
            .unwrap();
        assert!(out.summary.starts_with("已编辑 d.txt"));
        assert_eq!(read_back(&root, "d.txt"), "keep | keep\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_chinese_roundtrip() {
        let root = unique_temp_dir("e-cn");
        std::fs::write(root.join("c.md"), "# 源石虫\n阻挡数 1\n").unwrap();
        let tool = edit_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"c.md","old_string":"阻挡数 1","new_string":"阻挡数 2"}"#,
            )
            .unwrap();
        assert_eq!(out.canonical["line"], 2);
        assert!(read_back(&root, "c.md").contains("阻挡数 2"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_noop_when_old_equals_new() {
        // old == new 且唯一命中 → 成功返回不落盘（noop 标记）
        let root = unique_temp_dir("e-noop");
        fixture(&root);
        let tool = edit_tool(&root);
        let before = std::fs::read(root.join("a.txt")).unwrap();

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"a.txt","old_string":"line2","new_string":"line2"}"#,
            )
            .unwrap();
        assert_eq!(out.canonical["noop"], true);
        assert_eq!(out.canonical["line"], 2);
        assert!(out.summary.contains("未修改 a.txt"), "got: {}", out.summary);
        let after = std::fs::read(root.join("a.txt")).unwrap();
        assert_eq!(before, after, "noop 不应落盘（内容不变）");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_crlf_multiline_normalized_match() {
        // CRLF 文件 + LF 多行 old_string：归一唯一命中 → 替换并标记
        let root = unique_temp_dir("e-crlf");
        std::fs::write(root.join("crlf.txt"), "aaa\r\nbbb\r\nccc\r\n").unwrap();
        let tool = edit_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"crlf.txt","old_string":"aaa\nbbb","new_string":"AAA\nBBB"}"#,
            )
            .unwrap();
        assert_eq!(out.canonical["normalizedCrlf"], true);
        assert_eq!(out.canonical["line"], 1);
        assert_eq!(out.canonical["lineEnd"], 2);
        assert!(out.summary.contains("（行尾 CRLF 已自动归一匹配）"), "got: {}", out.summary);
        // 命中区域整段替换（new 按原样 LF 落盘），区域外行尾保持 CRLF
        assert_eq!(read_back(&root, "crlf.txt"), "AAA\nBBB\r\nccc\r\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── edit：边界 / 防护 ──

    #[test]
    fn edit_not_found_error() {
        let root = unique_temp_dir("e-notfound");
        fixture(&root);
        let tool = edit_tool(&root);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"a.txt","old_string":"LINE9","new_string":"x"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("未找到待替换文本")),
            "got: {err}"
        );
        assert_eq!(read_back(&root, "a.txt"), "line1\nline2\nline3\n", "未命中不应落盘");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_ambiguous_multiple_occurrences_error() {
        let root = unique_temp_dir("e-ambig");
        std::fs::write(root.join("m.txt"), "dup\nkeep\ndup\nend\n").unwrap();
        let tool = edit_tool(&root);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"m.txt","old_string":"dup","new_string":"x"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m)
                if m.contains("出现 2 处") && m.contains("行 1、3")),
            "got: {err}"
        );
        assert_eq!(read_back(&root, "m.txt"), "dup\nkeep\ndup\nend\n", "不唯一不应落盘");

        // 出现 4 处：行号只列前 3 个 + 省略号
        std::fs::write(root.join("m.txt"), "d\nd\nd\nd\n").unwrap();
        let err2 = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"m.txt","old_string":"d","new_string":"x"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err2, ToolError::Execution(m)
                if m.contains("出现 4 处") && m.contains("行 1、2、3…")),
            "got: {err2}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_rejects_non_utf8_file() {
        let root = unique_temp_dir("e-utf8");
        fixture(&root);
        let tool = edit_tool(&root);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"bin.bin","old_string":"x","new_string":"y"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("不是 UTF-8 文本文件")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_rejects_over_10mb_file() {
        let root = unique_temp_dir("e-big");
        fixture(&root);
        let tool = edit_tool(&root);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"big.txt","old_string":"x","new_string":"y"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("超过处理上限 10MB")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_after_replace_over_10mb_cancelled() {
        // 替换后超出 10MB：取消且未落盘（文件保持原样）
        let root = unique_temp_dir("e-explode");
        std::fs::write(root.join("s.txt"), "tiny content\n").unwrap();
        let tool = edit_tool(&root);

        let payload = "y".repeat(MAX_FILE_BYTES as usize); // 新文本撑爆
        let err = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({
                    "path": "s.txt", "old_string": "tiny content", "new_string": payload
                })
                .to_string(),
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("替换后内容将超过 10MB")),
            "got: {err}"
        );
        assert_eq!(read_back(&root, "s.txt"), "tiny content\n", "取消后文件应保持原样");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_file_not_found_error() {
        let root = unique_temp_dir("e-missing");
        let tool = edit_tool(&root);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"nope.txt","old_string":"x","new_string":"y"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("文件不存在：nope.txt")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_rejects_directory() {
        let root = unique_temp_dir("e-isdir");
        fixture(&root);
        let tool = edit_tool(&root);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"sub","old_string":"x","new_string":"y"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("不是普通文件：sub")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_two_sequential_edits_apply_in_order() {
        // 同一工具连续两次 edit：按调用序作用于文件（is_concurrency_safe=false）
        let root = unique_temp_dir("e-seq");
        std::fs::write(root.join("s.txt"), "a\nb\nc\n").unwrap();
        let tool = edit_tool(&root);

        tool.execute(
            &ToolContext::new(),
            r#"{"path":"s.txt","old_string":"b","new_string":"B"}"#,
        )
        .unwrap();
        // 第二次基于第一次结果设计（b→B 后，B 附近内容改）
        tool.execute(
            &ToolContext::new(),
            r#"{"path":"s.txt","old_string":"B\nc","new_string":"B2\nC"}"#,
        )
        .unwrap();
        assert_eq!(read_back(&root, "s.txt"), "a\nB2\nC\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── edit：守卫拒绝路径 ──

    #[test]
    fn edit_read_only_dir_rejected() {
        let root = unique_temp_dir("e-ro");
        fixture(&root);
        let tool = EditTool::new(make_policy(&root));
        std::fs::write(root.join("ro/x.txt"), "content").unwrap();

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"ro/x.txt","old_string":"content","new_string":"x"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("只读路径，禁止写入")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_git_dir_rejected() {
        let root = unique_temp_dir("e-git");
        fixture(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "[core]").unwrap();
        let tool = edit_tool(&root);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":".git/config","old_string":"[core]","new_string":"x"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("保护路径")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_outside_workspace_rejected() {
        let root = unique_temp_dir("e-outside");
        let tool = edit_tool(&root);
        let other = unique_temp_dir("e-target");
        std::fs::write(other.join("x.txt"), "content").unwrap();

        let abs = other.join("x.txt");
        let err = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({
                    "path": abs.to_string_lossy(), "old_string": "content", "new_string": "x"
                })
                .to_string(),
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("目标不在工作区内")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_blocked_file_rejected() {
        // #B：blocked 文件读即拒（resolve_read 先拦，文案同 fs.rs）
        let root = unique_temp_dir("e-blocked");
        std::fs::write(root.join("secret.txt"), "secret").unwrap();
        let tool = EditTool::new(make_blocked_policy(&root));

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"path":"secret.txt","old_string":"secret","new_string":"x"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("文件不可读（受保护路径）")),
            "got: {err}"
        );
        assert_eq!(read_back(&root, "secret.txt"), "secret");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_display_text_truncation_rules() {
        // 单行 ≤80 整行回显
        assert_eq!(display_text("short line"), "short line");
        // 多行：片段 + 规模标注（跨行数 = 换行段数）
        let s = "aaa\nbbb";
        assert_eq!(display_text(s), "aaa…（7 字符，跨 2 行）");
        // 单行但超 80：截断首段 + 规模
        let long = "x".repeat(100);
        let out = display_text(&long);
        assert!(out.starts_with(&format!("{}…", "x".repeat(60))), "got: {out}");
        assert!(out.ends_with("…（100 字符，跨 1 行）"), "got: {out}");
        // 超 200 字符不整段回显：只给规模标注
        let huge = "y".repeat(201);
        let out2 = display_text(&huge);
        assert_eq!(out2, "…（201 字符，跨 1 行）");
        // 尾换行的空行计入跨行数（与 lineEnd 口径一致）
        assert_eq!(display_text("a\nb\n"), "a…（4 字符，跨 3 行）");
    }

    #[test]
    fn edit_is_concurrency_safe_false() {
        let root = unique_temp_dir("e-conc");
        let tool = edit_tool(&root);
        assert!(!tool.is_concurrency_safe(), "写工具必须串行");

        let _ = std::fs::remove_dir_all(&root);
    }
}
