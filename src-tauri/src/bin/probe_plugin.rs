//! 测试探针插件：验证 process 通道（分帧、握手、工具调用）。
//!
//! 这个二进制**故意不依赖主 crate**：分帧按协议手写一遍，宿主侧的实现错了
//! 它不会跟着一起错。只用于 `cargo test`，不随包发布。
//!
//! 协议（规格 §5）：帧 = `Content-Length: <bytes>\r\n\r\n<UTF-8 JSON>`。

use std::io::{BufRead, BufReader, Write};
use std::process::ExitCode;

use serde_json::{json, Value};

/// 单帧上限，与宿主侧一致。
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

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
            "initialize" => reply(
                &mut output,
                &id,
                json!({
                    "protocol": 1,
                    "capabilities": [],
                    "tools": [{
                        "name": "echo",
                        "description": "把参数原样回给宿主（探针用）",
                        "parameters": {
                            "type": "object",
                            "properties": { "text": { "type": "string" } },
                            "required": []
                        }
                    }]
                }),
            ),
            "tool.call" => {
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
                    error(&mut output, &id, -32601, "探针只有 echo 一个工具");
                }
            }
            "shutdown" => {
                reply(&mut output, &id, json!({ "ok": true }));
                return ExitCode::SUCCESS;
            }
            other => error(&mut output, &id, -32601, &format!("未知方法：{other}")),
        }
    }
}
