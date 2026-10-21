//! 系统提示词 —— 编译期嵌入
//!
//! 用 `include_str!` 将 `prts.md` 在编译期嵌入二进制，
//! 运行时零开销读取（不需要文件 I/O、不需要路径处理）。
//! prts.md 为无人机角色人格。

/// PRTS 终端助手的全量系统提示词。
///
/// 内容来源：`src-tauri/content/prompts/prts.md`，
/// 编译期嵌入，pub 常量子以 `&'static str` 暴露。
/// context 模块直接引用此常量组装上下文消息。
pub const SYSTEM_PROMPT: &str = include_str!("../../content/prompts/prts.md");

/// 界面认知段（Base 层，人格之后）。
///
/// 内容来源：`src-tauri/content/prompts/ui.md`，编译期嵌入。
/// 讲主界面三区的位置与用途，以及随包《使用说明》的位置——性质是
/// “知道自己长什么样 / 东西在哪”，与人格文件（是谁 / 怎么想）分节。
pub const UI_PROMPT: &str = include_str!("../../content/prompts/ui.md");
