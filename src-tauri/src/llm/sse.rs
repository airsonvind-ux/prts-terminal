//! sse —— 共享 SSE 帧读取器（2026-10-09）
//!
//! 为什么存在：三个适配器各自手写读循环，且都先把单个网络 chunk 做
//! `from_utf8_lossy` 再拼进字符串缓冲。chunk 边界落在多字节字符中间时，每个
//! 碎片各解码成一个 U+FFFD，原始字节不可恢复（存档实证：一个三字节汉字被网络
//! 拆成三个单字节块，产出三个替换符）。此处收口唯一一份正确实现。
//!
//! 切帧语义：
//! - 分隔符 `\n\n` 与 `\r\n\r\n` 都认，取最先出现者。
//! - 分隔符是 ASCII，多字节字符不可能跨帧，所以按帧解码无损。
//! - 未成帧的尾字节留在缓冲里等下一个 chunk；流结束时冲刷一次（末帧可能没有
//!   空行终结符，丢掉它就会丢末尾增量与 usage）。
//!
//! 边界：本模块只切帧与取 `data:` 负载，不解释事件语义。

use futures_util::StreamExt;

/// 逐帧读取 SSE 字节流。`on_frame` 收到的是**完整帧**的文本（含 `event:` 等行）。
///
/// 流错误原样上抛为 `Err(消息)`，由调用方决定如何收尾（各适配器对"已有输出"
/// 的处理不同）。
pub async fn read_frames<St, E, F>(stream: St, mut on_frame: F) -> Result<(), String>
where
    St: futures_util::Stream<Item = Result<Vec<u8>, E>> + Unpin,
    E: std::fmt::Display,
    F: FnMut(&str),
{
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = stream;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buf.extend_from_slice(&bytes);
                // 逐帧切出：未成帧的尾字节留在 buf 里，等下一个 chunk 补齐
                while let Some((idx, sep_len)) = find_separator(&buf) {
                    let frame = String::from_utf8_lossy(&buf[..idx]).into_owned();
                    buf.drain(..idx + sep_len);
                    if !frame.trim().is_empty() {
                        on_frame(&frame);
                    }
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    // 冲刷：末帧可能没有空行终结符
    let tail = String::from_utf8_lossy(&buf).into_owned();
    if !tail.trim().is_empty() {
        on_frame(&tail);
    }
    Ok(())
}

/// 取一帧的 `data:` 负载。
///
/// 以 `data:` 开头的行取出来（前缀后那个空格可有可无），多行按 SSE 规范用 `\n`
/// 连接。无 data 行 → None。
pub fn frame_data(frame: &str) -> Option<String> {
    let parts: Vec<&str> = frame
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|rest| rest.trim_start())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// 找最早出现的分隔符：返回 (位置, 长度)，长度 2 = `\n\n`，4 = `\r\n\r\n`。
fn find_separator(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = find_seq(buf, b"\n\n").map(|p| (p, 2));
    let crlf = find_seq(buf, b"\r\n\r\n").map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn find_seq(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn bytes_stream(chunks: Vec<Vec<u8>>) -> impl futures_util::Stream<Item = Result<Vec<u8>, String>> + Unpin {
        stream::iter(chunks.into_iter().map(Ok))
    }

    async fn collect(chunks: Vec<Vec<u8>>) -> Vec<String> {
        let mut out = Vec::new();
        read_frames(bytes_stream(chunks), |f| out.push(f.to_string()))
            .await
            .unwrap();
        out
    }

    /// 逐字节喂入：三字节汉字被网络拆成三个单字节块，读出必须完整（原缺陷点）。
    #[tokio::test]
    async fn multibyte_split_across_chunks_is_lossless() {
        let payload = "data: {\"t\":\"记录的可信度不由我担保\"}\n\n";
        let chunks: Vec<Vec<u8>> = payload.as_bytes().iter().map(|b| vec![*b]).collect();
        let frames = collect(chunks).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], "data: {\"t\":\"记录的可信度不由我担保\"}");
        assert!(!frames[0].contains('\u{FFFD}'), "不应出现替换符: {}", frames[0]);
    }

    /// `\r\n\r\n` 分隔的帧同样要切得开。
    #[tokio::test]
    async fn accepts_crlf_frames() {
        let frames = collect(vec![b"data: a\r\n\r\ndata: b\r\n\r\n".to_vec()]).await;
        assert_eq!(frames, vec!["data: a".to_string(), "data: b".to_string()]);
    }

    /// 末帧没有空行终结符时也要交付（否则丢末尾增量与 usage）。
    #[tokio::test]
    async fn flushes_trailing_frame_without_terminator() {
        let frames = collect(vec![b"data: a\n\ndata: b".to_vec()]).await;
        assert_eq!(frames, vec!["data: a".to_string(), "data: b".to_string()]);
    }

    /// 帧被 chunk 切开也不影响切帧（未成帧的尾字节留在缓冲里）。
    #[tokio::test]
    async fn frame_split_across_chunks() {
        let frames = collect(vec![b"data: he".to_vec(), b"llo\n".to_vec(), b"\n".to_vec()]).await;
        assert_eq!(frames, vec!["data: hello".to_string()]);
    }

    #[test]
    fn frame_data_is_prefix_lenient() {
        assert_eq!(frame_data("data:{\"a\":1}").as_deref(), Some("{\"a\":1}"));
        assert_eq!(frame_data("data: {\"a\":1}").as_deref(), Some("{\"a\":1}"));
        assert_eq!(
            frame_data("event: x\r\ndata: {\"a\":\r\ndata: 1}").as_deref(),
            Some("{\"a\":\n1}")
        );
        assert_eq!(frame_data("event: ping").as_deref(), None);
    }
}
