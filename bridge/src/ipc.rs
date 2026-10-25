//! PRTS ↔ 桥 的帧协议。
//!
//! 帧格式：`[4 字节大端 u32 长度][UTF-8 JSON]`，走字节流。
//!
//! 传输**不走** stdin/stdout：第三方 DLL（MaaFramework 控制单元）会往进程
//! stdout 打日志，实测会插进帧流让读端失步。改用**本机 TCP**（`127.0.0.1`），
//! 与 stdout/stderr 天然隔离，且跨平台、易调试。
//!
//! 采用长度前缀而非行分隔：`details` 里可能含换行，行分隔会失步。

use std::io::{self, Read, Write};
use std::sync::Mutex;

/// 单帧上限，与核心侧 `Bridge::MAX_FRAME` 保持一致。
///
/// 长度字段来自对端，不能无条件相信：没有上限时，一个 4GiB 的声明会让
/// 读端先分配 4GiB 再等正文。正常命令只有几百字节到几 KB，64MiB 足够覆盖
/// 大任务列表与识别帧。
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// 事件/响应的写出端。多个线程（回调转发、命令处理）共用，内部加锁串行化。
pub struct Sink {
    out: Mutex<Box<dyn Write + Send>>,
}

impl Sink {
    pub fn from_writer(w: Box<dyn Write + Send>) -> Self {
        Sink { out: Mutex::new(w) }
    }

    /// 无 TCP 时的回退：直接写 stdout（会有被第三方 DLL 污染的风险）。
    pub fn new() -> Self {
        Sink::from_writer(Box::new(io::stdout()))
    }

    pub fn emit(&self, value: &serde_json::Value) -> io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "帧过大"));
        }
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        out.write_all(&(bytes.len() as u32).to_be_bytes())?;
        out.write_all(&bytes)?;
        out.flush()
    }
}

/// 读一帧。返回 `Ok(None)` 表示对端关闭（EOF）——父进程死了，桥应随之退出。
pub fn read_frame(input: &mut (impl Read + ?Sized)) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    match input.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("帧长度超限：{len}（上限 {MAX_FRAME}）"),
        ));
    }
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf)?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn sink_and_buffer() -> (Sink, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = Sink::from_writer(Box::new(SharedWriter(buffer.clone())));
        (sink, buffer)
    }

    #[test]
    fn emit_writes_length_prefixed_json_frame() {
        let (sink, buffer) = sink_and_buffer();
        sink.emit(&serde_json::json!({"event": "maa", "code": 20003}))
            .unwrap();

        let bytes = buffer.lock().unwrap().clone();
        assert!(bytes.len() > 4, "应写出长度前缀和正文");
        let expected_len = (bytes.len() - 4) as u32;
        assert_eq!(&bytes[..4], &expected_len.to_be_bytes());

        let mut cursor = io::Cursor::new(bytes);
        let frame = read_frame(&mut cursor).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(value["event"], "maa");
        assert_eq!(value["code"], 20003);
    }

    #[test]
    fn read_frame_returns_none_on_empty_input() {
        let mut cursor = io::Cursor::new(Vec::new());
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn read_frame_treats_short_header_as_eof_and_rejects_short_body() {
        // 头部不足 4 字节按 EOF 处理（对端关闭）；正文不足声明长度才是错误。
        let mut short_header = io::Cursor::new(vec![0u8, 0, 0]);
        assert!(read_frame(&mut short_header).unwrap().is_none());

        let mut short_body = io::Cursor::new(vec![0u8, 0, 0, 8, b'{']);
        assert!(read_frame(&mut short_body).is_err());
    }

    #[test]
    fn read_frame_rejects_declared_length_over_limit_before_reading_body() {
        // 只有帧头、没有正文：若实现先分配/读正文，会得到 UnexpectedEof；
        // 正确实现应在读正文前就以 InvalidData 拒绝。
        let header = (MAX_FRAME as u32 + 1).to_be_bytes();
        let mut cursor = io::Cursor::new(header.to_vec());
        let err = read_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_frame_reads_multiple_frames_in_order() {
        let (sink, buffer) = sink_and_buffer();
        sink.emit(&serde_json::json!({"seq": 1})).unwrap();
        sink.emit(&serde_json::json!({"seq": 2})).unwrap();

        let bytes = buffer.lock().unwrap().clone();
        let mut cursor = io::Cursor::new(bytes);
        let first: serde_json::Value =
            serde_json::from_slice(&read_frame(&mut cursor).unwrap().unwrap()).unwrap();
        let second: serde_json::Value =
            serde_json::from_slice(&read_frame(&mut cursor).unwrap().unwrap()).unwrap();
        assert_eq!(first["seq"], 1);
        assert_eq!(second["seq"], 2);
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }
}
