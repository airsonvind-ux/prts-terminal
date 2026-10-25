//! MaaCore 实例的生命周期封装。
//!
//! 全局一次性初始化：设置工作目录 → 载入资源。之后可创建实例（带回调）。
//! 实例 `Drop` 时调用 `AsstDestroy`；库本身永不卸载（见 `ffi` 模块说明）。

use std::ffi::{c_char, c_void, CString};
use std::path::Path;
use std::sync::Mutex;

use crate::ffi::{AsstApiCallback, AsstHandle, MaaCore};

/// 全局初始化：`AsstSetUserDir`（MAA 的工作/缓存目录）+ `AsstLoadResource`（资源根目录）。
///
/// `user_dir` 指向 **PRTS 自己的数据目录**（隔离，不污染用户 MAA）；
/// `resource_root` 指向**含 `resource/` 的 MAA 根目录**（MaaCore 内部会拼 `resource/`）。
/// 这两处都与 MAA GUI 惯例略有不同，需在实测中确认 MAA 无额外假设（见设计文档 §6/§7）。
pub fn init(core: &'static MaaCore, user_dir: &Path, resource_root: &Path) -> Result<(), String> {
    let ud = cstring(&user_dir.to_string_lossy())?;
    let rr = cstring(&resource_root.to_string_lossy())?;
    unsafe {
        if (core.fns.set_user_dir)(ud.as_ptr()) == 0 {
            return Err(format!("AsstSetUserDir 失败：{}", user_dir.display()));
        }
        // MaaCore 内部拼 `resource/`（见 src/MaaCore/AsstCaller.cpp），故传根目录。
        if (core.fns.load_resource)(rr.as_ptr()) == 0 {
            return Err(format!(
                "AsstLoadResource 失败：{}（应指向含 resource/ 的 MAA 根目录）",
                resource_root.display()
            ));
        }
    }
    Ok(())
}

pub struct Instance {
    core: &'static MaaCore,
    handle: AsstHandle,
    /// 已追加任务的书签。MaaCore 不暴露任务参数，需要自己记（`task_list` 用）。
    tasks: Mutex<Vec<TaskRec>>,
}

/// 一条已追加任务的记录（id / 类型 / 参数）。
pub struct TaskRec {
    pub id: i32,
    pub type_: String,
    pub params: String,
}

impl Instance {
    /// 创建实例并注册回调。`arg` 会原样传回每个回调，用于携带上下文指针。
    /// 返回 `None` 表示 `AsstCreateEx` 给出了空句柄（创建失败）。
    pub fn create(core: &'static MaaCore, cb: AsstApiCallback, arg: *mut c_void) -> Option<Self> {
        let handle = unsafe { (core.fns.create_ex)(cb, arg) };
        if handle.is_null() {
            None
        } else {
            Some(Instance {
                core,
                handle,
                tasks: Mutex::new(Vec::new()),
            })
        }
    }

    pub fn connected(&self) -> bool {
        unsafe { (self.core.fns.connected)(self.handle) == 1 }
    }

    pub fn running(&self) -> bool {
        unsafe { (self.core.fns.running)(self.handle) == 1 }
    }

    /// 追加任务。`params_json` 是任务参数对象序列化后的字符串（`{}` 表示默认）。
    pub fn append_task(&self, type_: &str, params_json: &str) -> Result<i32, String> {
        let t = cstring(type_)?;
        let p = cstring(params_json)?;
        let task_id = unsafe { (self.core.fns.append_task)(self.handle, t.as_ptr(), p.as_ptr()) };
        if task_id == 0 {
            Err(format!(
                "AsstAppendTask 返回 0（任务类型 {type_} 未被接受）"
            ))
        } else {
            self.tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(TaskRec {
                    id: task_id,
                    type_: type_.to_string(),
                    params: params_json.to_string(),
                });
            Ok(task_id)
        }
    }

    /// 修改已追加任务的参数。
    pub fn set_task_params(&self, task_id: i32, params_json: &str) -> Result<(), String> {
        let p = cstring(params_json)?;
        let ret = unsafe { (self.core.fns.set_task_params)(self.handle, task_id, p.as_ptr()) };
        if ret != 1 {
            return Err("AsstSetTaskParams 失败".into());
        }
        if let Some(t) = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
            .find(|t| t.id == task_id)
        {
            t.params = params_json.to_string();
        }
        Ok(())
    }

    /// 当前任务队列（id / 类型 / 参数），已剔除已不在队列中的。
    pub fn tasks_snapshot(&self) -> Result<Vec<(i32, String, String)>, String> {
        let live = self.live_task_ids()?;
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.retain(|t| live.contains(&t.id));
        Ok(tasks
            .iter()
            .map(|t| (t.id, t.type_.clone(), t.params.clone()))
            .collect())
    }

    fn live_task_ids(&self) -> Result<Vec<i32>, String> {
        let null = unsafe { (self.core.fns.get_null_size)() };
        let mut size: usize = 64;
        loop {
            if size > 1 << 16 {
                return Err("任务列表缓冲区过大".into());
            }
            let mut buf: Vec<i32> = vec![0i32; size];
            let n = unsafe {
                (self.core.fns.get_tasks_list)(self.handle, buf.as_mut_ptr(), size as u64)
            };
            if n == null {
                size *= 2;
                continue;
            }
            buf.truncate(n as usize);
            return Ok(buf);
        }
    }

    /// 设备唯一码（连接后才有意义）。
    pub fn uuid(&self) -> Result<String, String> {
        let null = unsafe { (self.core.fns.get_null_size)() };
        let mut size: usize = 1024;
        loop {
            if size > 1 << 20 {
                return Err("UUID 缓冲区过大".into());
            }
            let mut buf: Vec<u8> = vec![0u8; size];
            let n = unsafe {
                (self.core.fns.get_uuid)(self.handle, buf.as_mut_ptr() as *mut c_char, size as u64)
            };
            if n == null {
                size *= 2;
                continue;
            }
            buf.truncate(n as usize);
            return Ok(String::from_utf8_lossy(&buf).trim().to_string());
        }
    }

    /// ADB 异步连接（模拟器/移动设备）。
    pub fn connect_adb(
        &self,
        adb_path: &str,
        address: &str,
        config: Option<&str>,
    ) -> Result<i32, String> {
        let a = cstring(adb_path)?;
        let addr = cstring(address)?;
        let cfg = match config {
            Some(c) => Some(cstring(c)?),
            None => None,
        };
        let cfg_ptr = cfg.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        let call_id = unsafe {
            (self.core.fns.async_connect)(self.handle, a.as_ptr(), addr.as_ptr(), cfg_ptr, 0)
        };
        if call_id != 0 {
            Ok(call_id)
        } else {
            Err("AsstAsyncConnect 返回 0".into())
        }
    }

    /// 按坐标点击。
    pub fn click(&self, x: i32, y: i32) -> Result<i32, String> {
        let call_id = unsafe { (self.core.fns.async_click)(self.handle, x, y, 0) };
        if call_id != 0 {
            Ok(call_id)
        } else {
            Err("AsstAsyncClick 返回 0".into())
        }
    }

    /// 返回主界面（回首页）。
    pub fn back_to_home(&self) -> Result<(), String> {
        let ret = unsafe { (self.core.fns.back_to_home)(self.handle) };
        if ret == 1 {
            Ok(())
        } else {
            Err("AsstBackToHome 失败".into())
        }
    }

    /// 设置实例级参数（如 `2` = 连接方案、`3` = 触控方案，见 MAA 文档）。
    pub fn set_instance_option(&self, key: i32, value: &str) -> Result<(), String> {
        let v = cstring(value)?;
        let ret = unsafe { (self.core.fns.set_instance_option)(self.handle, key, v.as_ptr()) };
        if ret == 1 {
            Ok(())
        } else {
            Err("AsstSetInstanceOption 失败".into())
        }
    }

    pub fn start(&self) -> Result<(), String> {
        let ret = unsafe { (self.core.fns.start)(self.handle) };
        if ret == 1 {
            Ok(())
        } else {
            Err("AsstStart 失败（可能未连接或已有任务在跑）".into())
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        let ret = unsafe { (self.core.fns.stop)(self.handle) };
        if ret == 1 {
            Ok(())
        } else {
            Err("AsstStop 失败".into())
        }
    }

    /// 取当前帧（`AsstGetImage`，返回 PNG 字节）。
    /// 缓冲区不够时 MaaCore 返回 `AsstGetNullSize`，需翻倍重试。
    pub fn screenshot(&self) -> Result<Vec<u8>, String> {
        let null_size = unsafe { (self.core.fns.get_null_size)() };
        let mut size: usize = 2 * 1920 * 1080 * 4;
        loop {
            if size > 16 * 1920 * 1080 * 4 {
                return Err("截图缓冲区过大".into());
            }
            let mut buf: Vec<u8> = vec![0u8; size];
            let n = unsafe {
                (self.core.fns.get_image)(self.handle, buf.as_mut_ptr() as *mut c_void, size as u64)
            };
            if n == null_size {
                size *= 2;
                continue;
            }
            if n == 0 {
                return Err("AsstGetImage 返回 0（可能未连接或无帧）".into());
            }
            buf.truncate(n as usize);
            return Ok(buf);
        }
    }

    /// 异步绑定到 Win32 窗口（PC 客户端）。返回 `AsyncCallId`（非 0 表示已受理）；
    /// 结果经回调（`AsyncCallInfo` / `ConnectionInfo`）送达。
    #[cfg(windows)]
    pub fn attach_window(
        &self,
        hwnd: i64,
        screencap_method: u64,
        mouse_method: u64,
        keyboard_method: u64,
    ) -> Result<i32, String> {
        let call_id = unsafe {
            (self.core.fns.async_attach_window)(
                self.handle,
                hwnd as *mut c_void,
                screencap_method,
                mouse_method,
                keyboard_method,
                0,
            )
        };
        if call_id != 0 {
            Ok(call_id)
        } else {
            Err("AsstAsyncAttachWindow 返回 0".into())
        }
    }

    /// 设置连接 extras（**进程级、非 per-handle**——MaaCore 内部存进 ResourceLoader）。
    /// 典型用途：MuMu 12 的截图/触控增强——
    /// `AsstSetConnectionExtras("MuMuEmulator12", {"path":…,"touch":…,"client_type":…})`，
    /// 须在 `connect_adb` **之前**调用，且 connect 的 `config` 传字符串 `"MuMuEmulator12"`。
    /// C 接口为 void，无返回值可判错，仅保证参数被受理。
    pub fn set_connection_extras(&self, name: &str, extras: &str) -> Result<(), String> {
        let n = cstring(name)?;
        let e = cstring(extras)?;
        unsafe { (self.core.fns.set_connection_extras)(n.as_ptr(), e.as_ptr()) };
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { (self.core.fns.destroy)(self.handle) }
    }
}

fn cstring(s: &str) -> Result<CString, String> {
    // 路径/JSON 里的 NUL 会截断 C 字符串，直接拒掉。
    CString::new(s.as_bytes()).map_err(|_| "字符串含 NUL 字节".to_string())
}
