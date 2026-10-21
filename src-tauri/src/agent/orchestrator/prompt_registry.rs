//! 静态提示词 section 注册表（模块 2 P0）。
//!
//! system prompt 只保留静态两层（Base 人格 + Catalog Skill 目录），
//! 按 `order` 升序拼接，同名重复注册报错。动态内容一律以消息形态进历史，
//! 保证请求前缀逐字节稳定，不破坏 DeepSeek 前缀缓存。

use std::sync::OnceLock;

use super::prompt::{SYSTEM_PROMPT, UI_PROMPT};

/// order 区间映射：Base 0 / Catalog 100 / Active 200 / Reference 300。
///
/// 同层多个 section 可用相邻整数值细化顺序（如 Base 层人格 0，同层后续内容 10、20）。
pub const ORDER_BASE: i32 = 0;
/// Base 层同层后续内容（界面认知；人格 0 之后、Catalog 之前）。
pub const ORDER_UI: i32 = 10;
/// Catalog 层区间（Skill 目录）
pub const ORDER_CATALOG: i32 = 100;
/// 记忆区区间（Phase 4 任务二 · 批 1）：置于 Catalog 之后（system 尾部，
/// 「跨对话分叉段后置」）。对话内冻结快照，不随记忆变化重组装。
pub const ORDER_MEMORY: i32 = 150;
// —— 动态层区间已移除（2026-08 上下文管理修正：动态内容改为消息注入，
//    不再进 system prompt）。保留注释供未来记忆系统复用。
// /// Active 层区间（激活 Skill 动态注入）
// pub const ORDER_ACTIVE: i32 = 200;
// /// Reference 层区间（知识/缓存动态注入，缓存条目在场层）
// pub const ORDER_REFERENCE: i32 = 300;

/// 静态提示词 section（注册制）。
///
/// 对应 DSH `PromptSection`（name/order/text）的 Rust 近似：
/// 按 `order` 升序拼接，同名重复注册报错。
#[derive(Debug, Clone)]
pub struct PromptSection {
    pub name: String,
    pub order: i32,
    pub text: String,
}

impl PromptSection {
    pub fn new(name: impl Into<String>, order: i32, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            order,
            text: text.into(),
        }
    }
}

/// 提示词注册表：静态 sections，按 order 统一排序装配。
#[derive(Debug, Default, Clone)]
pub struct PromptRegistry {
    pub(crate) sections: Vec<PromptSection>,
}

impl PromptRegistry {
    /// 内置注册表：Base 层两段（角色人格 + 界面认知）。
    pub fn builtin() -> Self {
        let mut reg = Self::default();
        // Base 0：角色人格（prts.md）
        reg.register_section(PromptSection::new(
            "prts.persona",
            ORDER_BASE,
            SYSTEM_PROMPT,
        ))
        .expect("builtin section 注册冲突");
        // Base 10：界面认知（ui.md）——人格之后、Catalog 之前。
        // 讲主界面三区的位置与用途，以及随包《使用说明》的位置。
        reg.register_section(PromptSection::new("prts.ui", ORDER_UI, UI_PROMPT))
            .expect("builtin section 注册冲突");
        reg
    }

    /// 注册静态 section。同名重复注册返回错误（对齐 DSH 同名抛错语义）。
    pub fn register_section(&mut self, section: PromptSection) -> Result<(), String> {
        if self.sections.iter().any(|s| s.name == section.name) {
            return Err(format!("PromptSection '{}' 重复注册", section.name));
        }
        self.sections.push(section);
        Ok(())
    }

    /// 按 order 升序装配：相邻条目以空行分隔（对齐原 `"{}\n\n{}"` 拼接语义）。
    pub fn assemble(&self) -> String {
        let mut entries: Vec<(&str, i32)> = self
            .sections
            .iter()
            .map(|s| (s.text.as_str(), s.order))
            .collect();
        entries.sort_by_key(|(_, order)| *order);

        entries
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// 内置注册表缓存（assemble_system_prompt 的重复构造开销为零）。
pub(super) fn builtin_registry() -> &'static PromptRegistry {
    static REGISTRY: OnceLock<PromptRegistry> = OnceLock::new();
    REGISTRY.get_or_init(PromptRegistry::builtin)
}
