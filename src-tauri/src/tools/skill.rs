//! tools::skill —— Skill 激活工具（行为规则渐进披露的正文加载通道）
//!
//! Skill 管「怎么做」（行为规则），与知识库（领域事实）解耦，LLM 语义激活。
//! 渐进披露：目录只注入 name + description（Catalog 层），正文按需加载——
//! 本工具即正文加载通道：激活后正文直接作为工具结果返回，随 tool_result
//! 消息注入对话历史（与读知识库同形态），并落账 skill/activate 事件（可回放）。
//!
//! - **效果侧**：经 ToolContext 的 skill_index 通道取 turn 级索引快照
//!   （process 入口重建，Arc 共享）；正文随工具结果消息保留在历史中
//! - **落账侧**：返回 SkillEventMark::Activate，orchestrator 统一转为
//!   session log 的 skill/activate 事件（工具不改 log，只陈述事实）
//!
//! 生命周期：激活后正文留在对话历史中持续生效，不设卸载机制
//! （历史即上下文，跨轮自然持续；上下文压缩时随旧轮次一起清理）。

use crate::skills::trust::SkillTrust;
use crate::tools::{
    DisplayLink, SkillEventMark, Tool, ToolContext, ToolDisplay, ToolError, ToolOutput,
};

// ═══════════════════════════════════════════════════════
// ActivateSkillTool —— 激活 Skill
// ═══════════════════════════════════════════════════════

/// 从 Skill 目录激活指定 Skill：正文载入 Active 层（当轮上下文生效）。
pub struct ActivateSkillTool;

impl Tool for ActivateSkillTool {
    fn name(&self) -> &str {
        "activate_skill"
    }

    fn description(&self) -> &str {
        "从 Skill 目录激活指定 Skill。激活后其正文注入上下文。目录见系统提示词的 Skill 目录节；任务简单（纯查询/闲聊）时无需激活。工作落入某技能的范围时，先激活、再动手；技能正文是其领域内的专门指引，与一般行为条款冲突时以它为准，底线不因技能改变。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Skill 名"
                }
            },
            "required": ["name"]
        })
    }

    fn display(&self, ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let Some(name) = v
            .get("name")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
        else {
            return ToolDisplay::default();
        };
        // Skill 名 → 源 md 绝对路径（ctx.skill_index，后端解析）
        // 命中仅链接（标签 = 技能名，承担全部信息）；文本不再重复，未命中降级纯文本。
        match ctx.skill_index.as_ref().and_then(|idx| idx.path(name)) {
            Some(p) => ToolDisplay::link(DisplayLink::file(
                p.to_string_lossy().into_owned(),
                name.to_string(),
            )),
            None => ToolDisplay::info(name),
        }
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let name = v["name"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 name 必须是字符串".into()))?;

        // 索引经 ToolContext 通道注入（turn 级快照）；Mock 场景未注入 → 明确报错
        // （与缓存工具 cache_channel 缺失时报「缓存区不可用」同风格）
        let index = ctx
            .skill_index
            .as_ref()
            .ok_or_else(|| ToolError::Execution("Skill 目录不可用".into()))?;

        // 未命中 → 报错 + 目录清单（names 已按 name 排序，可复现）。
        // 风格仿 read_knowledge 未命中：「知识库中没有条目」+ 候选提示
        if index.resolve(name).is_none() {
            let mut msg = format!("Skill 目录中没有「{name}」");
            let names = index.names();
            if !names.is_empty() {
                msg.push_str(&format!("\n可用 Skill：{}", names.join("、")));
            }
            return Err(ToolError::Execution(msg));
        }

        // R-29：激活不授予执行权，但记录不可信来源——后续 run_command 预检
        // 在命令未显式引用包路径时以此为兜底强制 Ask。
        if index.trust(name) == SkillTrust::Untrusted {
            ctx.mark_active_untrusted_skill(name);
        }

        // 命中 → 正文（剥 frontmatter）作为工具结果返回，随 tool_result
        // 消息注入对话历史。激活语义 = 读取正文进上下文（与读知识库同形态）：
        // 正文保留在历史中持续生效，不再有轮末卸载机制。
        // 文件夹包（源为 SKILL.md）：附配套文件目录——模型可按需读取
        // references 等资源（文件域工具已注册，技能目录可读）。
        let content = index.body(name).unwrap_or_default().to_string();
        let mut summary = format!("已激活 Skill「{name}」\n\n{content}");
        if let Some(p) = index.path(name) {
            if p.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                if let Some(dir) = p.parent() {
                    // 仅当包内有 SKILL.md 之外的配套文件时才附目录行
                    // （内置包多为纯 SKILL.md，无资源则不加噪音）
                    let has_resources = std::fs::read_dir(dir)
                        .map(|rd| {
                            rd.flatten()
                                .any(|e| e.file_name().to_string_lossy() != "SKILL.md")
                        })
                        .unwrap_or(false);
                    if has_resources {
                        summary.push_str(&format!(
                            "\n\n（配套文件目录：{}，引用其中资料时按需读取）",
                            dir.display()
                        ));
                    }
                }
            }
        }
        let mut output = ToolOutput::text(summary);
        output.skill_events = vec![SkillEventMark::Activate {
            name: name.to_string(),
        }];
        Ok(output)
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::skills_index::SkillIndex;

    /// 创建唯一临时目录（不引入 tempfile crate，模式照 skills_index.rs）。
    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-skill-tool-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 假 skills 目录：两个有效条目（文件名字母序与 name 序相反，
    /// 验证未命中报错的目录清单按 name 排序）。
    fn fixture(root: &std::path::Path) {
        std::fs::write(
            root.join("alpha.md"),
            "---\nname: 贝塔\ndescription: 条目 B\n---\n\n## 正文 B",
        )
        .unwrap();
        std::fs::write(
            root.join("gamma.md"),
            "---\nname: 阿尔法\ndescription: 条目 A\n---\n\n## 正文 A",
        )
        .unwrap();
    }

    /// 注入 skill_index 的 ToolContext（模拟 process 入口的 turn 级快照注入）。
    fn skill_ctx(index: SkillIndex) -> ToolContext {
        let mut ctx = ToolContext::new();
        ctx.skill_index = Some(Arc::new(index));
        ctx
    }

    #[test]
    fn activate_hit_returns_body_as_tool_result() {
        let root = unique_temp_dir("hit");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();
        let ctx = skill_ctx(idx);
        let tool = ActivateSkillTool;

        let out = tool.execute(&ctx, r#"{"name":"阿尔法"}"#).unwrap();
        // 正文（剥 frontmatter）随工具结果注入历史；不再携带上下文变更指令
        assert_eq!(out.summary, "已激活 Skill「阿尔法」\n\n## 正文 A");
        assert!(out.context_ops.is_empty(), "消息注入不再需要 ContextOp");
        assert_eq!(
            out.skill_events,
            vec![SkillEventMark::Activate {
                name: "阿尔法".into()
            }]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn display_resolves_skill_name_to_source_md_link() {
        let root = unique_temp_dir("display");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();
        let ctx = skill_ctx(idx);
        let tool = ActivateSkillTool;

        // 源 md 链接（命中仅链接；标签 = 技能名，不再重复；文件名≠name）
        let d = tool.display(&ctx, r#"{"name":"阿尔法"}"#);
        assert!(d.text.is_empty());
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["link"]["kind"], "file");
        assert_eq!(json["link"]["label"], "阿尔法");
        let target = json["link"]["target"].as_str().unwrap();
        assert!(target.ends_with("gamma.md"), "got {target}");

        // 未命中 Skill：仅文本、无链接
        let miss = tool.display(&ctx, r#"{"name":"不存在"}"#);
        assert_eq!(miss.text, "不存在");
        assert!(miss.link.is_none());

        // 未注入索引（Mock 场景）：仍降级为纯文本（不 panic）
        let no_ctx = tool.display(&ToolContext::new(), r#"{"name":"阿尔法"}"#);
        assert!(no_ctx.link.is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn activate_miss_lists_catalog_sorted() {
        let root = unique_temp_dir("miss");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();
        let ctx = skill_ctx(idx);
        let tool = ActivateSkillTool;

        let err = tool.execute(&ctx, r#"{"name":"不存在"}"#).unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m)
                if m.contains("Skill 目录中没有「不存在」")
                && m.contains("可用 Skill：贝塔、阿尔法")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn activate_without_index_channel_errors() {
        // Mock 场景（ToolContext::new()）未注入索引通道 → 明确报错
        let ctx = ToolContext::new();
        let err = ActivateSkillTool
            .execute(&ctx, r#"{"name":"阿尔法"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m == "Skill 目录不可用"),
            "got: {err}"
        );
    }

    #[test]
    fn activate_untrusted_skill_marks_turn_but_trusted_does_not() {
        let root = unique_temp_dir("trust");
        fixture(&root);
        std::fs::create_dir_all(root.join("脚本技能")).unwrap();
        std::fs::write(
            root.join("脚本技能").join("SKILL.md"),
            "---\nname: 脚本技能\ndescription: d\nsource: community\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(root.join("脚本技能").join("run.ps1"), "Write-Host hi").unwrap();
        let idx = SkillIndex::load(&root).unwrap();
        let ctx = skill_ctx(idx);

        ActivateSkillTool
            .execute(&ctx, r#"{"name":"阿尔法"}"#)
            .unwrap();
        assert!(ctx.active_untrusted_skill().is_none(), "可信技能不应标记");

        ActivateSkillTool
            .execute(&ctx, r#"{"name":"脚本技能"}"#)
            .unwrap();
        assert_eq!(ctx.active_untrusted_skill().as_deref(), Some("脚本技能"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn activate_non_string_name_is_parse_error() {
        let root = unique_temp_dir("parse");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();
        let ctx = skill_ctx(idx);
        let tool = ActivateSkillTool;

        let err = tool.execute(&ctx, r#"{"name":123}"#).unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("name")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn activate_package_skill_appends_resource_dir_only_when_resources_exist() {
        let root = unique_temp_dir("pkg");
        // 带资源的包：附目录行
        std::fs::create_dir_all(root.join("黑话小抄").join("references")).unwrap();
        std::fs::write(
            root.join("黑话小抄").join("SKILL.md"),
            "---\nname: 黑话小抄\ndescription: 术语\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("黑话小抄").join("references").join("terms.md"),
            "词条",
        )
        .unwrap();
        // 纯 SKILL.md 的包（无资源）：不附行
        std::fs::create_dir_all(root.join("纯条目")).unwrap();
        std::fs::write(
            root.join("纯条目").join("SKILL.md"),
            "---\nname: 纯条目\ndescription: d\n---\n\n## 正文2",
        )
        .unwrap();

        let idx = SkillIndex::load(&root).unwrap();
        let ctx = skill_ctx(idx);
        let out = ActivateSkillTool
            .execute(&ctx, r#"{"name":"黑话小抄"}"#)
            .unwrap();
        assert!(out
            .summary
            .starts_with("已激活 Skill「黑话小抄」\n\n## 正文"));
        assert!(out.summary.contains("配套文件目录"), "有资源的包应附目录行");

        let out2 = ActivateSkillTool
            .execute(&ctx, r#"{"name":"纯条目"}"#)
            .unwrap();
        assert!(
            !out2.summary.contains("配套文件目录"),
            "无资源的包不应附目录行"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
