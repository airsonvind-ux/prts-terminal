//! PRTS wiki 标记统一清洗模块。
//!
//! 将 PRTS wiki 文本中常用的模板标记转换为可读纯文本。
//! 各查询工具输出 wiki 来源的文本字段时统一调用 `clean_wiki()`，
//! 避免清洗逻辑分散在多个文件中。
//!
//! ## 处理的标记
//!
//! | 标记                                        | 转换结果          | 示例                                          |
//! |--------------------------------------------|------------------|----------------------------------------------|
//! | `{{popup\|内容=XXX}}`                       | （补充：XXX）      | 天赋、技能、情报                                  |
//! | `{{简易折叠\|折叠文字=T\|内容=C}}`           | 【T】\nC          | 情报（关卡机制）                                 |
//! | `{{ruby\|X\|Y}}`                           | X                | 情报（注音）                                    |
//! | `{{变动数值\|X\|Y}}`                        | X                | 情报（难度变值）                                 |
//! | `'''text'''` / `''text''`                  | text             | 天赋、技能                                     |
//! | `<hr>`                                    | （空行）           | 情报                                          |
//!
//! ## 设计原则
//!
//! - 每个函数独立可测：纯文本变换，不依赖外部状态
//! - `clean_wiki()` 是聚合入口，施加全部清洗规则
//! - 清洗顺序有意义：popup 先处理（可能含嵌套模板），
//!   fold 再处理（内容可能含 popup），最后处理简单标记

// ═══════════════════════════════════════════════════════
// 聚合入口
// ═══════════════════════════════════════════════════════

/// 施加全部 wiki 清洗规则，返回纯文本。
///
/// 清洗顺序：
/// 1. popup（`{{popup|内容=...}}`）
/// 2. 简易折叠（`{{简易折叠|...}}`）
/// 3. ruby（`{{ruby|...}}`）
/// 4. 变动数值（`{{变动数值|...}}`）
/// 5. 粗体/斜体（`'''` `''`）
/// 6. 水平线（`<hr>`）
pub fn clean_wiki(text: &str) -> String {
    let mut result = text.to_string();
    result = clean_popup(&result);
    result = clean_fold(&result);
    result = clean_ruby(&result);
    result = clean_variable(&result);
    result = clean_bold(&result);
    result = clean_hr(&result);
    result
}

// ═══════════════════════════════════════════════════════
// clean_popup —— {{popup|内容=XXX}} → （补充：XXX）
// ═══════════════════════════════════════════════════════

/// 将所有 `{{popup|...内容=XXX...}}` 替换为 `（补充：XXX）`。
///
/// 大小写不敏感（popup / Popup / POPUP 均可）。
/// 支持嵌套 `{{}}`（popup 的内容参数值中可能含其他模板）。
/// 多个 popup 各自独立替换。
pub fn clean_popup(text: &str) -> String {
    let mut result = text.to_string();
    loop {
        let Some(start) = find_template(&result, "popup") else {
            break;
        };
        let Some(end) = find_matching_braces(&result, start) else {
            break;
        };

        // 提取 "内容=" 参数值
        let inner = &result[start + 2..end];
        let content = extract_param_value(inner, "内容");

        let replacement = format!("（补充：{content}）");
        result.replace_range(start..end + 2, &replacement);
    }
    result
}

// ═══════════════════════════════════════════════════════
// clean_fold —— {{简易折叠|折叠文字=T|内容=C}} → 【T】\nC
// ═══════════════════════════════════════════════════════

/// 将所有 `{{简易折叠|...折叠文字=T|内容=C}}` 展开为 `【T】\nC`。
///
/// "内容" 参数的值延续到块结束（`}}`），因为它本身可能含 `|` 字符
/// （嵌套的其他模板）。折叠文字和内容之间的其他参数（如 id、overcss）忽略。
pub fn clean_fold(text: &str) -> String {
    let mut result = text.to_string();
    loop {
        let Some(start) = find_template(&result, "简易折叠") else {
            break;
        };
        let Some(end) = find_matching_braces(&result, start) else {
            break;
        };

        // 整个块（含 {{ 和 }}）
        let block = &result[start..end + 2];

        // 解析参数
        let title = extract_fold_title(block);
        let content = extract_content_param_of_block(block);

        let replacement = if title.is_empty() {
            content
        } else if content.is_empty() {
            format!("【{title}】")
        } else {
            format!("【{title}】\n{content}")
        };

        result.replace_range(start..end + 2, &replacement);
    }
    result
}

// ═══════════════════════════════════════════════════════
// clean_ruby —— {{ruby|X|Y}} → X
// ═══════════════════════════════════════════════════════

/// 将所有 `{{ruby|X|Y}}` 替换为 `X`（去掉注音）。
///
/// ruby 模板不嵌套，直接匹配简单模式。
pub fn clean_ruby(text: &str) -> String {
    clean_simple_template(text, "ruby")
}

// ═══════════════════════════════════════════════════════
// clean_variable —— {{变动数值|X|Y}} → X
// ═══════════════════════════════════════════════════════

/// 将所有 `{{变动数值|X|Y}}` 替换为 `X`（取基准值，丢弃修正）。
pub fn clean_variable(text: &str) -> String {
    clean_simple_template(text, "变动数值")
}

// ═══════════════════════════════════════════════════════
// clean_bold —— ''' / '' → 去除
// ═══════════════════════════════════════════════════════

/// 去除 wiki 粗体（`'''`）和斜体（`''`）标记。
pub fn clean_bold(text: &str) -> String {
    text.replace("'''", "").replace("''", "")
}

// ═══════════════════════════════════════════════════════
// clean_hr —— <hr> → 空行
// ═══════════════════════════════════════════════════════

/// 将 `<hr>` 替换为空行。
pub fn clean_hr(text: &str) -> String {
    text.replace("<hr>", "\n")
}

// ═══════════════════════════════════════════════════════
// 内部辅助函数
// ═══════════════════════════════════════════════════════

/// 查找 "{{模板名" 的起始位置（`{{` 的索引），大小写不敏感。
fn find_template(text: &str, name: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let name_lower = name.to_lowercase();
    let name_bytes = name_lower.as_bytes();
    let min_len = 2 + name_bytes.len(); // {{ + 模板名

    let mut i = 0;
    while i + min_len < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let after = &bytes[i + 2..];
            if after.len() >= name_bytes.len()
                && after[..name_bytes.len()].eq_ignore_ascii_case(name_bytes)
            {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// 从 start（`{{` 起始位置）找到匹配的 `}}`，返回 `}}` 的起始索引。
///
/// 正确处理嵌套的 `{{...}}` 对。
fn find_matching_braces(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth: u32 = 0;
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

/// 寻找简单模板（无嵌套）的 `}}` 结束位置。
///
/// 适用于 ruby、变动数值 等不会嵌套其他 `{{}}` 的模板。
/// 直接找下一个 `}}` 即可。
fn find_simple_template_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = start + 2; // 跳过 {{
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 从模板内部文本中提取指定参数的值。
///
/// `template_inner` 是去掉外层 `{{` 和 `}}` 的模板文本。
/// 值终止于下一个 `|` 或文本末尾。
fn extract_param_value(template_inner: &str, param_name: &str) -> String {
    let search = format!("{param_name}=");
    let Some(pos) = template_inner.find(&search) else {
        return String::new();
    };
    let val_start = pos + search.len();
    let val_slice = &template_inner[val_start..];

    if let Some(pipe) = val_slice.find('|') {
        val_slice[..pipe].trim().to_string()
    } else {
        val_slice.trim().to_string()
    }
}

/// 从简易折叠块中提取"折叠文字"参数。
///
/// `block` 是完整的 `{{简易折叠|...}}` 块（含外层括号）。
fn extract_fold_title(block: &str) -> String {
    // 去掉 {{ 和 }}
    let inner = if block.len() > 4 {
        &block[2..block.len() - 2]
    } else {
        return String::new();
    };
    extract_param_value(inner, "折叠文字")
}

/// 从模板块中提取"内容"参数的值。
///
/// 与 `extract_param_value` 不同：内容参数的值延续到块末尾（`}}`），
/// 因为内容中可能含 `|`（嵌套模板）。这是 PRTS wiki 的约定：
/// 内容总是简易折叠的最后一个参数。
fn extract_content_param_of_block(block: &str) -> String {
    let search = "内容=";
    let Some(pos) = block.find(search) else {
        return String::new();
    };
    let val_start = pos + search.len();

    // 内容值延续到块的倒数第 2 个字符（去掉 `}}`）
    let end = block.len().saturating_sub(2);
    if val_start >= end {
        return String::new();
    }

    block[val_start..end].trim().to_string()
}

/// 清洗简单模板（无嵌套 `{{}}` 的模板），提取第一个 `|` 后的参数值。
///
/// 用于 `{{ruby|X|Y}} → X` 和 `{{变动数值|X|Y}} → X` 的场景。
/// 这些模板的格式为 `{{模板名|第一个参数|后续参数...}}`，
/// 始终取第一个参数作为输出值。
fn clean_simple_template(text: &str, template_name: &str) -> String {
    let mut result = text.to_string();
    loop {
        let Some(start) = find_template(&result, template_name) else {
            break;
        };
        let Some(end) = find_simple_template_end(&result, start) else {
            break;
        };

        let inner = &result[start + 2..end]; // 不含 {{ 和 }}
        let after_name = &inner[template_name.len()..]; // |X|Y 或 | X | Y
        let trimmed = after_name.trim_start_matches('|').trim();

        // 取第一个 | 之前的内容作为输出
        let value = if let Some(pipe) = trimmed.find('|') {
            trimmed[..pipe].trim().to_string()
        } else {
            trimmed.to_string()
        };

        result.replace_range(start..end + 2, &value);
    }
    result
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── clean_popup ──

    #[test]
    fn popup_basic() {
        let input = "前方{{popup|内容=包含以下敌人}}区域";
        let result = clean_popup(input);
        assert_eq!(result, "前方（补充：包含以下敌人）区域");
    }

    #[test]
    fn popup_case_insensitive() {
        let input = "{{Popup|内容=测试}}";
        let result = clean_popup(input);
        assert_eq!(result, "（补充：测试）");
    }

    #[test]
    fn popup_multiple() {
        let input = "{{popup|内容=A}}和{{popup|内容=B}}";
        let result = clean_popup(input);
        assert_eq!(result, "（补充：A）和（补充：B）");
    }

    #[test]
    fn popup_multiline_content() {
        let input = "{{popup|内容=行1\n行2}}";
        let result = clean_popup(input);
        assert_eq!(result, "（补充：行1\n行2）");
    }

    #[test]
    fn popup_no_content_param() {
        // 没有内容=参数的 popup 不匹配（实际数据中不会出现，但防御性处理）
        let input = "{{popup|其他=xxx}}";
        let result = clean_popup(input);
        assert_eq!(result, "（补充：）");
    }

    #[test]
    fn popup_with_other_params() {
        let input = "{{popup|问号大小=15px|内容=环境压力效果}}";
        let result = clean_popup(input);
        assert_eq!(result, "（补充：环境压力效果）");
    }

    #[test]
    fn popup_no_marker() {
        let input = "普通文本没有 popup";
        let result = clean_popup(input);
        assert_eq!(result, input);
    }

    // ── clean_fold ──

    #[test]
    fn fold_basic() {
        let input = "{{简易折叠|id=test|折叠文字=分布情况|内容=地图左上角有残骸}}";
        let result = clean_fold(input);
        assert_eq!(result, "【分布情况】\n地图左上角有残骸");
    }

    #[test]
    fn fold_multiline_content() {
        let input = "{{简易折叠|折叠文字=规则|内容=第1条\n第2条\n第3条}}";
        let result = clean_fold(input);
        assert_eq!(result, "【规则】\n第1条\n第2条\n第3条");
    }

    #[test]
    fn fold_with_hr_in_content() {
        let input = "{{简易折叠|id=hidden|折叠文字=残骸|内容=某些残骸不会出现<hr>}}后续文本";
        let result = clean_fold(input);
        // 注意：clean_fold 不处理 <hr>，那是 clean_hr 的职责
        assert_eq!(result, "【残骸】\n某些残骸不会出现<hr>后续文本");
    }

    #[test]
    fn fold_no_title() {
        let input = "{{简易折叠|id=test|内容=仅内容}}";
        let result = clean_fold(input);
        assert_eq!(result, "仅内容");
    }

    #[test]
    fn fold_no_content() {
        let input = "{{简易折叠|id=test|折叠文字=仅标题}}";
        let result = clean_fold(input);
        assert_eq!(result, "【仅标题】");
    }

    // ── clean_ruby ──

    #[test]
    fn ruby_basic() {
        let input = "{{ruby|丰川祥子|Oblivionis}}";
        let result = clean_ruby(input);
        assert_eq!(result, "丰川祥子");
    }

    #[test]
    fn ruby_multiple_in_text() {
        let input = "{{ruby|祥子|Sakiko}}和{{ruby|初华|Uika}}";
        let result = clean_ruby(input);
        assert_eq!(result, "祥子和初华");
    }

    #[test]
    fn ruby_no_match() {
        let input = "普通文本";
        let result = clean_ruby(input);
        assert_eq!(result, input);
    }

    // ── clean_variable ──

    #[test]
    fn variable_basic() {
        let input = "冷却{{变动数值|7s|+}}";
        let result = clean_variable(input);
        assert_eq!(result, "冷却7s");
    }

    #[test]
    fn variable_multiple() {
        let input = "{{变动数值|10|+}}~{{变动数值|20|+}}";
        let result = clean_variable(input);
        assert_eq!(result, "10~20");
    }

    // ── clean_bold ──

    #[test]
    fn bold_basic() {
        let input = "'''粗体'''和''斜体''";
        let result = clean_bold(input);
        assert_eq!(result, "粗体和斜体");
    }

    #[test]
    fn bold_no_match() {
        let input = "没有标记";
        let result = clean_bold(input);
        assert_eq!(result, input);
    }

    // ── clean_hr ──

    #[test]
    fn hr_basic() {
        let input = "段落1<hr>段落2";
        let result = clean_hr(input);
        assert_eq!(result, "段落1\n段落2");
    }

    // ── clean_wiki 聚合 ──

    #[test]
    fn clean_wiki_combined() {
        let input = "'''注意'''：{{ruby|祥子|Sakiko}}的技能{{变动数值|7s|+}}{{popup|内容=含隐藏效果}}<hr>以上";
        let result = clean_wiki(input);
        assert_eq!(result, "注意：祥子的技能7s（补充：含隐藏效果）\n以上");
    }

    #[test]
    fn clean_wiki_fold_with_hr() {
        // 折叠 + hr 组合：clean_wiki 先展开折叠，再替换 hr
        let input = "{{简易折叠|折叠文字=规则|内容=第一条<hr>第二条}}";
        let result = clean_wiki(input);
        assert_eq!(result, "【规则】\n第一条\n第二条");
    }

    #[test]
    fn clean_wiki_empty() {
        let result = clean_wiki("");
        assert_eq!(result, "");
    }

    #[test]
    fn clean_wiki_no_markup() {
        let input = "纯文本没有任何标记";
        let result = clean_wiki(input);
        assert_eq!(result, input);
    }

    // ── 辅助函数 ──

    #[test]
    fn find_template_basic() {
        let text = "前面{{popup|内容=x}}后面";
        assert_eq!(find_template(text, "popup"), Some(6)); // '前'=3bytes, '面'=3bytes
    }

    #[test]
    fn find_template_case_insensitive() {
        let text = "{{POPUP|内容=x}}";
        assert!(find_template(text, "popup").is_some());
    }

    #[test]
    fn find_template_not_found() {
        assert_eq!(find_template("普通文本", "popup"), None);
    }

    #[test]
    fn find_matching_braces_simple() {
        let text = "{{popup|内容=x}}";
        let end = find_matching_braces(text, 0).unwrap();
        assert_eq!(&text[..end + 2], "{{popup|内容=x}}");
    }

    #[test]
    fn find_matching_braces_nested() {
        let text = "{{outer|{{inner|x}}}}";
        let end = find_matching_braces(text, 0).unwrap();
        assert_eq!(end, text.len() - 2); // 指向最后一个 }} 的起始
    }

    #[test]
    fn extract_param_value_basic() {
        assert_eq!(
            extract_param_value("popup|内容=xxx|颜色=red", "内容"),
            "xxx"
        );
    }

    #[test]
    fn extract_param_value_last_param() {
        // 内容= 是最后一个参数，没有后续 |
        assert_eq!(extract_param_value("popup|内容=xxx", "内容"), "xxx");
    }

    #[test]
    fn extract_param_value_not_found() {
        assert_eq!(extract_param_value("popup|其他=xxx", "内容"), "");
    }
}
