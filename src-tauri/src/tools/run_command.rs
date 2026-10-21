//! run_command —— 通用命令执行工具（M3-A §1）
//!
//! 职责分层（与方案 §1.3 一致）：
//! - 本工具 = **纯执行**（参数归一 → exec::runner 内核 → 结果渲染）。
//!   审批/模式/危险度分类**不在** execute 内——由 orchestrator 预检通道
//!   （preflight_tool_calls）在生产路径消费；本 execute 的裸调用（registry
//!   直调 / 测试）= 无审批直接执行（语义见方案 §1.3）。
//! - 超时消费点：`Tool::timeout()` 提供工具级默认（120s），调用级参数
//!   timeout 覆盖之（params.timeout > 工具默认；registry 不做全域包装）。
//!
//! ## 渲染口径（对齐既有 exec_command）
//!
//! - 双通道共用一份尾部额度：2000 行 / 50KB，stderr 优先占额；
//! - 单通道有内容不加标记，双通道带 `[stderr]` / `[stdout]` 标记，顺序统一
//!   **stderr 在前**（异常优先；与真实写入次序无关——两条管道各读，交织早已不可得）；
//! - 截断时把完整输出落盘并把路径写进文案（模型按需 read_file）；
//!   给消费方的路径一律普通形式（剥 `\\?\` 前缀）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::runner::{self, Outcome, RunOptions, RunOutput, ShellKind};
use crate::workspace::WorkspaceRoot;

use super::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 双通道共用的尾部额度（行 / 字节，与既有 exec_command 同口径）。
const MAX_TAIL_LINES: usize = 2000;
const MAX_TAIL_BYTES: usize = 50 * 1024;
/// canonical 中通道尾部字段上限（结构化数据，不进模型上下文）。
const MAX_CANONICAL_CHARS: usize = 4000;
/// canonical 中命令回显截断。
const MAX_COMMAND_CHARS: usize = 512;

/// 工具级默认超时（方案 §3：Tool::timeout() = 执行类工具默认）。
const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub struct RunCommandTool {
    /// 当前工作根（共享句柄）：cwd 默认值 + 输出落盘目录基准
    /// （workspace_root/.prts-exec-output）+ 受限启动的 ACL 放行根。
    /// 每次执行读当前值——设置页改路径即跟随（见 crate::workspace）。
    workspace_root: WorkspaceRoot,
}

impl RunCommandTool {
    /// `workspace_root` 接受路径（单测）或共享句柄（生产装配）。
    pub fn new(workspace_root: impl Into<WorkspaceRoot>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    fn output_dir(&self) -> PathBuf {
        self.workspace_root.current().join(".prts-exec-output")
    }
}

impl Tool for RunCommandTool {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "执行 shell 命令并返回输出：非零退出码按失败返回（含退出码与输出尾部），完整输出落盘工作区可 read_file 读取。命令以受限身份运行，仅可写工作目录内，区外写入被系统拒绝；确需访问区外时声明 permission=full 请求确认（完整语义见参数说明）。审批：询问模式（默认）每次执行前弹确认卡；自动化模式下只有网络访问、安装卸载、进程控制、系统配置、仓库破坏性操作、删除，以及声明 permission=full 的调用会弹确认卡，其余命令（含脚本与管道）直接执行；不可逆破坏类命令任何档位直接拒绝。仅限 Windows（powershell/cmd）；写文件请优先用 write_file/edit。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的完整命令串（含参数），由系统 shell 解释；shell 参数可切换。"
                },
                "cwd": {
                    "type": "string",
                    "description": "工作目录（须已存在；相对路径相对工作空间根）。省略时默认工作空间根。"
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "超时秒数，覆盖工具默认 120s；超时后终止整个进程树。"
                },
                "shell": {
                    "type": "string",
                    "enum": ["auto", "powershell", "cmd"],
                    "description": "shell 选择：auto=平台默认（Windows 为 powershell），可显式指定 cmd 处理批处理内建语法。"
                },
                "permission": {
                    "type": "string",
                    "enum": ["default", "full"],
                    "description": "执行身份（缺省 default）：default=受限执行（仅可写工作目录内；区外写入被拒）；full=完整权限（应用自身令牌，可读写用户级缓存/工作区外，仍需 justification 并等待你确认；full 不提供管理员权限）。"
                },
                "justification": {
                    "type": "string",
                    "description": "permission=full 时必须填写的一句话理由，将展示在确认卡上供你判断。"
                }
            },
            "required": ["command"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let cmd = v
            .get("command")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if cmd.is_empty() {
            return ToolDisplay::default();
        }
        // 命令内容常显；full 档标注完整权限
        let text = if v.get("permission").and_then(|x| x.as_str()) == Some("full") {
            format!("{cmd}（完整权限）")
        } else {
            cmd.to_string()
        };
        ToolDisplay::info_with_full(text.clone(), text)
    }

    /// 工具级默认超时（执行内核消费点：params.timeout 覆盖此值）。
    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        // ── 参数归一 ──
        let value: serde_json::Value = serde_json::from_str(params)
            .map_err(|e| ToolError::Parse(format!("参数 JSON 解析失败：{e}")))?;

        let command = value
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::Parse("缺少必填参数: command".into()))?
            .to_string();

        let cwd = match value.get("cwd") {
            Some(v) => {
                let raw = v
                    .as_str()
                    .ok_or_else(|| ToolError::Parse("参数类型错误: cwd 应为字符串".into()))?;
                let p = PathBuf::from(raw);
                let resolved = if p.is_absolute() {
                    p
                } else {
                    self.workspace_root.current().join(p)
                };
                if !resolved.is_dir() {
                    return Err(ToolError::Parse(format!(
                        "cwd 不是已存在目录：{}",
                        resolved.display()
                    )));
                }
                resolved
            }
            None => self.workspace_root.current(),
        };

        let shell = match value.get("shell") {
            None => ShellKind::Auto,
            Some(v) => {
                let raw = v
                    .as_str()
                    .ok_or_else(|| ToolError::Parse("参数类型错误: shell 应为字符串".into()))?;
                ShellKind::from_str(raw).ok_or_else(|| {
                    ToolError::Parse(format!("shell 仅支持 auto/powershell/cmd，got: {raw}"))
                })?
            }
        };

        let timeout = match value.get("timeout") {
            None => self.timeout().expect("run_command timeout 必有默认值"),
            Some(v) => {
                let secs = v
                    .as_i64()
                    .ok_or_else(|| ToolError::Parse("参数类型错误: timeout 应为整数".into()))?;
                if secs < 1 {
                    return Err(ToolError::Parse("timeout 必须 ≥ 1 秒".into()));
                }
                Duration::from_secs(secs as u64)
            }
        };

        // ── 执行身份（M4）：permission=full → 完整权限通道。校验（full 必须
        // 带 justification）在 preflight（有审批上下文）；本 execute 纯执行，
        // 非法值在此 Parse 拒绝（与 shell 参数同风格）。justification 仅供
        // 审批卡展示，execute 不消费。
        let full_permission = match value.get("permission") {
            None => false,
            Some(v) => match v.as_str() {
                Some("default") => false,
                Some("full") => true,
                _ => {
                    return Err(ToolError::Parse(format!(
                        "permission 仅支持 default/full，got: {}",
                        v.as_str().unwrap_or("非字符串")
                    )))
                }
            },
        };

        // ── 执行内核 ──
        // M3-B：ctx.abort 注入 RunOptions.cancel（runner 轮询 kill 的取消源；
        // 直调/测试场景 ctx.abort = None → 无取消，行为不变）。
        // M4：workspace_root 随 spec 下传（受限启动的 ACL 放行根 + TEMP/缓存
        // 重定向基准；与工具持有的共享句柄同源——同一次执行内取同一个值）。
        let spec = runner::CommandSpec {
            command,
            cwd,
            shell,
            workspace_root: self.workspace_root.current(),
            full_permission,
        };
        let timeout_secs = timeout.as_secs();
        let out = runner::run_command(
            &spec,
            &RunOptions {
                timeout: Some(timeout),
                output_dir: Some(self.output_dir()),
                log_stem: None, // 默认词干（pid-nanos；Tool::execute 无 session/callId 可达）
                cancel: _ctx.abort.clone(),
            },
        );

        render_output(&spec, &out, timeout_secs, &self.output_dir())
    }
}

// ═══════════════════════════════════════════════════════
// 结果渲染（§2.4 错误映射表 / §2.5 canonical 结构）
// ═══════════════════════════════════════════════════════

/// 截断到字符上限（UTF-8 char 边界安全）。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// 取文本尾部字节（UTF-8 char 边界安全）。
fn tail_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// 单通道尾部切片。
struct ChannelSlice {
    text: String,
    /// 已占用的额度（行 / 字节）
    lines: usize,
    bytes: usize,
    /// 原内容多于 `text`（被行数或字节上限裁掉）
    clipped: bool,
}

/// 取尾部：自末尾向上累加，最多 `max_lines` 行且不超过 `max_bytes` 字节；
/// 单行本身超字节上限时按字符边界取其尾部（对齐既有 lastLinePartial）。
fn tail_slice(text: &str, max_lines: usize, max_bytes: usize) -> ChannelSlice {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return ChannelSlice {
            text: String::new(),
            lines: 0,
            bytes: 0,
            clipped: false,
        };
    }
    let all: Vec<&str> = trimmed.lines().collect();
    let mut taken: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    for line in all.iter().rev() {
        if taken.len() >= max_lines {
            break;
        }
        let sep = if taken.is_empty() { 0 } else { 1 };
        if bytes + line.len() + sep > max_bytes {
            break;
        }
        taken.push(line);
        bytes += line.len() + sep;
    }
    if taken.is_empty() {
        // 行额度已尽，或单行本身超字节上限
        if max_lines == 0 {
            return ChannelSlice {
                text: String::new(),
                lines: 0,
                bytes: 0,
                clipped: true,
            };
        }
        let s = tail_bytes(all[all.len() - 1], max_bytes);
        return ChannelSlice {
            bytes: s.len(),
            lines: 1,
            text: s,
            clipped: true,
        };
    }
    let clipped = taken.len() < all.len();
    taken.reverse();
    ChannelSlice {
        text: taken.join("\n"),
        lines: taken.len(),
        bytes,
        clipped,
    }
}

/// 双通道渲染结果（共用一份额度）。
struct StreamRender {
    stderr: ChannelSlice,
    stdout: ChannelSlice,
}

impl StreamRender {
    fn stderr_has(&self) -> bool {
        !self.stderr.text.is_empty()
    }
    fn stdout_has(&self) -> bool {
        !self.stdout.text.is_empty()
    }
}

/// 两通道渲染：共用一份额度（2000 行 / 50KB），stderr 优先占额。
fn render_streams(stdout: &str, stderr: &str) -> StreamRender {
    let stderr = tail_slice(stderr, MAX_TAIL_LINES, MAX_TAIL_BYTES);
    let stdout = tail_slice(
        stdout,
        MAX_TAIL_LINES.saturating_sub(stderr.lines),
        MAX_TAIL_BYTES.saturating_sub(stderr.bytes),
    );
    StreamRender { stderr, stdout }
}

/// 正文拼装：单通道不加标记（保持紧凑形态），双通道带 `[stderr]` / `[stdout]`
/// 标记。顺序统一为 **stderr 在前**（异常优先：失败先看错误现场，成功先看警告），
/// 与真实写入次序无关——两条管道由两个读线程各读，交织信息在采集层就不再可得。
fn compose_body(header: &str, r: &StreamRender) -> String {
    let head = |text: &str| {
        if header.is_empty() {
            text.to_string()
        } else {
            format!("{header}：{text}")
        }
    };
    match (r.stderr_has(), r.stdout_has()) {
        (false, false) => header.to_string(),
        (true, false) => head(&r.stderr.text),
        (false, true) => head(&r.stdout.text),
        (true, true) => {
            let body = format!("[stderr]\n{}\n[stdout]\n{}", r.stderr.text, r.stdout.text);
            if header.is_empty() {
                body
            } else {
                format!("{header}\n{body}")
            }
        }
    }
}

/// 报告面路径：剥 Windows verbatim（`\\?\`）前缀，消费方（模型 / 卡片）用普通形式。
/// 与沙箱侧 `spawn_win32::child_path` 是同一条规则的两个面（那边是交给子进程）。
fn plain_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    match s.strip_prefix("\\\\?\\UNC\\") {
        Some(rest) => PathBuf::from(format!("\\\\{rest}")),
        None => match s.strip_prefix("\\\\?\\") {
            Some(rest) => PathBuf::from(rest),
            None => path.to_path_buf(),
        },
    }
}

/// 完整输出提示：`[输出已截断，完整输出：<path>（stderr）（可用 read_file 读取）]`。
fn spill_notice(entries: &[(&str, PathBuf)]) -> String {
    let list = entries
        .iter()
        .map(|(chan, p)| format!("{}（{chan}）", plain_path(p).display()))
        .collect::<Vec<_>>()
        .join("、");
    format!("\n\n[输出已截断，完整输出：{list}（可用 read_file 读取）]")
}

/// 确保被截断通道的完整输出可读：runner 已落盘则直接用其路径；
/// 否则（未达 runner 阈值、全文仍在内存）写入输出目录。
fn ensure_full_file(
    out_dir: &Path,
    chan: &str,
    content: &str,
    spilled: Option<&PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = spilled {
        return Some(p.clone());
    }
    if content.is_empty() {
        return None;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = out_dir.join(format!("run-{}-{nanos}.{chan}.log", std::process::id()));
    std::fs::create_dir_all(out_dir).ok()?;
    std::fs::write(&path, content).ok()?;
    Some(path)
}

/// 截断提示（无截断返回空串；顺带落实「截断必落盘」）。
fn truncation_notice(out_dir: &Path, out: &RunOutput, r: &StreamRender) -> String {
    let mut entries: Vec<(&str, PathBuf)> = Vec::new();
    if r.stderr.clipped {
        if let Some(p) = ensure_full_file(
            out_dir,
            "stderr",
            &out.stderr_tail,
            out.full_stderr_path.as_ref(),
        ) {
            entries.push(("stderr", p));
        }
    }
    if r.stdout.clipped {
        if let Some(p) = ensure_full_file(
            out_dir,
            "stdout",
            &out.stdout_tail,
            out.full_stdout_path.as_ref(),
        ) {
            entries.push(("stdout", p));
        }
    }
    if entries.is_empty() {
        String::new()
    } else {
        spill_notice(&entries)
    }
}

fn render_output(
    spec: &runner::CommandSpec,
    out: &RunOutput,
    timeout_secs: u64,
    out_dir: &Path,
) -> Result<ToolOutput, ToolError> {
    let canonical = serde_json::json!({
        "ok": out.ok(),
        "outcome": match out.outcome {
            Outcome::Completed => "completed",
            Outcome::TimedOut => "timed_out",
            Outcome::Killed => "killed",
            Outcome::SpawnError => "spawn_error",
        },
        "exit_code": out.exit_code,
        "command": truncate_chars(&spec.command, MAX_COMMAND_CHARS),
        "stdout_tail": truncate_chars(&out.stdout_tail, MAX_CANONICAL_CHARS),
        "stderr_tail": truncate_chars(&out.stderr_tail, MAX_CANONICAL_CHARS),
        "truncated": out.truncated,
        "output_bytes": out.total_bytes,
        "full_stdout_path": out.full_stdout_path.as_ref().map(|p| plain_path(p).to_string_lossy().into_owned()),
        "full_stderr_path": out.full_stderr_path.as_ref().map(|p| plain_path(p).to_string_lossy().into_owned()),
        "elapsed_ms": out.elapsed_ms,
    });

    // 双通道共用一份尾部额度（stderr 优先占额）；截断时把完整输出落盘并给路径。
    let r = render_streams(&out.stdout_tail, &out.stderr_tail);
    let notice = truncation_notice(out_dir, out, &r);

    match out.outcome {
        Outcome::Completed if out.exit_code == Some(0) => {
            // 成功：产物（stdout）+ 警告（stderr，非空时才附）
            let mut summary = compose_body("", &r);
            if summary.is_empty() {
                summary = "(无输出)".into();
            }
            summary.push_str(&notice);
            Ok(ToolOutput::json(canonical, summary))
        }
        Outcome::Completed => {
            // 非零退出码：异常优先（stderr 段在前）
            let header = format!("命令执行失败（退出码 {}）", out.exit_code.unwrap_or(-1));
            let mut msg = compose_body(&header, &r);
            msg.push_str(&notice);
            Err(ToolError::CommandFailed(msg))
        }
        Outcome::TimedOut => {
            let header = format!("命令超过 {timeout_secs}s 未完成，已终止进程树");
            let mut msg = compose_body(&header, &r);
            msg.push_str(&notice);
            Err(ToolError::CommandFailed(msg))
        }
        Outcome::Killed => Err(ToolError::CommandFailed("命令已被终止".to_string())),
        Outcome::SpawnError => Err(ToolError::CommandFailed(
            out.spawn_error
                .clone()
                .unwrap_or_else(|| "命令无法启动".into()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRegistry;

    #[test]
    fn display_command_and_full_permission_annotation() {
        let tool = RunCommandTool::new(std::env::temp_dir());
        let ctx = ToolContext::new();

        // 命令内容常显
        let d = tool.display(&ctx, r#"{"command":"Get-Process"}"#);
        assert_eq!(d.text, "Get-Process");
        assert!(d.full.is_none(), "未截断不必附带原文");

        // 超长命令：展示截断，但带完整原文（前端悬停显示全文）
        let long = "x".repeat(300);
        let d = tool.display(&ctx, &format!(r#"{{"command":"{long}"}}"#));
        assert!(d.text.ends_with('…'), "展示应截断: {}", d.text);
        assert_eq!(d.full.as_deref(), Some(long.as_str()));

        // permission=full → 标注完整权限
        let d = tool.display(&ctx, r#"{"command":"npm install","permission":"full"}"#);
        assert!(d.text.starts_with("npm install"));
        assert!(d.text.ends_with("（完整权限）"), "got {}", d.text);

        // 空命令 → 空切片
        assert!(tool.display(&ctx, r#"{"command":"  "}"#).is_empty());
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-run-command-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tool(ws: &Path) -> RunCommandTool {
        RunCommandTool::new(ws.to_path_buf())
    }

    #[test]
    fn name_and_schema_valid() {
        let t = tool(Path::new("C:/ws"));
        assert_eq!(t.name(), "run_command");
        // schema 顶层 object + required command
        let schema = t.parameters();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "command");
        // 注册校验应通过（含 schema::validate_tool_schema）
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(t)).unwrap();
        assert!(reg.get("run_command").is_some());
    }

    #[test]
    fn timeout_default_is_120s() {
        let t = tool(Path::new("C:/ws"));
        assert_eq!(t.timeout(), Some(Duration::from_secs(120)));
    }

    #[test]
    fn execute_rejects_missing_command() {
        let ws = unique_temp_dir("missing-cmd");
        let t = tool(&ws);
        let err = t
            .execute(&ToolContext::new(), r#"{"cwd":"x"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("command"), "got: {err}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn execute_rejects_bad_shell_and_timeout() {
        let ws = unique_temp_dir("bad-params");
        let t = tool(&ws);
        let e1 = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"echo hi","shell":"bash"}"#,
            )
            .unwrap_err();
        assert!(matches!(e1, ToolError::Parse(_)), "got: {e1}");
        let e2 = t
            .execute(&ToolContext::new(), r#"{"command":"echo hi","timeout":0}"#)
            .unwrap_err();
        assert!(matches!(e2, ToolError::Parse(_)), "got: {e2}");
        let e3 = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"echo hi","cwd":"C:/not-exist-dir-xyz"}"#,
            )
            .unwrap_err();
        assert!(matches!(e3, ToolError::Parse(_)), "got: {e3}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn execute_echo_success() {
        let ws = unique_temp_dir("echo-ok");
        let t = tool(&ws);
        let out = t
            .execute(&ToolContext::new(), r#"{"command":"echo m3a-echo-ok"}"#)
            .expect("echo 应成功");
        assert!(
            out.summary.contains("m3a-echo-ok"),
            "summary: {}",
            out.summary
        );
        assert_eq!(out.canonical["ok"], serde_json::json!(true));
        assert_eq!(out.canonical["exit_code"], serde_json::json!(0));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn execute_nonzero_exit_maps_to_command_failed() {
        let ws = unique_temp_dir("exit-fail");
        let t = tool(&ws);
        let err = t
            .execute(&ToolContext::new(), r#"{"command":"exit 7"}"#)
            .expect_err("非零退出应失败");
        match err {
            ToolError::CommandFailed(msg) => {
                assert!(msg.contains("退出码 7"), "got: {msg}");
            }
            other => panic!("应 CommandFailed，got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn failure_message_carries_both_streams_stderr_first() {
        let ws = unique_temp_dir("both-streams");
        let t = tool(&ws);
        let err = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"Write-Output 'out-line'; [Console]::Error.WriteLine('err-line'); exit 3"}"#,
            )
            .expect_err("非零退出应失败");
        match err {
            ToolError::CommandFailed(msg) => {
                assert!(msg.contains("退出码 3"), "got: {msg}");
                assert!(
                    msg.contains("[stderr]") && msg.contains("[stdout]"),
                    "双通道应有分段标记: {msg}"
                );
                let err_at = msg.find("[stderr]").unwrap();
                let out_at = msg.find("[stdout]").unwrap();
                assert!(err_at < out_at, "失败时 stderr 段应在前: {msg}");
                assert!(
                    msg.contains("err-line") && msg.contains("out-line"),
                    "got: {msg}"
                );
            }
            other => panic!("应 CommandFailed，got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn success_message_appends_stderr_section() {
        let ws = unique_temp_dir("succ-stderr");
        let t = tool(&ws);
        let out = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"Write-Output 'ok-line'; [Console]::Error.WriteLine('warn-line')"}"#,
            )
            .expect("退出码 0 应成功");
        assert!(
            out.summary.contains("[stdout]") && out.summary.contains("[stderr]"),
            "成功态也应带上 stderr 段: {}",
            out.summary
        );
        // 顺序统一：stderr 段恒在前（与失败态一致）
        assert!(
            out.summary.find("[stderr]") < out.summary.find("[stdout]"),
            "两段顺序应统一为 stderr 在前: {}",
            out.summary
        );
        assert!(
            out.summary.contains("ok-line") && out.summary.contains("warn-line"),
            "summary: {}",
            out.summary
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn tail_capped_by_lines_and_full_output_written() {
        let ws = unique_temp_dir("tail-lines");
        let t = tool(&ws);
        let err = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"1..3000 | ForEach-Object { [Console]::Error.WriteLine('e' + $_) }; exit 1"}"#,
            )
            .expect_err("应失败");
        match err {
            ToolError::CommandFailed(msg) => {
                assert!(msg.contains("e3000"), "尾部应保留最后一行");
                assert!(!msg.contains("e1000"), "被裁的行不应出现");
                assert!(msg.contains("read_file"), "截断应给落盘指引: {msg}");
                assert!(
                    !msg.contains("\\\\?\\"),
                    "提示里的路径应为普通形式（剥 verbatim 前缀）"
                );
                let path = msg
                    .split("完整输出：")
                    .nth(1)
                    .and_then(|s| s.split("（stderr）").next())
                    .expect("应含完整输出路径");
                let content = std::fs::read_to_string(path).expect("完整输出应落盘");
                assert_eq!(content.lines().count(), 3000, "落盘应为全文");
            }
            other => panic!("应 CommandFailed，got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn shared_budget_clips_second_stream_and_spills_only_it() {
        let ws = unique_temp_dir("shared-budget");
        let t = tool(&ws);
        // stderr 先占 1500 行额度，stdout 只剩 500 行 → 仅 stdout 被裁
        let err = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"1..1500 | ForEach-Object { [Console]::Error.WriteLine('e' + $_) }; 1..1500 | ForEach-Object { [Console]::Out.WriteLine('o' + $_) }; exit 1"}"#,
            )
            .expect_err("应失败");
        match err {
            ToolError::CommandFailed(msg) => {
                assert!(msg.contains("e1500"), "stderr 应完整（优先占额）");
                assert!(msg.contains("o1500"), "stdout 应保留尾部");
                assert!(!msg.contains("o1000"), "stdout 被裁部分不应出现");
                assert!(msg.contains("（stdout）"), "仅被裁通道需落盘: {msg}");
                assert!(
                    !msg.contains("（stderr）"),
                    "未截断通道不落盘（内容已在正文）"
                );
                assert!(
                    !msg.contains("\\\\?\\"),
                    "提示里的路径应为普通形式（剥 verbatim 前缀）"
                );
                let path = msg
                    .split("完整输出：")
                    .nth(1)
                    .and_then(|s| s.split("（stdout）").next())
                    .expect("应含 stdout 落盘路径");
                let content = std::fs::read_to_string(path).expect("完整输出应落盘");
                assert_eq!(content.lines().count(), 1500, "落盘应为 stdout 全文");
            }
            other => panic!("应 CommandFailed，got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn execute_timeout_maps_to_command_failed() {
        let ws = unique_temp_dir("timeout");
        let t = tool(&ws);
        let start = std::time::Instant::now();
        let err = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"Start-Sleep -Seconds 3","timeout":1}"#,
            )
            .expect_err("超时应失败");
        assert!(start.elapsed() < Duration::from_secs(3), "应提前返回");
        match err {
            ToolError::CommandFailed(msg) => {
                assert!(msg.contains("终止进程树"), "got: {msg}");
            }
            other => panic!("应 CommandFailed，got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn execute_cwd_relative_resolves_to_workspace() {
        let ws = unique_temp_dir("cwd-rel");
        std::fs::write(ws.join("probe.txt"), "m3a-cwd-ok").unwrap();
        let t = tool(&ws);
        let out = t
            .execute(
                &ToolContext::new(),
                r#"{"command":"Get-Content probe.txt","cwd":"."}"#,
            )
            .expect("相对 cwd 应解析到 workspace");
        assert!(
            out.summary.contains("m3a-cwd-ok"),
            "summary: {}",
            out.summary
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn oversized_output_spills_under_workspace_and_summary_guides_read_file() {
        let ws = unique_temp_dir("spill");
        let t = tool(&ws);
        let out = t
            .execute(
                &ToolContext::new(),
                &format!(r#"{{"command":"Write-Output ('x' * 300000)"}}"#),
            )
            .expect("成功输出但截断");
        assert!(out.canonical["truncated"] == serde_json::json!(true));
        assert!(
            out.summary.contains("read_file"),
            "应含 read_file 指引: {}",
            out.summary
        );
        let full = out.canonical["full_stdout_path"]
            .as_str()
            .expect("应有全文路径");
        assert!(
            full.contains(".prts-exec-output"),
            "全文应在 workspace/.prts-exec-output: {full}"
        );
        assert!(
            std::path::Path::new(full).exists(),
            "全文文件应存在: {full}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
