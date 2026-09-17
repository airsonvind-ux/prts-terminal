//! config —— data_dir 定位 + 数据文件路径解析（M1 地基）
//!
//! 设置页扩展定稿后职责收拢：workspace 根已并入 settings.json（`settings.rs`
//! workspace 段；空 = 回退 data_dir），`PRTS_WORKSPACE` env 与
//! `resources/config.json` 链**已废弃**（dev.ps1 不再设置）。本模块仅保留：
//!
//! - [`data_dir`]：应用数据目录（settings.json / notices.json / sessions/
//!   的落点）。开发期 = `APPDATA\com.tauri-app.prts-terminal`；APPDATA 缺失
//!   回退 `./.prts-workspace`。纯路径解析，不建目录——需要目录存在的地方
//!   （SettingsStore::load / SessionStore / Notices）自行 create_dir_all。
//! - [`resolve_db_path`]：arknights.db 路径（dev resources/ → 打包 exe 同级）。

use std::path::PathBuf;

/// 应用数据目录。
///
/// 开发期 = `APPDATA\com.tauri-app.prts-terminal`；APPDATA 缺失时回退
/// `./.prts-workspace`（原样沿用旧逻辑，含空串不 trim 的行为）。
pub fn data_dir() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(d) => PathBuf::from(d).join("com.tauri-app.prts-terminal"),
        Err(_) => PathBuf::from(".").join(".prts-workspace"),
    }
}

/// arknights.db 路径解析。
///
/// 优先级：开发目录（src-tauri/resources/）→ 可执行文件同级（打包后）。
/// Policy BLOCKED 名单调用此函数获取 db 文件路径。
pub fn resolve_db_path() -> PathBuf {
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arknights.db");
    if dev_path.exists() {
        return dev_path;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let prod_path = dir.join("resources/arknights.db");
            if prod_path.exists() {
                return prod_path;
            }
        }
    }
    dev_path
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 串行化修改进程环境变量的测试（cargo test 同进程并行；
    /// set_var/remove_var 无同步保护，env 相关测试持锁互斥）。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn data_dir_appdata_and_fallback_branches() {
        let _g = ENV_LOCK.lock().unwrap();
        let orig = std::env::var("APPDATA").ok();
        let fake = std::env::temp_dir().join("prts-fake-appdata");

        // APPDATA 存在 → <appdata>/com.tauri-app.prts-terminal
        std::env::set_var("APPDATA", &fake);
        assert_eq!(data_dir(), fake.join("com.tauri-app.prts-terminal"));

        // APPDATA 缺失 → ./.prts-workspace
        std::env::remove_var("APPDATA");
        assert_eq!(data_dir(), PathBuf::from(".").join(".prts-workspace"));

        // 恢复原值
        match orig {
            Some(v) => std::env::set_var("APPDATA", v),
            None => std::env::remove_var("APPDATA"),
        }
    }
}
