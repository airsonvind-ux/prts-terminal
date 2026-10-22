//! Skill 系统 —— 目录 schema
//!
//! Skill 管「怎么做」（行为规则），与知识库（领域事实）解耦，LLM 语义激活。
//! 渐进披露：目录只注入 name + description（Catalog 层），正文按需加载。
//!
//! ## frontmatter schema（与知识条目两套 schema 边界分明）
//!
//! ```markdown
//! ---
//! name: 推演分析
//! description: 关卡推演时的行为规则（一句话）
//! source: builtin | user | community（模块 9 能力分层预留）
//! ---
//! ```
//!
//! 第三轮 schema 收敛：激活判别信息并入 description（何时激活由 LLM 对
//! description 的语义识别决定），frontmatter 不再设独立触发字段；
//! 旧文件的 when_to_use 等字段按未知字段忽略（向前兼容）。

pub mod index;
pub mod install;
pub mod snapshot;
pub mod trust;

use crate::knowledge::parse_frontmatter_fields;

/// Skill 目录条目元数据。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    /// 来源分层（模块 9：内置/用户/社区）
    pub source: Option<String>,
}

/// 解析 Skill 条目的 frontmatter。缺少 name 时返回 None。
pub fn parse_skill_frontmatter(text: &str) -> Option<SkillMeta> {
    let fields = parse_frontmatter_fields(text)?;
    let mut meta = SkillMeta::default();
    for (key, values) in fields {
        match key.as_str() {
            "name" => meta.name = values.into_iter().next().unwrap_or_default(),
            "description" => meta.description = values.into_iter().next().unwrap_or_default(),
            "source" => meta.source = values.into_iter().next(),
            _ => {} // 未知字段忽略（向前兼容：旧 schema 的 when_to_use 等在此被忽略）
        }
    }
    if meta.name.is_empty() {
        return None;
    }
    Some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_skill_frontmatter() {
        let text =
            "---\nname: 推演分析\ndescription: 推演时的行为规则\nsource: builtin\n---\n\n## 正文";
        let meta = parse_skill_frontmatter(text).unwrap();
        assert_eq!(meta.name, "推演分析");
        assert_eq!(meta.description, "推演时的行为规则");
        assert_eq!(meta.source.as_deref(), Some("builtin"));
    }

    #[test]
    fn missing_name_returns_none() {
        assert!(parse_skill_frontmatter("---\ndescription: 没有名字\n---").is_none());
        assert!(parse_skill_frontmatter("无 frontmatter").is_none());
    }

    #[test]
    fn knowledge_schema_does_not_leak_into_skill() {
        // 两套 frontmatter schema 边界分明：知识字段在 Skill 中被忽略
        let text = "---\nname: 测试\nbrief: 这是知识的字段\nkey_points: [a]\n---";
        let meta = parse_skill_frontmatter(text).unwrap();
        assert_eq!(meta.name, "测试");
        assert!(meta.description.is_empty());
    }

    #[test]
    fn unknown_fields_ignored() {
        // 旧 schema 的 when_to_use 等字段现为未知字段：解析成功、name 正确、
        // 不残留到任何新字段（老文件向前兼容验证）
        let text = "---\nname: 推演分析\nwhen_to_use: 老文件的激活时机\nprovider: 老字段\n---";
        let meta = parse_skill_frontmatter(text).unwrap();
        assert_eq!(meta.name, "推演分析");
        assert!(meta.description.is_empty());
        assert!(meta.source.is_none());
    }
}
