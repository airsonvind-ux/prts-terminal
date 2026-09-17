//! memory —— 记忆系统（批 1 · 记忆地基）
//!
//! 两条存储线 + 冻结快照 + 提示词记忆区：
//! - 成长层（[`store::MemoryStore`] 的 `growth_*`）：由记忆工具与用户写；
//! - 经历层（`experience.jsonl`）：后台经历管线写（批 2），本模块**只读**；
//! - 冻结快照（[`snapshot::SnapshotStore`]）：随对话档案落盘，对话内逐字不变；
//! - 记忆区文本（[`prompt::assemble_memory_block`]）：四节来源纯读组装。
//!
//! 存储位置：`data_dir()/memory/`（与 sessions / settings / env-ledger 同级）。

pub mod prompt;
pub mod schema;
pub mod snapshot;
pub mod store;

pub use snapshot::SnapshotStore;
pub use store::MemoryStore;
