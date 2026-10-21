//! tools::knowledge —— 知识库读取工具（read_knowledge）
//!
//! 按条目名（frontmatter title）从 KnowledgeIndex 定位条目文件，只负责
//! **阅读**：正文全文或指定小节。大纲（节名列表）由 list_knowledge 在
//! 条目清单中随附（决策信息归导航，内容获取归阅读）；节名两侧同规则，
//! list_knowledge 返回的节名可直接用于本工具的 sections 参数。
//!
//! 条目要点（frontmatter key_points）随内容返回：作为 PRTS 阅读后整理
//! 静态缓存（再学习总结）时的纠偏锚点，不在导航层提前给出。
//!
//! 未命中条目时给出模糊候选提示。

use std::sync::Arc;

use crate::knowledge::index::KnowledgeIndex;
use crate::tools::{DisplayLink, Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 按条目名读取知识库条目的完整内容。
pub struct ReadKnowledgeTool {
    index: Arc<KnowledgeIndex>,
}

impl ReadKnowledgeTool {
    pub fn new(index: Arc<KnowledgeIndex>) -> Self {
        Self { index }
    }
}

impl Tool for ReadKnowledgeTool {
    fn name(&self) -> &str {
        "read_knowledge"
    }

    fn description(&self) -> &str {
        "按条目名读取知识库条目内容。省略 sections 返回正文全文；sections=[节名列表] 只读取指定小节，节名见 list_knowledge 返回的大纲。未命中会返回候选提示；条目要点（如有）随内容附带"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "entry": {
                    "type": "string",
                    "description": "知识条目名"
                },
                "sections": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "节名数组，可选。省略或空数组=返回正文全文；[\"节A\",\"节B\"]=只读取这些小节（节名见 list_knowledge 返回的大纲）"
                }
            },
            "required": ["entry"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let Some(entry) = v
            .get("entry")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
        else {
            return ToolDisplay::default();
        };
        // 逻辑条目名 → 实际 md 绝对路径（KnowledgeIndex，后端解析）
        // 命中仅链接（标签 = 文件名，承担全部信息）；文本不再重复条目名，
        // 未命中降级纯文本（见 None 分支）。
        match self.index.entry_path(entry) {
            Some(p) => {
                let target = p.to_string_lossy().into_owned();
                let label = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.to_string());
                ToolDisplay::link(DisplayLink::file(target, label))
            }
            None => ToolDisplay::info(entry),
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        let entry = v["entry"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 entry 必须是字符串".into()))?;

        // ── 0. 解析可选参数 sections（省略/空 = 全文；节名列表 = 节选）──
        let sections: Vec<String> = match v.get("sections") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(arr) => arr
                .as_array()
                .ok_or_else(|| ToolError::Parse("参数 sections 必须是字符串数组".into()))?
                .iter()
                .map(|s| {
                    s.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| ToolError::Parse("参数 sections 的元素必须是字符串".into()))
                })
                .collect::<Result<Vec<_>, _>>()?,
        };

        // ── 1. 索引查条目；未命中 → 报错 + 候选提示 ──
        if self.index.resolve(entry).is_none() {
            let mut msg = format!("知识库中没有条目「{entry}」");
            let suggestions = self.index.suggest(entry);
            if !suggestions.is_empty() {
                msg.push_str(&format!("\n可能指：{}", suggestions.join("、")));
            }
            return Err(ToolError::Execution(msg));
        }

        // ── 2. 读取全文（resolve 已确认命中，entry_path 必为 Some）──
        let entry_path = self
            .index
            .entry_path(entry)
            .ok_or_else(|| ToolError::Execution(format!("知识库中没有条目「{entry}」")))?;
        let content = std::fs::read_to_string(&entry_path)
            .map_err(|e| ToolError::Execution(format!("读取条目失败：{e}")))?;
        // canonical 只携带相对路径（resolve 即相对 root 的路径）——
        // 绝对路径属实现细节，不进任何 LLM/缓存可见输出（去路径化契约）
        let rel_path = self
            .index
            .resolve(entry)
            .unwrap_or_else(|| std::path::Path::new(entry))
            .to_string_lossy()
            .to_string();

        // 条目要点（frontmatter key_points）：LLM 再学习整理静态缓存时的
        // 纠偏锚点（静态缓存以标注点为纲，见 cache.rs 角色说明）。
        // 大纲响应随附下发；节选分支不携带。无 frontmatter / 无要点 → 空。
        let key_points = crate::knowledge::parse_frontmatter(&content)
            .map(|meta| meta.key_points)
            .unwrap_or_default();

        // ── 3. 分支：全文 / 节选（省略 sections = 全文；大纲已由 list_knowledge 承担）──
        // 正文 = 去掉 frontmatter 区；节解析只针对正文（frontmatter 不参与）
        let body = crate::knowledge::body_after_frontmatter(&content);

        // ── 全文：正文全文 + 条目要点（如有）──
        if sections.is_empty() {
            let mut s = format!("条目「{entry}」全文：\n{body}");
            if !key_points.is_empty() {
                s.push_str(&format!(
                    "\n\n条目要点：\n{}",
                    format_key_points(&key_points)
                ));
            }
            let mut canonical = serde_json::json!({
                "entry": entry,
                "path": rel_path,
                "content": body,
            });
            if !key_points.is_empty() {
                canonical["key_points"] = serde_json::json!(key_points);
            }
            return Ok(ToolOutput::json(canonical, s));
        }

        // ── 节选：按条目内出现顺序拼接命中的节（非传参顺序）──
        match extract_sections(body, &sections) {
            Ok(parts) => {
                let names: Vec<String> = parts.iter().map(|(n, _)| n.clone()).collect();
                // 节内容保留原文（行内空白不动），仅去除每节尾随换行，
                // 再以空行连接——保证"节与节之间空一行"而不叠加节尾空行
                let joined = parts
                    .iter()
                    .map(|(_, c)| c.trim_end_matches('\n'))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let mut s = format!("（条目「{entry}」节选）\n{joined}");
                if !key_points.is_empty() {
                    s.push_str(&format!(
                        "\n\n条目要点：\n{}",
                        format_key_points(&key_points)
                    ));
                }
                let mut canonical = serde_json::json!({
                    "entry": entry,
                    "path": rel_path,
                    "sections": names,
                    "content": joined,
                });
                if !key_points.is_empty() {
                    canonical["key_points"] = serde_json::json!(key_points);
                }
                Ok(ToolOutput::json(canonical, s))
            }
            Err(missing) => {
                // 未命中：列出未命中的节名 + 附完整大纲（让 LLM 看到全部可用节名）。
                // 与 list_knowledge 大纲同规则（section_names 去前缀节名），
                // 保证清单节名与阅读节名两侧形态完全一致
                let toc = crate::knowledge::section_names(body);
                let mut msg = format!("条目「{entry}」未找到节：{}", missing.join("、"));
                if toc.is_empty() {
                    msg.push_str("（该条目无分节，请直接读取全文）");
                } else {
                    msg.push_str(&format!("\n该条目可用节：\n{}", toc.join("\n")));
                }
                Err(ToolError::Execution(msg))
            }
        }
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }
}

// ═══════════════════════════════════════════════════════
// 节解析辅助（手写行扫描，不引入 regex 依赖）
// ═══════════════════════════════════════════════════════

/// 要点列表渲染为 markdown 列表行（`- 要点`，逐行）。
/// 供大纲 summary 的「条目要点」段使用。
fn format_key_points(key_points: &[String]) -> String {
    key_points
        .iter()
        .map(|k| format!("- {k}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 按节名提取节内容：`(节名, 该节原文)`，按条目内出现顺序返回。
/// 某一节内容 = 该节标题行起，到下一个 `## ` 标题行（或文件末尾）为止的原文
/// （含中间所有行，不 trim 内容，`### ` 归入所属节）。
/// 任一节名未命中 → `Err(未命中的节名列表)`。
fn extract_sections(body: &str, names: &[String]) -> Result<Vec<(String, String)>, Vec<String>> {
    // 扫描所有节标题：行号 + 节名（去掉 `## ` 前缀后 trim 后的值）
    let lines: Vec<&str> = body.lines().collect();
    let headings: Vec<(String, usize)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            l.strip_prefix("## ")
                .map(|rest| (rest.trim().to_string(), i))
        })
        .collect();

    // 先整体校验：任一节名未命中 → 整次请求失败（大纲由工具层补充）
    let missing: Vec<String> = names
        .iter()
        .filter(|n| !headings.iter().any(|(t, _)| t == *n))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(missing);
    }

    // 按条目内出现顺序拼接命中的节（重复请求同一节只输出一次）
    let mut out: Vec<(String, String)> = Vec::new();
    for (i, (title, start)) in headings.iter().enumerate() {
        if !names.iter().any(|n| n == title) {
            continue;
        }
        let end = headings.get(i + 1).map(|(_, s)| *s).unwrap_or(lines.len());
        out.push((title.clone(), lines[*start..end].join("\n")));
    }
    Ok(out)
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
            std::env::temp_dir().join(format!("prts-rk-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 临时知识库：两个条目 + 一个无 frontmatter 文件。
    /// buff.md 带 key_points（大纲随附要点段的 fixture）；damage.md 不带
    /// （无要点行为逐字节不变的另一侧验证，见 fixture_long）。
    fn fixture(root: &Path) {
        std::fs::create_dir_all(root.join("sub1")).unwrap();
        std::fs::write(
            root.join("sub1/buff.md"),
            "---\ntitle: Buff机制基础\nbrief: Buff 是最小效果单位\nkey_points:\n  - Buff 是最小效果单位\n  - 同类效果取最高\n---\n\n## Buff 是什么\n\nBuff 是效果付与的最小单位。",
        )
        .unwrap();
        std::fs::write(
            root.join("sub1/damage.md"),
            "---\ntitle: 伤害计算基本公式\nbrief: 公式\n---\n\n## 公式\n\n攻击力 - 防御力。",
        )
        .unwrap();
        std::fs::write(root.join("sub1/nofront.md"), "## 无元数据").unwrap();
    }

    #[test]
    fn display_entry_resolves_to_md_file_link() {
        let root = unique_temp_dir("display");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        // 实际 md 链接（命中仅链接；标签 = 文件名，不再重复条目名）
        let d = tool.display(&ToolContext::new(), r#"{"entry":"Buff机制基础"}"#);
        assert!(d.text.is_empty());
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["link"]["kind"], "file");
        assert_eq!(json["link"]["label"], "buff.md");
        let target = json["link"]["target"].as_str().unwrap();
        assert!(target.ends_with("buff.md"), "got {target}");

        // 未命中条目：仅文本、无链接（不报错）
        let miss = tool.display(&ToolContext::new(), r#"{"entry":"不存在"}"#);
        assert_eq!(miss.text, "不存在");
        assert!(miss.link.is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_default_returns_full_with_key_points() {
        let root = unique_temp_dir("hit");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        // 有分节条目：省略 sections 返回正文全文（大纲已由 list_knowledge
        // 承担），并随附 frontmatter 的 key_points（阅读后辅助理解、整理
        // 静态缓存时的纠偏锚点，不在导航层提前给出）
        let out = tool
            .execute(&ToolContext::new(), r#"{"entry":"Buff机制基础"}"#)
            .unwrap();
        assert_eq!(
            out.summary,
            "条目「Buff机制基础」全文：\n## Buff 是什么\n\nBuff 是效果付与的最小单位。\n\n条目要点：\n- Buff 是最小效果单位\n- 同类效果取最高"
        );
        assert_eq!(out.canonical["entry"], "Buff机制基础");
        assert_eq!(
            out.canonical["content"],
            "## Buff 是什么\n\nBuff 是效果付与的最小单位。"
        );
        assert_eq!(
            out.canonical["key_points"],
            serde_json::json!(["Buff 是最小效果单位", "同类效果取最高"])
        );
        let path = out.canonical["path"].as_str().unwrap();
        assert!(path.ends_with("buff.md"), "got: {path}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_miss_errors_with_suggestions() {
        let root = unique_temp_dir("miss");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        // 未命中 + 有候选
        let err = tool
            .execute(&ToolContext::new(), r#"{"entry":"Buff"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m)
                if m.contains("知识库中没有条目「Buff」")
                && m.contains("可能指：Buff机制基础")),
            "got: {err}"
        );

        // 未命中 + 无候选
        let err = tool
            .execute(&ToolContext::new(), r#"{"entry":"完全不存在的条目"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m) if m.contains("知识库中没有条目「完全不存在的条目」") && !m.contains("可能指")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_missing_entry_param_parse_error() {
        let root = unique_temp_dir("param");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        let err = tool.execute(&ToolContext::new(), r#"{}"#).unwrap_err();
        assert!(matches!(&err, ToolError::Parse(_)), "got: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 节级阅读 fixture：多节条目（节顺序故意与常用阅读顺序相反，
    /// 验证按条目内顺序输出）+ 无分节条目。
    fn fixture_sections(root: &Path) {
        std::fs::create_dir_all(root.join("sub1")).unwrap();
        std::fs::write(
            root.join("sub1/multi.md"),
            "---\ntitle: 多节条目\nbrief: 多节示例\n---\n\n## 第三节内容\n\nC 正文。\n\n## 第一节内容\n\nA 正文。\n\n### 子标题\n\n子内容。\n\n## 第二节内容\n\nB 正文。",
        )
        .unwrap();
        std::fs::write(
            root.join("sub1/plain.md"),
            "---\ntitle: 无分节条目\nbrief: 无标题\n---\n\n直接正文，没有分节标题。",
        )
        .unwrap();
    }

    // （_toc 哨兵已删除：大纲由 list_knowledge 条目清单承担，本工具只做阅读）

    #[test]
    fn read_knowledge_single_section_omits_frontmatter() {
        let root = unique_temp_dir("single");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"entry":"Buff机制基础","sections":["Buff 是什么"]}"#,
            )
            .unwrap();
        assert_eq!(
            out.summary,
            "（条目「Buff机制基础」节选）\n## Buff 是什么\n\nBuff 是效果付与的最小单位。\n\n条目要点：\n- Buff 是最小效果单位\n- 同类效果取最高"
        );
        // 节选不返回 frontmatter；key_points 随阅读返回附带（全文/节选一致，
        // 不在导航层提前给出，也绝不在阅读层缺失）
        assert!(
            !out.summary.contains("---"),
            "节选不应含 frontmatter 分隔线"
        );
        assert!(
            !out.summary.contains("title:"),
            "节选不应含 frontmatter 字段"
        );
        // canonical
        assert_eq!(out.canonical["entry"], "Buff机制基础");
        assert_eq!(
            out.canonical["sections"],
            serde_json::json!(["Buff 是什么"])
        );
        assert_eq!(
            out.canonical["content"],
            "## Buff 是什么\n\nBuff 是效果付与的最小单位。"
        );
        assert_eq!(
            out.canonical["key_points"],
            serde_json::json!(["Buff 是最小效果单位", "同类效果取最高"])
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_multi_sections_in_document_order_keep_h3() {
        let root = unique_temp_dir("multi");
        fixture_sections(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        // 传参顺序与文档顺序相反 → 输出按条目内出现顺序（fixture 中
        // 第三节内容在文档最前）；`### 子标题` 归入"第一节内容"，节内容不被截断
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"entry":"多节条目","sections":["第二节内容","第三节内容","第一节内容"]}"#,
            )
            .unwrap();
        let expected = concat!(
            "（条目「多节条目」节选）\n",
            "## 第三节内容\n",
            "\n",
            "C 正文。\n",
            "\n",
            "## 第一节内容\n",
            "\n",
            "A 正文。\n",
            "\n",
            "### 子标题\n",
            "\n",
            "子内容。\n",
            "\n",
            "## 第二节内容\n",
            "\n",
            "B 正文。",
        );
        assert_eq!(out.summary, expected);
        // canonical 节名列表 = 条目内顺序
        assert_eq!(
            out.canonical["sections"],
            serde_json::json!(["第三节内容", "第一节内容", "第二节内容"])
        );
        assert_eq!(
            out.canonical["content"],
            out.summary.trim_start_matches("（条目「多节条目」节选）\n")
        );
        // 节选不包含 frontmatter
        assert!(!out.summary.contains("---"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_section_miss_errors_with_toc() {
        let root = unique_temp_dir("misssec");
        fixture_sections(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        // 未命中 → 列出未命中节名 + 附完整大纲（含全部可用节名）
        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"entry":"多节条目","sections":["不存在的节"]}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m)
                if m.contains("未找到节：不存在的节")
                && m.contains("第三节内容")
                && m.contains("第一节内容")
                && m.contains("第二节内容")),
            "got: {err}"
        );

        // 无分节条目：任何节名都未命中，提示无分节
        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"entry":"无分节条目","sections":["任意节"]}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Execution(m)
                if m.contains("未找到节：任意节") && m.contains("无分节")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_empty_sections_equals_full() {
        let root = unique_temp_dir("empty");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        // 空数组 = 省略 sections = 正文全文（有分节条目也返回全文，
        // 大纲决策信息在 list_knowledge）
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"entry":"Buff机制基础","sections":[]}"#,
            )
            .unwrap();
        assert!(out.summary.contains("条目「Buff机制基础」全文"));
        assert!(out.summary.contains("Buff 是效果付与的最小单位。"));
        assert!(out.canonical.get("sections").is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_sections_wrong_type_parse_error() {
        let root = unique_temp_dir("badsec");
        fixture(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        let err = tool
            .execute(
                &ToolContext::new(),
                r#"{"entry":"Buff机制基础","sections":"Buff 是什么"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("sections 必须是字符串数组")),
            "got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 长条目 fixture：分节长条目 + 无分节长条目（均超过 FULL_ENTRY_CHARS）。
    fn fixture_long(root: &Path) {
        std::fs::create_dir_all(root.join("sub1")).unwrap();
        let pad = "内容。".repeat(600); // 1800 字符，超过阈值 1500
        std::fs::write(
            root.join("sub1/long.md"),
            format!(
                "---\ntitle: 长条目\nbrief: 长\n---\n\n## 第一节\n\n{pad}\n\n## 第二节\n\n结束。"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("sub1/longplain.md"),
            format!("---\ntitle: 长无分节\nbrief: 长\n---\n\n{pad}"),
        )
        .unwrap();
    }

    #[test]
    fn read_knowledge_default_returns_full_regardless_of_length() {
        // 省略 sections 返回正文全文，与条目长短/是否分节无关（固定契约；
        // 大纲决策信息已由 list_knowledge 条目清单承担，本工具只做阅读）
        let root = unique_temp_dir("downgrade");
        fixture_long(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        let out = tool
            .execute(&ToolContext::new(), r#"{"entry":"长条目"}"#)
            .unwrap();
        // 无 key_points 条目：不带「条目要点」段
        assert!(out.summary.starts_with("条目「长条目」全文："));
        assert!(out.summary.contains("## 第一节"));
        assert!(
            out.summary.contains("结束。"),
            "省略 sections 应返回全文正文"
        );
        assert!(
            out.canonical.get("key_points").is_none(),
            "无要点不加 key_points 字段"
        );
        assert!(
            !out.summary.contains("条目要点"),
            "无 key_points 不带要点段"
        );
        assert!(
            out.canonical.get("sections").is_none(),
            "全文模式不带 sections 字段"
        );

        // 按节读取可用
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"entry":"长条目","sections":["第二节"]}"#,
            )
            .unwrap();
        assert!(out.summary.contains("## 第二节"));
        assert!(out.summary.contains("结束。"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_knowledge_unsectioned_returns_full() {
        // 无分节条目没有节可读：省略 sections 返回全文
        let root = unique_temp_dir("longplain");
        fixture_long(&root);
        let idx = Arc::new(KnowledgeIndex::load(&root).unwrap());
        let tool = ReadKnowledgeTool::new(idx);

        let out = tool
            .execute(&ToolContext::new(), r#"{"entry":"长无分节"}"#)
            .unwrap();
        assert!(out.summary.contains("内容。"));
        assert!(!out.summary.contains("大纲"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
