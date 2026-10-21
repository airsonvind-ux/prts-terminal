//! query_operator —— 干员名称查询工具
//!
//! 语义：输入名称 → 返回干员属性（含数值计算）、技能、天赋、模组。
//! 与 search_entities 互为逆操作：
//!   query_operator：身份 → 属性（名称查询）
//!   search_entities：条件 → 身份列表（集合筛选）
//!
//! ## 数值计算模型
//!
//! 最终数值 = 精英化&等级插值 + 信赖按比例 + 潜能数值加成 + 模组数值更新
//! 攻击速度变更单独处理：基础攻击间隔 / (1 + 攻速变更%/100)
//!
//! ## 参数契约
//!
//! `names` / `ids`（数组，互斥，必填其一）+ `extra`（数组，可选）。
//! 不提供 `fields` 参数；不提供"技能列表"整列、逐级技能/天赋数据、天赋点单。
//!
//! ## 默认行为
//!
//! 默认返回：名称、备注、描述、职业、职业分支、默认练度属性、天赋（当前练度最优备选）、
//! 模组（选中模组完整效果）。技能不随默认返回，需在 extra 中点单获取。
//!
//! | 维度   | 默认                      | extra 可切换              |
//! |--------|--------------------------|--------------------------|
//! | 精英化 | 最高精英化满级            | `等级=精一满级`, `等级=精二40级` |
//! | 信赖   | 满信赖 100%               | `信赖=50`, `信赖=零`     |
//! | 潜能   | 0潜                       | `潜能=五潜`              |
//! | 模组   | 首个模组满级（3级，不支持指定等级） | `模组=X模`, `模组=无模组` |
//! | 技能   | 不随默认返回，点单制       | `一技能`, `3级火山`, `专2黄昏` |
//! | 天赋   | 当前练度最优备选           | （不提供点单）           |
//!
//! ## extra 语法
//!
//! 含 `=` 为**状态指定**：键限 等级/潜能/信赖/模组 四种，指定即替换默认练度，
//! 属性与天赋随之重算。旧裸写语法（`精一20级`、`五潜`、`零信赖`、`X模`）不再
//! 作为状态指定解析，落入附加列后无法命中时报错并指路键=值形式。
//!
//! 不含 `=` 为**附加列**：技能点单（序号制或技能名，可一次多个，每个独立输出）、
//! 常用列（稀有度/部署位置/代号/标签/获取方式/潜能提升/关联token）、或任意完整
//! 表列（JSON 列按现有格式化方式输出）。无法识别的项报错并指路。
//!
//! ## 特殊列加工
//!
//! | 列名       | 原始格式              | 输出格式           |
//! |-----------|----------------------|-------------------|
//! | 标签      | JSON `["a","b"]`     | "、"拼接           |
//! | 信赖加成  | JSON 键值对           | 按当前信赖比例输出   |
//! | 精英化阶段 | JSON 多阶段数组       | 属性区块（含计算）/ 附加列时 raw 输出 |
//! | 天赋列表   | JSON 含备选的天赋数组  | 默认：逐天赋最优备选；附加列：全部备选展开 |
//! | 模组列表   | JSON 含等级效果的数组  | 默认：选中模组完整效果；附加列：全部模组完整效果 |
//! | 潜能提升   | JSON 字符串数组       | 编号列表           |
//! | 关联token | JSON `["id"]` 或 null | "、"拼接 / "无"   |

use crate::tools::common::{self, Target};
use crate::tools::common_fmt::{
    find_bounding_keyframes, format_skill_expanded, format_talents, parse_json, ComputedStats,
    ElitePhase, Skill, Talent,
};
use crate::tools::display::{array_summary, targets_summary};
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

use serde::Deserialize;
use std::collections::HashMap;

/// operators 表完整列名（附加列识别与查询字段共用）。
const ALL_COLUMNS: &[&str] = &[
    "干员ID", "名称", "代号", "描述", "部署位置", "标签", "获取方式",
    "是否不可获取", "是否为异格", "稀有度", "职业", "职业分支", "最大潜能",
    "备注", "信赖加成", "精英化阶段", "技能列表", "关联token", "天赋列表",
    "潜能提升", "模组列表",
];

/// 默认结果已包含的列：附加列中重复指定时忽略（不重复输出、不报错）。
const DEFAULT_COLUMNS: &[&str] = &["名称", "备注", "描述", "职业", "职业分支"];

pub struct QueryOperator;

impl Tool for QueryOperator {
    fn name(&self) -> &str {
        "query_operator"
    }

    fn description(&self) -> &str {
        "按名称或 ID 查询干员信息，支持批量（ids 主键精确 / names 精确优先、未中模糊取最佳，二选一）。默认返回：名称、备注、描述、职业、职业分支、默认练度属性、天赋（最优备选）、模组；默认练度 = 最高精英化满级、满信赖、0潜、首个模组满级，属性已计入信赖与模组加成并标注来源。技能默认不返回，用 extra 点单。仅 operators 表"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "干员 ID 列表，如 [“char_002_amiya”]（阿米娅）。与 names 二选一"
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "干员名称列表，如 [“阿米娅”]。与 ids 二选一"
                },
                "extra": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "增量声明，可选，每项独立生效。含 = 为状态指定，不含 = 为附加列。状态指定（键限 等级/潜能/信赖/模组）：等级=精二40级、精一满级、精二（省略即满级）；潜能=满潜、五潜、0潜；信赖=50（50%）、满、零；模组=无模组、X模、Y模、D模、特限模组。指定即替换对应默认值，属性与天赋随之重算；同一键重复指定时，以最后一项为准。附加列：技能点单（如 一技能、7级2技能、专2二技能；或技能名：火山、3级火山、专2黄昏；默认专三，可一次多个，每项独立输出），或常用列（稀有度、部署位置、代号、标签、关联token）。完整列名见知识库《干员表结构》条目"
                }
            }
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let mut parts = Vec::new();
        if let Some(t) = targets_summary(&v) {
            parts.push(t);
        }
        if let Some(e) = array_summary(&v, "extra") {
            parts.push(e);
        }
        if parts.is_empty() {
            return ToolDisplay::default();
        }
        ToolDisplay::info(parts.join(" · "))
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        // ── 1. 解析查询目标 ──
        let targets = common::parse_targets(&v)?;

        // ── 2. 解析 extra（状态指定 + 附加列）──
        let extra = parse_extra(&v["extra"]);

        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        // ── 3. 固定查询完整列（不裁剪）──
        let all_fields: Vec<String> = ALL_COLUMNS.iter().map(|s| s.to_string()).collect();

        // ── 4. 逐个查询并格式化 ──
        let mut output = String::new();
        let mut all_rows: Vec<HashMap<String, String>> = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            let row = match target {
                Target::Id(id) => common::query_by_id(db, "operators", "干员ID", id, &all_fields)?,
                Target::Name(name) => common::query_by_name(db, "operators", name, &all_fields)?,
            };

            match row {
                Some(row) => {
                    all_rows.push(row.clone());
                    format_one_operator(&row, &extra, &mut output);
                }
                None => {
                    let label = match target {
                        Target::Id(id) => format!("ID: {id}"),
                        Target::Name(name) => format!("名称: {name}"),
                    };
                    output.push_str(&format!("✗ {label}：未找到\n"));
                }
            }
        }

        Ok(ToolOutput::json(
            serde_json::json!({ "rows": all_rows, "dropped_fields": [] }),
            output.trim_end(),
        ))
    }
}

// ═══════════════════════════════════════════════════════
// 单干员格式化
// ═══════════════════════════════════════════════════════

fn format_one_operator(
    row: &HashMap<String, String>,
    extra: &ExtraConfig,
    out: &mut String,
) {
    let elite_phases: Vec<ElitePhase> =
        parse_json(row, "精英化阶段").unwrap_or_default();
    let trust: HashMap<String, f64> =
        parse_json(row, "信赖加成").unwrap_or_default();
    let skills: Vec<Skill> =
        parse_json(row, "技能列表").unwrap_or_default();
    let talents: Vec<Talent> =
        parse_json(row, "天赋列表").unwrap_or_default();
    let potentials: Vec<String> =
        parse_json(row, "潜能提升").unwrap_or_default();
    let modules: Vec<Module> =
        parse_json(row, "模组列表").unwrap_or_default();
    let tags_json: Vec<String> =
        parse_json(row, "标签").unwrap_or_default();
    let tokens_json: Vec<String> =
        parse_json(row, "关联token").unwrap_or_default();

    let op_name = row.get("名称").map(|s| s.as_str()).unwrap_or("未知");
    let rarity: u32 = row.get("稀有度").and_then(|s| s.parse().ok()).unwrap_or(0);

    let phase_idx = extra.elite_phase.unwrap_or(elite_phases.len().saturating_sub(1));
    let phase = if let Some(p) = elite_phases.get(phase_idx) { p } else { return; };
    let level = extra.elite_level.unwrap_or(phase.max_level).min(phase.max_level);

    let trust_pct = extra.trust_pct.unwrap_or(100.0) / 100.0;
    let pot_level = extra.pot_level.unwrap_or(0);

    // 模组：默认首个模组，恒满级（3级），不支持指定模组等级
    const MODULE_LEVEL: u32 = 3;
    let module_idx = if let Some(ref spec) = extra.module_spec {
        find_module_index(&modules, spec).unwrap_or(0)
    } else {
        0
    };

    // 模组仅在达到解锁条件时生效（精二 + 稀有度对应最低等级）；"模组=无模组"强制不生效
    let module_active = !extra.module_none
        && modules.get(module_idx).is_some()
        && can_use_module(phase_idx, level, rarity);
    let effective_module = if module_active { modules.get(module_idx) } else { None };

    let stats = compute_stats(phase, level, &trust, trust_pct, &potentials, pot_level, effective_module, MODULE_LEVEL);

    out.push_str(&format!("━━━ {op_name} ━━━\n"));

    // ── 基本信息（默认返回）──
    for col in ["备注", "描述", "职业", "职业分支"] {
        if let Some(raw) = row.get(col) {
            if raw.is_empty() || raw == "null" {
                out.push_str(&format!("{col}: 无\n"));
            } else {
                out.push_str(&format!("{col}: {raw}\n"));
            }
        }
    }

    // ── 属性（默认或状态指定后的练度）──
    let phase_label = if phase_idx == 0 { "精0".to_string() } else if phase_idx == 1 { "精1".to_string() } else { format!("精{phase_idx}") };
    let module_name = if extra.module_none || modules.is_empty() {
        "无模组".to_string()
    } else if module_active {
        modules.get(module_idx).map(|m| format!("{}级{}", ordinal_cn(MODULE_LEVEL), m.name.as_str())).unwrap_or_else(|| "无模组".to_string())
    } else {
        modules.get(module_idx).map(|m| format!("{}（未解锁）", m.name.as_str())).unwrap_or_else(|| "无模组".to_string())
    };
    let trust_label = if (trust_pct - 1.0).abs() < 0.001 { "满信赖" } else { &format!("{}%信赖", (trust_pct * 100.0) as u32) };
    let pot_label = if pot_level == 0 { "0潜" } else { &format!("{pot_level}潜") };
    out.push_str(&format!("\n── 属性（{phase_label} {level}级 / {trust_label} / {pot_label} / {module_name}）──\n"));
    format_stats(&stats, out);
    // 攻击范围
    let range_str = phase.range_id.as_deref().unwrap_or("无");
    out.push_str(&format!("攻击范围: {range_str}\n"));

    // ── 天赋（当前练度生效的最优备选，不展开备选列表）──
    if !talents.is_empty() {
        out.push_str("\n── 天赋 ──\n");
        format_talents(&talents, None, None, phase_idx, pot_level, false, None, out);
    }

    // ── 模组（选中模组在当前练度下的解锁状态与完整效果）──
    if !modules.is_empty() {
        out.push_str("\n── 模组 ──\n");
        if extra.module_none {
            out.push_str("未装配\n");
        } else {
            if !module_active {
                let req_level = module_level_requirement(rarity);
                out.push_str(&format!("（当前 {phase_label} {level}级，未达解锁条件：精二 {req_level}级，模组加成未计入属性）\n"));
            }
            format_modules(&modules, module_idx, true, out);
        }
    }

    // ── 技能点单（每个请求独立输出）──
    if !extra.skill_requests.is_empty() {
        out.push_str("\n── 技能 ──\n");
        for req in &extra.skill_requests {
            match (req.idx, req.name.as_deref()) {
                (Some(idx), _) => {
                    if idx >= skills.len() {
                        out.push_str(&format!("（无第{}个技能）\n", idx + 1));
                    } else {
                        out.push('\n');
                        format_skill_expanded(&skills[idx], req.level, false, out);
                    }
                }
                (None, Some(name)) => {
                    if let Some(idx) = skills.iter().position(|s| s.name == *name) {
                        out.push('\n');
                        format_skill_expanded(&skills[idx], req.level, false, out);
                    } else if talents.iter().any(|t| t.name == *name) {
                        out.push_str(&format!(
                            "✗ '{name}' 是天赋名：天赋不提供点单，天赋随默认结果或状态指定后的练度呈现\n"
                        ));
                    } else {
                        out.push_str(&format!(
                            "✗ 无法识别的 extra 项 '{name}'：状态请用 键=值 形式（等级/潜能/信赖/模组）；完整列名见知识库《干员表结构》条目；技能请用序号（如 二技能）或技能名\n"
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    // ── 附加列（常用列 + 其余完整表列）──
    for field in &extra.extra_columns {
        match field.as_str() {
            "精英化阶段" => format_raw_phases(&elite_phases, out),
            "信赖加成" => {
                out.push('\n');
                if trust.is_empty() {
                    out.push_str("信赖加成: 无\n");
                } else {
                    let parts: Vec<String> = trust
                        .iter()
                        .map(|(k, v)| format!("{k}+{:.0}", v * trust_pct))
                        .collect();
                    let label = if (trust_pct - 1.0).abs() < 0.001 {
                        "满信赖"
                    } else {
                        &format!("{}%信赖", (trust_pct * 100.0) as u32)
                    };
                    out.push_str(&format!("信赖加成（{label}）: {}\n", parts.join(" / ")));
                }
            }
            "潜能提升" => {
                if potentials.is_empty() {
                    out.push_str("潜能提升: 无\n");
                } else {
                    out.push_str("\n── 潜能提升 ──\n");
                    for (i, p) in potentials.iter().enumerate() {
                        out.push_str(&format!("  {}. {}\n", i + 1, p));
                    }
                }
            }
            "关联token" => {
                out.push('\n');
                if tokens_json.is_empty() {
                    out.push_str("关联token: 无\n");
                } else {
                    out.push_str(&format!("关联token: {}\n", tokens_json.join("、")));
                }
            }
            "天赋列表" => {
                if talents.is_empty() {
                    out.push_str("天赋列表: 无\n");
                } else {
                    out.push_str("\n── 天赋列表（完整数据）──\n");
                    format_talents(&talents, None, None, phase_idx, pot_level, true, None, out);
                }
            }
            "模组列表" => {
                if modules.is_empty() {
                    out.push_str("模组列表: 无\n");
                } else {
                    out.push_str("\n── 模组列表（完整数据）──\n");
                    format_modules(&modules, 0, false, out);
                }
            }
            _ => {
                if let Some(raw) = row.get(field) {
                    if raw.is_empty() || raw == "null" {
                        out.push_str(&format!("{field}: 无\n"));
                    } else if field == "标签" {
                        out.push_str(&format!("{field}: {}\n", tags_json.join("、")));
                    } else {
                        out.push_str(&format!("{field}: {raw}\n"));
                    }
                }
            }
        }
    }

    // ── extra 解析期错误（逐条标注，不阻塞其它有效结果）──
    for err in &extra.errors {
        out.push_str(&format!("✗ {err}\n"));
    }
}

// ═══════════════════════════════════════════════════════
// 数据模型
// ═══════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
struct Module {
    #[serde(rename = "模组ID")]
    #[allow(dead_code)]
    module_id: Option<String>,
    #[serde(rename = "名称")]
    name: String,
    #[serde(rename = "分支名1")]
    branch1: Option<String>,
    #[serde(rename = "分支名2")]
    branch2: Option<String>,
    #[serde(rename = "各级效果")]
    effects: Vec<ModuleEffect>,
}

#[derive(Debug, Deserialize)]
struct ModuleEffect {
    #[serde(rename = "数值更新")]
    stat_updates: Option<Vec<StatUpdate>>,
    #[serde(rename = "特性")]
    trait_text: Option<String>,
    #[serde(rename = "天赋更新文本")]
    talent_update: Option<String>,
    #[serde(default)]
    #[serde(rename = "新增天赋")]
    new_talent: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StatUpdate {
    #[serde(rename = "属性")]
    attr: String,
    #[serde(rename = "增加值")]
    value: f64,
}

// ═══════════════════════════════════════════════════════
// extra 解析
// ═══════════════════════════════════════════════════════

/// 技能点单条目：序号制（idx + 可选等级）或名字制（name + 可选等级）。
#[derive(Debug)]
struct SkillRequest {
    idx: Option<usize>,
    level: Option<u32>,
    name: Option<String>,
}

#[derive(Debug, Default)]
struct ExtraConfig {
    // ── 状态指定（含 =）：替换默认练度 ──
    elite_phase: Option<usize>,
    elite_level: Option<u32>,
    trust_pct: Option<f64>,
    pot_level: Option<u32>,
    module_spec: Option<String>,
    /// 模组=无模组：属性不计模组加成，模组区块显示未装配
    module_none: bool,
    // ── 附加列（不含 =）──
    skill_requests: Vec<SkillRequest>,
    extra_columns: Vec<String>,
    /// 解析期无法识别的项（正文，输出时加 ✗ 前缀）；名字制技能未命中在 format 阶段判定
    errors: Vec<String>,
}

fn parse_extra(extra_val: &serde_json::Value) -> ExtraConfig {
    let arr = match extra_val.as_array() {
        Some(a) => a,
        None => return ExtraConfig::default(),
    };

    let mut cfg = ExtraConfig::default();
    for item in arr {
        // 非字符串项忽略（与旧行为一致）
        let s = match item.as_str() {
            Some(s) => s.trim(),
            None => continue,
        };
        if s.is_empty() {
            continue;
        }

        // ── 状态指定（含 =）──
        if let Some(eq) = s.find('=') {
            let key = &s[..eq];
            let value = &s[eq + 1..];
            match key {
                "等级" => parse_elite_status(value, &mut cfg),
                "潜能" => parse_potential_status(value, &mut cfg),
                "信赖" => parse_trust_status(value, &mut cfg),
                "模组" => parse_module_status(value, &mut cfg),
                _ => cfg.errors.push(format!(
                    "状态键 '{key}' 不支持：仅限 等级/潜能/信赖/模组，请用 键=值 形式"
                )),
            }
            continue;
        }

        // ── 附加列（不含 =），按判定顺序 ──

        // 1. 技能序号制："一技能" / "7级2技能" / "专2二技能"
        if let Some((idx, level)) = parse_skill_index(s) {
            cfg.skill_requests.push(SkillRequest {
                idx: Some(idx),
                level,
                name: None,
            });
            continue;
        }

        // 2. 表列名（含常用列）
        if ALL_COLUMNS.contains(&s) {
            if s == "技能列表" {
                // 不提供"技能列表"整列：指路点单制
                cfg.errors.push(
                    "'技能列表' 不提供整列获取：技能请点单（如 二技能、火山、专2黄昏）".to_string(),
                );
            } else if !DEFAULT_COLUMNS.contains(&s) {
                cfg.extra_columns.push(s.to_string());
            }
            // 默认已返回的列（名称/备注/描述/职业/职业分支）：重复指定忽略
            continue;
        }

        // 3. 天赋点单语法（"天赋二" / "一天赋" / "天赋1"）→ 报错指路
        if is_talent_point_syntax(s) {
            cfg.errors.push(format!(
                "'{s}' 是天赋点单：天赋不提供点单，天赋随默认结果或状态指定后的练度呈现"
            ));
            continue;
        }

        // 4. 其余：技能名候选（可带等级前缀："3级火山"、"专2黄昏"）。
        //    匹配延后到 format 阶段（需干员数据判定技能/天赋/未知）
        let (lvl, name) = split_level_prefix(s);
        if !name.is_empty() {
            cfg.skill_requests.push(SkillRequest {
                idx: None,
                level: lvl,
                name: Some(name.to_string()),
            });
        }
    }

    cfg
}

/// 解析技能序号制："一技能" → (0, None)、"7级2技能" → (1, Some(7))、"专2二技能" → (1, Some(9))。
/// 无序号（如"1级技能"）或非序号制返回 None。
fn parse_skill_index(s: &str) -> Option<(usize, Option<u32>)> {
    if !s.ends_with("技能") {
        return None;
    }
    let core = &s[..s.len() - "技能".len()];
    // 分离等级前缀："专N" → 7+N 、"N级" → N 、"" → None
    let (lvl, rest) = if let Some(r) = core.strip_prefix("专") {
        let digit_count = r.chars().take_while(|c| c.is_ascii_digit()).count();
        let num_str: String = r.chars().take(digit_count).collect();
        if let Ok(n) = num_str.parse::<u32>() {
            (Some(7 + n), &r[digit_count..])
        } else {
            (None, core)
        }
    } else if let Some(pos) = core.find("级") {
        let num_str = &core[..pos];
        if let Ok(n) = num_str.parse::<u32>() {
            (Some(n), &core[pos + "级".len()..])
        } else {
            (None, core)
        }
    } else {
        (None, core)
    };
    let idx = parse_chinese_or_digit(rest)?;
    if idx >= 1 {
        Some((idx as usize - 1, lvl))
    } else {
        None
    }
}

/// 状态指定 · 等级：等级=精二40级 / 等级=精一满级 / 等级=精二（省略等级=该阶段满级）。
fn parse_elite_status(value: &str, cfg: &mut ExtraConfig) {
    let rest = match value.strip_prefix("精") {
        Some(r) => r,
        None => {
            cfg.errors.push(format!(
                "等级 值 '{value}' 无法解析：请用 精N、精N满级 或 精N+等级，如 等级=精二40级、等级=精一满级、等级=精二"
            ));
            return;
        }
    };
    let (phase_str, level_str) = split_digits(rest);
    let mut phase: Option<usize> = None;
    let mut level: Option<u32> = None;
    let mut matched = false;
    // "精0"/"精1"/"精2" — 无中文标记时 0/1/2 视为阶段数字
    if phase_str.is_empty() && matches!(level_str, "0" | "1" | "2") {
        phase = Some(level_str.parse::<u32>().unwrap() as usize);
        matched = true;
    }
    if let Some(idx) = parse_chinese_or_digit(phase_str) {
        phase = Some(idx as usize);
        matched = true;
    }
    if level_str == "满级" || level_str == "满" {
        level = None;
        matched = true;
    } else if !level_str.is_empty() {
        if let Ok(n) = level_str.trim_end_matches('级').parse::<u32>() {
            level = Some(n);
            matched = true;
        }
    }
    if !matched {
        cfg.errors.push(format!(
            "等级 值 '{value}' 无法解析：请用 等级=精二40级 / 等级=精一满级 / 等级=精二"
        ));
        return;
    }
    // 同一键重复指定时以最后一项为准：整体替换（未写明的部分回到默认语义）
    cfg.elite_phase = phase;
    cfg.elite_level = level;
}

/// 状态指定 · 潜能：潜能=满潜 / 潜能=五潜 / 潜能=0潜~五潜（中文或数字）。
fn parse_potential_status(value: &str, cfg: &mut ExtraConfig) {
    if value == "满潜" {
        cfg.pot_level = Some(5);
        return;
    }
    if let Some(num_part) = value.strip_suffix("潜") {
        if num_part == "零" || num_part == "0" {
            cfg.pot_level = Some(0);
            return;
        }
        if let Some(n) = parse_chinese_or_digit(num_part) {
            if n <= 5 {
                cfg.pot_level = Some(n);
                return;
            }
            cfg.errors.push(format!(
                "潜能 值 '{value}' 超出范围：最大为满潜（五潜），请用 潜能=满潜 / 潜能=五潜 / 潜能=0潜"
            ));
            return;
        }
    }
    cfg.errors.push(format!(
        "潜能 值 '{value}' 无法解析：请用 潜能=满潜 / 潜能=五潜 / 潜能=0潜"
    ));
}

/// 状态指定 · 信赖：信赖=50（50%）/ 信赖=满 / 信赖=零。
fn parse_trust_status(value: &str, cfg: &mut ExtraConfig) {
    if value == "满" {
        cfg.trust_pct = Some(100.0);
        return;
    }
    if value == "零" || value == "0" {
        cfg.trust_pct = Some(0.0);
        return;
    }
    if let Ok(n) = value.parse::<f64>() {
        cfg.trust_pct = Some(n);
        return;
    }
    cfg.errors.push(format!(
        "信赖 值 '{value}' 无法解析：请用 信赖=50（百分比）/ 信赖=满 / 信赖=零"
    ));
}

/// 状态指定 · 模组：模组=无模组 / 模组=X模 / 模组=Y模 / 模组=D模 / 模组=特限模组。
/// 不支持指定模组等级（模组恒满级）。
fn parse_module_status(value: &str, cfg: &mut ExtraConfig) {
    if value == "无模组" {
        cfg.module_none = true;
        cfg.module_spec = None;
        return;
    }
    if value == "X模" || value == "X模组" {
        cfg.module_spec = Some("X".to_string());
        cfg.module_none = false;
        return;
    }
    if value == "Y模" || value == "Y模组" {
        cfg.module_spec = Some("Y".to_string());
        cfg.module_none = false;
        return;
    }
    if value == "D模" || value == "D模组" || value == "Δ模" || value == "Delta模" {
        cfg.module_spec = Some("D".to_string());
        cfg.module_none = false;
        return;
    }
    if value.contains("特限") {
        cfg.module_spec = Some("特限".to_string());
        cfg.module_none = false;
        return;
    }
    cfg.errors.push(format!(
        "模组 值 '{value}' 无法解析：请用 无模组 / X模 / Y模 / D模 / 特限模组（模组默认首个模组满级，不支持指定模组等级）"
    ));
}

/// 天赋点单语法识别："天赋二" / "一天赋" / "天赋1" 等 → 报错指路（不提供天赋点单）。
fn is_talent_point_syntax(s: &str) -> bool {
    if let Some(num_part) = s.strip_suffix("天赋") {
        if parse_chinese_or_digit(num_part).is_some() {
            return true;
        }
    }
    if let Some(num_part) = s.strip_prefix("天赋") {
        if parse_chinese_or_digit(num_part).is_some() {
            return true;
        }
    }
    false
}

/// 剥离技能等级前缀："3级火山" → (Some(3), "火山")，"专2黄昏" → (Some(9), "黄昏")，"火山" → (None, "火山")。
/// 剥离失败时原样返回，不会误伤以"专"开头的技能名（如"专业喷绘技巧"）。
fn split_level_prefix(s: &str) -> (Option<u32>, &str) {
    if let Some(r) = s.strip_prefix("专") {
        let digit_count = r.chars().take_while(|c| c.is_ascii_digit()).count();
        let num_str: String = r.chars().take(digit_count).collect();
        if let Ok(n) = num_str.parse::<u32>() {
            return (Some(7 + n), &r[digit_count..]);
        }
    }
    if let Some(pos) = s.find('级') {
        let num_str = &s[..pos];
        if let Ok(n) = num_str.parse::<u32>() {
            return (Some(n), &s[pos + "级".len()..]);
        }
    }
    (None, s)
}

/// "精一20级" → ("一", "20级"), "精二满级" → ("二", "满级"), "精二" → ("二", "")
fn split_digits<'a>(s: &'a str) -> (&'a str, &'a str) {
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() || c == '满' || c == '级' {
            return (&s[..i], &s[i..]);
        }
    }
    (s, "")
}

fn parse_chinese_or_digit(s: &str) -> Option<u32> {
    match s {
        "零" | "0" => Some(0),
        "一" => Some(1), "二" => Some(2), "三" => Some(3),
        "四" => Some(4), "五" => Some(5), "六" => Some(6),
        _ => s.parse::<u32>().ok(),
    }
}

// ═══════════════════════════════════════════════════════
// 数据库查询
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// 数值计算
// ═══════════════════════════════════════════════════════

/// 按模组标识（X/Y/特限）查找 modules 数组中的索引。
fn find_module_index(modules: &[Module], spec: &str) -> Option<usize> {
    modules.iter().position(|m| {
        match spec {
            "X" | "Y" | "D" => m.branch2.as_deref() == Some(spec),
            // 特限模组由 branch1 标记：ISW=集成战略, RA=生息演算, SO=其他特限（如电弧）
            "特限" => {
                matches!(m.branch1.as_deref(), Some("ISW") | Some("RA") | Some("SO"))
            }
            _ => false,
        }
    })
}

/// 模组解锁所需的最低等级，取决于干员稀有度。
/// 6★=精二60级, 5★=精二50级, 4★=精二40级。
fn module_level_requirement(rarity: u32) -> u32 {
    match rarity {
        6 => 60,
        5 => 50,
        4 => 40,
        _ => 0, // 1~3 星无模组或达不到精二
    }
}

/// 判断当前等级是否满足模组解锁条件：精二 + 达到对应稀有度的最低等级。
fn can_use_module(phase_idx: usize, level: u32, rarity: u32) -> bool {
    if phase_idx < 2 {
        return false;
    }
    let required = module_level_requirement(rarity);
    required > 0 && level >= required
}

// ═══════════════════════════════════════════════════════
// 精英化阶段 · raw 输出
// ═══════════════════════════════════════════════════════

/// 输出精英化阶段的完整原始数据（非插值）。
/// 每个阶段输出范围ID、最高等级、以及最低/最高两级关键帧的全部属性。
fn format_raw_phases(phases: &[ElitePhase], out: &mut String) {
    out.push_str("\n── 精英化阶段（完整数据）──\n");

    // 属性固定输出顺序（与 format_stats 一致）
    let attr_order = [
        "生命上限", "攻击", "防御", "法术抗性",
        "部署费用", "阻挡数", "再部署时间", "攻击间隔",
    ];

    for (i, phase) in phases.iter().enumerate() {
        let phase_label = if i == 0 {
            "精0".to_string()
        } else if i == 1 {
            "精1".to_string()
        } else {
            format!("精{i}")
        };
        let range = phase.range_id.as_deref().unwrap_or("无");
        out.push_str(&format!(
            "\n{phase_label}（范围: {range}） 最高等级: {}\n",
            phase.max_level
        ));

        // 取最低和最高两级关键帧
        let kf_indices: Vec<usize> = {
            let n = phase.keyframes.len();
            if n <= 2 {
                (0..n).collect()
            } else {
                vec![0, n - 1]
            }
        };

        for &idx in &kf_indices {
            if let Some(kf) = phase.keyframes.get(idx) {
                let attrs: Vec<String> = attr_order
                    .iter()
                    .filter_map(|attr| {
                        kf.attrs.get(*attr).map(|v| {
                            if *attr == "攻击间隔" || *attr == "法术抗性" {
                                format!("{} {}", attr, v)
                            } else {
                                format!("{} {:.0}", attr, v)
                            }
                        })
                    })
                    .collect();
                out.push_str(&format!("  {}级 — {}\n", kf.level, attrs.join(", ")));
            }
        }
    }
}

fn compute_stats(
    phase: &ElitePhase,
    level: u32,
    trust: &HashMap<String, f64>,
    trust_pct: f64,
    potentials: &[String],
    pot_level: u32,
    module: Option<&Module>,
    module_lvl: u32,
) -> ComputedStats {
    let mut stats = ComputedStats::default();

    // 1. 精英化阶段插值
    let (lo, hi) = find_bounding_keyframes(&phase.keyframes, level);
    let frac = if hi.level == lo.level {
        0.0
    } else {
        (level - lo.level) as f64 / (hi.level - lo.level) as f64
    };

    for attr in CANONICAL_ATTRS {
        let lo_val = lo.attrs.get(*attr).copied().unwrap_or(0.0);
        let hi_val = hi.attrs.get(*attr).copied().unwrap_or(lo_val);
        let interp = if (hi_val - lo_val).abs() < 0.001 {
            lo_val
        } else {
            lo_val + (hi_val - lo_val) * frac
        };
        stats.attrs.insert(attr.to_string(), (interp, 0.0, 0.0, 0.0));
    }

    stats.base_atk_interval = stats.attrs.get("攻击间隔").map(|v| v.0).unwrap_or(1.0);

    // 2. 信赖加成
    if trust_pct > 0.0 {
        for (trust_attr, raw_val) in trust {
            let canon = canonical_attr(&trust_attr);
            if canon == "攻击速度" {
                stats.atk_speed_bonus += raw_val * trust_pct;
                stats.atk_speed_sources.push(format!("信赖+{:.0}", raw_val * trust_pct));
            } else if let Some(entry) = stats.attrs.get_mut(canon) {
                let bonus = raw_val * trust_pct;
                entry.1 = bonus;
            }
        }
    }

    // 3. 潜能加成
    if pot_level > 0 && pot_level as usize <= potentials.len() {
        for i in 0..pot_level as usize {
            if let Some((attr_name, val)) = parse_potential_bonus(&potentials[i]) {
                if attr_name == "攻击速度" {
                    stats.atk_speed_bonus += val as f64;
                    stats.atk_speed_sources.push(format!("潜能+{:.0}", val));
                } else {
                    let canon = canonical_attr(attr_name);
                    if let Some(entry) = stats.attrs.get_mut(canon) {
                        entry.2 += val as f64;
                    }
                }
            }
        }
    }

    // 4. 模组加成
    if let Some(module) = module {
        let effect_idx = (module_lvl.saturating_sub(1) as usize)
            .min(module.effects.len().saturating_sub(1));
        if let Some(effect) = module.effects.get(effect_idx) {
            if let Some(ref updates) = effect.stat_updates {
                for u in updates {
                    let canon = canonical_attr(&u.attr);
                    if canon == "攻击速度" {
                        stats.atk_speed_bonus += u.value;
                        stats.atk_speed_sources.push(format!("模组+{:.0}", u.value));
                    } else if let Some(entry) = stats.attrs.get_mut(canon) {
                        entry.3 += u.value;
                    }
                }
            }
        }
    }

    stats
}

fn parse_potential_bonus(text: &str) -> Option<(&str, i32)> {
    let bytes = text.as_bytes();
    let mut i = bytes.len();

    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == bytes.len() {
        return None;
    }

    if i == 0 || (bytes[i - 1] != b'+' && bytes[i - 1] != b'-') {
        return None;
    }
    let sign = if bytes[i - 1] == b'+' { 1 } else { -1 };
    let attr_name = text[..i - 1].trim();
    let num: i32 = text[i..].parse().ok()?;

    Some((attr_name, sign * num))
}

fn canonical_attr(name: &str) -> &str {
    match name {
        "攻击力" => "攻击",
        "防御力" => "防御",
        "attack_speed" => "攻击速度",
        "block_cnt" => "阻挡数",
        "magic_resistance" => "法术抗性",
        "respawn_time" => "再部署时间",
        "cost" => "部署费用",
        _ => name,
    }
}

const CANONICAL_ATTRS: &[&str] = &[
    "生命上限", "攻击", "防御", "法术抗性",
    "部署费用", "阻挡数", "再部署时间", "攻击间隔",
];

// ═══════════════════════════════════════════════════════
// 输出格式化
// ═══════════════════════════════════════════════════════

fn format_stats(stats: &ComputedStats, out: &mut String) {
    let attr_order = [
        "生命上限", "攻击", "防御", "法术抗性",
        "部署费用", "阻挡数", "再部署时间", "攻击间隔",
    ];

    for attr in attr_order {
        if let Some(&(base, trust, pot, module)) = stats.attrs.get(attr) {
            let parts: Vec<String> = [
                if trust.abs() > 0.001 { Some(format!("{trust:+.0}(信赖)")) } else { None },
                if pot.abs() > 0.001 { Some(format!("{pot:+.0}(潜能)")) } else { None },
                if module.abs() > 0.001 { Some(format!("{module:+.0}(模组)")) } else { None },
            ]
            .into_iter()
            .flatten()
            .collect();

            let unit = match attr {
                "再部署时间" | "攻击间隔" => "s",
                _ => "",
            };

            let final_val = base + trust + pot + module;
            // 攻击间隔需要小数点精度（如 1.20），其余属性用整数
            let (base_disp, final_disp) = if attr == "攻击间隔" {
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

/// 输出单个技能。`detail_level` 指定等级（点单带等级前缀），None 输出最高级。
/// 模组输出。`filter=true`：只输出 selected 指向的模组（默认/模组=指定）；
/// `filter=false`：输出全部模组（附加列"模组列表"）。两者均遍历模组全部等级效果（累加展示）。
fn format_modules(modules: &[Module], selected: usize, filter: bool, out: &mut String) {
    for (i, m) in modules.iter().enumerate() {
        if filter && i != selected {
            continue;
        }
        let branch = match (&m.branch1, &m.branch2) {
            (Some(b1), Some(b2)) => format!("{b1}-{b2}"),
            (Some(b1), None) => b1.clone(),
            _ => String::new(),
        };
        out.push_str(&format!("[{}] {}\n", m.name, branch));

        for (i, effect) in m.effects.iter().enumerate() {
            let lv = i + 1;
            let mut parts: Vec<String> = Vec::new();

            if let Some(ref updates) = effect.stat_updates {
                let nums: Vec<String> = updates
                    .iter()
                    .map(|u| format!("{}{:+}", canonical_attr(&u.attr), u.value as i64))
                    .collect();
                if !nums.is_empty() {
                    parts.push(nums.join(" "));
                }
            }

            if let Some(ref t) = effect.trait_text {
                if !t.is_empty() {
                    parts.push(t.clone());
                }
            }

            if let Some(ref t) = effect.talent_update {
                if !t.is_empty() {
                    parts.push(format!("天赋更新：{t}"));
                }
            }

            if let Some(ref t) = effect.new_talent {
                if !t.is_empty() {
                    parts.push(format!("新增天赋：{t}"));
                }
            }

            if !parts.is_empty() {
                out.push_str(&format!("  Lv{lv}: {}\n", parts.join(" | ")));
            }
        }
    }
}

fn ordinal_cn(n: u32) -> String {
    match n {
        1 => "一".to_string(),
        2 => "二".to_string(),
        3 => "三".to_string(),
        _ => n.to_string(),
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn db_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arknights.db")
    }

    fn make_ctx() -> ToolContext {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        ToolContext::with_db(Arc::new(db))
    }

    #[test]
    fn tool_metadata() {
        let tool = QueryOperator;
        assert_eq!(tool.name(), "query_operator");
        assert!(tool.description().contains("干员"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"].get("fields").is_none(), "parameters 不应再有 fields");
        assert!(params["properties"].get("extra").is_some());
        // 增量声明契约由 extra 参数描述单点承载（desc 不再重复声明，2026-08-31）
        assert!(
            params["properties"]["extra"]["description"]
                .as_str()
                .map(|s| s.contains("增量声明"))
                .unwrap_or(false),
            "extra 参数应声明增量声明契约"
        );
    }

    // ── 默认结果 ──

    #[test]
    fn default_output_blocks() {
        // 无 extra：名称/备注/描述/职业/职业分支/属性/天赋/模组；技能需点单，不随默认返回
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{"names":["阿米娅"]}"#).unwrap().summary;
        assert!(result.contains("阿米娅"));
        assert!(result.contains("备注"));
        assert!(result.contains("描述"));
        assert!(result.contains("职业"));
        assert!(result.contains("职业分支"));
        assert!(result.contains("── 属性"));
        assert!(result.contains("── 天赋"));
        assert!(result.contains("── 模组"));
        assert!(result.contains("(信赖)"), "属性应标注信赖来源\n结果: {result}");
        assert!(result.contains("(模组)"), "属性应标注模组来源\n结果: {result}");
        assert!(!result.contains("── 技能"), "默认不输出技能区块，技能需点单\n结果: {result}");
    }

    #[test]
    fn query_default_stats() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{"names":["阿米娅"]}"#).unwrap().summary;
        assert!(result.contains("阿米娅"));
        assert!(result.contains("属性"));
        assert!(result.contains("天赋"));
        assert!(result.contains("模组"));
        assert!(!result.contains("── 技能"), "默认不输出技能区块\n结果: {result}");
    }

    #[test]
    fn skill_output_includes_range_id() {
        // 远山的二技能"命运"开启后攻击范围变为 3-3，
        // 技能详情输出应包含 rangeID，LLM 才能解码实际攻击范围
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{"names":["远山"],"extra":["二技能"]}"#).unwrap().summary;
        assert!(result.contains("攻击范围: 3-3"), "技能详情应输出攻击范围ID\n结果: {result}");
    }

    #[test]
    fn talent_picks_correct_alternative() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{"names":["阿米娅"]}"#).unwrap().summary;
        assert!(result.contains("天赋"));
        assert!(result.contains("额外回复2点技力"));
    }

    #[test]
    fn talent_expand_all_alternatives() {
        // 默认天赋区块：每个天赋只取当前最优备选（不展开全部备选）
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{"names":["阿米娅"]}"#).unwrap().summary;
        assert!(result.contains("情绪吸收"), "应包含天赋名\n结果: {result}");
        // 最优备选模式：带括号条件、无缩进备选列表
        assert!(result.contains("（精2）"), "默认最高精英化应为精2\n结果: {result}");
        assert!(!result.contains("  精0"), "不应展开全部备选\n结果: {result}");
    }

    // ── 状态指定（含 =）──

    #[test]
    fn extra_status_level_syntax() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let r1 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=精二40级"]}"#)
            .unwrap().summary;
        assert!(r1.contains("精2 40级"), "应显示精2 40级\n结果: {r1}");
        let r2 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=精一满级"]}"#)
            .unwrap().summary;
        assert!(r2.contains("精1"), "应显示精1\n结果: {r2}");
        assert!(!r2.contains("精2"), "不应显示精2\n结果: {r2}");
        let r3 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=精二"]}"#)
            .unwrap().summary;
        assert!(r3.contains("精2 80级"), "省略等级=该阶段满级（阿米娅精2满级80）\n结果: {r3}");
    }

    #[test]
    fn extra_status_potential_syntax() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let r1 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=满潜"]}"#)
            .unwrap().summary;
        assert!(r1.contains("5潜"), "满潜=5潜\n结果: {r1}");
        let r2 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=五潜"]}"#)
            .unwrap().summary;
        assert!(r2.contains("5潜"), "五潜=5潜\n结果: {r2}");
        let r3 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=0潜"]}"#)
            .unwrap().summary;
        assert!(r3.contains("0潜"), "0潜\n结果: {r3}");
    }

    #[test]
    fn extra_status_trust_syntax() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let r1 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["信赖=50"]}"#)
            .unwrap().summary;
        assert!(r1.contains("50%信赖"), "50 表示50%\n结果: {r1}");
        let r2 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["信赖=满"]}"#)
            .unwrap().summary;
        assert!(r2.contains("满信赖"), "满=100%\n结果: {r2}");
        let r3 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["信赖=零"]}"#)
            .unwrap().summary;
        assert!(r3.contains("0%信赖"), "零=0%\n结果: {r3}");
    }

    #[test]
    fn extra_status_module_syntax() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        // 模组=X模 → 选中 X 模，输出其完整效果（Lv1~3）
        let r1 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["模组=X模"]}"#)
            .unwrap().summary;
        assert!(r1.contains("DWDB-221E"), "应显示 X 模\n结果: {r1}");
        assert!(r1.contains("Lv1"), "应输出完整效果\n结果: {r1}");
        assert!(r1.contains("Lv3"), "应输出完整效果（累加展示）\n结果: {r1}");
        // 模组=无模组 → 属性不计模组加成、模组区块显示未装配
        let r2 = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["模组=无模组"]}"#)
            .unwrap().summary;
        assert!(r2.contains("未装配"), "模组区块应显示未装配\n结果: {r2}");
        assert!(!r2.contains("(模组)"), "属性不应有模组来源标注\n结果: {r2}");
    }

    #[test]
    fn extra_same_key_last_wins() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        // 潜能：最后一项（满潜=5潜）生效；反序则 0潜生效
        let r = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=0潜","潜能=五潜","潜能=满潜"]}"#)
            .unwrap().summary;
        assert!(r.contains("/ 5潜"), "got: {r}");
        let r = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=满潜","潜能=0潜"]}"#)
            .unwrap().summary;
        assert!(r.contains("/ 0潜"), "got: {r}");
        // 等级：整体替换（精一40级 + 精二 → 精二满级，不留 40级）
        let r = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=精一40级","等级=精二"]}"#)
            .unwrap().summary;
        assert!(r.contains("精2 80级"), "got: {r}");
        assert!(!r.contains("精2 40级"), "got: {r}");
    }

    #[test]
    fn extra_module_last_wins() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        // 无模组 → X模：以最后一项为准（X 模生效）
        let r = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["模组=无模组","模组=X模"]}"#)
            .unwrap().summary;
        assert!(r.contains("DWDB-221E"), "got: {r}");
        assert!(!r.contains("未装配"), "got: {r}");
        // X模 → 无模组：以最后一项为准（无模组生效）
        let r = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["模组=X模","模组=无模组"]}"#)
            .unwrap().summary;
        assert!(r.contains("未装配"), "got: {r}");
        assert!(!r.contains("DWDB-221E"), "got: {r}");
    }

    #[test]
    fn extra_potential_out_of_range_rejected() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let r = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=六潜"]}"#)
            .unwrap().summary;
        assert!(r.contains("超出范围"), "got: {r}");
    }

    #[test]
    fn extra_status_recomputes_talent() {
        // 状态指定后天赋随练度重算：等级=精一 → 天赋条件标注按精一
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=精一"]}"#)
            .unwrap().summary;
        assert!(result.contains("── 天赋"), "应含天赋区块\n结果: {result}");
        assert!(!result.contains("（精2）"), "精1练度不应出现精2天赋条件（重算生效）\n结果: {result}");
    }

    #[test]
    fn extra_status_invalid_key_rejected() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["职业=近卫"]}"#)
            .unwrap().summary;
        assert!(result.contains("✗"), "非法状态键应报错\n结果: {result}");
        assert!(result.contains("仅限 等级/潜能/信赖/模组"), "应指路合法键\n结果: {result}");
    }

    #[test]
    fn extra_status_invalid_value_rejected() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=三级跳"]}"#)
            .unwrap().summary;
        assert!(result.contains("✗"), "非法状态值应报错\n结果: {result}");
        assert!(result.contains("无法解析"), "应指路\n结果: {result}");
    }

    // ── 旧语法废除 ──

    #[test]
    fn extra_legacy_syntax_rejected() {
        // 旧裸写语法不再作为状态指定解析 → 附加列无法命中 → 报错指路键=值
        let ctx = make_ctx();
        let tool = QueryOperator;
        for legacy in ["精一20级", "五潜", "零信赖", "X模"] {
            let result = tool
                .execute(&ctx, &format!(r#"{{"names":["阿米娅"],"extra":["{legacy}"]}}"#))
                .unwrap().summary;
            assert!(result.contains("✗"), "旧语法 '{legacy}' 应报错\n结果: {result}");
            assert!(result.contains("键=值"), "旧语法 '{legacy}' 应指路键=值\n结果: {result}");
        }
    }

    // ── 技能点单 ──

    #[test]
    fn extra_skill_by_name() {
        // 技能名指定 → 只输出该技能最高级
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["缓蚀"]}"#)
            .unwrap().summary;
        assert!(result.contains("[缓蚀]"));
        assert!(!result.contains("[封护]"), "不应输出其他技能\n结果: {result}");
        assert!(!result.contains("[旧日绽放]"), "不应输出其他技能\n结果: {result}");
    }

    #[test]
    fn extra_skill_by_name_with_level() {
        // "3级缓蚀" → 缓蚀 Lv3
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["3级缓蚀"]}"#)
            .unwrap().summary;
        assert!(result.contains("[缓蚀]"));
        assert!(result.contains("Lv3: 攻击力+60%"), "应显示缓蚀 Lv3\n结果: {result}");
        assert!(!result.contains("[封护]"), "不应输出其他技能\n结果: {result}");
        assert!(!result.contains("[旧日绽放]"), "不应输出其他技能\n结果: {result}");
    }

    #[test]
    fn extra_skill_by_name_with_mastery() {
        // "专2旧日绽放" → 旧日绽放专2
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["专2旧日绽放"]}"#)
            .unwrap().summary;
        assert!(result.contains("[旧日绽放]"));
        assert!(result.contains("专2"), "应显示专2\n结果: {result}");
        assert!(!result.contains("专3"), "不应显示专3\n结果: {result}");
    }

    #[test]
    fn extra_skill_name_starting_with_jing() {
        // 技能名"精神爆发"以"精"开头，不应被"精N"练度解析误捕获
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["精神爆发"]}"#)
            .unwrap().summary;
        assert!(result.contains("[精神爆发]"), "应匹配技能精神爆发\n结果: {result}");
        assert!(!result.contains("[战术咏唱"), "不应输出其他技能\n结果: {result}");
        assert!(!result.contains("[奇美拉]"), "不应输出其他技能\n结果: {result}");
    }

    #[test]
    fn extra_skill_select_single() {
        // 指定"二技能"，只输出一个技能
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["二技能"]}"#)
            .unwrap().summary;
        // 缇缇三个技能：缓蚀/封护/旧日绽放
        assert!(!result.contains("缓蚀"), "指定二技能不应含一技能\n结果: {result}");
        assert!(result.contains("封护"), "应包含二技能\n结果: {result}");
        assert!(!result.contains("旧日绽放"), "指定二技能不应含三技能\n结果: {result}");
    }

    #[test]
    fn extra_skill_level_number() {
        // "3级一技能"，只显示第一个技能 Lv3
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["3级一技能"]}"#)
            .unwrap().summary;
        assert!(result.contains("Lv3"), "应显示 Lv3\n结果: {result}");
        assert!(!result.contains("专3"), "Lv3 不应显示专3\n结果: {result}");
        assert!(!result.contains("封护"), "不应包含二技能\n结果: {result}");
        assert!(!result.contains("旧日绽放"), "不应包含三技能\n结果: {result}");
    }

    #[test]
    fn extra_skill_mastery() {
        // "专2二技能"，显示专2
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["专2二技能"]}"#)
            .unwrap().summary;
        assert!(result.contains("专2"), "应显示专2\n结果: {result}");
        assert!(!result.contains("专3"), "不应显示专3\n结果: {result}");
    }

    #[test]
    fn extra_skill_out_of_range() {
        // 指定超出实际技能数的编号
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["五技能"]}"#)
            .unwrap().summary;
        assert!(result.contains("无第5个技能"), "超界应提示\n结果: {result}");
    }

    #[test]
    fn extra_skill_multi_select() {
        // 一次指定多个技能点单，每个独立输出
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["二技能","3级三技能"]}"#)
            .unwrap().summary;
        assert!(result.contains("[封护]"), "应输出二技能\n结果: {result}");
        assert!(result.contains("[旧日绽放]"), "应输出三技能\n结果: {result}");
        assert!(result.contains("Lv3"), "三技能应显示 Lv3\n结果: {result}");
        assert!(!result.contains("[缓蚀]"), "不应输出一技能\n结果: {result}");
    }

    #[test]
    fn extra_mixed_elite_and_skill() {
        // 同时指定精英化和技能选择，默认输出全部区块
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["等级=精一满级","三技能"]}"#)
            .unwrap().summary;
        // 属性块应显示精1
        assert!(result.contains("精1"), "应包含精1\n结果: {result}");
        // 技能块只含三技能
        assert!(result.contains("旧日绽放"), "应包含三技能\n结果: {result}");
        assert!(!result.contains("封护"), "不应包含二技能\n结果: {result}");
    }

    // ── 技能/天赋 禁用项 ──

    #[test]
    fn extra_skill_list_block_rejected() {
        // "技能列表" 不提供整列获取 → 报错指路点单制
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["技能列表"]}"#)
            .unwrap().summary;
        assert!(result.contains("不提供整列"), "应指路点单制\n结果: {result}");
        assert!(result.contains("技能请点单"), "应指路点单制\n结果: {result}");
    }

    #[test]
    fn extra_talent_point_rejected() {
        // "天赋二" 天赋点单 → 报错指路
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["天赋二"]}"#)
            .unwrap().summary;
        assert!(result.contains("天赋不提供点单"), "应指路\n结果: {result}");
    }

    #[test]
    fn extra_talent_name_rejected() {
        // 天赋名点单（旧"凝固的时光"能力）→ 报错指路
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["凝固的时光"]}"#)
            .unwrap().summary;
        assert!(result.contains("天赋不提供点单"), "应指路\n结果: {result}");
    }

    #[test]
    fn extra_unknown_extra_error() {
        // 完全未知的字符串 → 报错并指路（键=值 / 知识库列名）
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["不存在的技能xyz"]}"#)
            .unwrap().summary;
        assert!(result.contains("✗"), "应有报错\n结果: {result}");
        assert!(result.contains("键=值"), "应指路键=值\n结果: {result}");
    }

    // ── 附加列（不含 =）──

    #[test]
    fn extra_common_columns() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["稀有度","部署位置","代号","标签","获取方式","潜能提升","关联token"]}"#)
            .unwrap().summary;
        assert!(result.contains("稀有度: 5"), "应输出稀有度（阿米娅5星）\n结果: {result}");
        assert!(result.contains("部署位置: "), "应输出部署位置\n结果: {result}");
        assert!(result.contains("代号: "), "应输出代号\n结果: {result}");
        assert!(result.contains("标签: "), "应输出标签\n结果: {result}");
        assert!(result.contains("获取方式: "), "应输出获取方式\n结果: {result}");
        assert!(result.contains("── 潜能提升 ──"), "应输出潜能提升\n结果: {result}");
        assert!(result.contains("关联token: "), "应输出关联token\n结果: {result}");
    }

    #[test]
    fn extra_table_column_raw() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["干员ID","最大潜能","是否不可获取"]}"#)
            .unwrap().summary;
        assert!(result.contains("干员ID: char_002_amiya"), "应输出干员ID\n结果: {result}");
        assert!(result.contains("最大潜能: "), "应输出最大潜能\n结果: {result}");
        assert!(result.contains("是否不可获取: "), "应输出是否不可获取\n结果: {result}");
    }

    #[test]
    fn extra_table_column_json() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["精英化阶段","信赖加成"]}"#)
            .unwrap().summary;
        assert!(result.contains("── 精英化阶段（完整数据）──"), "应输出精英化阶段 raw\n结果: {result}");
        assert!(result.contains("信赖加成（满信赖）"), "应输出信赖加成\n结果: {result}");
    }

    #[test]
    fn extra_table_column_talent_list() {
        // 附加列"天赋列表" → 全部天赋的全部备选展开，与默认天赋区块并存
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["缇缇"],"extra":["天赋列表"]}"#)
            .unwrap().summary;
        assert!(result.contains("── 天赋 ──"), "默认天赋区块应照常输出（并存）\n结果: {result}");
        assert!(result.contains("── 天赋列表（完整数据）──"), "应输出完整天赋列表\n结果: {result}");
        assert!(result.contains("[凝固的时光]"), "应包含天赋名\n结果: {result}");
        assert!(result.contains("[勇气的报偿]"), "应包含天赋名\n结果: {result}");
        assert!(result.contains("  精"), "展开模式应有缩进条件标注\n结果: {result}");
    }

    #[test]
    fn extra_table_column_module_list() {
        // 附加列"模组列表" → 所有模组的全部等级效果，与默认模组区块并存
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["模组列表"]}"#)
            .unwrap().summary;
        assert!(result.contains("── 模组 ──"), "默认模组区块应照常输出（并存）\n结果: {result}");
        assert!(result.contains("── 模组列表（完整数据）──"), "应输出完整模组列表\n结果: {result}");
        assert!(result.contains("Lv1"), "应包含全部等级效果\n结果: {result}");
        assert!(result.contains("Lv3"), "应包含全部等级效果\n结果: {result}");
    }

    #[test]
    fn extra_unknown_column_error() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["不存在的列"]}"#)
            .unwrap().summary;
        assert!(result.contains("✗"), "未命中应报错\n结果: {result}");
        assert!(result.contains("知识库"), "应指路知识库列名\n结果: {result}");
    }

    // ── 练度/数值相关（语法迁移后的旧用例）──

    #[test]
    fn extra_elite_one() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=精一满级"]}"#)
            .unwrap().summary;
        assert!(result.contains("精1"));
        assert!(!result.contains("精2"));
    }

    #[test]
    fn extra_module_x() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["模组=X模"]}"#)
            .unwrap().summary;
        assert!(result.contains("DWDB-221E"), "应显示默认模组");
    }

    #[test]
    fn extra_potential() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=五潜"]}"#)
            .unwrap().summary;
        assert!(result.contains("5潜"));
    }

    #[test]
    fn extra_trust() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["信赖=零"]}"#)
            .unwrap().summary;
        assert!(result.contains("0%信赖"));
    }

    #[test]
    fn stats_with_trust_visible() {
        // 默认输出插值属性（含信赖、模组来源标注）
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["等级=精二满级"]}"#)
            .unwrap().summary;
        assert!(result.contains("信赖"));
        assert!(result.contains("模组"));
    }

    #[test]
    fn talent_with_potential() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool
            .execute(&ctx, r#"{"names":["阿米娅"],"extra":["潜能=五潜"]}"#)
            .unwrap().summary;
        assert!(result.contains("额外回复3") || result.contains("+1"));
    }

    // ── 边界 ──

    #[test]
    fn missing_name_returns_error() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{}"#);
        assert!(result.is_err());
    }

    #[test]
    fn not_found_returns_message() {
        let ctx = make_ctx();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{"names":["绝对不存在的干员xyz"]}"#).unwrap().summary;
        assert!(result.contains("未找到"));
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new();
        let tool = QueryOperator;
        let result = tool.execute(&ctx, r#"{"names":["阿米娅"]}"#);
        assert!(result.is_err());
    }

    // ── 单元解析 ──

    #[test]
    fn parse_potential_bonus_attack() {
        assert_eq!(parse_potential_bonus("攻击力+30"), Some(("攻击力", 30)));
    }

    #[test]
    fn parse_potential_bonus_cost_down() {
        assert_eq!(parse_potential_bonus("部署费用-1"), Some(("部署费用", -1)));
    }

    #[test]
    fn parse_potential_bonus_talent() {
        assert_eq!(parse_potential_bonus("天赋效果增强"), None);
    }

    #[test]
    fn parse_potential_bonus_atk_speed() {
        assert_eq!(parse_potential_bonus("攻击速度+8"), Some(("攻击速度", 8)));
    }

    #[test]
    fn canonical_attr_mapping() {
        assert_eq!(canonical_attr("攻击力"), "攻击");
        assert_eq!(canonical_attr("防御力"), "防御");
        assert_eq!(canonical_attr("attack_speed"), "攻击速度");
        assert_eq!(canonical_attr("block_cnt"), "阻挡数");
    }

    #[test]
    fn split_digits_basic() {
        assert_eq!(split_digits("一20级"), ("一", "20级"));
        assert_eq!(split_digits("二满级"), ("二", "满级"));
        assert_eq!(split_digits("二"), ("二", ""));
        assert_eq!(split_digits("一"), ("一", ""));
    }

    #[test]
    fn parse_skill_index_basic() {
        assert_eq!(parse_skill_index("一技能"), Some((0, None)));
        assert_eq!(parse_skill_index("7级2技能"), Some((1, Some(7))));
        assert_eq!(parse_skill_index("专2二技能"), Some((1, Some(9))));
        assert_eq!(parse_skill_index("1级技能"), None);
        assert_eq!(parse_skill_index("火山"), None);
    }

    #[test]
    fn is_talent_point_syntax_basic() {
        assert!(is_talent_point_syntax("天赋二"));
        assert!(is_talent_point_syntax("一天赋"));
        assert!(is_talent_point_syntax("天赋1"));
        assert!(!is_talent_point_syntax("天赋"));
        assert!(!is_talent_point_syntax("凝固的时光"));
    }

    #[test]
    fn show_output() {
        fn call(ctx: &ToolContext, name: &str, extra: Option<&[&str]>) -> String {
            let params = match extra {
                Some(ext) => serde_json::json!({
                    "names": [name], "extra": ext
                }),
                None => serde_json::json!({"names": [name]}),
            };
            let tool = QueryOperator;
            match tool.execute(ctx, &params.to_string()) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        }

        let ctx = make_ctx();

        // 支持环境变量交互：PRTS_NAME / PRTS_EXTRA（逗号分隔）
        if let Ok(name) = std::env::var("PRTS_NAME") {
            let evec: Option<Vec<String>> = std::env::var("PRTS_EXTRA").ok().map(|s| {
                if s.is_empty() { vec![] } else { s.split(',').map(|x| x.trim().to_string()).collect() }
            });
            let extra_ref: Option<Vec<&str>> = evec.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect());
            println!("{}  extra={:?}", name, extra_ref);
            println!("{}", call(&ctx, &name, extra_ref.as_deref()));
            return;
        }

        let cases: &[(&str, &str, Option<&[&str]>)] = &[
            ("1. 阿米娅 · 默认", "阿米娅", None),
            ("2. 阿米娅 · 等级=精一满级", "阿米娅", Some(&["等级=精一满级"])),
            ("3. 缇缇 · 技能点单（二技能 + 3级三技能）", "缇缇", Some(&["二技能", "3级三技能"])),
            ("4. 阿米娅 · 潜能=五潜 + 模组=无模组", "阿米娅", Some(&["潜能=五潜", "模组=无模组"])),
            ("5. 缇缇 · 附加列 天赋列表", "缇缇", Some(&["天赋列表"])),
            ("6. 阿米娅 · 旧语法报错示例", "阿米娅", Some(&["精一20级"])),
        ];
        for (label, name, extra) in cases {
            println!("========== {label} ==========");
            println!("{}", call(&ctx, name, *extra));
            println!();
        }
    }
}
