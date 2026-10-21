//! Agent 内核：turn 循环、上下文、记忆、技能与知识索引。

pub mod knowledge;
pub mod memory;
pub mod orchestrator;
pub mod prefix_snapshot;
pub mod skills;

pub use skills::{index as skills_index, install as skills_install, snapshot as skills_snapshot};
