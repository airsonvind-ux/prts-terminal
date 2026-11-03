//! PRTS 插件示例（Rust）。
//!
//! 宿主与插件用一条 stdio 管道说话：每一帧是
//! `Content-Length: <字节数>\r\n\r\n<UTF-8 JSON>`。
//! 这里只做三件事：回 initialize、应答一个工具、收到 shutdown 后退出。
//!
//! 改这里就能做自己的插件：把 `tools()` 换成你的工具面，把 `handle_tool`
//! 里的分支换成你的实现。协议细节见随包手册 05-protocol。

use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};

const PROTOCOL: u32 = 1;

fn main() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    loop {
        let Some(message) = read_frame(&mut input) else {
            return std::process::ExitCode::SUCCESS;
        };

        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();

        // shutdown 可能带 id（宿主把它当请求），也可能只是通知；两种都收场。
        if method == Some("shutdown") {
            if let Some(id) = &id {
                reply(&mut output, id, Value::Null);
            }
            return std::process::ExitCode::SUCCESS;
        }

        // 其余通知没有 id，不需要回。
        let Some(id) = id else { continue };

        match method {
            Some("initialize") => {
                let result = json!({
                    "protocol": PROTOCOL,
                    "capabilities": [],
                    "tools": tools(),
                });
                reply(&mut output, &id, result);
            }
            Some("tool.call") => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("args").cloned().unwrap_or(Value::Null);
                match handle_tool(name, &args) {
                    Some(text) => reply(&mut output, &id, Value::String(text)),
                    // 工具名不认识按参数错处理（-32602），-32601 只留给方法不认识。
                    None => reply_error(&mut output, &id, -32602, &format!("没有这个工具：{name}")),
                }
            }
            Some(other) => {
                reply_error(&mut output, &id, -32601, &format!("未知方法：{other}"));
            }
            None => {}
        }
    }
}

/// 工具面：这一份就是宿主看到的全部。
fn tools() -> Value {
    json!([{
        "name": "hello",
        "action": "和人打招呼",
        "description": "向一个人打招呼（示例工具）",
        "parameters": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "要打招呼的人，默认「世界」" }
            },
            "required": []
        }
    }])
}

fn handle_tool(name: &str, args: &Value) -> Option<String> {
    match name {
        "hello" => {
            let who = args
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or("世界");
            Some(format!("你好，{who}！"))
        }
        _ => None,
    }
}

fn reply(out: &mut impl Write, id: &Value, result: Value) {
    let message = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    let _ = write_frame(out, &message);
}

fn reply_error(out: &mut impl Write, id: &Value, code: i64, message: &str) {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    let _ = write_frame(out, &frame);
}

/// 读一帧；文件结束返回 `None`。
fn read_frame(input: &mut impl BufRead) -> Option<Value> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().ok();
            }
        }
    }

    let mut body = vec![0u8; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_frame(out: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()
}
