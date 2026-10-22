//! 系统剪贴板写入（R-18）。
//!
//! 只提供 Rust 侧 `set_text`：完整 API Key 在进程内写入系统剪贴板，
//! 不经过前端返回值。调用方只拿得到 `last_four` 的末 4 位提示。
//!
//! Windows 使用 user32/kernel32 的剪贴板与全局内存 API；其他平台返回
//! 明确错误（本项目桌面目标为 Windows，编译期不依赖非 Windows 实现）。

/// 把文本写入系统剪贴板。
///
/// 失败时返回可读错误；成功不保证剪贴板内容之后不会被其他程序覆盖。
#[cfg(windows)]
pub fn set_text(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    // CF_UNICODETEXT 要求以 NUL 结尾的 UTF-16 序列。
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        // 剪贴板可能被其他程序短暂占用：重试几次再报错。
        let mut opened = false;
        let mut last_error = String::new();
        for attempt in 0..5 {
            match OpenClipboard(None) {
                Ok(()) => {
                    opened = true;
                    break;
                }
                Err(e) => {
                    last_error = e.to_string();
                    if attempt < 4 {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
        }
        if !opened {
            return Err(format!("剪贴板正被其他程序占用：{last_error}"));
        }

        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| format!("清空剪贴板失败：{e}"))?;

            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|e| format!("分配剪贴板内存失败：{e}"))?;
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                let _ = GlobalFree(Some(handle));
                return Err("锁定剪贴板内存失败".into());
            }

            // 复制到 GMEM_MOVEABLE 块；GlobalUnlock 的返回值在计数归零时
            // 按 Win32 约定为“失败”，因此这里忽略结果。
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
            let _ = GlobalUnlock(handle);

            // 成功后内存所有权归系统，不能再 GlobalFree。
            if let Err(e) = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(handle.0))) {
                let _ = GlobalFree(Some(handle));
                return Err(format!("写入剪贴板失败：{e}"));
            }
            Ok(())
        })();

        let _ = CloseClipboard();
        result
    }
}

#[cfg(not(windows))]
pub fn set_text(_text: &str) -> Result<(), String> {
    Err("当前平台不支持系统剪贴板写入".into())
}

/// 返回只在界面展示的末 4 位。
///
/// 密钥过短时不展示任何字符，避免“末 4 位”退化成完整密钥。
pub fn last_four(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 4 {
        return "****".into();
    }
    chars[chars.len() - 4..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::last_four;

    #[test]
    fn last_four_returns_only_the_tail() {
        assert_eq!(last_four("sk-real-secret"), "cret");
        assert_eq!(last_four("12345"), "2345");
        assert_eq!(last_four("密钥abcdef"), "cdef");
    }

    #[test]
    fn last_four_masks_short_or_empty_secrets() {
        assert_eq!(last_four(""), "****");
        assert_eq!(last_four("1234"), "****");
        assert_eq!(last_four("密钥"), "****");
    }

    #[test]
    #[ignore = "会改写系统剪贴板；手动冒烟用"]
    fn set_text_writes_clipboard() {
        super::set_text("PRTS-R18-CLIPBOARD-TEST").unwrap();
    }
}
