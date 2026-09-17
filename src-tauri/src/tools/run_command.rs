//! run_command —— 通用命令执行工具（M3-A §1）
//!
//! 职责分层（与方案 §1.3 一致）：
//! - 本工具 = **纯执行**（参数归一 → exec::runner 内核 → 结果渲染）。
//!   审批/模式/危险度分类**不在** execute 内——由 orchestrator 预检通道
//!   （preflight_tool_calls）在生产路径消费；本 execute 的裸调用（registry
//!   直调 / 测试）= 无审批直接执行（语义见方案 §1.3）。
//! - 超时消费点：`Tool::timeout()` 提供工具级默认（120s），调用级参数
//!   timeout 覆盖之（params.timeout > 工具默认；registry 不做全域包装）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::runner::{self, Outcome, RunOptions, RunOutput, ShellKind};

use super::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 返回给 LLM/前端的 summary 尾部上限（方案 §3 数值定稿）。
const MAX_SUMMARY_CHARS: usize = 4000;
/// canonical 中命令回显截断。
const MAX_COMMAND_CHARS: usize = 512;

/// 工具级默认超时（方案 §3：Tool::timeout() = 执行类工具默认）。
const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub struct RunCommandTool {
    /// 工作空间根：cwd 默认值 + 输出落盘目录基准（workspace_root/.prts-exec-output）。
    workspace_root: PathBuf,
}

impl RunCommandTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    fn output_dir(&self) -> PathBuf {
        self.workspace_root.join(".prts-exec-output")
    }
}

impl Tool for RunCommandTool {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "执行 shell 命令并返回输出：非零退出码按失败返回（含退出码与输出尾部），完整输出落盘工作区可 read_file 读取。命令以受限身份运行，仅可写工作目录内，区外写入被系统拒绝；确需访问区外时声明 permission=full 请求确认（完整语义见参数说明）。审批：询问模式（默认）每次执行前弹确认卡；自动化模式仅低危只读命令自动放行，其余（含所有 full）仍弹确认卡；不可逆破坏类命令任何模式任何档位直接拒绝。仅限 Windows（powershell/cmd）；写文件请优先用 write_file/edit。"
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
        let cmd = v.get("command").and_then(|x| x.as_str()).unwrap_or("").trim();
        if cmd.is_empty() {
            return ToolDisplay::default();
        }
        // 命令内容常显；full 档标注完整权限
        let text = if v.get("permission").and_then(|x| x.as_str()) == Some("full") {
            format!("{cmd}（完整权限）")
        } else {
            cmd.to_string()
        };
        ToolDisplay::info(text)
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
                let raw = v.as_str().ok_or_else(|| ToolError::Parse("参数类型错误: cwd 应为字符串".into()))?;
                let p = PathBuf::from(raw);
                let resolved = if p.is_absolute() {
                    p
                } else {
                    self.workspace_root.join(p)
                };
                if !resolved.is_dir() {
                    return Err(ToolError::Parse(format!(
                        "cwd 不是已存在目录：{}",
                        resolved.display()
                    )));
                }
                resolved
            }
            None => self.workspace_root.clone(),
        };

        let shell = match value.get("shell") {
            None => ShellKind::Auto,
            Some(v) => {
                let raw = v.as_str().ok_or_else(|| ToolError::Parse("参数类型错误: shell 应为字符串".into()))?;
                ShellKind::from_str(raw)
                    .ok_or_else(|| ToolError::Parse(format!("shell 仅支持 auto/powershell/cmd，got: {raw}")))?
            }
        };

        let timeout = match value.get("timeout") {
            None => self.timeout().expect("run_command timeout 必有默认值"),
            Some(v) => {
                let secs = v.as_i64().ok_or_else(|| ToolError::Parse("参数类型错误: timeout 应为整数".into()))?;
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
        // 重定向基准；与工具构造时注入的 policy.workspace_root 同源）。
        let spec = runner::CommandSpec {
            command,
            cwd,
            shell,
            workspace_root: self.workspace_root.clone(),
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

        render_output(&spec, &out, timeout_secs)
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

/// 截断提示附注（truncated 时）。
fn spill_notice(path: &Path) -> String {
    format!(
        "\n\n[输出已截断，完整输出：{}（可用 read_file 读取）]",
        path.display()
    )
}

fn render_output(
    spec: &runner::CommandSpec,
    out: &RunOutput,
    timeout_secs: u64,
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
        "stdout_tail": truncate_chars(&out.stdout_tail, MAX_SUMMARY_CHARS),
        "stderr_tail": truncate_chars(&out.stderr_tail, MAX_SUMMARY_CHARS),
        "truncated": out.truncated,
        "output_bytes": out.total_bytes,
        "full_stdout_path": out.full_stdout_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "full_stderr_path": out.full_stderr_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "elapsed_ms": out.elapsed_ms,
    });

    match out.outcome {
        Outcome::Completed if out.exit_code == Some(0) => {
            // 成功：summary = stdout 尾部（截断附 read_file 指引）
            let stdout = out.stdout_tail.trim_end().to_string();
            let mut summary = truncate_chars(&stdout, MAX_SUMMARY_CHARS);
            if let Some(full) = out.full_stdout_path.as_ref() {
                if stdout.chars().count() > MAX_SUMMARY_CHARS || out.truncated {
                    summary.push_str(&spill_notice(full));
                }
            }
            if summary.trim().is_empty() {
                summary = "(无输出)".into();
            }
            Ok(ToolOutput::json(canonical, summary))
        }
        Outcome::Completed => {
            // 非零退出码：错误文案 = 退出码 + stderr/stdout 摘要
            let detail = if !out.stderr_tail.trim().is_empty() {
                &out.stderr_tail
            } else {
                &out.stdout_tail
            };
            let code = out.exit_code.unwrap_or(-1);
            let detail = truncate_chars(detail.trim_end(), MAX_SUMMARY_CHARS);
            let mut msg = format!("命令执行失败（退出码 {code}）");
            if !detail.is_empty() {
                msg.push_str(&format!("：{detail}"));
            }
            Err(ToolError::CommandFailed(msg))
        }
        Outcome::TimedOut => {
            let mut msg = format!("命令超过 {timeout_secs}s 未完成，已终止进程树");
            let detail = if !out.stderr_tail.trim().is_empty() {
                &out.stderr_tail
            } else {
                &out.stdout_tail
            };
            let detail = truncate_chars(detail.trim_end(), 2000);
            if !detail.is_empty() {
                msg.push_str(&format!("。已产生输出：{detail}"));
            }
            Err(ToolError::CommandFailed(msg))
        }
        Outcome::Killed => Err(ToolError::CommandFailed(
            "命令已被终止".to_string(),
        )),
        Outcome::SpawnError => Err(ToolError::CommandFailed(
            out.spawn_error.clone().unwrap_or_else(|| "命令无法启动".into()),
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

        // permission=full → 标注完整权限
        let d = tool.display(
            &ctx,
            r#"{"command":"npm install","permission":"full"}"#,
        );
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
        let err = t.execute(&ToolContext::new(), r#"{"cwd":"x"}"#).unwrap_err();
        assert!(err.to_string().contains("command"), "got: {err}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn execute_rejects_bad_shell_and_timeout() {
        let ws = unique_temp_dir("bad-params");
        let t = tool(&ws);
        let e1 = t
            .execute(&ToolContext::new(), r#"{"command":"echo hi","shell":"bash"}"#)
            .unwrap_err();
        assert!(matches!(e1, ToolError::Parse(_)), "got: {e1}");
        let e2 = t
            .execute(&ToolContext::new(), r#"{"command":"echo hi","timeout":0}"#)
            .unwrap_err();
        assert!(matches!(e2, ToolError::Parse(_)), "got: {e2}");
        let e3 = t
            .execute(&ToolContext::new(), r#"{"command":"echo hi","cwd":"C:/not-exist-dir-xyz"}"#)
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
        assert!(out.summary.contains("m3a-echo-ok"), "summary: {}", out.summary);
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
            .execute(&ToolContext::new(), r#"{"command":"Get-Content probe.txt","cwd":"."}"#)
            .expect("相对 cwd 应解析到 workspace");
        assert!(out.summary.contains("m3a-cwd-ok"), "summary: {}", out.summary);
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
        assert!(out.summary.contains("read_file"), "应含 read_file 指引: {}", out.summary);
        let full = out.canonical["full_stdout_path"].as_str().expect("应有全文路径");
        assert!(
            full.contains(".prts-exec-output"),
            "全文应在 workspace/.prts-exec-output: {full}"
        );
        assert!(std::path::Path::new(full).exists(), "全文文件应存在: {full}");
        let _ = std::fs::remove_dir_all(&ws);
    }
}
