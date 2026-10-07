//! 把进程的 stdout/stderr 重定向到日志文件，并把**原始 stdout** 留给协议独占。
//!
//! 为什么需要：MaaFramework 的控制单元（`MaaWin32ControlUnit`）会直接往进程
//! stdout 打带 ANSI 色的日志。若协议也用 stdout，这些字节会插进帧流，让读端失步
//! ——实测已复现。解法：启动时（在加载任何第三方 DLL 之前）先抓住原始 stdout
//! 句柄交给协议，再把 `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE` 指向日志文件。
//! 之后加载的 DLL 初始化 stdio 时会读到被重定向的句柄，日志自然落到文件里。
//!
//! 仅 Windows 使用。

use core::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

// kernel32 永远在，直接声明，避免依赖 windows crate 的签名细节。
#[link(name = "kernel32")]
extern "system" {
    fn SetStdHandle(nstdhandle: u32, hhandle: *mut c_void) -> i32;
}

// UCRT 的 fd 层。进程启动时 fd 1/2 已绑定到原始句柄，光改 Win32 句柄不够。
extern "C" {
    fn _open_osfhandle(osfhandle: isize, flags: i32) -> i32;
    fn _dup2(fd1: i32, fd2: i32) -> i32;
}

const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // (DWORD)-12

/// 把进程 stdout / stderr 重定向到 `log_path`。
///
/// 用于管道模式：进程标准流只剩日志用途，让第三方 DLL 的日志与 stderr 落进
/// 文件，既不干扰协议、也不会因管道堵塞而卡住。
pub fn redirect_std_to_log(log_path: &Path) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let handle = file.as_raw_handle() as *mut c_void;
    unsafe {
        // ① Win32 层：给直接调 GetStdHandle 的代码。
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
        // ② CRT 层：printf / std::cout 走的是 fd 1/2，必须一起重定向。
        let fd = _open_osfhandle(handle as isize, 0);
        if fd >= 0 {
            let _ = _dup2(fd, 1);
            let _ = _dup2(fd, 2);
        }
    }
    // _open_osfhandle 接管了句柄所有权（fd 会关它），故不让 File drop 重复关闭。
    std::mem::forget(file);
    Ok(())
}
