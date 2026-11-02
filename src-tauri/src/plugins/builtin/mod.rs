//! 内置插件：代码与宿主同进程、同一二进制，manifest 只承担契约声明。
//!
//! 安装入口是显式表（`install`），不做反射、不做动态加载——内置插件必须与
//! 宿主一起编译，id 与实现的一一对应关系在编译期就能看见。

use crate::plugins::PluginError;
use crate::tools::ToolRegistry;

pub mod maa;

/// 按 manifest 的 id 安装内置插件，返回本次注册的工具名。
///
/// 显式表而不是注册宏：漏写一个 id 在这里是可见的，将来加插件也只有这一处
/// 需要改；未知 id 一律 `UnknownBuiltin`（fail-closed，不静默跳过）。
pub fn install(id: &str, reg: &mut ToolRegistry) -> Result<Vec<String>, PluginError> {
    match id {
        "maa" => maa::install(reg),
        other => Err(PluginError::UnknownBuiltin(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_id_is_rejected() {
        let mut reg = ToolRegistry::new();
        let err = install("no-such-plugin", &mut reg).unwrap_err();
        assert!(
            matches!(&err, PluginError::UnknownBuiltin(id) if id == "no-such-plugin"),
            "got: {err}"
        );
        assert!(reg.list_defs().is_empty(), "失败的安装不得留下工具");
    }

    #[test]
    fn maa_installs_run_maa_task() {
        let mut reg = ToolRegistry::new();
        let names = install("maa", &mut reg).expect("内置插件 maa 应装配成功");
        assert_eq!(names, vec!["run_maa_task".to_string()]);
        assert!(reg.get("run_maa_task").is_some());
        assert_eq!(reg.list_defs().len(), 1);
    }
}
