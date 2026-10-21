//! tools::list_knowledge —— 知识库导航工具
//!
//! 两种模式：
//! - **框架模式**（省略 dir）：返回知识库整体框架（根目录 `_index.md` 原文——
//!   四大领域划分与各领域子目录简介，人工维护的固定架构）。PRTS 一次调用
//!   确定领域框架与各领域介绍，直接定位领域目录。
//! - **下钻模式**（指定 dir）：返回该目录的**下一级**内容（子目录列表 + 直属
//!   条目清单，每条目含标题、简介、节数与大纲），LLM 沿目录层级深入定位条目。
//!
//! 下钻模式非递归是刻意的：递归全量（模块 A 实现）导致高层目录一次倒出
//! 数十条无关条目（实测：列「基础概念」返回 35 条，绝大多数与当前问题无关），
//! 违背逐层导航的定位。框架模式只给目录骨架与简介，不给条目，两者不冲突：
//! 骨架全量给（框架认知优先），条目仍按需下钻。

use std::sync::Arc;

use crate::knowledge::index::KnowledgeIndex;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 知识库目录导航：指定目录的下一级内容（子目录列表 + 直属条目清单）。
pub struct ListKnowledgeTool {
    index: Arc<KnowledgeIndex>,
}

impl ListKnowledgeTool {
    pub fn new(index: Arc<KnowledgeIndex>) -> Self {
        Self { index }
    }
}

impl Tool for ListKnowledgeTool {
    fn name(&self) -> &str {
        "list_knowledge"
    }

    fn description(&self) -> &str {
        "知识库导航。省略 dir 返回知识库整体框架（四大领域与各领域子目录简介），用于确定领域划分与定位领域目录；指定 dir 返回该目录下一级内容：子目录列表与该目录直属的条目清单（标题、简介、节数、大纲），大纲节名可直接用于 read_knowledge 按节阅读"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "dir": {
                    "type": "string",
                    "description": "相对知识库根目录的目录路径，例如 基础概念/移动与阻挡；省略=知识库整体框架"
                }
            }
        })
    }

    /// 展示切片：在知识库的哪里（下钻目录 / 整体框架）。
    /// 此前无实现 → 卡片只剩「PRTS浏览完成」，用户不知道在浏览什么。
    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let dir = serde_json::from_str::<serde_json::Value>(params)
            .ok()
            .and_then(|v| {
                v.get("dir")
                    .and_then(|d| d.as_str())
                    .map(|s| s.trim().trim_matches('/').to_string())
                    .filter(|s| !s.is_empty())
            });
        ToolDisplay::info(match dir {
            Some(d) => format!("知识库 › {d}"),
            None => "知识库框架".to_string(),
        })
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        // dir 可选：省略/空 = 框架模式（四大领域 + 各领域子目录简介）
        let dir = match v.get("dir") {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(d) => d
                .as_str()
                .ok_or_else(|| {
                    ToolError::Parse("参数 dir 必须是字符串（目录名见知识库框架）".into())
                })?
                .trim()
                .trim_matches('/')
                .to_string(),
        };

        // ── 框架模式：省略/空 dir → 返回 _index.md 原文 ──
        if dir.is_empty() {
            let fw = self
                .index
                .framework()
                .ok_or_else(|| ToolError::Execution("知识库框架不可用（缺少 _index.md）".into()))?;
            return Ok(ToolOutput::json(
                serde_json::json!({ "mode": "framework", "content": fw }),
                format!("知识库框架：\n\n{fw}"),
            ));
        }

        // ── 下钻模式：指定目录的下一级内容（子目录 + 直属条目，条目带大纲）──
        match self.index.dir_children(&dir) {
            Some((subdirs, entries)) => {
                let head = format!("目录「{dir}」下：");
                let mut summary = head;
                if !subdirs.is_empty() {
                    summary.push_str(&format!("\n子目录：{}", subdirs.join("、")));
                }
                if !entries.is_empty() {
                    let lines = entries
                        .iter()
                        .map(|(t, b, s)| {
                            // 节数标注：0 = 无分节；1+ 显示节数（大纲随附）
                            let sec = if *s == 0 {
                                "无分节".to_string()
                            } else {
                                format!("{s} 节")
                            };
                            let mut block = format!("- {t}：{b}（{sec}）");
                            if let Some(toc) = self.index.toc(t) {
                                for name in toc {
                                    block.push_str(&format!("\n  - {name}"));
                                }
                            }
                            block
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    summary.push_str(&format!("\n条目：\n{lines}"));
                }
                let entries_json: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|(t, b, s)| {
                        serde_json::json!({
                            "title": t,
                            "brief": b,
                            "sections": s,
                            "toc": self.index.toc(t).unwrap_or(&[]),
                        })
                    })
                    .collect();
                let canonical = serde_json::json!({
                    "mode": "listing",
                    "dir": dir,
                    "subdirs": subdirs,
                    "entries": entries_json,
                });
                Ok(ToolOutput::json(canonical, summary))
            }
            None => Err(ToolError::Execution(format!(
                "目录不存在：{dir}。可省略 dir 查看知识库整体框架，确认目录名。"
            ))),
        }
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 创建唯一临时目录（不引入 tempfile crate）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-lk-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 多级目录知识库：top1（a/b 直接条目 + nested/c）、top2（brief 空串）。
    /// 根目录含 _index.md（框架数据源，人工维护的固定架构）。
    fn fixture(root: &Path) {
        std::fs::create_dir_all(root.join("top1/nested")).unwrap();
        std::fs::create_dir_all(root.join("top2")).unwrap();

        std::fs::write(
            root.join("_index.md"),
            "# 知识库索引\n\n## 四大领域\n\n- **top1**：示例领域一\n- **top2**：示例领域二",
        )
        .unwrap();
        std::fs::write(
            root.join("top1/a.md"),
            "---\ntitle: A1\nbrief: 简介A\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("top1/b.md"),
            "---\ntitle: B1\nbrief: 简介B\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("top1/nested/c.md"),
            "---\ntitle: C1\nbrief: 简介C\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("top2/d.md"),
            "---\ntitle: D1\nbrief: \n---\n\n## 正文",
        )
        .unwrap();
    }

    #[test]
    fn dir_listing_returns_subdirs_and_direct_entries_with_toc() {
        let root = unique_temp_dir("listing");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ListKnowledgeTool::new(idx);

        // 目录命中：子目录列表 + 直属条目（非递归，不含 nested/c.md），
        // 条目附大纲（节名缩进列出，可直接用于 read_knowledge 的 sections）
        let out = tool
            .execute(&ToolContext::new(), r#"{"dir":"top1"}"#)
            .unwrap();
        assert_eq!(
            out.summary,
            "目录「top1」下：\n子目录：nested\n条目：\n- A1：简介A（1 节）\n  - 正文\n- B1：简介B（1 节）\n  - 正文"
        );
        assert_eq!(out.canonical["mode"], "listing");
        assert_eq!(out.canonical["dir"], "top1");
        assert_eq!(out.canonical["subdirs"], serde_json::json!(["nested"]));
        let entries = out.canonical["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            serde_json::json!({ "title": "A1", "brief": "简介A", "sections": 1, "toc": ["正文"] })
        );

        // 叶子目录：无子目录，只列条目
        let out = tool
            .execute(&ToolContext::new(), r#"{"dir":"top1/nested"}"#)
            .unwrap();
        assert_eq!(
            out.summary,
            "目录「top1/nested」下：\n条目：\n- C1：简介C（1 节）\n  - 正文"
        );
        assert_eq!(out.canonical["subdirs"], serde_json::json!([]));

        // brief 空串：冒号后留空
        let out = tool
            .execute(&ToolContext::new(), r#"{"dir":"top2"}"#)
            .unwrap();
        assert_eq!(
            out.summary,
            "目录「top2」下：\n条目：\n- D1：（1 节）\n  - 正文"
        );

        // 首尾 / 归一化后 canonical 显示归一化值
        let out = tool
            .execute(&ToolContext::new(), r#"{"dir":"/top1/"}"#)
            .unwrap();
        assert_eq!(out.canonical["dir"], "top1");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn framework_returns_index_content() {
        let root = unique_temp_dir("rootlist");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ListKnowledgeTool::new(idx);

        // dir 省略 → 知识库整体框架（_index.md 原文：领域划分与各领域介绍）
        let out = tool.execute(&ToolContext::new(), "{}").unwrap();
        assert_eq!(out.canonical["mode"], "framework");
        assert!(out.summary.contains("知识库框架："));
        assert!(out.summary.contains("四大领域"));
        assert!(out.summary.contains("**top1**：示例领域一"));
        assert!(out.summary.contains("**top2**：示例领域二"));
        assert!(out.canonical["content"]
            .as_str()
            .unwrap()
            .contains("## 四大领域"));

        // 显式空串同省略
        let out = tool.execute(&ToolContext::new(), r#"{"dir":""}"#).unwrap();
        assert_eq!(out.canonical["mode"], "framework");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn framework_missing_index_errors() {
        // 知识库缺少 _index.md：框架模式报错（固定架构缺失是配置问题）
        let root = unique_temp_dir("nofw");
        std::fs::create_dir_all(root.join("top1")).unwrap();
        std::fs::write(
            root.join("top1/a.md"),
            "---\ntitle: A1\nbrief: 简介A\n---\n\n## 正文",
        )
        .unwrap();
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ListKnowledgeTool::new(idx);

        let err = tool.execute(&ToolContext::new(), "{}").unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("知识库框架不可用")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_dir_errors_pointing_to_framework() {
        let root = unique_temp_dir("missing");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ListKnowledgeTool::new(idx);

        let err = tool
            .execute(&ToolContext::new(), r#"{"dir":"不存在的目录"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m)
                if m.contains("目录不存在：不存在的目录")
                && m.contains("查看知识库整体框架")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_wrong_type_parse_error() {
        let root = unique_temp_dir("badtype");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ListKnowledgeTool::new(idx);

        // dir 类型错误报 Parse
        let err = tool
            .execute(&ToolContext::new(), r#"{"dir":123}"#)
            .unwrap_err();
        assert!(matches!(&err, ToolError::Parse(_)), "got: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn entries_show_unsectioned_label() {
        // 无 `## ` 标题的条目：清单显示（无分节）
        let root = unique_temp_dir("nosec");
        std::fs::create_dir_all(root.join("top1")).unwrap();
        std::fs::write(
            root.join("top1/plain.md"),
            "---\ntitle: P1\nbrief: 简介P\n---\n\n直接正文，没有分节标题。",
        )
        .unwrap();
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ListKnowledgeTool::new(idx);

        let out = tool
            .execute(&ToolContext::new(), r#"{"dir":"top1"}"#)
            .unwrap();
        assert_eq!(
            out.summary,
            "目录「top1」下：\n条目：\n- P1：简介P（无分节）"
        );
        assert_eq!(
            out.canonical["entries"][0],
            serde_json::json!({ "title": "P1", "brief": "简介P", "sections": 0, "toc": [] })
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn real_knowledge_framework_and_listing() {
        // 真实知识库回归：框架模式返回四大领域；下钻模式返回真实条目清单
        // 且条目带大纲（节名可直接用于 read_knowledge 的 sections）
        //
        // 注意：真实库测试**不得**清理目录（无 remove_dir_all）——
        // 曾因照搬 fixture 清理惯用法导致每次测试运行删除真实知识库
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("content/knowledge");
        let idx = Arc::new(KnowledgeIndex::load(&root).expect("真实知识库加载失败"));
        let tool = ListKnowledgeTool::new(idx);

        // 框架模式：四大领域齐全
        let out = tool.execute(&ToolContext::new(), "{}").unwrap();
        assert_eq!(out.canonical["mode"], "framework");
        for domain in ["基础概念", "底层机制", "数据参考", "特殊模式"] {
            assert!(out.summary.contains(domain), "框架应含领域：{domain}");
        }
        assert!(
            out.summary.contains("现象层") && out.summary.contains("本质层"),
            "框架应含各领域介绍"
        );

        // 下钻模式：表结构目录（叶子目录，条目带大纲）
        let out = tool
            .execute(&ToolContext::new(), r#"{"dir":"数据参考/表结构"}"#)
            .unwrap();
        assert!(out.summary.contains("敌人表结构"), "应含真实条目");
        assert!(out.summary.contains("字段清单"), "大纲节名应随条目附带");
        let entries = out.canonical["entries"].as_array().unwrap();
        assert!(!entries.is_empty());
        assert!(
            entries[0].get("toc").and_then(|t| t.as_array()).is_some(),
            "条目应带 toc 字段"
        );

        // 下钻模式：基础概念/伤害（深层目录）
        let out = tool
            .execute(&ToolContext::new(), r#"{"dir":"基础概念/伤害"}"#)
            .unwrap();
        assert!(out.summary.contains("条目："));
    }
}
