//! risk —— 命令风险判定数据层（M3-A 地域性信任门）
//!
//! M3-A 定稿设计（_temp_m3_gate_design.md）拆分后的风险模块：
//!
//! - [`check_crit`]：CRIT 恒拦名单（任何模式、先于一切）。原 CRIT 正则表
//!   原样保留、逐条语义不变，只把"档位"产物拆为独立入口。
//! - 语义族数据层：命令名 → [`OpKind`] 操作族映射、命名选项白名单、
//!   git 子命令族、解释器族——判定逻辑（词法、删除门、分类编排、地域）
//!   全部在 `exec::gate` 消费本表实现；本模块不产生"档位 → 放行"路径。
//!
//! 旧 LOW/HIGH 静态名单与 classify_low_single / high_rule_id 档位语义已删除：
//! 放行语义由 GateDecision（Auto/Ask/Blocked）取代；RiskLevel 保留仅作
//! 事件文案族（ApprovalRisk low/high/crit 的事件映射独立于本模块）。

use std::sync::LazyLock;

/// CRIT 判定结果：命中规则 id + 命中文本摘要（审计与错误文案携带）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CritVerdict {
    pub rule_id: &'static str,
    pub matched: String,
}

// ═══════════════════════════════════════════════════════
// CRIT —— 恒拦名单（任何模式，含操作前询问模式）
// ═══════════════════════════════════════════════════════

/// CRIT 规则表：(regex, rule_id)。正则不区分大小写、整串扫描。
/// 参照既有 PREFLIGHT_WIN32_HARD 的 Windows 语境落地（M3-A 设计 §4.1）。
/// 本表语义原样保留（M3-A 设计：crit 恒拦集合逐条语义不变）。
static CRIT_PATTERNS: LazyLock<Vec<(regex::Regex, &'static str)>> = LazyLock::new(|| {
    let pairs: &[(&str, &'static str)] = &[
        // 递归删除（del/erase/rd/rmdir + /s 或 -s）
        (
            r"\b(?:del|erase|rd|rmdir)\b[^\r\n]*(?:/|-)\s*s\b",
            "crit-recursive-delete",
        ),
        // 磁盘格式化/分区（format + 盘符、diskpart）
        (r"\bformat\b\s+[a-zA-Z]:", "crit-format-drive"),
        (r"\bdiskpart\b", "crit-diskpart"),
        // 注册表写
        (
            r"\breg\b\s+(?:add|delete|import|restore)\b",
            "crit-registry-write",
        ),
        // ACL/所有权变更
        (r"\btakeown\b", "crit-takeown"),
        (r"\bicacls\b", "crit-icacls"),
        // 账户/服务/计划任务持久化
        (r"\bnet\s+(?:user|localgroup)\b", "crit-net-account"),
        (
            r"\bsc\s+(?:create|delete|config|failure)\b",
            "crit-sc-service",
        ),
        (r"\bschtasks\s+/(?:create|delete|change)\b", "crit-schtasks"),
        // 引导配置
        (r"\bbcdedit\b", "crit-bcdedit"),
    ];
    pairs
        .iter()
        .map(|(pat, id)| {
            (
                regex::RegexBuilder::new(pat)
                    .case_insensitive(true)
                    .build()
                    .expect("CRIT 正则非法"),
                *id,
            )
        })
        .collect()
});

/// CRIT 恒拦判定（任何模式先于一切）：命中 → 规则 id + 命中文本。
pub fn check_crit(command: &str) -> Option<CritVerdict> {
    let raw = command.trim();
    for (re, id) in CRIT_PATTERNS.iter() {
        if let Some(m) = re.find(raw) {
            return Some(CritVerdict {
                rule_id: id,
                matched: m.as_str().chars().take(120).collect(),
            });
        }
    }
    None
}

// ═══════════════════════════════════════════════════════
// 操作族（OpKind）—— 程序分类 C 的语义族（M3-A 设计 §4）
// ═══════════════════════════════════════════════════════

/// 程序语义操作族。evaluate 按族决定放行通道：
/// 地域面（Read/Write/Navigate/Run）+ 类型面（SystemQuery）+ 询问面（其余）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// 文件读（路径全 ∈ 区内 → AUTO）
    FileReadOp,
    /// 文件写/移动/新建（路径全 ∈ 区内 → AUTO）
    FileWriteOp,
    /// 目录导航（目标 ∈ 区内 → AUTO）
    FileNavigateOp,
    /// 运行区内程序/脚本（目标 ∈ 区内 → AUTO）
    RunProgramOp,
    /// 系统查询：无副作用类型放行（无条件 AUTO）
    SystemQueryOp,
    /// 代码片段执行（ASK code-exec）
    CodeExecOp,
    /// 网络访问（ASK network-op，无区内豁免）
    NetworkOp,
    /// 安装/卸载（ASK install-op）
    InstallOp,
    /// 进程控制（ASK process-control）
    ProcessControlOp,
    /// 系统配置（ASK config-op）
    ConfigOp,
    /// 仓库破坏性/丢内容面（ASK git-undo）
    UndoOp,
    /// 删除语义（删除门 D 提前拦；防御兜底 ASK delete-always-ask）
    FileDeleteOp,
    /// 未知程序（首个非选项参数 = 运行目标候选，地域判定）
    UnknownOp,
}

/// 路径参数规约：命令如何从参数流提取参与地域判定的路径目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRule {
    /// 参数不携带路径（echo/get-process/git status 等）
    NoPaths,
    /// 全部非选项参数为路径（type/copy/mkdir 等）
    PositionalAll,
    /// 前两个非选项参数为路径（robocopy：源 + 目的）
    PositionalFirstTwo,
}

/// 命名选项策略：白名单外 -x 参数是否直接 ASK(unlisted-option)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionPolicy {
    /// 严格（PowerShell cmdlet/解释器系）：白名单外任何 -x → ASK
    Strict,
    /// 宽松（cmd/跨 shell 共用名/robocopy）：-x 与 /x 一律按选项跳过，
    /// 只收集位置参数（cmd 风格开关不触发 unlisted-option）
    Lenient,
}

/// 命令 → 语义规格（程序分类 C 的命令映射表主体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramSpec {
    pub kind: OpKind,
    pub path_rule: PathRule,
    pub option_policy: OptionPolicy,
}

const fn spec(kind: OpKind, path_rule: PathRule, option_policy: OptionPolicy) -> ProgramSpec {
    ProgramSpec {
        kind,
        path_rule,
        option_policy,
    }
}

/// 严格选项语义的常用缩写（PowerShell cmdlet 形态）。
const fn strict(kind: OpKind, path_rule: PathRule) -> ProgramSpec {
    spec(kind, path_rule, OptionPolicy::Strict)
}

/// 删除词全表（M3-A 设计 §5；git rm 单独并入——见 [`is_delete_word`]）。
const DELETE_WORDS: &[&str] = &[
    "del",
    "erase",
    "rm",
    "rd",
    "rmdir",
    "ri",
    "remove-item",
    "remove-itemproperty",
    "clc",
    "clear-content",
    "clear-recyclebin",
];

/// 是否命中删除词表（删除门 D；任何位置都 ASK，不论目标在哪）。
pub fn is_delete_word(name: &str) -> bool {
    DELETE_WORDS.contains(&name)
}

/// 版本探测可执行（命令形如 `<probe> --version|-v` 属系统查询类型放行）。
pub const PROBE_EXECUTABLES: &[&str] = &[
    "python",
    "python3",
    "node",
    "npm",
    "git",
    "powershell",
    "pwsh",
    "cmd",
];

// ═══════════════════════════════════════════════════════
// 命令映射表（命令名归一化后查表：小写、剥首尾引号、剥 .exe）
// ═══════════════════════════════════════════════════════

/// 命令名 → 语义规格（M3-A 设计 §4 表）。git/reg/解释器走独立子表。
fn command_spec(name: &str) -> Option<ProgramSpec> {
    let s = match name {
        // ── 系统查询族：无副作用信息读取，类型放行 AUTO ──
        "echo" | "write-output" | "write-host" => strict(OpKind::SystemQueryOp, PathRule::NoPaths),
        "get-process" | "get-service" | "systeminfo" | "ver" | "ipconfig" | "date" | "time"
        | "tasklist" | "get-location" | "pwd" => strict(OpKind::SystemQueryOp, PathRule::NoPaths),

        // ── 文件读族：全部非选项参数即路径，地域判定 ──
        "type" | "cat" | "gc" | "get-content" | "get-childitem" | "dir" | "ls" | "gci" | "tree"
        | "more" | "select-string" | "findstr" | "where" | "get-command" | "get-item" => {
            strict(OpKind::FileReadOp, PathRule::PositionalAll)
        }

        // ── 文件写/移动/新建族：地域判定 ──
        "copy" | "cp" | "cpi" | "move" | "mv" | "mi" | "xcopy" | "rename" | "ren" | "rni"
        | "mkdir" | "md" | "ni" | "new-item" | "set-content" | "out-file" | "add-content" => {
            strict(OpKind::FileWriteOp, PathRule::PositionalAll)
        }
        // robocopy：前两个路径参数；/ 开关全部宽松（/mir、/purge 由删除门拦）
        "robocopy" => spec(
            OpKind::FileWriteOp,
            PathRule::PositionalFirstTwo,
            OptionPolicy::Lenient,
        ),

        // ── 目录导航族 ──
        "cd" | "chdir" | "set-location" | "sl" | "pushd" | "popd" => {
            strict(OpKind::FileNavigateOp, PathRule::PositionalAll)
        }

        // ── 删除族（删除门 D 提前拦；此处仅防御兜底）──
        "del"
        | "erase"
        | "rm"
        | "rd"
        | "rmdir"
        | "ri"
        | "remove-item"
        | "remove-itemproperty"
        | "clc"
        | "clear-content" => strict(OpKind::FileDeleteOp, PathRule::NoPaths),

        // ── 网络族（ASK network-op）──
        "curl" | "wget" | "iwr" | "invoke-webrequest" | "invoke-restmethod" => {
            strict(OpKind::NetworkOp, PathRule::NoPaths)
        }

        // ── 安装族（ASK install-op）──
        "winget" | "choco" | "scoop" | "pip" | "pip3" | "cargo" => {
            strict(OpKind::InstallOp, PathRule::NoPaths)
        }

        // ── 进程控制族（ASK process-control）──
        "taskkill" | "stop-process" | "kill" | "shutdown" | "logoff" | "stop-computer"
        | "restart-computer" | "start-process" => {
            strict(OpKind::ProcessControlOp, PathRule::NoPaths)
        }

        // ── 系统配置族（ASK config-op；netsh/net/route 整命令族保守）──
        "netsh" | "net" | "route" | "set-executionpolicy" | "wmic" => {
            strict(OpKind::ConfigOp, PathRule::NoPaths)
        }

        // ── 代码执行载体（ASK code-exec）：未在解释器子表覆盖的宿主/脚本宿主 ──
        "invoke-expression" | "iex" | "add-type" | "rundll32" | "regsvr32" | "mshta"
        | "cscript" | "wscript" => strict(OpKind::CodeExecOp, PathRule::NoPaths),

        // git/reg/解释器走子表特判
        "git" | "reg" | "python" | "python3" | "node" | "npm" | "npx" | "pwsh" | "powershell"
        | "cmd" | "bash" | "sh" | "deno" | "bun" | "ruby" | "perl" => return None,

        _ => return None,
    };
    Some(s)
}

/// git 子命令族表（只读面 → SystemQueryOp；破坏面由 caller 按
/// git_spec 特判返回 UndoOp 等——见 gate::classify_git）。
pub const GIT_READ_SUBCOMMANDS: &[&str] = &["status", "log", "diff", "show"];

/// 命名选项白名单：值 = 路径、参与地域判定。
pub const PATH_VALUE_OPTIONS: &[&str] = &[
    "-path",
    "-literalpath",
    "-destination",
    "-targetpath",
    "-filepath",
    "-fullname",
    "-source",
];

/// 选项值显式跳过（不参与地域判定）：-filter 的过滤器值。
pub const SKIP_VALUE_OPTIONS: &[&str] = &["-filter"];

/// 解释器清单（-c/-Command/-e 等形态 → CodeExecOp ASK；脚本文件 → RunProgramOp）。
pub const INTERPRETER_NAMES: &[&str] = &[
    "python",
    "python3",
    "node",
    "npm",
    "npx",
    "pwsh",
    "powershell",
    "cmd",
    "bash",
    "sh",
    "deno",
    "bun",
    "ruby",
    "perl",
];

/// 命令名 → 基础语义规格（表驱动分类；git/reg/解释器 = None 走子表）。
/// 含路径形态的命令名（`C:\ws\tools\my.exe`、`.\x.ps1`）不进本表——由
/// caller 先行判定为"运行目标"。
pub fn classify_program_spec(name: &str) -> Option<ProgramSpec> {
    command_spec(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CRIT 恒拦集合（原样语义迁移：旧 classify_risk 断言 → check_crit）──

    fn crit_rule(command: &str) -> Option<&'static str> {
        check_crit(command).map(|c| c.rule_id)
    }

    #[test]
    fn crit_recursive_delete_variants() {
        assert_eq!(
            crit_rule("del /s /q C:\\temp\\*"),
            Some("crit-recursive-delete")
        );
        assert_eq!(crit_rule("del /S C:\\temp"), Some("crit-recursive-delete"));
        assert_eq!(crit_rule("erase /s C:\\x"), Some("crit-recursive-delete"));
        assert_eq!(crit_rule("rmdir /s C:\\x"), Some("crit-recursive-delete"));
        assert_eq!(crit_rule("rd /s /q C:\\x"), Some("crit-recursive-delete"));
    }

    #[test]
    fn crit_non_recursive_delete_is_not_crit() {
        // 非递归删除：不恒拦（删除门 ASK 由 gate 承担）
        assert_eq!(check_crit("del C:\\temp\\a.txt"), None);
    }

    #[test]
    fn crit_format_and_diskpart() {
        assert_eq!(crit_rule("format D:"), Some("crit-format-drive"));
        assert_eq!(crit_rule("format d: /q"), Some("crit-format-drive"));
        assert_eq!(crit_rule("diskpart"), Some("crit-diskpart"));
    }

    #[test]
    fn crit_registry_write_variants() {
        assert_eq!(
            crit_rule("reg delete HKCU\\Software\\X"),
            Some("crit-registry-write")
        );
        assert_eq!(
            crit_rule("reg add HKLM\\Software\\X /v Y"),
            Some("crit-registry-write")
        );
        assert_eq!(
            crit_rule("reg import backup.reg"),
            Some("crit-registry-write")
        );
    }

    #[test]
    fn crit_acl_ownership_commands() {
        assert_eq!(
            crit_rule("takeown /f C:\\Windows\\System32\\x.dll"),
            Some("crit-takeown")
        );
        assert_eq!(
            crit_rule("icacls C:\\x /grant Users:F"),
            Some("crit-icacls")
        );
    }

    #[test]
    fn crit_account_service_scheduler_boot() {
        assert_eq!(crit_rule("net user hacker /add"), Some("crit-net-account"));
        assert_eq!(
            crit_rule("net localgroup Administrators hacker /add"),
            Some("crit-net-account")
        );
        assert_eq!(
            crit_rule("sc create EvilSvc binPath= C:\\x"),
            Some("crit-sc-service")
        );
        assert_eq!(
            crit_rule("schtasks /create /tn X /tr Y"),
            Some("crit-schtasks")
        );
        assert_eq!(
            crit_rule("bcdedit /set {current} safeboot minimal"),
            Some("crit-bcdedit")
        );
    }

    #[test]
    fn crit_case_insensitive() {
        assert_eq!(crit_rule("DEL /S C:\\TEMP"), Some("crit-recursive-delete"));
        assert_eq!(crit_rule("Reg Delete HKCU\\X"), Some("crit-registry-write"));
        assert_eq!(crit_rule("BCDEDIT /enum"), Some("crit-bcdedit"));
    }

    #[test]
    fn crit_compound_with_crit_still_crit() {
        // CRIT 先扫：复合串里的危险动词仍恒拦
        assert_eq!(
            crit_rule("echo ok & del /s C:\\x"),
            Some("crit-recursive-delete")
        );
    }

    #[test]
    fn verdict_carries_matched_text_for_crit() {
        let v = check_crit("del /s /q C:\\temp").expect("应命中 CRIT");
        assert_eq!(v.rule_id, "crit-recursive-delete");
        assert!(!v.matched.is_empty());
    }

    #[test]
    fn crit_blank_command_is_none() {
        assert_eq!(check_crit("   "), None);
        assert_eq!(check_crit("echo hi"), None);
    }

    // ── 语义族数据表 ──

    #[test]
    fn spec_read_and_write_families() {
        assert_eq!(
            classify_program_spec("type"),
            Some(strict(OpKind::FileReadOp, PathRule::PositionalAll))
        );
        assert_eq!(
            classify_program_spec("copy"),
            Some(strict(OpKind::FileWriteOp, PathRule::PositionalAll))
        );
        assert_eq!(
            classify_program_spec("robocopy"),
            Some(spec(
                OpKind::FileWriteOp,
                PathRule::PositionalFirstTwo,
                OptionPolicy::Lenient
            ))
        );
        assert_eq!(
            classify_program_spec("echo"),
            Some(strict(OpKind::SystemQueryOp, PathRule::NoPaths))
        );
        assert_eq!(
            classify_program_spec("curl"),
            Some(strict(OpKind::NetworkOp, PathRule::NoPaths))
        );
    }

    #[test]
    fn delete_words_table() {
        for w in ["del", "rm", "rd", "rmdir", "remove-item", "clear-content"] {
            assert!(is_delete_word(w), "{w} 应在删除词表");
        }
        for w in ["copy", "move", "mkdir", "echo", "type"] {
            assert!(!is_delete_word(w), "{w} 不应在删除词表");
        }
    }

    #[test]
    fn git_reg_interpreter_defer_to_subtables() {
        // git/reg/解释器不走基础映射表（caller 子表特判）
        for n in ["git", "reg", "python", "cmd", "powershell", "bash"] {
            assert_eq!(classify_program_spec(n), None, "{n} 应走子表");
        }
    }
}
