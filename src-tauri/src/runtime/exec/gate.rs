//! gate —— 自动化模式"地域性信任"放行门（M3-A 定稿设计）
//!
//! 判定管道（优先级定稿，顺序不变量写进测试）：
//! ```text
//! run_command 命令文本（自动化模式消费；询问模式经 check_crit 后直接弹卡）
//!   ↓
//! 0. CRIT 恒拦门（先于一切，任何模式）命中 → BLOCKED(crit-*)
//! 1. 词法子集门 G：不可解析 → 删除词扫描命中则 ASK(delete-*)；
//!    否则 AUTO(syntax-unproven)
//! 2. 删除语义门 D：任何删除形态 → ASK(delete-always-ask / delete-flag-option)
//! 3. 程序分类 C：程序名 → 操作族（OpKind）
//! 4. 危险族 → ASK；系统查询 → AUTO(system-query)；其余 → AUTO(no-risk-family)
//!    工作区外目标只置 touched_external 审计位，不再驱动询问
//! ```
//!
//! 子集 G 为 PowerShell/cmd 通用形态的显式形式化子集（见 [`scan_tokens`]）：
//! 语法不可穷尽部分显式排除；排除后的放行语义见上（2026-10 换轴后不再
//! 一律询问——删除词仍由 raw_delete_hit 兜住）。
//!
//! 语义族静态数据（命令 → OpKind、命名选项白名单、git/解释器名表）在
//! `exec::risk`；本模块为判定逻辑（词法、删除门、分类编排、目标提取、
//! 路径、管道组装）。判定结果 [`GateVerdict`] 三值 + reason 码供 preflight
//! 消费（reason 码并入 approval 事件的 rule_id 通道）。

use std::path::{Path, PathBuf};

use super::risk::{self, OpKind, PathRule};

// ═══════════════════════════════════════════════════════
// 判定结果类型（M3-A 设计 §7）
// ═══════════════════════════════════════════════════════

/// 判定三值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// 自动放行（区内读/写/运行、系统查询族）
    Auto,
    /// 询问用户（删除/区外/代码执行/网络/不确定形态……）
    Ask,
    /// 恒拦（CRIT；任何模式直接拒绝）
    Blocked,
}

/// 判定结果：decision + reason 码（并入现有 rule_id 通道）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateVerdict {
    pub decision: GateDecision,
    /// reason 码（§7 码表：crit-* / unanalyzable-syntax / region-in / …）
    pub reason: &'static str,
    /// 原真规则 id（CRIT 家族；非 CRIT 命中为 None——reason 已入事件 rule_id 通道）
    pub rule_id: Option<&'static str>,
    /// 判定过程中是否观察到工作区外触及
    pub touched_external: bool,
}

impl GateVerdict {
    fn auto(reason: &'static str) -> Self {
        Self {
            decision: GateDecision::Auto,
            reason,
            rule_id: None,
            touched_external: false,
        }
    }

    fn ask(reason: &'static str) -> Self {
        Self {
            decision: GateDecision::Ask,
            reason,
            rule_id: None,
            touched_external: false,
        }
    }

    fn blocked(crit_rule: &'static str) -> Self {
        Self {
            decision: GateDecision::Blocked,
            reason: crit_rule,
            rule_id: Some(crit_rule),
            touched_external: false,
        }
    }
}

// ═══════════════════════════════════════════════════════
// 词法子集 G 扫描器（单趟确定性扫描）
// ═══════════════════════════════════════════════════════

/// 词法 token：剥引号后的值 + 是否含裸 `$`（展开构造标记，豁免判定用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub value: String,
    bare_dollar: bool,
}

impl Token {
    /// 命令名归一化：小写 + 剥 `.exe`（值已剥引号）。
    fn norm_name(&self) -> String {
        let lower = self.value.to_lowercase();
        lower
            .strip_suffix(".exe")
            .map(|s| s.to_string())
            .unwrap_or(lower)
    }

    /// 是否含路径形态（分隔符/盘符/显式 .exe）——程序定位规则（§4）。
    fn has_path_shape(&self) -> bool {
        self.value.contains('\\')
            || self.value.contains('/')
            || self.value.to_lowercase().ends_with(".exe")
            || self.value.contains(':')
    }
}

/// 重定向三元组 (fd, op, target)。target 为剥离引号后的词法值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Redirect {
    /// fd：1/2（数字重定向）；裸 `>`/`<` 为 None
    pub fd: Option<u8>,
    pub op: RedirectOp,
    /// 合并重定向（`2>&1`）无目标；其余重定向必有目标 token
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedirectOp {
    /// `<`
    In,
    /// `>`
    Out,
    /// `>>`
    Append,
    /// `>&`（无路径目标，如 `2>&1`）
    Merge,
}

/// 子集 G 扫描结果。
#[derive(Debug, Clone)]
pub(crate) struct ScanResult {
    /// 命令是否 ∈ 子集 G（false = 整条 OUT → unanalyzable-syntax）
    pub ok: bool,
    pub tokens: Vec<Token>,
    pub redirects: Vec<Redirect>,
}

/// 引号态。
#[derive(Clone, Copy, PartialEq, Eq)]
enum QuoteMode {
    None,
    Single,
    Double,
}

/// 裸 token 中 `$` 的豁免形态：`$PSVersionTable`（单 token 特例，§6）。
fn is_psversiontable_single(tokens: &[Token]) -> bool {
    tokens.len() == 1 && tokens[0].value.eq_ignore_ascii_case("$psversiontable")
}

/// 单趟确定性词法扫描（M3-A 设计 §2 子集 G）。
///
/// 状态机：(引号态 none/single/double, token 缓冲)。
/// - 引号字符只承担"分组"职责、不进缓冲：`"a b"` → 单 token `a b`；
///   `'...'` 内容为字面量（仅 `'` 闭合）；`"..."` 内容扫展开构造
///   （`$ % \`` 出现 → 整条 OUT）；引号未闭合 → OUT。
/// - none 态 OUT 字符：`| & ; ( ) \` %`（任意位置即整条 OUT）；
///   `$` 累积标记，扫描末非 `$PSVersionTable` 单 token 形态 → OUT。
/// - 重定向操作符（token 边界处 `< > >>`；紧贴 FD 前缀 ∈ {1,2}；
///   `2>&1` 合并无目标 OK）→ 提取三元组；>3 次或 FD 越界 → OUT。
/// - none 态空白 → token 结束；token 中间出现 `>`（`hi>a.txt`）→ OUT。
pub(crate) fn scan_tokens(command: &str) -> ScanResult {
    let mut mode = QuoteMode::None;
    let mut buf = String::new();
    let mut bare_dollar = false;
    let mut tokens: Vec<Token> = Vec::new();
    let mut redirects: Vec<Redirect> = Vec::new();
    let mut ok = true;

    let mut it = command.chars().peekable();
    while ok {
        let Some(c) = it.next() else { break };
        match mode {
            QuoteMode::Single => {
                if c == '\'' {
                    mode = QuoteMode::None;
                } else {
                    buf.push(c); // single 内容 = 字面量（含 $ % ` | ; & 等）
                }
            }
            QuoteMode::Double => match c {
                '"' => mode = QuoteMode::None,
                '$' | '%' | '`' => {
                    ok = false; // double 内展开构造 → OUT
                }
                _ => buf.push(c),
            },
            QuoteMode::None => match c {
                ' ' | '\t' | '\r' | '\n' => {
                    if !buf.is_empty() {
                        tokens.push(Token {
                            value: std::mem::take(&mut buf),
                            bare_dollar,
                        });
                        bare_dollar = false;
                    }
                }
                '"' => mode = QuoteMode::Double,
                '\'' => mode = QuoteMode::Single,
                '|' | '&' | ';' | '(' | ')' | '`' | '%' => {
                    ok = false; // OUT 字符
                }
                '<' => {
                    if !buf.is_empty() {
                        ok = false; // token 中间 <
                        break;
                    }
                    if redirects.len() >= 3 {
                        ok = false;
                        break;
                    }
                    match read_target(&mut it) {
                        Some(target) => redirects.push(Redirect {
                            fd: None,
                            op: RedirectOp::In,
                            target: Some(target),
                        }),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                '>' => {
                    // 输出/追加重定向；紧贴数字 1|2 = FD 前缀
                    let fd: Option<u8> = match buf.as_str() {
                        "" => None,
                        "1" | "2" => {
                            let fd = buf.parse::<u8>().ok();
                            buf.clear();
                            bare_dollar = false;
                            fd
                        }
                        _ => {
                            ok = false; // token 内含 > → ∉ Token 字符集 → OUT
                            break;
                        }
                    };
                    if redirects.len() >= 3 {
                        ok = false;
                        break;
                    }
                    let op = if it.peek() == Some(&'>') {
                        it.next();
                        RedirectOp::Append
                    } else {
                        RedirectOp::Out
                    };
                    if it.peek() == Some(&'&') {
                        // 合并重定向 2>&1（无路径目标 OK；& 后必须 1|2）；
                        // 裸 >&1（无 fd 前缀）不在子集 G → OUT
                        if fd.is_none() {
                            ok = false;
                            break;
                        }
                        it.next();
                        match it.next() {
                            Some('1') | Some('2') => redirects.push(Redirect {
                                fd,
                                op: RedirectOp::Merge,
                                target: None,
                            }),
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                        continue;
                    }
                    match read_target(&mut it) {
                        Some(target) => redirects.push(Redirect {
                            fd,
                            op,
                            target: Some(target),
                        }),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                '$' => {
                    bare_dollar = true;
                    buf.push('$');
                }
                _ => buf.push(c),
            },
        }
    }
    // 收尾：flush + 未闭合引号检查
    if ok {
        if mode != QuoteMode::None {
            ok = false; // 未闭合引号
        } else if !buf.is_empty() {
            tokens.push(Token {
                value: std::mem::take(&mut buf),
                bare_dollar,
            });
        }
    }
    // 裸 $ 豁免判定：仅 `$PSVersionTable` 单 token 形态合法
    if ok && tokens.iter().any(|t| t.bare_dollar) && !is_psversiontable_single(&tokens) {
        ok = false;
    }
    if ok && tokens.is_empty() {
        ok = false;
    }
    ScanResult {
        ok,
        tokens,
        redirects,
    }
}

/// 重定向目标读取：跳过前导空白后读一个 token（引号分组语义同参数），
/// 返回剥离引号后的值；未闭合引号 / 目标内出现 OUT 字符 / 空 → None。
fn read_target(it: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
    while matches!(it.peek(), Some(c) if c.is_whitespace()) {
        it.next();
    }
    let mut tbuf = String::new();
    let mut mode = QuoteMode::None;
    loop {
        // 流末 = 目标正常结束（重定向目标可为命令末尾词）；
        // 未闭合引号由收尾检查拦（mode != None → None）
        let Some(&c) = it.peek() else { break };
        match mode {
            QuoteMode::Single => {
                if c == '\'' {
                    it.next();
                    mode = QuoteMode::None;
                } else {
                    tbuf.push(c);
                    it.next();
                }
            }
            QuoteMode::Double => {
                if c == '"' {
                    it.next();
                    mode = QuoteMode::None;
                } else if c == '$' || c == '%' || c == '`' {
                    return None; // 引号内展开构造
                } else {
                    tbuf.push(c);
                    it.next();
                }
            }
            QuoteMode::None => match c {
                ' ' | '\t' => break, // 目标结束
                '"' => {
                    it.next();
                    mode = QuoteMode::Double;
                }
                '\'' => {
                    it.next();
                    mode = QuoteMode::Single;
                }
                '|' | '&' | ';' | '<' | '>' | '(' | ')' | '%' | '`' => return None,
                '$' => {
                    // 裸 $：目标内展开构造 → 非法（除非整串即 $PSVersionTable，
                    // 但重定向目标场景无豁免价值 → 一律 OUT）
                    return None;
                }
                _ => {
                    tbuf.push(c);
                    it.next();
                }
            },
        }
    }
    if mode != QuoteMode::None {
        return None; // 未闭合
    }
    if tbuf.is_empty() {
        None
    } else {
        Some(tbuf)
    }
}

// ═══════════════════════════════════════════════════════
// 删除门 D（M3-A 设计 §5）
// ═══════════════════════════════════════════════════════

/// 原始文本删除词扫描（换轴新增）：词法门失败时 token 不可信、删除门失效，
/// 改用本扫描兜住“脚本式删除”。粗粒度、可绕，但挡住最直接的写法。
pub(crate) fn raw_delete_hit(command: &str) -> Option<&'static str> {
    // 以「非 [a-z0-9_.-]」为分隔切词；保留连字符（remove-item）与点（.exe）
    let mut word = String::new();
    let mut words: Vec<String> = Vec::new();
    for ch in command.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
            word.push(ch.to_ascii_lowercase());
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    for w in &words {
        let bare = w.strip_suffix(".exe").unwrap_or(w.as_str());
        if risk::is_delete_word(bare) {
            return Some("delete-always-ask");
        }
    }
    None
}

/// 删除语义判定：命中 → reason（delete-always-ask / delete-flag-option）。
/// 凡命中删除词表，无论参数指向何处（区内也问）一律询问。
pub(crate) fn is_delete_program(tokens: &[Token]) -> Option<&'static str> {
    let first = tokens.first()?;
    let name = first.norm_name();
    // 删除词表（del/rm/rd/rmdir/remove-item/clear-content 等）
    if risk::is_delete_word(&name) {
        return Some("delete-always-ask");
    }
    // git rm：删除门（§5 删除词全表含 git rm；分类层 git 子表 rm 同拦）
    if name == "git" {
        if let Some(sub) = tokens.get(1) {
            if sub.norm_name() == "rm" {
                return Some("delete-always-ask");
            }
        }
    }
    // robocopy /mir | /purge：删除开关（§5）
    if name == "robocopy" {
        let has_delete_flag = tokens.iter().skip(1).any(|t| {
            let v = t.value.to_lowercase();
            v == "/mir" || v == "/purge"
        });
        if has_delete_flag {
            return Some("delete-flag-option");
        }
    }
    None
}

// ═══════════════════════════════════════════════════════
// 程序分类 C + 目标提取
// ═══════════════════════════════════════════════════════

/// 分类产物：操作族 + 目标提取模式 + 固定询问 reason（分类层已定的形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Classified {
    kind: OpKind,
    mode: TargetMode,
    /// 分类层即确定的询问 reason（如解释器未列开关 → unlisted-option）；
    /// None = 走标准 kind 分支。
    fixed_ask: Option<&'static str>,
}

impl Classified {
    fn new(kind: OpKind, mode: TargetMode) -> Self {
        Self {
            kind,
            mode,
            fixed_ask: None,
        }
    }

    fn ask(kind: OpKind, reason: &'static str) -> Self {
        Self {
            kind,
            mode: TargetMode::None,
            fixed_ask: Some(reason),
        }
    }
}

/// 目标提取模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetMode {
    /// 无路径目标（参数非路径）
    None,
    /// 全部非选项参数为路径
    AllPositional,
    /// 前两个非选项参数为路径（robocopy）
    FirstTwoPositional,
    /// 运行/脚本目标：命令名自身（含路径形态程序）或首个非选项参数
    Script,
    /// git 子命令路径（sub 后非选项 token）
    GitPaths,
}

/// 版本探测形态：`<probe> --version|-v|-?|/?`（系统查询类型放行）。
fn is_version_probe(tokens: &[Token]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    let name = first.norm_name();
    if !risk::PROBE_EXECUTABLES.contains(&name.as_str()) {
        return false;
    }
    tokens.iter().skip(1).any(|t| {
        let v = t.value.to_lowercase();
        v == "--version" || v == "-v" || v == "-?" || v == "/?"
    })
}

/// git 子命令分类（§6 git 子表；含 undo/网络/只读面）。
fn classify_git(tokens: &[Token]) -> Classified {
    let sub = tokens.get(1).map(|t| t.norm_name()).unwrap_or_default();
    let has = |needle: &str| {
        tokens
            .iter()
            .skip(1)
            .any(|t| t.value.to_lowercase() == needle)
    };
    let any_prefix = |prefix: &str| {
        tokens
            .iter()
            .skip(1)
            .any(|t| t.value.to_lowercase().starts_with(prefix))
    };
    match sub.as_str() {
        // 只读子命令（§6：status/log/diff/show → SystemQueryOp AUTO）
        _ if sub.is_empty() || risk::GIT_READ_SUBCOMMANDS.contains(&sub.as_str()) => {
            Classified::new(OpKind::SystemQueryOp, TargetMode::None)
        }
        "branch" => {
            if any_prefix("-d") {
                // -d/-D 删除分支 → 丢内容面（git-undo）
                Classified::ask(OpKind::UndoOp, "git-undo")
            } else if has("-v") || has("-vv") || tokens.len() == 2 {
                Classified::new(OpKind::SystemQueryOp, TargetMode::None)
            } else {
                // -m 改名 / 纯名字创建 = 区内仓库写
                Classified::new(OpKind::FileWriteOp, TargetMode::None)
            }
        }
        "remote" => {
            if has("-v") || tokens.len() == 2 {
                Classified::new(OpKind::SystemQueryOp, TargetMode::None)
            } else {
                // remote add/remove/set-url：远端仓库操作（网络面）
                Classified::ask(OpKind::NetworkOp, "network-op")
            }
        }
        "rm" => Classified::ask(OpKind::FileDeleteOp, "delete-always-ask"),
        "add" | "mv" => Classified::new(OpKind::FileWriteOp, TargetMode::GitPaths),
        "commit" | "merge" | "tag" => Classified::new(OpKind::FileWriteOp, TargetMode::None),
        "stash" => {
            let sub2 = tokens.get(2).map(|t| t.norm_name()).unwrap_or_default();
            match sub2.as_str() {
                "push" | "pop" => Classified::new(OpKind::FileWriteOp, TargetMode::None),
                // list/show 只读；apply 恢复工作区写；drop/clear 丢 stash → undo
                "list" | "show" => Classified::new(OpKind::SystemQueryOp, TargetMode::None),
                "" | "apply" => Classified::new(OpKind::FileWriteOp, TargetMode::None),
                _ => Classified::ask(OpKind::UndoOp, "git-undo"),
            }
        }
        "checkout" => {
            if any_prefix("-f") {
                Classified::ask(OpKind::UndoOp, "git-undo")
            } else {
                // 无 -f：切分支 / checkout -- file（恢复路径）
                Classified::new(OpKind::FileWriteOp, TargetMode::GitPaths)
            }
        }
        "reset" => Classified::ask(OpKind::UndoOp, "git-undo"),
        "clean" => {
            // 无 -f 时 git clean 仅预览不删除；有 -f 丢内容
            if any_prefix("-f") {
                Classified::ask(OpKind::UndoOp, "git-undo")
            } else {
                Classified::new(OpKind::FileWriteOp, TargetMode::None)
            }
        }
        "push" | "fetch" | "pull" | "clone" => Classified::ask(OpKind::NetworkOp, "network-op"),
        // 未列 git 子命令（rebase/cherry-pick/config/fsck…）→ 保守询问
        _ => Classified::ask(OpKind::UndoOp, "git-undo"),
    }
}

/// reg 子命令分类：query = 只读（SystemQueryOp AUTO）；其余 = 系统配置面保守询问。
fn classify_reg(tokens: &[Token]) -> Classified {
    let sub = tokens.get(1).map(|t| t.norm_name()).unwrap_or_default();
    if sub.is_empty() || sub == "query" {
        Classified::new(OpKind::SystemQueryOp, TargetMode::None)
    } else {
        // add/delete/import/restore 已由 CRIT 先行恒拦；export/copy/save 等保守询问
        Classified::ask(OpKind::ConfigOp, "config-op")
    }
}

/// 解释器特判（M3-A 设计 §4 解释器/代码执行规约）。
fn classify_interpreter(name: &str, tokens: &[Token]) -> Option<Classified> {
    let second = tokens
        .get(1)
        .map(|t| t.value.to_lowercase())
        .unwrap_or_default();
    match name {
        // python：script → RunProgramOp；-c → CodeExec；-m/其它开关 → unlisted
        "python" | "python3" => {
            let c = match second.as_str() {
                "-c" => Classified::ask(OpKind::CodeExecOp, "code-exec"),
                "-m" => Classified::ask(OpKind::UnknownOp, "unlisted-option"),
                "" => Classified::ask(OpKind::CodeExecOp, "code-exec"), // REPL
                s if s.starts_with('-') => Classified::ask(OpKind::UnknownOp, "unlisted-option"),
                _ => Classified::new(OpKind::RunProgramOp, TargetMode::Script),
            };
            Some(c)
        }
        // powershell/pwsh：-Command/-c → CodeExec；-File/-f → 脚本地域
        "powershell" | "pwsh" => {
            let c = match second.as_str() {
                "-command" | "-c" => Classified::ask(OpKind::CodeExecOp, "code-exec"),
                "-file" | "-f" => Classified::new(OpKind::RunProgramOp, TargetMode::Script),
                "-?" | "/?" => Classified::new(OpKind::SystemQueryOp, TargetMode::None),
                "" => Classified::ask(OpKind::CodeExecOp, "code-exec"), // 交互会话
                s if s.starts_with('-') => Classified::ask(OpKind::UnknownOp, "unlisted-option"),
                _ => Classified::new(OpKind::RunProgramOp, TargetMode::Script),
            };
            Some(c)
        }
        // cmd：/c、/k 整串执行 → CodeExec；/x 未列开关 → unlisted-option
        "cmd" => {
            let c = match second.as_str() {
                "/c" | "/k" => Classified::ask(OpKind::CodeExecOp, "code-exec"),
                "/?" => Classified::new(OpKind::SystemQueryOp, TargetMode::None),
                "" => Classified::ask(OpKind::CodeExecOp, "code-exec"),
                _ => Classified::ask(OpKind::UnknownOp, "unlisted-option"),
            };
            Some(c)
        }
        // bash/sh：-c 执行串；位置参数 = 脚本
        "bash" | "sh" => {
            let c = match second.as_str() {
                "-c" => Classified::ask(OpKind::CodeExecOp, "code-exec"),
                "" => Classified::ask(OpKind::CodeExecOp, "code-exec"), // 交互
                s if s.starts_with('-') => Classified::ask(OpKind::UnknownOp, "unlisted-option"),
                _ => Classified::new(OpKind::RunProgramOp, TargetMode::Script),
            };
            Some(c)
        }
        // node/deno/bun/ruby/perl：-e/-p/eval 代码串；run/位置 = 脚本
        "node" | "deno" | "bun" | "ruby" | "perl" => {
            let c = match second.as_str() {
                "-e" | "-p" | "eval" => Classified::ask(OpKind::CodeExecOp, "code-exec"),
                "" => Classified::ask(OpKind::CodeExecOp, "code-exec"), // REPL
                "run" if name == "deno" || name == "bun" => {
                    Classified::new(OpKind::RunProgramOp, TargetMode::Script)
                }
                s if s.starts_with('-') => Classified::ask(OpKind::UnknownOp, "unlisted-option"),
                _ => Classified::new(OpKind::RunProgramOp, TargetMode::Script),
            };
            Some(c)
        }
        // npm/npx：安装/执行通道（网络+代码面，保守询问；版本探测已前置）
        "npm" | "npx" => Some(Classified::ask(OpKind::InstallOp, "install-op")),
        _ => None,
    }
}

/// 程序分类 C：程序名（tokens[0] 归一化）→ (OpKind, 目标模式, 固定 reason)。
/// 表驱动（risk::classify_program_spec）+ git/reg/解释器子表 + 路径形态特判。
fn classify_program(tokens: &[Token]) -> Classified {
    let Some(first) = tokens.first() else {
        return Classified::new(OpKind::UnknownOp, TargetMode::None);
    };
    // 语义族表内程序优先（M3-A §4/§6）：表内程序即使带 .exe 也走其族语义
    // （curl.exe → NetworkOp 网络族）；表外程序才落路径形态/未知程序规则。
    let name = first.norm_name();
    match name.as_str() {
        "git" => return classify_git(tokens),
        "reg" => return classify_reg(tokens),
        _ => {}
    }
    if risk::INTERPRETER_NAMES.contains(&name.as_str()) {
        if let Some(c) = classify_interpreter(&name, tokens) {
            return c;
        }
    }
    if let Some(spec) = risk::classify_program_spec(&name) {
        let mode = match (spec.kind, spec.path_rule) {
            (OpKind::SystemQueryOp, _)
            | (
                OpKind::NetworkOp | OpKind::InstallOp | OpKind::ProcessControlOp | OpKind::ConfigOp,
                _,
            ) => TargetMode::None,
            (_, PathRule::NoPaths) => TargetMode::None,
            (_, PathRule::PositionalAll) => TargetMode::AllPositional,
            (_, PathRule::PositionalFirstTwo) => TargetMode::FirstTwoPositional,
        };
        return Classified::new(spec.kind, mode);
    }
    // 表外程序含路径形态（分隔符/盘符/.exe）→ 命令名自身是运行目标（§4 程序定位）
    if first.has_path_shape() {
        return Classified::new(OpKind::RunProgramOp, TargetMode::Script);
    }
    // 表外未知程序：首个非选项参数 = 运行目标候选
    Classified::new(OpKind::UnknownOp, TargetMode::Script)
}

/// 目标提取：按分类模式收集路径目标（原始 token 值）。
/// 遇 unlisted-option（strict 面白名单外 -x）→ Err。
fn extract_targets(cls: &Classified, tokens: &[Token]) -> Result<Vec<String>, &'static str> {
    match cls.mode {
        TargetMode::None => Ok(Vec::new()),
        TargetMode::AllPositional | TargetMode::FirstTwoPositional => {
            let spec = risk::classify_program_spec(&tokens[0].norm_name());
            let lenient = matches!(
                spec.map(|s| s.option_policy),
                Some(risk::OptionPolicy::Lenient)
            );
            let limit = if cls.mode == TargetMode::FirstTwoPositional {
                2
            } else {
                usize::MAX
            };
            let mut out = Vec::new();
            let mut after_double_dash = false;
            let mut skip_value = false;
            let mut i = 1;
            while i < tokens.len() && out.len() < limit {
                let t = &tokens[i];
                let v = t.value.as_str();
                if !after_double_dash && v == "--" {
                    after_double_dash = true;
                    i += 1;
                    continue;
                }
                if !after_double_dash && v.starts_with('-') {
                    match is_path_value_option(t) {
                        Some(true) => {
                            // 路径值选项：值参与判定（-Path/-Destination/…）
                            if let Some(next) = tokens.get(i + 1) {
                                if !next.value.starts_with('-') {
                                    out.push(next.value.clone());
                                    i += 1;
                                }
                            }
                        }
                        Some(false) => skip_value = true, // -filter 值跳过
                        None => {
                            if !lenient {
                                return Err("unlisted-option");
                            }
                        }
                    }
                    i += 1;
                    continue;
                }
                if !after_double_dash && v.starts_with('/')
                    && lenient {
                        // robocopy 等：cmd 风格开关跳过（/mir /purge 已由删除门拦）
                        i += 1;
                        continue;
                    }
                    // strict 面 `/x`：PS/cmd 双态命令中按字面参数收进位置流；
                    // join 判定 cwd 必在区内，不产生额外放行面（安全方向）。
                if skip_value {
                    skip_value = false;
                } else {
                    out.push(t.value.clone());
                }
                i += 1;
            }
            Ok(out)
        }
        TargetMode::Script => {
            // 运行面目标：命令名自身含路径形态（程序定位）→ 命令名即目标；
            // 否则（解释器脚本/未知程序）→ 首个非选项参数。
            let mut out = Vec::new();
            if tokens[0].has_path_shape() {
                out.push(tokens[0].value.clone());
                return Ok(out);
            }
            let mut after_double_dash = false;
            for t in tokens.iter().skip(1) {
                let v = t.value.as_str();
                if !after_double_dash && v == "--" {
                    after_double_dash = true;
                    continue;
                }
                if !after_double_dash && (v.starts_with('-') || v.starts_with('/')) {
                    continue;
                }
                out.push(t.value.clone());
                break;
            }
            Ok(out)
        }
        TargetMode::GitPaths => {
            // git 路径：sub 之后；跳过 -x 选项与 `--` 标记。
            let mut out = Vec::new();
            let mut after_double_dash = false;
            let mut i = 2;
            while i < tokens.len() {
                let t = &tokens[i];
                let v = t.value.as_str();
                if !after_double_dash && v == "--" {
                    after_double_dash = true;
                    i += 1;
                    continue;
                }
                if !after_double_dash && v.starts_with('-') {
                    i += 1;
                    continue;
                }
                out.push(t.value.clone());
                i += 1;
            }
            Ok(out)
        }
    }
}

/// 命名选项判定：PATH_VALUE 表（值参与路径）| SKIP_VALUE 表（值跳过）| 未列。
fn is_path_value_option(t: &Token) -> Option<bool> {
    let v = t.value.to_lowercase();
    if risk::PATH_VALUE_OPTIONS.contains(&v.as_str()) {
        Some(true)
    } else if risk::SKIP_VALUE_OPTIONS.contains(&v.as_str()) {
        Some(false)
    } else {
        None
    }
}

// ═══════════════════════════════════════════════════════
// 路径规范化与地域比较（M3-A 设计 §3.5）
// ═══════════════════════════════════════════════════════

/// 路径规范化：相对路径以 `cwd` join；存在路径 canonicalize（符号链接/
/// Junction 解析到真实目标，指向区外按区外判）；不存在路径沿祖先上溯找
/// 最近存在者 canonicalize，其余段词法拼接。不可解析 → None。
pub(crate) fn normalize_path(raw: &str, cwd: &Path) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let raw_path = Path::new(raw);
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        cwd.join(raw_path)
    };
    if let Ok(c) = candidate.canonicalize() {
        return Some(c);
    }
    // 祖先上溯：最近存在者 canonicalize + 缺失段词法拼接
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = candidate;
    loop {
        if std::fs::metadata(&cur).is_ok() {
            break;
        }
        let name = cur.file_name()?;
        missing.push(name.to_os_string());
        cur = cur.parent()?.to_path_buf();
    }
    let base = cur.canonicalize().ok()?;
    let mut resolved = base;
    for seg in missing.iter().rev() {
        resolved.push(seg);
    }
    Some(resolved)
}

/// 组件级前缀比较（大小写不敏感）：target 位于 workspace 内（== 或前缀）。
pub(crate) fn inside_workspace(target: &Path, workspace: &Path) -> bool {
    let t: Vec<String> = target
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let w: Vec<String> = workspace
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    t.len() >= w.len() && t[..w.len()] == w[..]
}

// ═══════════════════════════════════════════════════════
// evaluate —— 判定管道组装（优先级链）
// ═══════════════════════════════════════════════════════

/// 地域性信任判定入口（M3-A 设计 §1 管道）。
///
/// - `command`：run_command 的完整命令文本
/// - `cwd`：命令执行工作目录（缺省 = workspace_root；调用方解析后传入）
/// - `workspace`：信任根（Policy.workspace_root）
pub fn evaluate(command: &str, cwd: &Path, workspace: &Path) -> GateVerdict {
    // 0. CRIT 恒拦门（先于一切；任何模式，含询问模式）
    if let Some(cv) = risk::check_crit(command) {
        return GateVerdict::blocked(cv.rule_id);
    }
    // 1. 词法子集门 G：分析不了不再等于必须问（换轴）。
    //    但 token 不可信 → 删除门失效，改用词表级扫描兜住脚本式删除。
    let scan = scan_tokens(command);
    if !scan.ok || scan.tokens.is_empty() {
        if let Some(reason) = raw_delete_hit(command) {
            return GateVerdict::ask(reason);
        }
        return GateVerdict::auto("syntax-unproven");
    }
    let tokens = scan.tokens;
    // `$PSVersionTable` 单 token 特例豁免与版本探测形态（§6 类型放行）
    if is_psversiontable_single(&tokens) || is_version_probe(&tokens) {
        return GateVerdict::auto("system-query");
    }
    // 2. 删除语义门 D
    if let Some(reason) = is_delete_program(&tokens) {
        return GateVerdict::ask(reason);
    }
    // 3. 程序分类 C
    let cls = classify_program(&tokens);
    if let Some(reason) = cls.fixed_ask {
        return GateVerdict::ask(reason);
    }
    // 4. 目标收集与区外记录（换轴：地域门撤掉——区外写被 OS 拒、读无害、
    //    运行同受身份约束；此处只把“触及区外”记进审计位，不再驱动询问）
    let ws_norm = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let cwd_norm =
        normalize_path(&cwd.to_string_lossy(), &ws_norm).unwrap_or_else(|| ws_norm.clone());
    let mut watched_external = !inside_workspace(&cwd_norm, &ws_norm);

    // 目标集合 = 程序目标 ∪ 重定向目标
    let mut targets: Vec<String> = Vec::new();
    for r in &scan.redirects {
        if let Some(t) = &r.target {
            targets.push(t.clone());
        }
    }
    match extract_targets(&cls, &tokens) {
        Ok(mut t) => targets.append(&mut t),
        // 换轴：目标提取不了（未列开关等）不再等于必须问
        Err(_) => {
            let mut v = GateVerdict::auto("target-unresolved");
            v.touched_external = watched_external;
            return v;
        }
    }

    // 无条件 ASK 族与类型放行族先行
    match cls.kind {
        OpKind::SystemQueryOp
            // 类型放行族：无重定向目标才无条件 AUTO（重定向写盘走地域判定）
            if targets.is_empty() => {
                return GateVerdict::auto("system-query");
            }
        OpKind::CodeExecOp => return GateVerdict::ask("code-exec"),
        OpKind::NetworkOp => return GateVerdict::ask("network-op"),
        OpKind::InstallOp => return GateVerdict::ask("install-op"),
        OpKind::ProcessControlOp => return GateVerdict::ask("process-control"),
        OpKind::ConfigOp => return GateVerdict::ask("config-op"),
        OpKind::UndoOp => return GateVerdict::ask("git-undo"),
        OpKind::FileDeleteOp => return GateVerdict::ask("delete-always-ask"),
        _ => {}
    }

    // 区外触及只记审计位（不再询问）
    for t in &targets {
        match normalize_path(t, &cwd_norm) {
            Some(n) => {
                if !inside_workspace(&n, &ws_norm) {
                    watched_external = true;
                }
            }
            None => watched_external = true,
        }
    }
    let mut v = GateVerdict::auto("no-risk-family");
    v.touched_external = watched_external;
    v
}

#[cfg(test)]
mod tests;
