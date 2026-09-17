//! 系统提示词 —— 编译期嵌入
//!
//! 用 `include_str!` 将 `prts.md` 在编译期嵌入二进制，
//! 运行时零开销读取（不需要文件 I/O、不需要路径处理）。
//! prts.md 为无人机角色人格。

/// PRTS 终端助手的全量系统提示词。
///
/// 内容来源：`src-tauri/src/orchestrator/prts.md`，
/// 编译期嵌入，pub 常量子以 `&'static str` 暴露。
/// context 模块直接引用此常量组装上下文消息。
pub const SYSTEM_PROMPT: &str = include_str!("prts.md");
