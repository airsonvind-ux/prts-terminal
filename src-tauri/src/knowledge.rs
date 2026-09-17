//! 知识库 —— 条目 frontmatter schema（架构升级 · 模块 7 P0：要点标注）
//!
//! ## frontmatter schema（YAML 子集）
//!
//! ```markdown
//! ---
//! title: 条目名
//! brief: 一句话简介（Catalog 渐进披露用，几十 token）
//! key_points: 要点列表（LLM 读后自写总结的纲；实测中 LLM 易误解的点也写入此清单）
//!   - 要点一
//!   - 要点二
//! ---
//! ```
//!
//! - `title` / `brief`：必填（既有 schema 保留）
//! - `key_points`：可选（模块 7 新增；条目内容由索引器开发时
//!   逐步补充）。Catalog 只取 title+brief，标注点随全文 load 下发——
//!   静态缓存创建（LLM 自写总结）以标注点为纲。
//!
//! 手写 YAML 子集解析（不引入 yaml crate）：覆盖本项目 frontmatter 的
//! 实际形态（标量 + 行内数组 + 块列表）。解析失败返回 None（按无元数据处理，
//! 不阻塞条目加载——索引器/加载器负责告警）。

pub mod index;

/// 条目 frontmatter 元数据。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KnowledgeMeta {
    pub title: String,
    pub brief: String,
    /// 要点列表（LLM 自写总结的纲；实测中 LLM 易误解的点也写入；可选）
    pub key_points: Vec<String>,
}

/// 泛化 frontmatter 解析（YAML 子集）：返回字段名 → 值列表。
///
/// - 标量字段（`key: value`）→ 单元素列表
/// - 行内数组（`key: ["a", "b"]`）与块列表（`key:\n  - a\n  - b`）→ 多元素列表
///
/// 剥离开头 `---` 与紧随的换行（`\n` 或 `\r\n`），返回其后内容；
/// 不以 `---` 开头或后无换行 → None。
///
/// 两个换行风格都兼容：条目文件可能被外部编辑工具写入为 CRLF
/// （实测：key_points 批量写入后一批条目转为 CRLF，旧实现仅认 LF，
/// 导致 frontmatter 解析静默失败）。
fn after_frontmatter_open(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?;
    rest.strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
}

/// 未知字段原样保留（消费方按 schema 提取）。
/// 无 frontmatter 或结构异常返回 None。
pub(crate) fn parse_frontmatter_fields(text: &str) -> Option<Vec<(String, Vec<String>)>> {
    let body = after_frontmatter_open(text)?;
    let end = body.find("\n---")?;
    let yaml = &body[..end];

    let mut fields: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_list_idx: Option<usize> = None;

    for raw_line in yaml.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("- ") {
            // 块列表项：归属于当前列表字段
            if let Some(idx) = current_list_idx {
                fields[idx].1.push(rest.trim().to_string());
            }
            continue;
        }

        // 标量或行内数组字段（非列表字段后重置列表归属）
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();

        // 空值字段：块列表跟随模式（后续 "- " 行归属此字段）
        if value.is_empty() {
            fields.push((key, Vec::new()));
            current_list_idx = Some(fields.len() - 1);
            continue;
        }

        // 行内数组 ["a", "b"]（JSON 数组形态）
        if value.starts_with('[') {
            let inner = value.trim_start_matches('[').trim_end_matches(']');
            let items: Vec<String> = inner
                .split(',')
                .map(|item| {
                    item.trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string()
                })
                .filter(|s| !s.is_empty())
                .collect();
            fields.push((key, items));
            current_list_idx = None;
            continue;
        }

        // 标量
        fields.push((key, vec![value.to_string()]));
        current_list_idx = None;
    }

    if fields.is_empty() {
        return None;
    }
    Some(fields)
}

/// 解析条目文本的 frontmatter（首行 `---` 至第二个 `---` 之间的 YAML 子集）。
/// 无 frontmatter 或解析失败返回 None。
pub fn parse_frontmatter(text: &str) -> Option<KnowledgeMeta> {
    let fields = parse_frontmatter_fields(text)?;
    let mut meta = KnowledgeMeta::default();
    for (key, values) in fields {
        match key.as_str() {
            "title" => meta.title = values.into_iter().next().unwrap_or_default(),
            "brief" => meta.brief = values.into_iter().next().unwrap_or_default(),
            "key_points" => meta.key_points = values,
            _ => {} // 未知字段忽略（向前兼容；旧 schema 的 pitfalls 字段在此被忽略）
        }
    }
    if meta.title.is_empty() && meta.brief.is_empty() {
        return None;
    }
    Some(meta)
}

/// 去掉 frontmatter 区（首行 `---` 至第二个 `---`）后的正文。
/// 判定规则与 parse_frontmatter 一致（LF/CRLF 兼容）；无 frontmatter 时返回全文。
/// 索引层（节数统计）与 read_knowledge 工具（节解析）共用。
pub(crate) fn body_after_frontmatter(content: &str) -> &str {
    match after_frontmatter_open(content) {
        Some(rest) => match rest.find("\n---") {
            Some(end) => {
                let body = &rest[end + 4..];
                // 剥掉 frontmatter 结束行后的全部前导换行（LF/CRLF 等价归一）：
                // 正文就是正文，不带残留空行。CRLF 下 end+4 指向结束标记
                // 之后的 \r，不剥则正文带前导 \r\n。
                body.trim_start_matches(['\r', '\n'])
            }
            None => content,
        },
        None => content,
    }
}

/// 正文中所有 `## ` 节标题名（去 `## ` 前缀并 trim，按出现顺序）。
/// `### ` 及更深标题归入所属节，不是独立节，不在此列；frontmatter 区不参与。
/// 目录清单（list_knowledge）据此输出条目大纲，read_knowledge 的节选
/// 匹配也以节名为准（两侧共用同一规则，保证清单节名可直接用于阅读）。
pub(crate) fn section_names(content: &str) -> Vec<String> {
    body_after_frontmatter(content)
        .lines()
        .filter(|l| l.starts_with("## "))
        .map(|l| l.trim_start_matches("## ").trim().to_string())
        .collect()
}

/// 统计正文中的 `## ` 节标题数（0 = 无分节）。
/// 目录清单（list_knowledge）据此标注条目长短，LLM 决定是否先读大纲。
pub(crate) fn count_sections(content: &str) -> usize {
    section_names(content).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_and_brief() {
        let text = "---\ntitle: Buff机制基础\nbrief: Buff 是最小效果单位\n---\n\n## 正文";
        let meta = parse_frontmatter(text).unwrap();
        assert_eq!(meta.title, "Buff机制基础");
        assert_eq!(meta.brief, "Buff 是最小效果单位");
        assert!(meta.key_points.is_empty());
    }

    #[test]
    fn parses_block_lists_for_key_points() {
        let text = "---\ntitle: 伤害计算\ntitle2: 忽略\nbrief: 公式\nkey_points:\n  - 攻击力 - 防御力\n  - 保底 5%\n---";
        let meta = parse_frontmatter(text).unwrap();
        assert_eq!(meta.key_points, vec!["攻击力 - 防御力", "保底 5%"]);
    }

    #[test]
    fn parses_inline_arrays_and_ignores_legacy_pitfalls() {
        // pitfalls 为旧 schema 字段（已并入 key_points）：解析时按未知字段忽略
        let text = "---\ntitle: 阻挡\nbrief: b\nkey_points: [\"一格一敌\", \"重量影响\"]\npitfalls: [\"旧字段\"]\n---";
        let meta = parse_frontmatter(text).unwrap();
        assert_eq!(meta.key_points, vec!["一格一敌", "重量影响"]);
    }

    #[test]
    fn parses_crlf_frontmatter() {
        // CRLF 换行（外部编辑工具写入）：解析与 LF 等价
        // （实测：key_points 批量写入后一批条目转为 CRLF 被静默跳过）
        let text = "---\r\ntitle: 阻挡\r\nbrief: b\r\nkey_points:\r\n  - 一格一敌\r\n---\r\n\r\n## 正文";
        let meta = parse_frontmatter(text).unwrap();
        assert_eq!(meta.title, "阻挡");
        assert_eq!(meta.key_points, vec!["一格一敌"]);
        assert_eq!(body_after_frontmatter(text), "## 正文");
    }

    #[test]
    fn no_frontmatter_returns_none() {
        assert!(parse_frontmatter("## 直接正文").is_none());
        assert!(parse_frontmatter("").is_none());
        // 有 frontmatter 但空 → None
        assert!(parse_frontmatter("---\n---").is_none());
    }

    #[test]
    fn unknown_fields_ignored() {
        let text = "---\ntitle: 索敌\nbrief: b\nfuture_field: x\n---";
        let meta = parse_frontmatter(text).unwrap();
        assert_eq!(meta.title, "索敌");
    }

    #[test]
    fn section_names_returns_trimmed_names_in_order() {
        // 节名 = 正文 `## ` 标题去前缀 trim，按出现顺序；`### ` 归入所属节不列
        let text = "---\ntitle: 多节\nbrief: b\n---\n\n## 第一节  \n\n内容\n\n## 第二节\n\n### 子标题\n\n内容\n\n普通段落";
        assert_eq!(section_names(text), vec!["第一节", "第二节"]);
        // 无分节：正文无 `## ` 标题 → 空
        assert!(section_names("---\ntitle: 无节\nbrief: b\n---\n\n直接正文").is_empty());
        // 无 frontmatter：全文作为正文参与扫描
        assert_eq!(section_names("## 直接节\n\n内容"), vec!["直接节"]);
        // count_sections 与 section_names 同源
        assert_eq!(count_sections(text), 2);
    }

    #[test]
    fn real_entry_parses_without_annotations() {
        // 现有条目只有 title+brief：解析兼容（标注字段为空）
        let text = "---\ntitle: Buff机制基础\nbrief: Buff 是最小效果单位：叠加规则、持续时间类型、抵抗机制\n---\n\n## Buff 是什么";
        let meta = parse_frontmatter(text).unwrap();
        assert_eq!(meta.title, "Buff机制基础");
        assert!(meta.key_points.is_empty());
    }
}
