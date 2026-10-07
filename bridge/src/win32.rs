//! Win32 顶层窗口枚举：用于发现「明日方舟 PC 客户端」窗口。
//!
//! MaaCore 的 `AsstAsyncAttachWindow` 只收 `hwnd`，**找窗口是调用方的活**
//! （官方 WPF GUI 自己做，见 `MaaWpfGui/Helper/*`）。这里按官方同款做法：
//! `EnumWindows` → 只留**可见**且**有标题**的顶层窗口，按标题匹配。
//!
//! PC 端窗口的类名/标题因构建而异，故同时返回 `title` 与 `class` 供上层判断，
//! 而不是把某一串写死。

#[derive(Clone)]
pub struct WindowEntry {
    /// 窗口句柄（`HWND` 的整数值）。
    pub hwnd: i64,
    pub title: String,
    pub class: String,
    pub pid: u32,
}

/// 列出可见、有标题的顶层窗口。
#[cfg(windows)]
pub fn list_windows() -> Vec<WindowEntry> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<WindowEntry>);
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let title_len = GetWindowTextLengthW(hwnd);
        if title_len <= 0 {
            return BOOL(1);
        }
        let mut tbuf = vec![0u16; title_len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut tbuf).max(0) as usize;
        let title = String::from_utf16_lossy(&tbuf[..n.min(tbuf.len())]);

        let mut cbuf = vec![0u16; 256];
        let cn = GetClassNameW(hwnd, &mut cbuf).max(0) as usize;
        let class = String::from_utf16_lossy(&cbuf[..cn.min(cbuf.len())]);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));

        out.push(WindowEntry {
            hwnd: hwnd.0 as i64,
            title,
            class,
            pid,
        });
        BOOL(1)
    }

    let mut out: Vec<WindowEntry> = Vec::new();
    let lparam = LPARAM(&mut out as *mut Vec<WindowEntry> as isize);
    unsafe {
        let _ = EnumWindows(Some(enum_proc), lparam);
    }
    out
}

#[cfg(not(windows))]
pub fn list_windows() -> Vec<WindowEntry> {
    Vec::new()
}

/// 按标题子串（大小写不敏感）找第一个匹配窗口。
pub fn find_by_title(needle: &str) -> Option<WindowEntry> {
    let needle = needle.to_lowercase();
    list_windows()
        .into_iter()
        .find(|w| w.title.to_lowercase().contains(&needle))
}

/// 窗口几何信息：用于诊断“实测分辨率与设定值差 1~2 像素”的问题。
#[derive(Clone)]
pub struct WindowInfo {
    pub hwnd: i64,
    /// 外框（含边框/标题栏）：left, top, right, bottom。
    pub window_rect: (i32, i32, i32, i32),
    /// 客户区尺寸：width, height。
    pub client_size: (i32, i32),
    /// 窗口 DPI（96 = 100%）。
    pub dpi: u32,
}

/// 读取窗口外框/客户区/DPI。
#[cfg(windows)]
pub fn window_info(hwnd: i64) -> Option<WindowInfo> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};

    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        let mut wr = RECT::default();
        if GetWindowRect(h, &mut wr).is_err() {
            return None;
        }
        let mut cr = RECT::default();
        if GetClientRect(h, &mut cr).is_err() {
            return None;
        }
        Some(WindowInfo {
            hwnd,
            window_rect: (wr.left, wr.top, wr.right, wr.bottom),
            client_size: (cr.right - cr.left, cr.bottom - cr.top),
            dpi: GetDpiForWindow(h),
        })
    }
}

#[cfg(not(windows))]
pub fn window_info(_hwnd: i64) -> Option<WindowInfo> {
    None
}
