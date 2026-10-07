//! maa —— 接入 MAA（MaaCore 的视觉识别能力）。
//!
//! 桥（`prts-maa-bridge`）是 PRTS 自带的独立进程：运行时加载用户 MAA 目录里的
//! `MaaCore.dll`，把 MAA 的连接 / 任务 / 图像 / 回调包成 PRTS 可用的本地接口。
//! 二者**只经本机 TCP 交互**——桥因链接 MaaCore 而是 AGPL 组件，绝不能静态
//! 链接进 PRTS（AGPL 边界，见 `bridge/README.md`）。
//!
//! 本模块负责：定位并启动桥、维护连接、把命令与事件翻译成 PRTS 能用的形状。
//! 设计见 `_maa-接入模块-设计.md`。

pub mod bridge;
pub mod connection;
pub mod manager;
pub mod records;
pub mod settings;
