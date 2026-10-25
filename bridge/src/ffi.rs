//! MaaCore 的动态加载与函数表。
//!
//! 与官方静态绑定（`src/Rust`，`build.rs` 里 `rustc-link-lib=MaaCore`）不同：
//! 这里用 `libloading` 在运行时加载**用户 MAA 目录**里的 `MaaCore.dll`，
//! 因此不需要导入库（`.lib`），也不把桥钉死在某个编译期版本上。
//!
//! ## 调用约定
//! MaaCore 的 C 接口在 Windows 上 `ASST_CALL = __stdcall`（见 `include/AsstPort.h`）。
//! x64 下 `__stdcall` 与 `__cdecl` 是同一套约定，故本项目**锁死 x64**；
//! 语义上正确的 Rust 写法是 `extern "system"`。
//!
//! ## 为什么要泄漏句柄
//! MaaCore 内部有自管线程。若在实例销毁后 `FreeLibrary`，线程未清干净会崩溃。
//! 官方静态链接从不卸载（进程退出即回收）。这里同样**不主动卸载**：库与核心
//! 结构一次性 `Box::leak`，进程退出时由系统回收。

use std::ffi::{c_char, c_void};
use std::path::Path;

use libloading::Library;

pub type AsstHandle = *mut c_void;
pub type AsstBool = u8;
pub type AsstSize = u64;
pub type AsstMsgId = i32;
pub type AsstTaskId = i32;
pub type AsstAsyncCallId = i32;
pub type AsstStaticOptionKey = i32;
pub type AsstInstanceOptionKey = i32;

pub type AsstApiCallback = Option<
    unsafe extern "system" fn(msg: AsstMsgId, details_json: *const c_char, custom_arg: *mut c_void),
>;

macro_rules! define_fns {
    ($( $field:ident : $name:literal => $sig:ty ),* $(,)?) => {
        /// MaaCore 导出函数表（逐个符号在运行时解析）。
        #[allow(dead_code)]
        pub struct Fns {
            $( pub $field: $sig, )*
        }

        impl Fns {
            /// 从已加载的库逐个取符号；缺任何一个都返回 [`LoadError::Symbol`]，
            /// 便于把「版本不兼容」挡在启动阶段，而不是运行中崩溃。
            unsafe fn load(lib: &'static Library) -> Result<Self, LoadError> {
                Ok(Fns {
                    $( $field: {
                        let sym: libloading::Symbol<'static, $sig> = lib
                            .get(concat!($name, "\0").as_bytes())
                            .map_err(|e| LoadError::Symbol {
                                name: $name,
                                detail: e.to_string(),
                            })?;
                        *sym
                    }, )*
                })
            }
        }
    };
}

define_fns! {
    // —— 全局 / 生命周期 ——
    set_user_dir:          "AsstSetUserDir"          => unsafe extern "system" fn(*const c_char) -> AsstBool,
    load_resource:         "AsstLoadResource"        => unsafe extern "system" fn(*const c_char) -> AsstBool,
    set_static_option:     "AsstSetStaticOption"     => unsafe extern "system" fn(AsstStaticOptionKey, *const c_char) -> AsstBool,
    create:                "AsstCreate"              => unsafe extern "system" fn() -> AsstHandle,
    create_ex:             "AsstCreateEx"            => unsafe extern "system" fn(AsstApiCallback, *mut c_void) -> AsstHandle,
    destroy:               "AsstDestroy"             => unsafe extern "system" fn(AsstHandle),
    set_instance_option:   "AsstSetInstanceOption"   => unsafe extern "system" fn(AsstHandle, AsstInstanceOptionKey, *const c_char) -> AsstBool,

    // —— 连接：ADB ——
    async_connect:         "AsstAsyncConnect"        => unsafe extern "system" fn(AsstHandle, *const c_char, *const c_char, *const c_char, AsstBool) -> AsstAsyncCallId,
    set_connection_extras: "AsstSetConnectionExtras" => unsafe extern "system" fn(*const c_char, *const c_char),

    // —— 连接：Win32（PC 客户端）——
    attach_window:         "AsstAttachWindow"        => unsafe extern "system" fn(AsstHandle, *mut c_void, u64, u64, u64) -> AsstBool,
    async_attach_window:   "AsstAsyncAttachWindow"   => unsafe extern "system" fn(AsstHandle, *mut c_void, u64, u64, u64, AsstBool) -> AsstAsyncCallId,

    // —— 任务 ——
    append_task:           "AsstAppendTask"          => unsafe extern "system" fn(AsstHandle, *const c_char, *const c_char) -> AsstTaskId,
    set_task_params:       "AsstSetTaskParams"       => unsafe extern "system" fn(AsstHandle, AsstTaskId, *const c_char) -> AsstBool,
    start:                 "AsstStart"               => unsafe extern "system" fn(AsstHandle) -> AsstBool,
    stop:                  "AsstStop"                => unsafe extern "system" fn(AsstHandle) -> AsstBool,
    running:               "AsstRunning"             => unsafe extern "system" fn(AsstHandle) -> AsstBool,
    connected:             "AsstConnected"           => unsafe extern "system" fn(AsstHandle) -> AsstBool,
    back_to_home:          "AsstBackToHome"          => unsafe extern "system" fn(AsstHandle) -> AsstBool,

    // —— 设备 ——
    async_click:           "AsstAsyncClick"          => unsafe extern "system" fn(AsstHandle, i32, i32, AsstBool) -> AsstAsyncCallId,
    async_screencap:       "AsstAsyncScreencap"      => unsafe extern "system" fn(AsstHandle, AsstBool) -> AsstAsyncCallId,
    get_image:             "AsstGetImage"            => unsafe extern "system" fn(AsstHandle, *mut c_void, AsstSize) -> AsstSize,
    get_image_bgr:         "AsstGetImageBgr"         => unsafe extern "system" fn(AsstHandle, *mut c_void, AsstSize) -> AsstSize,
    get_uuid:              "AsstGetUUID"             => unsafe extern "system" fn(AsstHandle, *mut c_char, AsstSize) -> AsstSize,
    get_tasks_list:        "AsstGetTasksList"        => unsafe extern "system" fn(AsstHandle, *mut AsstTaskId, AsstSize) -> AsstSize,

    // —— 杂项 ——
    get_null_size:         "AsstGetNullSize"         => unsafe extern "system" fn() -> AsstSize,
    get_version:           "AsstGetVersion"          => unsafe extern "system" fn() -> *const c_char,
    log:                   "AsstLog"                 => unsafe extern "system" fn(*const c_char, *const c_char),
}

#[derive(Debug)]
pub enum LoadError {
    /// 找不到 MaaCore.dll。
    NotFound(std::path::PathBuf),
    /// 库本身加载失败（多是依赖 DLL 未解析）。
    Library(String),
    /// 库加载了，但缺某个导出符号（版本不兼容）。
    Symbol { name: &'static str, detail: String },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound(p) => write!(f, "找不到 MaaCore.dll：{}", p.display()),
            LoadError::Library(e) => {
                write!(f, "MaaCore.dll 加载失败（常见原因：依赖 DLL 未解析）：{e}")
            }
            LoadError::Symbol { name, detail } => {
                write!(f, "缺少导出符号 {name}（MaaCore 版本不兼容）：{detail}")
            }
        }
    }
}

/// 加载 MaaCore.dll。
///
/// Windows 上要同时做两件事，缺一不可：
///
/// 1. `LOAD_WITH_ALTERED_SEARCH_PATH`：管 MaaCore 的**静态导入**依赖
///    （opencv / onnxruntime / paddleocr …）从它自己所在目录解析。
/// 2. `SetDllDirectory` 把 MAA 目录加进**进程搜索路径**：管 MaaCore 在运行时用
///    **裸名** `LoadLibrary("MaaWin32ControlUnit.dll")` / `MaaAdbControlUnit.dll`
///    动态加载的兄弟 DLL——这类不受 flag 1 约束（实测 error 126 就死在这）。
#[cfg(windows)]
fn load_library(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows::{Library as WinLibrary, LOAD_WITH_ALTERED_SEARCH_PATH};
    if let Some(dir) = path.parent() {
        set_dll_directory(dir);
    }
    unsafe { WinLibrary::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH).map(Library::from) }
}

/// 把目录追加进进程的 DLL 搜索路径（供后续 `LoadLibrary` 使用）。
#[cfg(windows)]
fn set_dll_directory(dir: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::SetDllDirectoryW;
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let _ = SetDllDirectoryW(PCWSTR(wide.as_ptr()));
    }
}

#[cfg(not(windows))]
fn load_library(path: &Path) -> Result<Library, libloading::Error> {
    unsafe { Library::new(path) }
}

pub struct MaaCore {
    /// 有意终身持有：见模块头「为什么要泄漏句柄」。
    _lib: &'static Library,
    pub fns: Fns,
}

impl MaaCore {
    pub fn load(dll_path: &Path) -> Result<&'static MaaCore, LoadError> {
        if !dll_path.is_file() {
            return Err(LoadError::NotFound(dll_path.to_path_buf()));
        }
        let lib = load_library(dll_path).map_err(|e| LoadError::Library(e.to_string()))?;
        let lib: &'static Library = Box::leak(Box::new(lib));
        let fns = unsafe { Fns::load(lib)? };
        Ok(Box::leak(Box::new(MaaCore { _lib: lib, fns })))
    }

    pub fn version(&self) -> String {
        unsafe {
            let p = (self.fns.get_version)();
            if p.is_null() {
                return String::new();
            }
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}
