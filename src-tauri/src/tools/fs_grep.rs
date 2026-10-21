//! fs_grep —— 通用内容搜索工具（grep）
//!
//! 按正则 / 字面在工作区内检索文本，输出 `相对路径:行号: 命中行`。
//! 行导向匹配（同 ripgrep）：逐行判定，一行多个命中仍输出一行；pattern
//! 默认不跨行。遍历每目录条目按名称排序（确定性输出，可测试可复现）。
//!
//! 搜索默认排除：隐藏项（`.` 开头，含 .git）、node_modules、以及沿路径
//! 生效的 .gitignore 规则（自研最小引擎，见 [`IgnoreRule`] / [`decide`]，
//! 不引入 ignore crate）。二进制（前 8KB 含 NUL）、非 UTF-8、超大（> 10MB，
//! 与 read/write 共用 [`MAX_FILE_BYTES`]）文件自动跳过并计数。
//!
//! 双重上限：limit（默认 100 / 最大 500）先达 → limitReached；累计展示
//! 文本 ≈48KB 先达 → outputTruncated。命中行超 500 字符截断 + read_file
//! 同款标记。目标解析收口于 path_guard::resolve_read（BLOCKED 拒读）；
//! 单文件搜索起点不做排除（显式指名即搜）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use regex::{Regex, RegexBuilder};

use crate::path_guard;
use crate::policy::Policy;
use crate::tools::fs_write::MAX_FILE_BYTES;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 命中条数默认上限 / 上限最大值（达上限提前停止并在末尾注明）。
const DEFAULT_GREP_LIMIT: usize = 100;
const MAX_GREP_LIMIT: usize = 500;
/// 结果展示文本预算（约 48KB；先达则停止收集——防 summary 撑爆上下文）。
const OUTPUT_BUDGET_BYTES: usize = 48 * 1024;
/// 命中行展示截断（字符数；marker 与 read_file 行截断同款，语义自明）。
const MAX_LINE_CHARS: usize = 500;
const LINE_TRUNC_MARKER: &str = " …[行过长已截断]";
/// 二进制嗅探窗口：前 8KB 含 NUL 即视为二进制（与 rg 语义同族）。
const BINARY_PROBE_BYTES: usize = 8 * 1024;

// ═══════════════════════════════════════════════════════
// 最小 gitignore 引擎（自研子集，语义文档化）
// ═══════════════════════════════════════════════════════

/// 单条 gitignore 规则。
///
/// 语义子集（与 git 的差异点在此显式化）：
/// - 规则来源：搜索域内每层目录的 `.gitignore`；进入目录前加载，规则沿
///   深度**累积继承**（父层规则对子路径生效，Walker 用栈 push/pop 模拟）。
/// - 判定：自后向前取第一条命中（子层 / 靠后规则优先）；`!` 否定命中 =
///   放行；目录被忽略即整棵剪枝——父目录被排除则其下 `.gitignore` 不再
///   加载，天然与 git「父被排除则无法重新包含」等价。
/// - 语法：`#` 注释、`!` 否定、`\x` 转义字面、尾 `/`（仅目录）、首 `/`
///   或含 `/`（锚定所在目录）、无 `/`（匹配任意层 basename）、通配
///   `*` `?` `[...]` `**`（glob→regex：`*`/`?` 不跨 `/`、`**` 跨目录）。
/// - 不做 repo 门控（域内 .gitignore 按文件存在性生效，无论是否 git
///   仓库）；不做 `.ignore` 文件；不实现行尾 `\` 续行与尾空格转义。
///   规则文件非 UTF-8 / 单条解析失败 → best-effort 忽略，不中断搜索。
struct IgnoreRule {
    /// 规则锚定目录（该 .gitignore 所在目录）；匹配对象相对本目录。
    base: PathBuf,
    /// `!` 前缀：命中 = 放行（否定）。
    negated: bool,
    /// 尾 `/`：仅目录命中（文件判定跳过本规则）。
    dir_only: bool,
    /// glob 翻译后的匹配器（匹配相对 base 的 posix 路径串）。
    regex: Regex,
}

/// 单行规则解析（None = 注释 / 空行 / 无意义行）。
fn parse_rule(base: &Path, line: &str) -> Option<IgnoreRule> {
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    let trimmed = line.trim_start();
    // 注释（`\#` 转义的字面 # 由转义解析兜底——此处只看未转义行首）
    if trimmed.starts_with('#') {
        return None;
    }
    // 否定前缀（`!` 开头；转义字面 `\!` 由 glob 转义解析兜底）
    let (negated, body) = match trimmed.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    if body.is_empty() {
        return None;
    }
    // 尾 `/` → 仅目录
    let (dir_only, pattern) = match body.strip_suffix('/') {
        Some(p) => (true, p),
        None => (false, body),
    };
    if pattern.is_empty() {
        return None;
    }
    // 锚定：首 `/` 或含 `/`（非尾部）→ 相对所在目录；无 `/` → 任意层 basename
    let (anchored, pattern) = if let Some(p) = pattern.strip_prefix('/') {
        (true, p)
    } else if pattern.contains('/') {
        (true, pattern)
    } else {
        (false, pattern)
    };
    if pattern.is_empty() {
        return None;
    }

    let glob = glob_to_regex(pattern);
    let source = if anchored {
        format!("^{glob}$")
    } else {
        format!("(?:^|/){glob}$")
    };
    let regex = Regex::new(&source).ok()?; // 解析失败 → 忽略该规则
    Some(IgnoreRule {
        base: base.to_path_buf(),
        negated,
        dir_only,
        regex,
    })
}

/// 解析 .gitignore 文件内容 → 规则列表（逐行容错）。
fn parse_gitignore(base: &Path, content: &str) -> Vec<IgnoreRule> {
    content
        .lines()
        .filter_map(|l| parse_rule(base, l))
        .collect()
}

/// 读目录级 .gitignore（best-effort：不存在 / 非 UTF-8 → 空规则集）。
fn load_dir_rules(dir: &Path) -> Vec<IgnoreRule> {
    let gi = dir.join(".gitignore");
    let Ok(bytes) = std::fs::read(&gi) else {
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    parse_gitignore(dir, &text)
}

/// glob（gitignore pattern 正文）→ regex 文本翻译。
///
/// 约定：匹配对象为 `/` 分隔的 posix 相对路径串或 basename；
/// `*`/`?` 不跨 `/`；`**`（含 `**/`）跨目录；`[...]` 字符类透传
/// （类内 `!` 转 `^`，与 regex 语法对齐）；`\x` → 字面 x。
fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next(); // 第二个 '*'
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        out.push_str("(?:[^/]+/)*");
                    } else {
                        out.push_str(".*");
                    }
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            '[' => {
                // 先探测闭合 ']'（扫描副本，不消费）；未闭合 → 字面 '['
                let mut probe = chars.clone();
                let mut closed = false;
                let mut esc = false;
                for c2 in probe.by_ref() {
                    if esc {
                        esc = false;
                        continue;
                    }
                    if c2 == '\\' {
                        esc = true;
                        continue;
                    }
                    if c2 == ']' {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    out.push_str("\\[");
                    continue;
                }
                // 闭合：收集类内容；类内首 '!' 转 '^'；'\\' 转义类内字符
                let mut cls = String::from("[");
                let mut first = true;
                let mut esc = false;
                for c2 in chars.by_ref() {
                    if c2 == ']' && !esc {
                        cls.push(']');
                        break;
                    }
                    if first && c2 == '!' {
                        cls.push('^');
                        first = false;
                        continue;
                    }
                    first = false;
                    if esc {
                        cls.push('\\');
                        cls.push(c2);
                        esc = false;
                        continue;
                    }
                    if c2 == '\\' {
                        esc = true;
                        continue;
                    }
                    cls.push(c2);
                }
                out.push_str(&cls);
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push_str(&regex::escape(&n.to_string()));
                }
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out
}

/// 规则判定：自后向前取第一条命中（子层 / 靠后优先）。
/// 返回 true = 放行；false = 忽略（目录剪枝 / 文件跳过）。
fn decide(rules: &[IgnoreRule], abs: &Path, is_dir: bool) -> bool {
    for rule in rules.iter().rev() {
        if rule.dir_only && !is_dir {
            continue;
        }
        let Ok(rel) = abs.strip_prefix(&rule.base) else {
            continue;
        };
        let rel_posix = rel.to_string_lossy().replace('\\', "/");
        if rule.regex.is_match(&rel_posix) {
            return rule.negated; // 命中规则决定：! → 放行；普通 → 忽略
        }
    }
    true
}

// ═══════════════════════════════════════════════════════
// GrepTool —— 内容检索
// ═══════════════════════════════════════════════════════

/// 单条命中记录（display 路径 + 行号 + 截断后文本）。
struct GrepMatch {
    path: String,
    line: usize,
    text: String,
}

/// 一次搜索的遍历 / 收集状态（可变现场，scan 与 walk 共用）。
struct ScanState {
    policy: Arc<Policy>,
    regex: Regex,
    limit: usize,
    matches: Vec<GrepMatch>,
    budget_used: usize,
    binary_skipped: usize,
    non_utf8_skipped: usize,
    too_large_skipped: usize,
    lines_truncated: bool,
    limit_reached: bool,
    output_truncated: bool,
}

impl ScanState {
    fn stopped(&self) -> bool {
        self.limit_reached || self.output_truncated
    }

    /// 单个文件的完整防护 + 行扫描（文件级防护，不判 ignore——
    /// 单文件搜索起点显式指名即搜；目录遍历中由 walker 先做 ignore 判定）。
    fn scan_file(&mut self, abs: &Path) {
        if self.stopped() {
            return;
        }
        let Ok(meta) = std::fs::metadata(abs) else {
            return; // best-effort：读不到（权限/竞态）跳过
        };
        if !meta.is_file() {
            return;
        }
        if meta.len() > MAX_FILE_BYTES {
            self.too_large_skipped += 1;
            return;
        }
        let Ok(bytes) = std::fs::read(abs) else {
            return;
        };
        // 二进制嗅探：前 8KB 含 NUL
        let probe = bytes.len().min(BINARY_PROBE_BYTES);
        if bytes[..probe].contains(&0) {
            self.binary_skipped += 1;
            return;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            self.non_utf8_skipped += 1;
            return;
        };
        self.scan_text(abs, &text);
    }

    /// 行导向扫描：逐行 regex 判定，一行多个命中仍输出一行。
    fn scan_text(&mut self, abs: &Path, text: &str) {
        for (idx, line) in text.lines().enumerate() {
            if self.stopped() {
                return;
            }
            if !self.regex.is_match(line) {
                continue;
            }
            self.push_match(abs, idx + 1, line);
        }
    }

    /// 收集一条命中（行截断 + 展示预算 + limit 计数）。
    fn push_match(&mut self, abs: &Path, line_no: usize, line_text: &str) {
        let display = display_path(&self.policy.workspace_root().canonical(), abs);
        let mut text = line_text.to_string();
        if text.chars().count() > MAX_LINE_CHARS {
            text = text.chars().take(MAX_LINE_CHARS).collect();
            text.push_str(LINE_TRUNC_MARKER);
            self.lines_truncated = true;
        }
        // 展示预算（≈ path + text + 行号/冒号开销）
        let est = display.len() + text.len() + 16;
        if self.budget_used + est > OUTPUT_BUDGET_BYTES {
            self.output_truncated = true;
            return; // 本条不入列，后续遍历由 stopped() 终止
        }
        self.budget_used += est;
        self.matches.push(GrepMatch {
            path: display,
            line: line_no,
            text,
        });
        if self.matches.len() >= self.limit {
            self.limit_reached = true;
        }
    }

    /// 目录递归遍历（确定性排序；忽略规则沿深度累积，返回前恢复栈）。
    fn walk_dir(&mut self, dir: &Path, rules: &mut Vec<IgnoreRule>) {
        if self.stopped() {
            return;
        }
        // 1. 本层 .gitignore → 追加（累积继承）
        let mut local = load_dir_rules(dir);
        let added = local.len();
        rules.append(&mut local);

        // 2. 读条目（类型不跟随符号链接——防环 + 确定性）
        let mut entries: Vec<(PathBuf, String, bool, bool)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                entries.push((
                    e.path(),
                    e.file_name().to_string_lossy().to_string(),
                    ft.is_dir(),
                    ft.is_symlink(),
                ));
            }
        }
        entries.sort_by(|a, b| a.1.cmp(&b.1));

        // 3. 逐条目：目录剪枝递归 / 文件防护扫描
        for (abs, name, is_dir, is_symlink) in entries {
            if self.stopped() {
                break;
            }
            if is_dir {
                // 目录：隐藏（含 .git）/ node_modules 硬剪枝（非 git 工作区
                // 无 .gitignore 兜底时的性能防线）→ gitignore 剪枝 → 递归
                if name.starts_with('.') || name == "node_modules" {
                    continue;
                }
                if !decide(rules, &abs, true) {
                    continue;
                }
                self.walk_dir(&abs, rules);
            } else {
                // 文件：符号链接跳过（不跟随，防环 / 逃逸）；隐藏跳过；
                // gitignore 忽略跳过（.gitignore 自身按隐藏规则）
                if is_symlink || name.starts_with('.') {
                    continue;
                }
                if !decide(rules, &abs, false) {
                    continue;
                }
                self.scan_file(&abs);
            }
        }

        // 4. 恢复规则栈（返回前必达）
        rules.truncate(rules.len() - added);
    }
}

/// summary / canonical 展示路径：工作区内 → 相对路径（`/` 分隔，与
/// read_file 相对路径口径一致）；strip 失败（工作区外读）回退绝对路径。
fn display_path(workspace_root: &Path, abs: &Path) -> String {
    match abs.strip_prefix(workspace_root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => abs.to_string_lossy().to_string(),
    }
}

/// 内容检索工具。
pub struct GrepTool {
    /// 沙箱策略：路径守卫（搜索起点解析；读语义同 read_file）
    policy: Arc<Policy>,
}

impl GrepTool {
    pub fn new(policy: Arc<Policy>) -> Self {
        Self { policy }
    }
}

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "在目录树或单文件中检索文本，返回 相对路径:行号: 匹配行。pattern 默认按正则（literal=true 按字面）；默认排除隐藏项、node_modules 与 .gitignore 规则，二进制/非 UTF-8 自动跳过。默认上限 100 条（最大 500）。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "minLength": 1,
                    "description": "搜索模式。默认按正则（Rust regex 语法：无回溯、无环视 lookaround、无条件分支）；纯字面搜索请设 literal=true"
                },
                "path": {
                    "type": "string",
                    "description": "搜索起点：目录（递归）或单个文件。相对路径基于工作目录解析；省略 = 工作区根。只搜文本文件"
                },
                "literal": {
                    "type": "boolean",
                    "description": "false（默认）= pattern 按正则；true = 纯字面文本"
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "false（默认）= 区分大小写；true = 忽略大小写"
                },
                "limit": {
                    "type": "number",
                    "description": "命中条数上限（每文件每命中行计 1），默认 100，最大 500；达上限提前停止并在末尾注明"
                }
            },
            "required": ["pattern"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        // 仅搜索词（目录起点与命中不展示）
        match v
            .get("pattern")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(p) => ToolDisplay::info(p),
            None => ToolDisplay::default(),
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        // ── 1. 参数解析 ──
        let pattern = v["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 pattern 必须是字符串".into()))?;
        if pattern.is_empty() {
            // guard minLength 拦截的直调防御（registry 层在 guard 阶段已拦）
            return Err(ToolError::Parse("参数 pattern 不能为空".into()));
        }
        let literal = match v.get("literal") {
            None | Some(serde_json::Value::Null) => false,
            Some(b) => b
                .as_bool()
                .ok_or_else(|| ToolError::Parse("参数 literal 必须是布尔值".into()))?,
        };
        let ignore_case = match v.get("ignore_case") {
            None | Some(serde_json::Value::Null) => false,
            Some(b) => b
                .as_bool()
                .ok_or_else(|| ToolError::Parse("参数 ignore_case 必须是布尔值".into()))?,
        };
        let limit = match v.get("limit") {
            None | Some(serde_json::Value::Null) => DEFAULT_GREP_LIMIT,
            Some(n) => {
                let n = n
                    .as_u64()
                    .ok_or_else(|| ToolError::Parse("参数 limit 必须是正整数".into()))?;
                if n == 0 {
                    return Err(ToolError::Parse("参数 limit 必须是正整数".into()));
                }
                if n > MAX_GREP_LIMIT as u64 {
                    return Err(ToolError::Parse(format!(
                        "参数 limit 超出上限 {MAX_GREP_LIMIT}"
                    )));
                }
                n as usize
            }
        };

        // ── 2. 正则编译（literal=true 时 pattern 不解析为正则——escape）──
        let source = if literal {
            regex::escape(pattern)
        } else {
            pattern.to_string()
        };
        let regex = match RegexBuilder::new(&source)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(r) => r,
            Err(e) => return Err(ToolError::Parse(format!("非法正则：{e}"))),
        };

        // ── 3. 目标解析：省略 → 工作区根；给出 → resolve_read（BLOCKED 拒）──
        let (target, single_file) = match v.get("path") {
            None | Some(serde_json::Value::Null) => {
                (self.policy.workspace_root().canonical(), false)
            }
            Some(p) => {
                let raw = p
                    .as_str()
                    .ok_or_else(|| ToolError::Parse("参数 path 必须是字符串".into()))?;
                let resolved =
                    path_guard::resolve_read(&self.policy, raw).map_err(|e| match e {
                        path_guard::PathGuardError::Io(_) => {
                            ToolError::Execution(format!("路径不存在：{raw}"))
                        }
                        path_guard::PathGuardError::Blocked(_) => {
                            ToolError::Execution(format!("路径不可读（受保护路径）：{raw}"))
                        }
                        _ => ToolError::Execution(format!("路径不可读：{raw}")),
                    })?;
                let meta = std::fs::metadata(&resolved)
                    .map_err(|_| ToolError::Execution(format!("路径不存在：{raw}")))?;
                if meta.is_dir() {
                    (resolved, false)
                } else if meta.is_file() {
                    (resolved, true)
                } else {
                    return Err(ToolError::Execution(format!("不是普通文件或目录：{raw}")));
                }
            }
        };

        // ── 4. 搜索 ──
        let mut state = ScanState {
            policy: Arc::clone(&self.policy),
            regex,
            limit,
            matches: Vec::new(),
            budget_used: 0,
            binary_skipped: 0,
            non_utf8_skipped: 0,
            too_large_skipped: 0,
            lines_truncated: false,
            limit_reached: false,
            output_truncated: false,
        };
        if single_file {
            state.scan_file(&target); // 单文件显式指名：不做 ignore 排除
        } else {
            let mut rules: Vec<IgnoreRule> = Vec::new();
            state.walk_dir(&target, &mut rules);
        }

        // ── 5. 组装输出 ──
        let pattern_display = pattern.to_string();
        let mut summary = String::new();
        if state.matches.is_empty() {
            summary.push_str(&format!("未找到匹配（{pattern_display}）"));
        } else {
            for m in &state.matches {
                summary.push_str(&format!("{}:{}: {}\n", m.path, m.line, m.text));
            }
            summary.push_str(&format!("（共 {} 处匹配）", state.matches.len()));
        }

        // footer（可组合，逐条引导）
        let mut footers: Vec<String> = Vec::new();
        if state.limit_reached {
            footers.push(format!(
                "达到 {limit} 条上限，可增大 limit（≤{MAX_GREP_LIMIT}）或细化 pattern"
            ));
        }
        if state.output_truncated {
            footers
                .push("结果文本超过约 48KB，已截断，请细化 pattern 或缩小 path 范围".to_string());
        }
        let mut skip_parts: Vec<String> = Vec::new();
        if state.binary_skipped > 0 {
            skip_parts.push(format!("{} 个二进制", state.binary_skipped));
        }
        if state.non_utf8_skipped > 0 {
            skip_parts.push(format!("{} 个非 UTF-8", state.non_utf8_skipped));
        }
        if state.too_large_skipped > 0 {
            skip_parts.push(format!("{} 个超大文件", state.too_large_skipped));
        }
        if !skip_parts.is_empty() {
            footers.push(format!("{}已跳过", skip_parts.join("、")));
        }
        if state.lines_truncated {
            footers.push(format!(
                "部分匹配行超 {MAX_LINE_CHARS} 字符已截断，用 read_file 查看全文"
            ));
        }
        for f in &footers {
            summary.push_str(&format!("\n（{f}）"));
        }

        let matches_json: Vec<serde_json::Value> = state
            .matches
            .iter()
            .map(|m| serde_json::json!({ "path": m.path, "line": m.line, "text": m.text }))
            .collect();
        let canonical = serde_json::json!({
            "path": target.to_string_lossy(),
            "pattern": pattern_display,
            "literal": literal,
            "ignoreCase": ignore_case,
            "matches": matches_json,
            "totalMatches": state.matches.len(),
            "limit": limit,
            "limitReached": state.limit_reached,
            "outputTruncated": state.output_truncated,
            "binarySkipped": state.binary_skipped,
            "nonUtf8Skipped": state.non_utf8_skipped,
            "tooLargeSkipped": state.too_large_skipped,
            "linesTruncated": state.lines_truncated,
        });

        Ok(ToolOutput::json(canonical, summary))
    }

    fn is_concurrency_safe(&self) -> bool {
        // 只读、无状态、确定性排序——可并行执行
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

    /// 创建唯一临时目录（不引入 tempfile crate，沿用 fs.rs 夹具模式）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-grep-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 以临时目录为工作区构造 grep 工具（沙箱策略注入）。
    fn grep_tool(root: &Path) -> GrepTool {
        GrepTool::new(Arc::new(Policy::new(root.to_path_buf())))
    }

    /// 工作区内注入 blocked 文件的策略（BLOCKED 拒测试用）。
    fn make_blocked_policy(root: &Path) -> Arc<Policy> {
        let mut p = Policy::new(root.to_path_buf());
        p.blocked.push(root.join("secret.txt"));
        Arc::new(p)
    }

    /// 引擎判定断言辅助：规则集命中 → 忽略（false）。
    fn rule(root: &Path, line: &str) -> IgnoreRule {
        parse_rule(root, line).expect("测试规则应可解析")
    }

    /// 规则集忽略某路径（目录/文件）。
    fn ignores(rules: &[IgnoreRule], abs: &Path, is_dir: bool) -> bool {
        !decide(rules, abs, is_dir)
    }

    // ── 引擎：语法与优先级 ──

    #[test]
    fn rule_comments_and_blank_lines_ignored() {
        let root = unique_temp_dir("r-comment");
        let rules = parse_gitignore(&root, "# 注释\n\n  # 带缩进注释\na.txt\n   \n");
        assert_eq!(rules.len(), 1, "注释/空行不应生成规则");
        assert!(ignores(&rules, &root.join("a.txt"), false));
        assert!(!ignores(&rules, &root.join("b.txt"), false));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_negation_reincludes() {
        let root = unique_temp_dir("r-neg");
        let rules = vec![rule(&root, "*.log"), rule(&root, "!keep.log")];

        assert!(ignores(&rules, &root.join("app.log"), false));
        // 后规则（!）胜 → 放行
        assert!(!ignores(&rules, &root.join("keep.log"), false));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_anchored_only_matches_from_base() {
        let root = unique_temp_dir("r-anchor");
        let rules = vec![rule(&root, "/build")];

        assert!(
            ignores(&rules, &root.join("build"), true),
            "根级 build 命中"
        );
        assert!(
            !ignores(&rules, &root.join("sub/build"), true),
            "含 / 锚定所在目录：sub/build 不命中"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_dir_only_trailing_slash() {
        let root = unique_temp_dir("r-dironly");
        let rules = vec![rule(&root, "cache/")];

        assert!(
            ignores(&rules, &root.join("cache"), true),
            "目录 cache 忽略"
        );
        assert!(
            !ignores(&rules, &root.join("cache"), false),
            "尾 / 仅目录：同名文件不忽略"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_non_anchored_matches_any_level() {
        let root = unique_temp_dir("r-anylevel");
        let rules = vec![rule(&root, "target")];

        assert!(ignores(&rules, &root.join("target"), true));
        assert!(ignores(&rules, &root.join("sub/target"), true));
        assert!(ignores(&rules, &root.join("sub/deep/target"), true));
        // 精确 basename：target.txt 不命中（$ 锚定）
        assert!(!ignores(&rules, &root.join("target.txt"), false));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_glob_star_question_bracket() {
        let root = unique_temp_dir("r-glob");
        let rules = vec![
            rule(&root, "*.rs"),
            rule(&root, "file?.txt"),
            rule(&root, "[ab].md"),
        ];

        assert!(ignores(&rules, &root.join("main.rs"), false));
        assert!(!ignores(&rules, &root.join("main.txt"), false));
        assert!(ignores(&rules, &root.join("file1.txt"), false));
        assert!(!ignores(&rules, &root.join("file12.txt"), false));
        assert!(ignores(&rules, &root.join("a.md"), false));
        assert!(!ignores(&rules, &root.join("c.md"), false));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_double_star_variants() {
        let root = unique_temp_dir("r-dstar");
        // logs/**/debug.log：任意中间层（含零层）
        let r1 = rule(&root, "logs/**/debug.log");
        assert!(ignores(&[r1], &root.join("logs/debug.log"), false));
        assert!(ignores(
            &[rule(&root, "logs/**/debug.log")],
            &root.join("logs/a/debug.log"),
            false
        ));
        assert!(ignores(
            &[rule(&root, "logs/**/debug.log")],
            &root.join("logs/a/b/debug.log"),
            false
        ));
        // **/vendor：任意层（含零层）
        assert!(ignores(
            &[rule(&root, "**/vendor")],
            &root.join("vendor"),
            true
        ));
        assert!(ignores(
            &[rule(&root, "**/vendor")],
            &root.join("a/b/vendor"),
            true
        ));
        // assets/**：子树全部（不含 assets 自身）
        assert!(ignores(
            &[rule(&root, "assets/**")],
            &root.join("assets/x"),
            false
        ));
        assert!(ignores(
            &[rule(&root, "assets/**")],
            &root.join("assets/a/b"),
            true
        ));
        assert!(!ignores(
            &[rule(&root, "assets/**")],
            &root.join("assets"),
            true
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_subdir_layer_wins_over_parent() {
        // 子层 .gitignore 追加在后 → rev 判定先命中子层规则
        let root = unique_temp_dir("r-subwin");
        let sub = root.join("pkg");
        let mut rules = vec![rule(&root, "*.log")];
        rules.push(parse_rule(&sub, "!keep.log").unwrap());

        // 子层 ! 否定胜父层忽略 → 放行
        assert!(!ignores(&rules, &sub.join("keep.log"), false));
        // 其余 .log 仍被父层忽略
        assert!(ignores(&rules, &sub.join("app.log"), false));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_escaped_chars_are_literal() {
        let root = unique_temp_dir("r-esc");
        // 注释行 #file.txt 不产生规则；转义行 \#keep.txt → 字面 #keep.txt
        let rules = parse_gitignore(&root, "#file.txt\n\\#keep.txt\n");

        assert_eq!(rules.len(), 1);
        assert!(
            ignores(&rules, &root.join("#keep.txt"), false),
            "转义 # 命中字面文件"
        );
        assert!(
            !ignores(&rules, &root.join("file.txt"), false),
            "注释行不忽略"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_invalid_glob_line_best_effort() {
        let root = unique_temp_dir("r-invalid");
        // 未闭合字符类：'[' 退化为字面（不崩溃），剩余字符继续翻译；
        // 同文件其它规则正常生效
        let rules = parse_gitignore(&root, "a.txt\n[unclosed\n");
        assert_eq!(rules.len(), 2);
        assert!(ignores(&rules, &root.join("a.txt"), false));
        assert!(
            ignores(&rules, &root.join("[unclosed"), false),
            "未闭合类退化字面匹配"
        );
        assert!(!ignores(&rules, &root.join("b.txt"), false));

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 工具：正常 ──

    #[test]
    fn grep_single_file_no_exclusion() {
        // 单文件显式指名：隐藏名也搜（不做排除）
        let root = unique_temp_dir("g-single");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".env"), "API_KEY=secret\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"pattern":"API_KEY","path":".env"}"#,
            )
            .unwrap();
        assert!(out.summary.contains(".env:1: API_KEY=secret"));
        assert_eq!(out.canonical["totalMatches"], 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_recursive_with_relative_paths() {
        let root = unique_temp_dir("g-rec");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/b.txt"), "hello grep\nworld\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"grep","path":"sub"}"#)
            .unwrap();
        assert!(
            out.summary.contains("sub/b.txt:1: hello grep"),
            "got: {}",
            out.summary
        );
        assert!(out.summary.contains("（共 1 处匹配）"));
        assert_eq!(out.canonical["matches"][0]["line"], 1);
        // canonical.path 为 canonicalize 后的绝对路径
        let canon = root.join("sub").canonicalize().unwrap();
        assert_eq!(out.canonical["path"], canon.to_string_lossy().to_string());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_default_path_is_workspace_root() {
        let root = unique_temp_dir("g-root");
        std::fs::write(root.join("a.txt"), "hit\n").unwrap();
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d/c.txt"), "hit\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"hit"}"#)
            .unwrap();
        // 全树 2 条，路径相对工作区根
        assert!(out.summary.contains("a.txt:1: hit"));
        assert!(out.summary.contains("d/c.txt:1: hit"));
        assert_eq!(out.canonical["totalMatches"], 2);
        assert!(out.canonical["path"]
            .as_str()
            .unwrap()
            .contains(root.canonicalize().unwrap().to_string_lossy().as_ref()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_regex_vs_literal() {
        let root = unique_temp_dir("g-re");
        std::fs::write(root.join("r.txt"), "a.b\naxb\n").unwrap();
        let tool = grep_tool(&root);

        // 正则：. 匹配任意字符 → 两行都命中
        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"a.b"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 2);
        // literal：只命中字面 a.b
        let out2 = tool
            .execute(&ToolContext::new(), r#"{"pattern":"a.b","literal":true}"#)
            .unwrap();
        assert_eq!(out2.canonical["totalMatches"], 1);
        assert!(out2.summary.contains("r.txt:1: a.b"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_ignore_case_flag() {
        let root = unique_temp_dir("g-case");
        std::fs::write(root.join("c.txt"), "Apple Pie\n").unwrap();
        let tool = grep_tool(&root);

        // 默认区分大小写
        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"apple"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 0);
        // ignore_case=true
        let out2 = tool
            .execute(
                &ToolContext::new(),
                r#"{"pattern":"apple","ignore_case":true}"#,
            )
            .unwrap();
        assert_eq!(out2.canonical["totalMatches"], 1);
        assert_eq!(out2.canonical["ignoreCase"], true);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_chinese_content() {
        let root = unique_temp_dir("g-cn");
        std::fs::write(root.join("z.md"), "干员：能天使\n阻挡数 2\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"能天使"}"#)
            .unwrap();
        assert!(out.summary.contains("z.md:1: 干员：能天使"));
        assert_eq!(out.canonical["totalMatches"], 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_no_match_summary() {
        let root = unique_temp_dir("g-nomatch");
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"zzz_not_exists"}"#)
            .unwrap();
        assert_eq!(out.summary, "未找到匹配（zzz_not_exists）");
        assert_eq!(out.canonical["totalMatches"], 0);
        assert_eq!(out.canonical["matches"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 工具：排除与防护 ──

    #[test]
    fn grep_skips_hidden_items() {
        let root = unique_temp_dir("g-hidden");
        std::fs::write(root.join(".hidden.txt"), "secret\n").unwrap();
        std::fs::create_dir_all(root.join(".cfg")).unwrap();
        std::fs::write(root.join(".cfg/x.txt"), "secret\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 0, "隐藏文件/目录不应被搜");
        assert_eq!(out.summary, "未找到匹配（secret）");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_prunes_node_modules() {
        let root = unique_temp_dir("g-nm");
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("node_modules/pkg/x.js"), "secret\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 0, "node_modules 硬剪枝");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_respects_gitignore_hit_and_negation() {
        let root = unique_temp_dir("g-gi");
        std::fs::write(root.join(".gitignore"), "*.log\n!keep.log\n").unwrap();
        std::fs::write(root.join("app.log"), "secret\n").unwrap();
        std::fs::write(root.join("keep.log"), "secret\n").unwrap();
        std::fs::write(root.join("src.txt"), "secret\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        // app.log 被忽略；keep.log 被 ! 重包含；src.txt 正常
        assert_eq!(out.canonical["totalMatches"], 2);
        assert!(out.summary.contains("keep.log:1: secret"));
        assert!(out.summary.contains("src.txt:1: secret"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_pruned_dir_gitignore_not_loaded() {
        // 父层忽略目录 → 剪枝 → 子目录 .gitignore 不加载（无法重包含）
        let root = unique_temp_dir("g-prune");
        std::fs::write(root.join(".gitignore"), "cache/\n").unwrap();
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(root.join("cache/.gitignore"), "!keep.txt\n").unwrap();
        std::fs::write(root.join("cache/keep.txt"), "secret\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        assert_eq!(
            out.canonical["totalMatches"], 0,
            "父目录被剪枝，子规则不可达"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_non_utf8_gitignore_best_effort() {
        let root = unique_temp_dir("g-giutf");
        // .gitignore 非法 UTF-8 → 整文件忽略（规则不生效），搜索正常
        std::fs::write(root.join(".gitignore"), b"\xff\xfeignored rule\n").unwrap();
        std::fs::write(root.join("a.txt"), "secret\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 1);
        assert!(out.summary.contains("a.txt:1: secret"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_skips_binary_and_counts() {
        let root = unique_temp_dir("g-bin");
        std::fs::write(root.join("bin.dat"), b"AB\x00CDsecret").unwrap();
        std::fs::write(root.join("ok.txt"), "secret\n").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 1);
        assert_eq!(out.canonical["binarySkipped"], 1);
        assert!(
            out.summary.contains("（1 个二进制已跳过）"),
            "got: {}",
            out.summary
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_skips_non_utf8_and_counts() {
        let root = unique_temp_dir("g-nonutf");
        // 非 UTF-8 且前 8KB 无 NUL → nonUtf8Skipped（区别于二进制）
        std::fs::write(root.join("bad.dat"), b"\xff\xfe\x80hello secret").unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 0);
        assert_eq!(out.canonical["nonUtf8Skipped"], 1);
        assert!(
            out.summary.contains("（1 个非 UTF-8已跳过）"),
            "got: {}",
            out.summary
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_skips_too_large_and_counts() {
        let root = unique_temp_dir("g-big");
        // 11MB 文本：超过 10MB 上限 → tooLargeSkipped（读前按 metadata 跳过）
        let mut big = vec![b'a'; 11 * 1024 * 1024];
        big.extend_from_slice(b"secret");
        std::fs::write(root.join("big.txt"), big).unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"secret"}"#)
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 0);
        assert_eq!(out.canonical["tooLargeSkipped"], 1);
        assert!(
            out.summary.contains("（1 个超大文件已跳过）"),
            "got: {}",
            out.summary
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_limit_reached_with_footer() {
        let root = unique_temp_dir("g-limit");
        let content = (1..=120)
            .map(|i| format!("hit line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root.join("many.txt"), content).unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"hit"}"#)
            .unwrap();
        assert_eq!(
            out.canonical["totalMatches"], 100,
            "默认 limit=100 提前停止"
        );
        assert_eq!(out.canonical["limitReached"], true);
        assert!(
            out.summary
                .contains("（达到 100 条上限，可增大 limit（≤500）或细化 pattern）"),
            "got: {}",
            out.summary
        );

        // limit 放大可收全
        let out2 = tool
            .execute(&ToolContext::new(), r#"{"pattern":"hit","limit":500}"#)
            .unwrap();
        assert_eq!(out2.canonical["totalMatches"], 120);
        assert_eq!(out2.canonical["limitReached"], false);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_line_truncation_marker() {
        let root = unique_temp_dir("g-line");
        // 命中行超 500 字符 → 截断 + read_file 同款标记
        let line = format!("TARGET{}", "x".repeat(600));
        std::fs::write(root.join("long.txt"), format!("{line}\n")).unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"TARGET"}"#)
            .unwrap();
        let text = out.canonical["matches"][0]["text"].as_str().unwrap();
        assert_eq!(
            text.chars().count(),
            MAX_LINE_CHARS + LINE_TRUNC_MARKER.chars().count()
        );
        assert!(text.ends_with(LINE_TRUNC_MARKER));
        assert_eq!(out.canonical["linesTruncated"], true);
        assert!(
            out.summary
                .contains("部分匹配行超 500 字符已截断，用 read_file 查看全文"),
            "got: {}",
            out.summary
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_output_budget_truncation() {
        // 大 limit + 长命中行 → 展示文本预算（≈48KB）先达 → outputTruncated
        let root = unique_temp_dir("g-budget");
        let mut content = String::new();
        for i in 0..200 {
            let line = format!("{i:03}:{}x", "y".repeat(460));
            content.push_str(&line);
            content.push('\n');
        }
        std::fs::write(root.join("wide.txt"), content).unwrap();
        let tool = grep_tool(&root);

        let out = tool
            .execute(&ToolContext::new(), r#"{"pattern":"x$","limit":500}"#)
            .unwrap();
        assert_eq!(
            out.canonical["outputTruncated"], true,
            "200 行长行应超 48KB 预算"
        );
        assert!(out.canonical["totalMatches"].as_u64().unwrap() < 200);
        assert!(
            out.summary.contains("（结果文本超过约 48KB，已截断"),
            "got: {}",
            out.summary
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 工具：参数与守卫 ──

    #[test]
    fn grep_invalid_regex_is_parse_error() {
        let root = unique_temp_dir("g-badre");
        let tool = grep_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"pattern":"["}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("非法正则")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_empty_pattern_rejected() {
        // guard minLength 的直调防御（registry 层 guard 阶段也会拦）
        let root = unique_temp_dir("g-empty");
        let tool = grep_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"pattern":""}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("pattern 不能为空")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_limit_over_500_is_parse_error() {
        let root = unique_temp_dir("g-limitcap");
        let tool = grep_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"pattern":"x","limit":501}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("上限 500")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_path_not_found_error() {
        let root = unique_temp_dir("g-missing");
        let tool = grep_tool(&root);

        let err = tool
            .execute(&ToolContext::new(), r#"{"pattern":"x","path":"nope"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("路径不存在：nope")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_outside_workspace_absolute_dir_allowed() {
        // 读语义同 read_file：工作区外绝对路径默认可读（仅 BLOCKED 拒）
        let root = unique_temp_dir("g-outside");
        let tool = grep_tool(&root);
        let other = unique_temp_dir("g-target");
        std::fs::create_dir_all(other.join("in")).unwrap();
        std::fs::write(other.join("in/x.txt"), "outside hit\n").unwrap();

        let abs = other.join("in");
        let out = tool
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "pattern": "outside hit", "path": abs.to_string_lossy() })
                    .to_string(),
            )
            .unwrap();
        assert_eq!(out.canonical["totalMatches"], 1);
        assert!(out.summary.contains("outside hit"));

        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_blocked_target_rejected() {
        let root = unique_temp_dir("g-blocked");
        std::fs::write(root.join("secret.txt"), "secret content\n").unwrap();
        let tool = GrepTool::new(make_blocked_policy(&root));

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"pattern":"secret","path":"secret.txt"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("路径不可读（受保护路径）")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_is_concurrency_safe_true() {
        let root = unique_temp_dir("g-conc");
        let tool = grep_tool(&root);
        assert!(tool.is_concurrency_safe(), "grep 只读可并行");

        let _ = std::fs::remove_dir_all(&root);
    }
}
