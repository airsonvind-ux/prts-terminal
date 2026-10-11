//! 查询工具共享格式化模块
//!
//! query_operator / query_token / query_trap 三个工具共用的数据模型、
//! 数值计算辅助函数和格式化函数。避免逐文件重复。
//!
//! ## 内容
//!
//! - 数据模型：ElitePhase, Keyframe, Skill, SpData, SkillDetail,
//!   Talent, TalentAlt, TalentCondition, ComputedStats
//! - 辅助函数：parse_json, split_digits, parse_chinese_or_digit,
//!   find_bounding_keyframes, collect_attr_names
//! - 格式化函数：format_stats, format_skill_expanded,
//!   format_talents, level_label, pick_talent_alt

use std::collections::HashMap;

use serde::Deserialize;

// ═══════════════════════════════════════════════════════
// 常量
// ═══════════════════════════════════════════════════════

/// 属性输出固定顺序（与 format_stats 共享）。
pub(crate) const ATTR_ORDER: &[&str] = &[
    "生命上限", "攻击", "防御", "法术抗性",
    "部署费用", "阻挡数", "再部署时间", "攻击间隔",
];

// ═══════════════════════════════════════════════════════
// 数据模型
// ═══════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub(crate) struct ElitePhase {
    #[serde(rename = "范围ID")]
    pub(crate) range_id: Option<String>,
    #[serde(rename = "最高等级")]
    pub(crate) max_level: u32,
    #[serde(rename = "关键帧")]
    pub(crate) keyframes: Vec<Keyframe>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct Keyframe {
    #[serde(rename = "等级")]
    pub(crate) level: u32,
    #[serde(rename = "属性")]
    pub(crate) attrs: HashMap<String, f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Skill {
    #[serde(rename = "技能ID")]
    #[allow(dead_code)] // 已解析但未输出，待后续轮次决定去留
    pub(crate) skill_id: Option<String>,
    #[serde(rename = "技能名")]
    pub(crate) name: String,
    #[serde(rename = "触发类型")]
    pub(crate) trigger_type: Option<String>,
    #[serde(rename = "回复类型")]
    pub(crate) sp_type: Option<String>,
    #[serde(rename = "持续类型")]
    pub(crate) duration_type: Option<String>,
    #[serde(rename = "持续时间")]
    #[allow(dead_code)] // 已解析但未输出，待后续轮次决定去留
    pub(crate) duration: Option<f64>,
    #[serde(rename = "各级详情")]
    pub(crate) details: Vec<SkillDetail>,
    pub(crate) 备注: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SpData {
    #[serde(rename = "可充能次数")]
    pub(crate) charges: Option<u32>,
    #[serde(rename = "技力消耗")]
    pub(crate) sp_cost: Option<u32>,
    #[serde(rename = "初始技力")]
    pub(crate) init_sp: Option<u32>,
    #[serde(rename = "技力回复速度")]
    #[allow(dead_code)] // 已解析但未输出，待后续轮次决定去留
    pub(crate) sp_recovery: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SkillDetail {
    #[serde(rename = "等级")]
    pub(crate) level: u32,
    #[serde(rename = "范围ID")]
    pub(crate) range_id: Option<String>,
    #[serde(rename = "描述")]
    pub(crate) description: Option<String>,
    pub(crate) 持续时间: Option<f64>,
    #[serde(rename = "SP")]
    pub(crate) sp: Option<SpData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Talent {
    #[allow(dead_code)] // 已解析但未输出，待后续轮次决定去留
    pub(crate) 序号: Option<String>,
    #[serde(rename = "天赋名")]
    pub(crate) name: String,
    pub(crate) 备注: Option<String>,
    #[serde(rename = "备选")]
    pub(crate) alternatives: Vec<TalentAlt>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TalentAlt {
    #[serde(rename = "描述")]
    pub(crate) description: Option<String>,
    #[serde(rename = "条件")]
    pub(crate) condition: TalentCondition,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TalentCondition {
    #[serde(rename = "精英化阶段")]
    pub(crate) elite: u32,
    #[serde(rename = "潜能等级")]
    pub(crate) potential: u32,
}

#[derive(Debug, Default)]
pub(crate) struct ComputedStats {
    pub(crate) attrs: HashMap<String, (f64, f64, f64, f64)>,
    pub(crate) atk_speed_bonus: f64,
    pub(crate) atk_speed_sources: Vec<String>,
    pub(crate) base_atk_interval: f64,
}

// ═══════════════════════════════════════════════════════
// 辅助函数
// ═══════════════════════════════════════════════════════

/// 从 JSON 字符串解析类型的泛型辅助。
pub(crate) fn parse_json<T: serde::de::DeserializeOwned>(
    row: &HashMap<String, String>,
    column: &str,
) -> Option<T> {
    let raw = row.get(column)?;
    if raw.is_empty() || raw == "null" {
        return None;
    }
    serde_json::from_str(raw).ok()
}

/// 将字符串拆分为头部（中文/字母）和尾部（数字/满/级）。
pub(crate) fn split_digits<'a>(s: &'a str) -> (&'a str, &'a str) {
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() || c == '满' || c == '级' {
            return (&s[..i], &s[i..]);
        }
    }
    (s, "")
}

/// 中文数字 / 阿拉伯数字 → u32。
pub(crate) fn parse_chinese_or_digit(s: &str) -> Option<u32> {
    match s {
        "零" | "0" => Some(0),
        "一" => Some(1), "二" => Some(2), "三" => Some(3),
        "四" => Some(4), "五" => Some(5), "六" => Some(6),
        _ => s.parse::<u32>().ok(),
    }
}

/// 在 keyframes 中找到包围给定等级的两个关键帧（lo ≤ level ≤ hi）。
pub(crate) fn find_bounding_keyframes<'a>(
    keyframes: &'a [Keyframe],
    level: u32,
) -> (&'a Keyframe, &'a Keyframe) {
    if keyframes.is_empty() {
        static DUMMY: std::sync::LazyLock<Keyframe> = std::sync::LazyLock::new(|| Keyframe {
            level: 1,
            attrs: HashMap::new(),
        });
        return (&DUMMY, &DUMMY);
    }

    let lo = keyframes
        .iter()
        .rev()
        .find(|kf| kf.level <= level)
        .unwrap_or(&keyframes[0]);

    let hi = keyframes
        .iter()
        .find(|kf| kf.level >= level)
        .unwrap_or(keyframes.last().unwrap());

    (lo, hi)
}

/// 收集 keyframe 中实际存在的所有属性名，按 ATTR_ORDER 排序。
pub(crate) fn collect_attr_names(keyframes: &[Keyframe]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for kf in keyframes {
        for attr in kf.attrs.keys() {
            let a = attr.to_string();
            if !names.contains(&a) {
                names.push(a);
            }
        }
    }
    names.sort_by(|a, b| {
        let ai = ATTR_ORDER.iter().position(|x| x == a).unwrap_or(usize::MAX);
        let bi = ATTR_ORDER.iter().position(|x| x == b).unwrap_or(usize::MAX);
        ai.cmp(&bi)
    });
    names
}

// ═══════════════════════════════════════════════════════
// 格式化函数
// ═══════════════════════════════════════════════════════

/// 格式化属性数值（含信赖/潜能/模组拆分）。
pub(crate) fn format_stats(stats: &ComputedStats, out: &mut String) {
    let attr_order = ATTR_ORDER;

    let mut ordered: Vec<&str> = Vec::new();
    for attr in attr_order {
        if stats.attrs.contains_key(*attr) {
            ordered.push(attr);
        }
    }
    for attr in stats.attrs.keys() {
        let a = attr.as_str();
        if !ordered.contains(&a) {
            ordered.push(a);
        }
    }

    for attr in ordered {
        if let Some(&(base, trust, pot, module)) = stats.attrs.get(attr) {
            let parts: Vec<String> = [
                if trust.abs() > 0.001 { Some(format!("{trust:+.0}(信赖)")) } else { None },
                if pot.abs() > 0.001 { Some(format!("{pot:+.0}(潜能)")) } else { None },
                if module.abs() > 0.001 { Some(format!("{module:+.0}(模组)")) } else { None },
            ]
            .into_iter()
            .flatten()
            .collect();

            let attr_name: &str = attr;
            let unit = match attr_name {
                "再部署时间" | "攻击间隔" => "s",
                _ => "",
            };

            let final_val = base + trust + pot + module;
            let (base_disp, final_disp) = if attr_name == "攻击间隔" {
                (format!("{base:.2}"), format!("{final_val:.2}"))
            } else {
                (format!("{base:.0}"), format!("{final_val:.0}"))
            };
            if parts.is_empty() {
                out.push_str(&format!("{attr}: {base_disp}{unit}\n"));
            } else {
                out.push_str(&format!(
                    "{attr}: {base_disp} {} = {final_disp}{unit}\n",
                    parts.join(" "),
                ));
            }
        }
    }

    if stats.atk_speed_bonus.abs() > 0.001 {
        let sources = if stats.atk_speed_sources.is_empty() {
            String::new()
        } else {
            format!("（{}）", stats.atk_speed_sources.join("、"))
        };
        let final_interval = stats.base_atk_interval / (1.0 + stats.atk_speed_bonus / 100.0);
        out.push_str(&format!(
            "攻击速度: +{:.0}{sources} → 攻击间隔 {:.2}s\n",
            stats.atk_speed_bonus, final_interval,
        ));
    }
}

/// 格式化单个技能的详细等级描述。
pub(crate) fn format_skill_expanded(
    skill: &Skill,
    detail_level: Option<u32>,
    expand_all: bool,
    out: &mut String,
) {
    out.push_str(&format!("[{}]\n", skill.name));

    let mut meta = Vec::new();
    if let Some(ref t) = skill.trigger_type { meta.push(t.as_str()); }
    if let Some(ref s) = skill.sp_type { meta.push(s.as_str()); }
    if let Some(ref d) = skill.duration_type { meta.push(d.as_str()); }
    if !meta.is_empty() {
        out.push_str(&format!("  {}\n", meta.join(" / ")));
    }

    fn write_level(detail: &SkillDetail, out: &mut String) {
        let label = level_label(detail.level);
        let desc = detail.description.as_deref().unwrap_or("（无描述）");
        out.push_str(&format!("  {label}: {desc}"));
        if let Some(dur) = detail.持续时间 {
            if dur > 0.0 {
                out.push_str(&format!("  持续 {:.0}s", dur));
            }
        }
        if let Some(ref rid) = detail.range_id {
            out.push_str(&format!("  攻击范围: {rid}"));
        }
        out.push('\n');
        if let Some(ref sp) = detail.sp {
            let mut sp_parts = Vec::new();
            if let Some(c) = sp.charges { if c > 1 { sp_parts.push(format!("可充能 {c}")); } }
            if let Some(c) = sp.sp_cost { sp_parts.push(format!("技力消耗 {c}")); }
            if let Some(s) = sp.init_sp { sp_parts.push(format!("初始技力 {s}")); }
            if !sp_parts.is_empty() {
                out.push_str(&format!("    {}\n", sp_parts.join(" / ")));
            }
        }
    }

    if let Some(lvl) = detail_level {
        if let Some(detail) = skill.details.iter().find(|d| d.level == lvl) {
            write_level(detail, out);
        }
    } else if expand_all && !skill.details.is_empty() {
        for detail in &skill.details {
            write_level(detail, out);
        }
    } else if let Some(last) = skill.details.last() {
        write_level(last, out);
    }

    if let Some(ref note) = skill.备注 {
        let note = note.trim();
        if !note.is_empty() {
            out.push_str(&format!("  技能备注: {note}\n"));
        }
    }
}

/// 格式化天赋列表。
pub(crate) fn format_talents(
    talents: &[Talent],
    elite_phase: Option<usize>,
    pot_level: Option<u32>,
    elite_default: usize,
    pot_default: u32,
    expand_all: bool,
    talent_idx: Option<usize>,
    out: &mut String,
) {
    for (i, talent) in talents.iter().enumerate() {
        if let Some(idx) = talent_idx {
            if i != idx {
                continue;
            }
        }
        if talent.alternatives.is_empty() {
            continue;
        }
        if expand_all {
            let do_filter = talent_idx.is_some();
            let matching: Vec<_> = talent.alternatives.iter().filter(|alt| {
                if do_filter {
                    if let Some(e) = elite_phase {
                        if alt.condition.elite as usize > e { return false; }
                    }
                    if let Some(p) = pot_level {
                        if alt.condition.potential > p { return false; }
                    }
                }
                true
            }).collect();
            if matching.is_empty() {
                continue;
            }
            out.push_str(&format!("[{}]\n", talent.name));
            for alt in matching {
                let cond = &alt.condition;
                let elite_label = if cond.elite == 0 { "精0".to_string() } else if cond.elite == 1 { "精1".to_string() } else { format!("精{}", cond.elite) };
                let pot_label = if cond.potential > 0 { format!("{}潜", cond.potential) } else { String::new() };
                let cond_str = if pot_label.is_empty() {
                    elite_label
                } else {
                    format!("{elite_label} {pot_label}")
                };
                let desc = alt.description.as_deref().unwrap_or("（无描述）");
                out.push_str(&format!("  {cond_str}: {desc}\n"));
            }
            if let Some(ref note) = talent.备注 {
                let note = note.trim();
                if !note.is_empty() {
                    out.push_str(&format!("  天赋备注: {note}\n"));
                }
            }
        } else {
            let elite = elite_default as u32;
            let pot = pot_default;
            let best = pick_talent_alt(&talent.alternatives, elite, pot);
            if let Some(alt) = best {
                let cond = &alt.condition;
                let elite_label = if cond.elite == 0 { "精0".to_string() } else if cond.elite == 1 { "精1".to_string() } else { format!("精{}", cond.elite) };
                let pot_label = if cond.potential > 0 { format!("{}潜", cond.potential) } else { String::new() };
                let cond_str = if pot_label.is_empty() {
                    format!("（{elite_label}）")
                } else {
                    format!("（{elite_label} {pot_label}）")
                };
                if let Some(ref desc) = alt.description {
                    out.push_str(&format!("[{}]{} {}\n", talent.name, cond_str, desc));
                }
                if let Some(ref note) = talent.备注 {
                    let note = note.trim();
                    if !note.is_empty() {
                        out.push_str(&format!("  天赋备注: {note}\n"));
                    }
                }
            }
        }
    }
}

/// 等级 → 显示标签（Lv1~7 / 专1~专3）。
pub(crate) fn level_label(lvl: u32) -> String {
    if lvl > 7 {
        format!("专{}", lvl - 7)
    } else {
        format!("Lv{lvl}")
    }
}

/// 从备选天赋中选最优匹配。
pub(crate) fn pick_talent_alt<'a>(
    alternatives: &'a [TalentAlt],
    elite: u32,
    pot: u32,
) -> Option<&'a TalentAlt> {
    alternatives
        .iter()
        .filter(|a| a.condition.elite <= elite && a.condition.potential <= pot)
        .max_by_key(|a| (a.condition.elite, a.condition.potential))
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(range_id: Option<String>) -> Skill {
        Skill {
            skill_id: None,
            name: "测试技能".into(),
            trigger_type: None,
            sp_type: None,
            duration_type: None,
            duration: None,
            details: vec![SkillDetail {
                level: 7,
                range_id,
                description: Some("攻击力+40%".into()),
                持续时间: None,
                sp: None,
            }],
            备注: None,
        }
    }

    #[test]
    fn skill_expanded_outputs_range_id() {
        // 技能改变攻击范围（携带 rangeID）→ 等级行应输出攻击范围标注
        let skill = make_skill(Some("3-3".into()));
        let mut out = String::new();
        format_skill_expanded(&skill, None, false, &mut out);
        assert!(out.contains("攻击范围: 3-3"));
    }

    #[test]
    fn skill_expanded_omits_range_when_absent() {
        // 技能不改变攻击范围（无 rangeID）→ 不输出攻击范围标注
        let skill = make_skill(None);
        let mut out = String::new();
        format_skill_expanded(&skill, None, false, &mut out);
        assert!(!out.contains("攻击范围:"));
    }
}
