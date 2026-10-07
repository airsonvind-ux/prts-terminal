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

/// 事件/响应的写出端。多个线程（回调转发、命令处理）共用，内部加锁串行化。
pub struct Sink {
    out: Mutex<Box<dyn Write + Send>>,
}

impl Sink {
    pub fn from_writer(w: Box<dyn Write + Send>) -> Self {
        Sink {
            out: Mutex::new(w),
        }
    }

    /// 无 TCP 时的回退：直接写 stdout（会有被第三方 DLL 污染的风险）。
    pub fn new() -> Self {
        Sink::from_writer(Box::new(io::stdout()))
    }

    pub fn emit(&self, value: &serde_json::Value) -> io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > u32::MAX as usize {
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
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf)?;
    Ok(Some(buf))
}
