//! maa —— 接入 MAA（MaaCore 的视觉识别能力）。
//!
//! 桥（`prts-maa-bridge`）是 PRTS 自带的独立进程：运行时加载用户 MAA 目录里的
//! `MaaCore.dll`，把 MAA 的连接 / 任务 / 图像 / 回调包成 PRTS 可用的本地接口。
//! 二者**只经本机 TCP 交互**——桥因链接 MaaCore 而是 AGPL 组件，绝不能静态
//! 链接进 PRTS（AGPL 边界，见 `bridge/README.md`）。
//!
//! 本模块负责：定位并启动桥、维护连接、把命令与事件翻译成 PRTS 能用的形状。
//! 设计见 `_maa-接入模块-设计.md`。
//!
//! 自插件系统 M1 起，MAA 是本宿主的一个内置插件：`content/plugins/maa/plugin.json`
//! 声明契约，工具经 `plugins::load_all` → `builtin::install("maa")` → [`install`]
//! 注册。桥仍是宿主管理的独立进程（D17），不随插件搬家而改变。

use crate::plugins::PluginError;
use crate::tools::ToolRegistry;

pub mod bridge;
pub mod connection;
pub mod manager;
pub mod records;
pub mod settings;
pub mod task;

/// 以内置插件身份安装 MAA 的工具，返回本次注册的工具名。
///
/// 只做注册：桥的定位与连接在 `manager` 的懒初始化里，与安装时机无关。
pub fn install(reg: &mut ToolRegistry) -> Result<Vec<String>, PluginError> {
    crate::plugins::register_plugin_tools(reg, "maa", vec![Box::new(task::MaaTaskTool)])
}
