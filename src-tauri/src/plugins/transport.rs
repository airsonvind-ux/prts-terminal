//! 插件协议的分帧（M2b）。
//!
//! 帧格式：`Content-Length: <字节数>\r\n\r\n<UTF-8 JSON>`，单帧上限 8 MiB，
//! 超限即 fault（规格 §5）。stdout 上只允许出现帧：出现非帧内容由
//! [`crate::plugins::process`] 判死；日志只能走 `log` 方法，stderr 归宿主日志。
//!
//! 这里刻意只用 `std::io` 与 `serde_json`：探针插件不链接主 lib，自己手写同样的
//! 二十行分帧（规格 §6），两边对不上时测试会先炸。

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// 单帧上限（规格 §5：8 MiB）。
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// 单行帧头上限：正常只有一行 `Content-Length`，给到 8 KiB 已经很宽松。
const MAX_HEADER_LINE: usize = 8 * 1024;

/// 分帧错误。`Display` 文案直接进 `PluginState::Faulted` 与 stderr。
#[derive(Debug)]
pub enum FrameError {
    /// 读写管道出错（含对端提前关闭）。
    Io(io::Error),
    /// 声明的长度超过上限。
    TooLarge { declared: usize },
    /// 帧头不合法（缺 `Content-Length`、不是 `名字: 值`、行长超限）。
    Header(String),
    /// 帧体不是合法 JSON。
    Json(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(error) => write!(f, "管道读写失败：{error}"),
            FrameError::TooLarge { declared } => write!(
                f,
                "帧长度 {declared} 字节超过上限 {MAX_FRAME_BYTES}（协议错误）"
            ),
            FrameError::Header(detail) => write!(f, "帧头不合法：{detail}"),
            FrameError::Json(detail) => write!(f, "帧体不是合法 JSON：{detail}"),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        FrameError::Io(error)
    }
}

/// 读端：从任意 `BufRead`（子进程 stdout）逐个取出 JSON 帧。
pub struct FrameReader<R> {
    inner: R,
}

impl<R: BufRead> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    /// 读一帧。`Ok(None)` 表示在帧边界上遇到 EOF（对端正常关闭）。
    ///
    /// 帧头读到一半就断开算错误——那说明对端没按协议收场。
    pub fn read_frame(&mut self) -> Result<Option<Value>, FrameError> {
        let mut content_length: Option<usize> = None;
        let mut line = Vec::new();

        loop {
            line.clear();
            let read = self.inner.read_until(b'\n', &mut line)?;
            if read == 0 {
                return match content_length {
                    None => Ok(None),
                    Some(_) => Err(FrameError::Header(
                        "帧头读到一半就断开了（对端没有按协议收场）".to_string(),
                    )),
                };
            }
            if line.len() > MAX_HEADER_LINE {
                return Err(FrameError::Header(format!(
                    "帧头单行超过 {MAX_HEADER_LINE} 字节"
                )));
            }

            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\r', '\n']);
            if text.is_empty() {
                break;
            }

            let Some((name, value)) = text.split_once(':') else {
                return Err(FrameError::Header(format!(
                    "期望 `名字: 值` 形式，实际是：{text}"
                )));
            };
            if name.trim().eq_ignore_ascii_case("content-length") {
                let trimmed = value.trim();
                let bytes: usize = trimmed.parse().map_err(|_| {
                    FrameError::Header(format!("Content-Length 不是整数：{trimmed}"))
                })?;
                content_length = Some(bytes);
            }
            // 其它头一律忽略：协议只定义了一个头，向前兼容靠忽略未知项。
        }

        let Some(length) = content_length else {
            return Err(FrameError::Header(
                "帧头缺少 Content-Length（协议错误）".to_string(),
            ));
        };
        if length > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge { declared: length });
        }

        let mut body = vec![0u8; length];
        self.inner.read_exact(&mut body)?;
        let value = serde_json::from_slice(&body)
            .map_err(|error| FrameError::Json(error.to_string()))?;
        Ok(Some(value))
    }
}

/// 写一帧：先写头，再写体，最后 flush（管道缓冲不 flush 等于没发）。
pub fn write_frame<W: Write>(out: &mut W, value: &Value) -> Result<(), FrameError> {
    let body = serde_json::to_vec(value).map_err(|error| FrameError::Json(error.to_string()))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            declared: body.len(),
        });
    }
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn encode(value: &Value) -> Vec<u8> {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, value).unwrap();
        buffer
    }

    #[test]
    fn round_trips_a_frame() {
        let value = json!({ "id": 1, "method": "initialize", "params": { "protocol": 1 } });
        let buffer = encode(&value);
        let mut reader = FrameReader::new(Cursor::new(buffer));
        assert_eq!(reader.read_frame().unwrap(), Some(value));
        assert_eq!(reader.read_frame().unwrap(), None);
    }

    #[test]
    fn reads_two_frames_and_then_eof() {
        let mut buffer = encode(&json!({ "a": 1 }));
        buffer.extend(encode(&json!({ "b": 2 })));

        let mut reader = FrameReader::new(Cursor::new(buffer));
        assert_eq!(reader.read_frame().unwrap(), Some(json!({ "a": 1 })));
        assert_eq!(reader.read_frame().unwrap(), Some(json!({ "b": 2 })));
        assert_eq!(reader.read_frame().unwrap(), None);
    }

    #[test]
    fn header_names_are_case_insensitive_and_unknown_ones_ignored() {
        let body = br#"{"ok":true}"#;
        let raw = format!(
            "X-Whatever: 1\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        let mut buffer = raw.into_bytes();
        buffer.extend_from_slice(body);

        let mut reader = FrameReader::new(Cursor::new(buffer));
        assert_eq!(reader.read_frame().unwrap(), Some(json!({ "ok": true })));
    }

    #[test]
    fn oversized_frame_is_rejected_before_reading_the_body() {
        let raw = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1);
        let mut reader = FrameReader::new(Cursor::new(raw.into_bytes()));
        match reader.read_frame() {
            Err(FrameError::TooLarge { declared }) => {
                assert_eq!(declared, MAX_FRAME_BYTES + 1);
            }
            other => panic!("应报 TooLarge，got: {other:?}"),
        }
    }

    #[test]
    fn missing_content_length_is_a_header_error() {
        let mut reader = FrameReader::new(Cursor::new(b"\r\n{}".to_vec()));
        match reader.read_frame() {
            Err(FrameError::Header(detail)) => assert!(detail.contains("Content-Length")),
            other => panic!("应报 Header，got: {other:?}"),
        }
    }

    #[test]
    fn non_header_line_is_rejected() {
        let mut reader = FrameReader::new(Cursor::new(b"Content-Length: 2\r\nnot-a-header\r\n\r\n{}".to_vec()));
        match reader.read_frame() {
            Err(FrameError::Header(detail)) => assert!(detail.contains("名字: 值")),
            other => panic!("应报 Header，got: {other:?}"),
        }
    }

    #[test]
    fn garbage_body_is_a_json_error() {
        let mut reader = FrameReader::new(Cursor::new(b"Content-Length: 3\r\n\r\nnot".to_vec()));
        match reader.read_frame() {
            Err(FrameError::Json(_)) => {}
            other => panic!("应报 Json，got: {other:?}"),
        }
    }

    #[test]
    fn truncated_body_is_an_io_error() {
        let mut reader = FrameReader::new(Cursor::new(b"Content-Length: 10\r\n\r\n{}".to_vec()));
        match reader.read_frame() {
            Err(FrameError::Io(_)) => {}
            other => panic!("应报 Io，got: {other:?}"),
        }
    }

    #[test]
    fn writing_an_oversized_frame_is_rejected() {
        let huge = json!(format!("{}", "x".repeat(MAX_FRAME_BYTES)));
        let mut buffer = Vec::new();
        match write_frame(&mut buffer, &huge) {
            Err(FrameError::TooLarge { .. }) => {}
            other => panic!("应报 TooLarge，got: {other:?}"),
        }
    }
}
