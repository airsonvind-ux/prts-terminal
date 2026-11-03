//! 测试探针插件：验证 process 通道（分帧、握手、工具调用、故障路径）。
//!
//! 这个二进制**故意不依赖主 crate**：分帧按协议手写一遍，宿主侧的实现错了
//! 它不会跟着一起错。只用于 `cargo test`，不随包发布。
//!
//! 协议（规格 §5）：帧 = `Content-Length: <bytes>\r\n\r\n<UTF-8 JSON>`。
//!
//! 故障模式按**自身 exe 文件名**选择（测试把二进制复制成
//! `probe_silent.exe` 之类的名字再起进程）：
//! - `silent`：收到 `initialize` 不回话（握手超时）
//! - `mismatch`：`initialize` 回 `protocol: 2`（协议版本不匹配）
//! - `crash`：收到第一次 `tool.call` 直接退出（进程崩溃）
//! - `hang`：收到 `tool.call` 不回话、也不理 `cancel`（调用超时 → 被杀）

use std::io::{BufRead, BufReader, Write};
use std::process::ExitCode;

use serde_json::{json, Value};

/// 单帧上限，与宿主侧一致。
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// 探针的故障模式（正常时只实现 `echo`）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Silent,
    Mismatch,
    Crash,
    Hang,
}

fn mode() -> Mode {
    let stem = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        })
        .unwrap_or_default();
    if stem.contains("silent") {
        Mode::Silent
    } else if stem.contains("mismatch") {
        Mode::Mismatch
    } else if stem.contains("crash") {
        Mode::Crash
    } else if stem.contains("hang") {
        Mode::Hang
    } else {
        Mode::Normal
    }
}

fn read_frame(input: &mut impl BufRead) -> Option<Value> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line).ok()?;
        if read == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let (name, value) = trimmed.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok();
        }
    }

    let length = length?;
    if length > MAX_FRAME_BYTES {
        return None;
    }
    let mut body = vec![0_u8; length];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_frame(output: &mut impl Write, frame: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(frame)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}

fn reply(output: &mut impl Write, id: &Value, result: Value) {
    let _ = write_frame(output, &json!({ "id": id, "result": result }));
}

fn error(output: &mut impl Write, id: &Value, code: i64, message: &str) {
    let _ = write_frame(
        output,
        &json!({ "id": id, "error": { "code": code, "message": message } }),
    );
}

fn main() -> ExitCode {
    let mode = mode();
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    loop {
        let Some(frame) = read_frame(&mut input) else {
            return ExitCode::SUCCESS;
        };
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            // 应答或通知：探针不发起请求，忽略。
            continue;
        };
        let id = frame.get("id").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => match mode {
                // 握手超时：什么都不回，让宿主等到自己的 deadline。
                Mode::Silent => continue,
                // 协议版本不匹配：报一个宿主不支持的版本。
                Mode::Mismatch => reply(
                    &mut output,
                    &id,
                    json!({ "protocol": 2, "capabilities": [], "tools": [] }),
                ),
                _ => reply(
                    &mut output,
                    &id,
                    json!({
                        "protocol": 1,
                        "capabilities": [],
                        "tools": [{
                            "name": "echo",
                            "action": "回显参数",
                            "description": "把参数原样回给宿主（探针用）",
                            "parameters": {
                                "type": "object",
                                "properties": { "text": { "type": "string" } },
                                "required": []
                            }
                        }]
                    }),
                ),
            },
            "tool.call" => {
                match mode {
                    // 进程崩溃：不回话，直接退出（stdout 断开 → 宿主判故障）。
                    Mode::Crash => return ExitCode::from(3),
                    // 调用超时且不理 cancel：宿主等满宽限期后会杀进程。
                    Mode::Hang => continue,
                    _ => {}
                }
                let name = frame
                    .get("params")
                    .and_then(|params| params.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = frame
                    .get("params")
                    .and_then(|params| params.get("args"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name == "echo" {
                    reply(&mut output, &id, json!({ "echo": args }));
                } else {
                    // M4 起：工具名是参数，未知工具回「参数错误」（-32602）；
                    // -32601 只留给「未知方法」。手册与校验器都按这一条写。
                    error(&mut output, &id, -32602, "探针只有 echo 一个工具");
                }
            }
            "shutdown" => {
                reply(&mut output, &id, json!({ "ok": true }));
                return ExitCode::SUCCESS;
            }
            // 通知没有 id，也不该有应答：回一个空 id 的错误帧会让宿主以为是
            // 一条坏应答（探针只验证通道，不模拟取消语义）。
            "cancel" => {}
            other => error(&mut output, &id, -32601, &format!("未知方法：{other}")),
        }
    }
}
