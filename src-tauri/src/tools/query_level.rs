//! query_level —— 关卡深入查询工具
//!
//! 语义：输入 levelId → 返回三部分结构化信息。
//! levelId 由 query_stage 获取，单次只查一个关卡。
//!
//! ## 输出三部分
//!
//! ① 地图基本信息：地块坐标分组（按类型列出坐标区间）+ 地块属性表（通行/部署/描述）
//! ② 路线信息：路线列表 + 额外路线列表
//! ③ 波次信息：波次时间线 + 随机组展开 + key 翻译 + 分支信息
//!
//! 输出格式详见 spec `Phase3-第二轮开发规范.md` 6.7 节。
//!
//! ## 文件组织
//!
//! 本文件有意保持单文件——三部分逻辑共享同一组反序列化结构体和
//! key 翻译缓存，拆分反而增加跨模块传参的复杂度。内部按 `// ═══`
//! 分隔符划分为四个逻辑段：常量/辅助 → 地图 → 路线 → 波次。
//! 后续若单段增长超过 500 行，再考虑提取子模块。

use crate::db::{Database, Filter, FilterOp};
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

use serde::Deserialize;
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════
// JSON 反序列化结构体
// ═══════════════════════════════════════════════════════

#[derive(Deserialize)]
struct MapData {
    width: i64,
    height: i64,
    grid: Vec<Vec<usize>>,
    tiles: Vec<TileEntry>,
}

#[derive(Deserialize)]
struct TileEntry(
    String, // tileKey
    i64,    // heightType: 0=低地 1=高台
    i64,    // buildableType: 0=不可 1=近战 2=远程 3=全部
    i64,    // passableMask: 0=不可 2=仅飞行 3=全部
);

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct WaveData {
    #[serde(default)]
    preDelay: f64,
    #[serde(default)]
    postDelay: f64,
    #[serde(default)]
    fragments: Vec<FragmentData>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct FragmentData {
    #[serde(default)]
    preDelay: f64,
    #[serde(default)]
    actions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct BranchPhase {
    #[serde(default)]
    preDelay: f64,
    #[serde(default)]
    actions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct BranchValue {
    #[serde(default)]
    phases: Vec<BranchPhase>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct RuneEntry {
    key: String,
    #[serde(default)]
    professionMask: serde_json::Value,
    #[serde(default)]
    buildableMask: serde_json::Value,
    #[serde(default)]
    blackboard: Vec<BlackboardItem>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct BlackboardItem {
    key: String,
    #[serde(default)]
    value: f64,
    #[serde(default)]
    valueStr: Option<String>,
}

/// 预置单位：4 元素数组，依次 [tokenInsts, characterInsts, tokenCards, characterCards]
type Predefines = Vec<Vec<Vec<serde_json::Value>>>;

// ═══════════════════════════════════════════════════════
// 常量映射
// ═══════════════════════════════════════════════════════

const ACTION_TYPE_NAMES: &[(&str, &str)] = &[
    ("0", "生成"),
    ("1", "显示路线"),
    ("2", "剧情浮窗"),
    ("3", "教程"),
    ("4", "切换BGM"),
    ("5", "显示敌人信息"),
    ("6", "预部署激活"),
    ("7", "画面特效"),
    ("8", "预部署触发"),
    ("9", "战斗事件"),
    ("10", "预部署撤回"),
    ("11", "局内演出"),
    ("12", "特定函数动作"),
    ("13", "空占位"),
];

const MOTION_MODE_NAMES: &[(&str, &str)] = &[("0", "WALK"), ("1", "FLY"), ("2", "E_NUM")];

const PHASE_NAMES: &[(&str, &str)] = &[("0", "精英0"), ("1", "精英1"), ("2", "精英2")];

const DIRECTION_NAMES: &[(&str, &str)] = &[("0", "上"), ("1", "右"), ("2", "下"), ("3", "左")];

fn action_type_name(t: i64) -> String {
    let s = t.to_string();
    for (k, v) in ACTION_TYPE_NAMES {
        if *k == s {
            return v.to_string();
        }
    }
    format!("类型{t}")
}

fn motion_mode_name(m: i64) -> String {
    let s = m.to_string();
    for (k, v) in MOTION_MODE_NAMES {
        if *k == s {
            return v.to_string();
        }
    }
    format!("mode{m}")
}

fn phase_name(p: i64) -> String {
    let s = p.to_string();
    for (k, v) in PHASE_NAMES {
        if *k == s {
            return v.to_string();
        }
    }
    format!("阶段{p}")
}

fn direction_name(d: i64) -> String {
    let s = d.to_string();
    for (k, v) in DIRECTION_NAMES {
        if *k == s {
            return v.to_string();
        }
    }
    format!("朝向{d}")
}

fn is_default_action_type(t: i64) -> bool {
    matches!(t, 0 | 1 | 5 | 6 | 7 | 8 | 9 | 10)
}

fn is_special_action_type(t: i64) -> bool {
    matches!(t, 2 | 4 | 11 | 13)
}

// ═══════════════════════════════════════════════════════
// action 数组提取辅助
// ═══════════════════════════════════════════════════════

fn act_i64(arr: &[serde_json::Value], idx: usize) -> i64 {
    arr.get(idx).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn act_f64(arr: &[serde_json::Value], idx: usize) -> f64 {
    arr.get(idx).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn act_str<'a>(arr: &'a [serde_json::Value], idx: usize) -> Option<&'a str> {
    arr.get(idx).and_then(|v| v.as_str())
}

fn inst_i64(arr: &[serde_json::Value], idx: usize) -> i64 {
    arr.get(idx).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn inst_str<'a>(arr: &'a [serde_json::Value], idx: usize) -> Option<&'a str> {
    arr.get(idx).and_then(|v| v.as_str())
}

fn inst_pos(arr: &[serde_json::Value], idx: usize) -> (i64, i64) {
    if let Some(pos) = arr.get(idx).and_then(|v| v.as_array()) {
        let row = pos.first().and_then(|v| v.as_i64()).unwrap_or(0);
        let col = pos.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
        (row, col)
    } else {
        (0, 0)
    }
}

fn inst_equip_ids(arr: &[serde_json::Value], idx: usize) -> Vec<String> {
    arr.get(idx)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ═══════════════════════════════════════════════════════
// 格式化辅助
// ═══════════════════════════════════════════════════════

/// 浮点数格式化：保留一位小数
fn fmt_float(v: f64) -> String {
    format!("{:.1}", v)
}

/// 批量查询 key→名称 映射
///
/// 当前从四表（enemies/traps/operators/tokens）全量加载到 HashMap，
/// 不做 LIMIT。62MB 数据库规模下总量可控（<5000 条），
/// 后续若翻译需求扩大可考虑按需缓存或 LRU 淘汰。
fn build_key_translations(db: &Database) -> HashMap<String, String> {
    let mut cache = HashMap::new();

    // enemies: 敌人ID → 名称（级别 0）
    if let Ok(result) = db.query_filtered(
        "enemies",
        &["敌人ID".into(), "名称".into()],
        &[Filter::new("级别", FilterOp::Eq, "0")],
        0,
    ) {
        for row in result.rows {
            if let (Some(id), Some(name)) = (row.get("敌人ID"), row.get("名称")) {
                if !id.is_empty() {
                    cache.insert(id.clone(), name.clone());
                }
            }
        }
    }

    // traps: 装置ID → 名称
    if let Ok(result) = db.query_filtered("traps", &["装置ID".into(), "名称".into()], &[], 0) {
        for row in result.rows {
            if let (Some(id), Some(name)) = (row.get("装置ID"), row.get("名称")) {
                if !id.is_empty() {
                    cache.insert(id.clone(), name.clone());
                }
            }
        }
    }

    // operators: 干员ID → 名称
    if let Ok(result) = db.query_filtered("operators", &["干员ID".into(), "名称".into()], &[], 0)
    {
        for row in result.rows {
            if let (Some(id), Some(name)) = (row.get("干员ID"), row.get("名称")) {
                if !id.is_empty() {
                    cache.insert(id.clone(), name.clone());
                }
            }
        }
    }

    // tokens: tokenID → 名称
    if let Ok(result) = db.query_filtered("tokens", &["tokenID".into(), "名称".into()], &[], 0) {
        for row in result.rows {
            if let (Some(id), Some(name)) = (row.get("tokenID"), row.get("名称")) {
                if !id.is_empty() {
                    cache.insert(id.clone(), name.clone());
                }
            }
        }
    }

    cache
}

fn translate_key(key: &str, cache: &HashMap<String, String>) -> String {
    // "default" 直接透传
    if key == "default" {
        return key.to_string();
    }
    // 空 key
    if key.is_empty() {
        return "（无）".to_string();
    }
    // 直接命中
    if let Some(name) = cache.get(key) {
        return format!("{name}({key})");
    }
    // 去掉 # 后缀的主 key 匹配
    if let Some(hash_pos) = key.find('#') {
        let main = &key[..hash_pos];
        if let Some(name) = cache.get(main) {
            return format!("{name}({key})");
        }
    }
    key.to_string()
}

// ═══════════════════════════════════════════════════════
// Tool trait 实现
// ═══════════════════════════════════════════════════════

pub struct QueryLevel;

impl Tool for QueryLevel {
    fn name(&self) -> &str {
        "query_level"
    }

    fn description(&self) -> &str {
        "按 levelId 精确查询关卡信息，单次只查一个关卡。fields 必填选择：地图、路线、波次为常用输出。地图输出会额外给出不同地块的信息，无需额外查询，后两者数据量大，如无必要勿查询。levelId 需从 query_stage 获取"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "levelId": {
                    "type": "string",
                    "description": "关卡 levelId，如 level_main_01-07。单次只查一个关卡"
                },
                "fields": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "enum": ["地图", "预置单位", "关卡符文", "路线", "波次"]
                    },
                    "description": "必填，选择输出部分，可多选如 [\"地图\", \"波次\"]。常用输出：地图 = 地块坐标分组+ 地块属性表；路线 = 敌人移动路径与额外路线；波次 = 刷怪调度与分支。预置单位、关卡符文 = 原始 JSON 文本，仅作保底对照，通常不需要（关卡情报与描述已提供自然语言信息）。"
                }
            },
            "required": ["levelId", "fields"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let level = v.get("levelId").and_then(|x| x.as_str()).unwrap_or("");
        let fields = v
            .get("fields")
            .and_then(|f| f.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            })
            .unwrap_or_default();
        match (level.is_empty(), fields.is_empty()) {
            (true, true) => ToolDisplay::default(),
            (false, true) => ToolDisplay::info(level),
            (true, false) => ToolDisplay::info(fields),
            (false, false) => ToolDisplay::info(format!("{level} · {fields}")),
        }
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        // ── 1. 解析参数 ──
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        let level_id = v["levelId"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("缺少必填参数: levelId".into()))?;

        // fields 必填（同 query_enemy/stage）：空数组报错，不设"默认全部"
        let fields: Vec<&str> = v["fields"]
            .as_array()
            .ok_or_else(|| ToolError::Parse("参数 fields 必须是数组".into()))?
            .iter()
            .filter_map(|f| f.as_str())
            .collect();
        if fields.is_empty() {
            return Err(ToolError::Parse("参数 fields 不能为空".into()));
        }

        let show_map = fields.iter().any(|f| *f == "地图");
        let show_predefines = fields.iter().any(|f| *f == "预置单位");
        let show_runes = fields.iter().any(|f| *f == "关卡符文");
        let show_routes = fields.iter().any(|f| *f == "路线");
        let show_waves = fields.iter().any(|f| *f == "波次");

        // ── 2. 查询数据库 ──
        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        let result = db
            .query_filtered(
                "levels",
                &[
                    "levelId".into(),
                    "地图数据".into(),
                    "路线".into(),
                    "额外路线".into(),
                    "波次".into(),
                    "分支".into(),
                    "预置单位".into(),
                    "关卡符文".into(),
                ],
                &[Filter::new("levelId", FilterOp::Eq, level_id)],
                1,
            )
            .map_err(|e| ToolError::Execution(e))?;

        let row = result
            .rows
            .into_iter()
            .next()
            .ok_or_else(|| ToolError::Execution(format!("未找到关卡: {level_id}")))?;

        // ── 3. 构建 key 翻译缓存（需要时）──
        let key_cache = if show_predefines || show_waves {
            Some(build_key_translations(db))
        } else {
            None
        };

        // ── 4. 格式化各部分 ──
        let mut parts: Vec<String> = Vec::new();

        if show_map {
            parts.push(format_map_section(&row)?);
        }
        if show_predefines {
            parts.push(format_predefines_section(&row, key_cache.as_ref())?);
        }
        if show_runes {
            parts.push(format_runes_section(&row)?);
        }
        if show_routes {
            parts.push(format_routes_section(&row)?);
        }
        if show_waves {
            parts.push(format_waves_section(&row, key_cache.as_ref())?);
        }

        Ok(ToolOutput::json(
            serde_json::json!({
                "rows": [row],
                "dropped_fields": result.dropped_fields,
            }),
            parts.join("\n"),
        ))
    }
}

// ═══════════════════════════════════════════════════════
// 一、地图（地块坐标分组 + 地块属性表）
// ═══════════════════════════════════════════════════════

/// 属性表不附描述文本的白名单地块名：属性列（高度·通行·部署）已完整
/// 表达语义，tile_info.json 的自然语言描述与之重复（如平地"可以放置
/// 近战单位，可以通行"）。白名单外类型均附加描述文本（地穴、冰面、
/// 化境地块等特殊效果地块）。
const NO_DESC_WHITELIST: &[&str] = &[
    "侵入点",
    "空袭侵入点",
    "保护目标",
    "通道入口",
    "通道出口",
    "不可放置位",
    "禁入区",
    "平地",
    "高台",
];

/// tile 信息缓存：tileKey → (中文名, 描述文本)。编译期嵌入，一次解析。
/// 中文名与描述来自 tile_info.json（与 query_table 的 tile 表同源）；
/// 名称为空（待补充）时回退 tileKey。
fn tile_info_cache() -> &'static HashMap<String, (String, String)> {
    static CACHE: std::sync::OnceLock<HashMap<String, (String, String)>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let raw: HashMap<String, serde_json::Value> =
            serde_json::from_str(include_str!("../../resources/tile_info.json"))
                .expect("tile_info.json 格式错误");
        raw.into_iter()
            .map(|(k, v)| {
                let name = v
                    .get("名称")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let desc = v
                    .get("描述")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                (k, (name, desc))
            })
            .collect()
    })
}

/// 地块通行/部署属性中文描述：`低地·全通行，近战部署`。
fn tile_attrs_desc(tile: &TileEntry) -> String {
    let height = if tile.1 == 1 { "高台" } else { "低地" };
    let pass = match tile.3 {
        3 => "全通行",
        2 => "仅飞行",
        _ => "不可通行",
    };
    let build = match tile.2 {
        1 => "近战部署",
        2 => "远程部署",
        3 => "全部部署",
        _ => "不可部署",
    };
    format!("{height}·{pass}，{build}")
}

/// 地块坐标分组：按（显示名, 属性描述）聚合，组内坐标按 (y, x) 排序。
/// 同名同属性的不同 tileKey（如化境地块 1/2/3）合并为一行；
/// 同名不同属性（如保护目标的不同形态）靠属性列区分。
struct TileGroup {
    name: String,
    attrs: String,
    desc: String,
    coords: Vec<(i64, i64)>,
}

fn group_tile_coords(map: &MapData) -> Vec<TileGroup> {
    let info = tile_info_cache();
    let mut order: Vec<String> = Vec::new(); // 分组键（name|attrs）首次出现顺序
    let mut groups: HashMap<String, TileGroup> = HashMap::new();
    for (r, row) in map.grid.iter().enumerate() {
        // grid[0] 为顶部行（y = height-1），坐标 y 自底向上
        let y = map.height - 1 - r as i64;
        for (x, &idx) in row.iter().enumerate() {
            let tile = match map.tiles.get(idx) {
                Some(t) => t,
                None => continue,
            };
            let name = info
                .get(&tile.0)
                .map(|(n, _)| n.as_str())
                .filter(|n| !n.is_empty())
                .unwrap_or(tile.0.as_str())
                .to_string();
            let attrs = tile_attrs_desc(tile);
            let key = format!("{name}|{attrs}");
            let group = groups.entry(key.clone()).or_insert_with(|| {
                order.push(key);
                let desc = if NO_DESC_WHITELIST.contains(&name.as_str()) {
                    String::new()
                } else {
                    info.get(&tile.0)
                        .map(|(_, d)| d.clone())
                        .unwrap_or_default()
                };
                TileGroup {
                    name: name.clone(),
                    attrs,
                    desc,
                    coords: Vec::new(),
                }
            });
            group.coords.push((x as i64, y as i64));
        }
    }
    let mut out: Vec<TileGroup> = Vec::with_capacity(order.len());
    for key in order {
        out.push(groups.remove(&key).unwrap());
    }
    for g in &mut out {
        g.coords.sort_by_key(|&(x, y)| (y, x));
    }
    out
}

/// 坐标区间压缩：同行连续 x 合并为 (s,y)~(e,y)，其余输出单格 (x,y)。
fn fmt_coord_group(coords: &[(i64, i64)]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < coords.len() {
        let (x0, y0) = coords[i];
        let mut j = i;
        while j + 1 < coords.len() && coords[j + 1].1 == y0 && coords[j + 1].0 == coords[j].0 + 1 {
            j += 1;
        }
        if j > i {
            parts.push(format!("({x0},{y0})~({},{y0})", coords[j].0));
        } else {
            parts.push(format!("({x0},{y0})"));
        }
        i = j + 1;
    }
    parts.join(", ")
}

fn format_map(map_data: &MapData) -> String {
    let mut lines = Vec::new();
    lines.push(format!("【地图】{}×{}", map_data.width, map_data.height));
    lines.push(String::new());

    // ── 地块坐标：按类型分组，行内连续区间 ──
    lines.push("【地块坐标】按地块类型分组，行内连续坐标以 ~ 连接".to_string());
    lines.push(String::new());
    let groups = group_tile_coords(map_data);
    // 同名分组（不同属性）在坐标行附加完整属性作消歧锚，
    // 与属性表的属性段直接匹配，不依赖行顺序
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for g in &groups {
        *name_counts.entry(g.name.as_str()).or_insert(0) += 1;
    }
    for g in &groups {
        if name_counts[g.name.as_str()] > 1 {
            lines.push(format!(
                "{}({}): {}",
                g.name,
                g.attrs,
                fmt_coord_group(&g.coords)
            ));
        } else {
            lines.push(format!("{}: {}", g.name, fmt_coord_group(&g.coords)));
        }
    }
    lines.push(String::new());

    // ── 地块属性表：通行/部署属性 + 描述文本（白名单外）──
    lines.push("【地块属性表】".to_string());
    lines.push(String::new());
    for g in &groups {
        if g.desc.is_empty() {
            lines.push(format!("{}: {}", g.name, g.attrs));
        } else {
            lines.push(format!("{}: {}，{}", g.name, g.attrs, g.desc));
        }
    }

    lines.join("\n")
}

fn format_map_section(row: &HashMap<String, String>) -> Result<String, ToolError> {
    let mut lines = Vec::new();
    lines.push("══════ 地图 ══════".to_string());
    lines.push(String::new());
    let map_data: MapData = parse_json_field(row, "地图数据")?;
    lines.push(format_map(&map_data));
    Ok(lines.join("\n"))
}

// ═══════════════════════════════════════════════════════
// 二、预置单位
// ═══════════════════════════════════════════════════════

fn format_predefines_section(
    row: &HashMap<String, String>,
    key_cache: Option<&HashMap<String, String>>,
) -> Result<String, ToolError> {
    let mut lines = Vec::new();
    lines.push("══════ 预置单位 ══════".to_string());
    lines.push(String::new());
    let predefines_str = row
        .get("预置单位")
        .filter(|s| !s.is_empty() && *s != "null");
    if let Some(raw) = predefines_str {
        let key_cache = key_cache.ok_or_else(|| ToolError::Execution("缓存未初始化".into()))?;
        lines.push(format_predefines(raw, key_cache));
    } else {
        lines.push("无".to_string());
    }
    Ok(lines.join("\n"))
}

// ═══════════════════════════════════════════════════════
// 三、关卡符文
// ═══════════════════════════════════════════════════════

fn format_runes_section(row: &HashMap<String, String>) -> Result<String, ToolError> {
    let mut lines = Vec::new();
    lines.push("══════ 关卡符文 ══════".to_string());
    lines.push(String::new());
    let runes_str = row.get("关卡符文").map(|s| s.as_str()).unwrap_or("{}");
    if runes_str == "{}" || runes_str.is_empty() {
        lines.push("无".to_string());
    } else {
        lines.push(format_runes(runes_str)?);
    }
    Ok(lines.join("\n"))
}

fn parse_json_field<T: serde::de::DeserializeOwned>(
    row: &HashMap<String, String>,
    field: &str,
) -> Result<T, ToolError> {
    let raw = row
        .get(field)
        .ok_or_else(|| ToolError::Execution(format!("字段 {} 缺失", field)))?;
    if raw == "null" {
        return Err(ToolError::Execution(format!("字段 {} 为 null", field)));
    }
    serde_json::from_str(raw)
        .map_err(|e| ToolError::Execution(format!("解析 {} 失败: {}", field, e)))
}

// ── 预置单位 ──

fn format_predefines(raw: &str, key_cache: &HashMap<String, String>) -> String {
    let data: Predefines = match serde_json::from_str(raw) {
        Ok(d) => d,
        Err(e) => return format!("解析预置单位失败: {e}"),
    };

    if data.is_empty() {
        return "无".to_string();
    }

    let mut lines = Vec::new();
    let token_insts = data.get(0).cloned().unwrap_or_default();
    let char_insts = data.get(1).cloned().unwrap_or_default();
    let token_cards = data.get(2).cloned().unwrap_or_default();
    let char_cards = data.get(3).cloned().unwrap_or_default();

    if !token_insts.is_empty() {
        lines.push("装置实例：".to_string());
        for inst in &token_insts {
            lines.push(format_token_inst(inst, key_cache));
        }
    }
    if !char_insts.is_empty() {
        lines.push("干员实例：".to_string());
        for inst in &char_insts {
            lines.push(format_char_inst(inst, key_cache));
        }
    }
    if !token_cards.is_empty() {
        lines.push("装置卡片：".to_string());
        for card in &token_cards {
            lines.push(format_token_card(card, key_cache));
        }
    }
    if !char_cards.is_empty() {
        lines.push("干员卡片：".to_string());
        for card in &char_cards {
            lines.push(format_char_card(card, key_cache));
        }
    }

    if lines.is_empty() {
        "无".to_string()
    } else {
        lines.join("\n")
    }
}

fn format_token_inst(inst: &[serde_json::Value], cache: &HashMap<String, String>) -> String {
    let alias = inst_str(inst, 0);
    let key = inst_str(inst, 1).unwrap_or("");
    let level = inst_i64(inst, 2);
    let phase = phase_name(inst_i64(inst, 3));
    let skill_idx = inst_i64(inst, 4);
    let skill_lvl = inst_i64(inst, 5);
    let equip_ids = inst_equip_ids(inst, 6);
    let (row, col) = inst_pos(inst, 7);
    let dir = direction_name(inst_i64(inst, 8));

    let name = translate_key(key, cache);
    let mut out = format!("  {name}");
    if let Some(a) = alias {
        if !a.is_empty() {
            out.push_str(&format!("\n    别名: {a}"));
        }
    }
    // trap 类装置无精英化阶段与技能概念，省略阶段/技能；非 trap（干员/召唤物）全字段
    let is_trap = key.starts_with("trap_");
    if is_trap {
        out.push_str(&format!("\n    等级: {level}"));
    } else {
        out.push_str(&format!(
            "\n    等级: {level}  阶段: {phase}  技能: #{skill_idx} Lv.{skill_lvl}"
        ));
    }
    if !equip_ids.is_empty() {
        out.push_str(&format!("\n    模组: {}", equip_ids.join(", ")));
    }
    out.push_str(&format!("\n    位置: ({col}, {row})  朝向: {dir}"));
    out
}

fn format_char_inst(inst: &[serde_json::Value], cache: &HashMap<String, String>) -> String {
    // 结构同 tokenInst
    format_token_inst(inst, cache)
}

fn format_token_card(card: &[serde_json::Value], cache: &HashMap<String, String>) -> String {
    let alias = inst_str(card, 0);
    let key = inst_str(card, 1).unwrap_or("");
    let level = inst_i64(card, 2);
    let phase = phase_name(inst_i64(card, 3));
    let skill_idx = inst_i64(card, 4);
    let skill_lvl = inst_i64(card, 5);
    let equip_ids = inst_equip_ids(card, 6);
    let initial_cnt = inst_i64(card, 7);

    let name = translate_key(key, cache);
    let mut out = format!("  {name}");
    if let Some(a) = alias {
        if !a.is_empty() {
            out.push_str(&format!("\n    别名: {a}"));
        }
    }
    // trap 类装置无精英化阶段与技能概念，省略阶段/技能；非 trap（干员/召唤物）全字段
    let is_trap = key.starts_with("trap_");
    if is_trap {
        out.push_str(&format!("\n    等级: {level}"));
    } else {
        out.push_str(&format!(
            "\n    等级: {level}  阶段: {phase}  技能: #{skill_idx} Lv.{skill_lvl}"
        ));
    }
    if !equip_ids.is_empty() {
        out.push_str(&format!("\n    模组: {}", equip_ids.join(", ")));
    }
    out.push_str(&format!("\n    初始数量: {initial_cnt}"));
    out
}

fn format_char_card(card: &[serde_json::Value], cache: &HashMap<String, String>) -> String {
    let alias = inst_str(card, 0);
    let key = inst_str(card, 1).unwrap_or("");
    let level = inst_i64(card, 2);
    let phase = phase_name(inst_i64(card, 3));
    let skill_idx = inst_i64(card, 4);
    let skill_lvl = inst_i64(card, 5);
    let equip_ids = inst_equip_ids(card, 6);

    let name = translate_key(key, cache);
    let mut out = format!("  {name}");
    if let Some(a) = alias {
        if !a.is_empty() {
            out.push_str(&format!("\n    别名: {a}"));
        }
    }
    out.push_str(&format!(
        "\n    等级: {level}  阶段: {phase}  技能: #{skill_idx} Lv.{skill_lvl}"
    ));
    if !equip_ids.is_empty() {
        out.push_str(&format!("\n    模组: {}", equip_ids.join(", ")));
    }
    out
}

// ── 关卡符文 ──

fn format_runes(raw: &str) -> Result<String, ToolError> {
    let data: HashMap<String, serde_json::Value> =
        serde_json::from_str(raw).map_err(|e| ToolError::Execution(format!("解析符文: {e}")))?;

    let mut lines = Vec::new();
    for (group_key, runes_val) in &data {
        // 可选符文：level1/level2 → Ⅰ组/Ⅱ组
        if group_key == "可选" {
            if let Some(opt_groups) = runes_val.as_object() {
                // 收集 level1_* 和 level2_* 条目
                let mut level1_runes: Vec<&serde_json::Value> = Vec::new();
                let mut level2_runes: Vec<&serde_json::Value> = Vec::new();
                for (sub_key, sub_val) in opt_groups {
                    if let Some(arr) = sub_val.as_array() {
                        if sub_key.starts_with("level1") {
                            level1_runes.extend(arr);
                        } else if sub_key.starts_with("level2") {
                            level2_runes.extend(arr);
                        }
                    }
                }
                if !level1_runes.is_empty() {
                    lines.push("  Ⅰ组".to_string());
                    for rune in &level1_runes {
                        format_one_rune(rune, &mut lines);
                    }
                }
                if !level2_runes.is_empty() {
                    lines.push("  Ⅱ组".to_string());
                    for rune in &level2_runes {
                        format_one_rune(rune, &mut lines);
                    }
                }
            }
        } else {
            lines.push(format!("  [{group_key}]"));
            if let Some(runes) = runes_val.as_array() {
                for rune in runes {
                    format_one_rune(rune, &mut lines);
                }
            }
        }
    }

    Ok(if lines.is_empty() {
        "无".to_string()
    } else {
        lines.join("\n")
    })
}

fn format_one_rune(rune: &serde_json::Value, lines: &mut Vec<String>) {
    let entry: RuneEntry = serde_json::from_value(rune.clone()).unwrap_or_else(|_| RuneEntry {
        key: "?".into(),
        professionMask: serde_json::Value::Null,
        buildableMask: serde_json::Value::Null,
        blackboard: vec![],
    });
    let prof = format_prof_mask(&entry.professionMask);
    let bm = format_buildable_mask(&entry.buildableMask);
    let bb = format_blackboard(&entry.blackboard);
    lines.push(format!(
        "    {}  职业: {prof}  部署: {bm}  {{{bb}}}",
        entry.key
    ));
}

fn format_prof_mask(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => {
            let map: &[(&str, &str)] = &[
                ("CASTER", "术士"),
                ("MEDIC", "医疗"),
                ("PIONEER", "先锋"),
                ("SNIPER", "狙击"),
                ("SPECIAL", "特种"),
                ("SUPPORT", "辅助"),
                ("TANK", "重装"),
                ("TRAP", "装置"),
                ("WARRIOR", "近卫"),
                ("NONE", "无"),
            ];
            for (k, v) in map {
                if *k == s.as_str() {
                    return v.to_string();
                }
            }
            s.clone()
        }
        serde_json::Value::Number(n) => {
            let v = n.as_i64().unwrap_or(0);
            if v == 1023 {
                return "全部职业".to_string();
            }
            let bits: &[(i64, &str)] = &[
                (1, "近卫"),
                (2, "狙击"),
                (4, "重装"),
                (8, "医疗"),
                (16, "辅助"),
                (32, "术士"),
                (64, "特种"),
                (128, "召唤物"),
                (256, "装置"),
                (512, "先锋"),
            ];
            let parts: Vec<&str> = bits
                .iter()
                .filter(|(b, _)| (v & b) != 0)
                .map(|(_, name)| *name)
                .collect();
            if parts.is_empty() {
                v.to_string()
            } else {
                parts.join("、")
            }
        }
        _ => "无".to_string(),
    }
}

fn format_buildable_mask(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => {
            let map: &[(&str, &str)] = &[
                ("ALL", "全部单位"),
                ("MELEE", "近战单位"),
                ("RANGED", "远程单位"),
                ("NONE", "无"),
            ];
            for (k, v) in map {
                if *k == s.as_str() {
                    return v.to_string();
                }
            }
            s.clone()
        }
        serde_json::Value::Number(n) => {
            let v = n.as_i64().unwrap_or(0);
            match v {
                1 => "近战干员".into(),
                2 => "远程干员".into(),
                3 => "全部单位".into(),
                _ => v.to_string(),
            }
        }
        _ => "无".to_string(),
    }
}

fn format_blackboard(entries: &[BlackboardItem]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = entries
        .iter()
        .map(|e| {
            if let Some(vs) = &e.valueStr {
                if (e.value - 0.0).abs() < f64::EPSILON && !vs.is_empty() {
                    return format!("{}={}", e.key, vs);
                }
            }
            format!("{}={}", e.key, fmt_float(e.value))
        })
        .collect();
    parts.join(", ")
}

// ═══════════════════════════════════════════════════════
// 二、路线信息 (spec 6.7.3)
// ═══════════════════════════════════════════════════════

fn format_routes_section(row: &HashMap<String, String>) -> Result<String, ToolError> {
    let routes: Vec<serde_json::Value> = parse_json_field(row, "路线")?;
    let extra_str = row
        .get("额外路线")
        .filter(|s| !s.is_empty() && *s != "null");
    let extra: Vec<serde_json::Value> = if let Some(s) = extra_str {
        serde_json::from_str(s).unwrap_or_default()
    } else {
        vec![]
    };

    let mut lines = Vec::new();
    lines.push("══════ 路线信息 ══════".to_string());
    lines.push(String::new());

    // 路线列表
    lines.push(format!("路线列表（共 {} 条）", routes.len()));
    lines.push(String::new());
    for (i, r) in routes.iter().enumerate() {
        lines.push(format_one_route(i, r));
    }
    lines.push(String::new());

    // 额外路线列表（编号从 0 起，与分支波次的 routeIndex 相对索引一致）
    if !extra.is_empty() {
        lines.push(format!("额外路线列表（共 {} 条）", extra.len()));
        lines.push(String::new());
        for (i, r) in extra.iter().enumerate() {
            lines.push(format_one_route(i, r));
        }
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

fn format_one_route(idx: usize, route: &serde_json::Value) -> String {
    // null 路线
    if route.is_null() {
        return format!("{idx}(null)");
    }

    let arr = match route.as_array() {
        Some(a) => a,
        None => return format!("{idx}(?)"),
    };

    let motion_mode = arr.get(0).and_then(|v| v.as_i64()).unwrap_or(0);

    // E_NUM
    if motion_mode == 2 {
        return format!("{idx}(E_NUM)");
    }

    let mode_str = motion_mode_name(motion_mode);

    // 起点
    let (start_row, start_col) = pos_from_json(arr.get(1));
    let (end_row, end_col) = pos_from_json(arr.get(2));
    let spawn_random = offset_from_json(arr.get(3));
    let spawn_offset = offset_from_json(arr.get(4));
    let checkpoints: &[serde_json::Value] = arr
        .get(5)
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    let mut parts = Vec::new();

    // 起点
    let mut start_extras = Vec::new();
    if !is_zero_offset(&spawn_offset) {
        start_extras.push(format!(
            "spawnOffset=({},{})",
            fmt_float(spawn_offset.0),
            fmt_float(spawn_offset.1)
        ));
    }
    if !is_zero_offset(&spawn_random) {
        start_extras.push(format!(
            "spawnRandomRange=({},{})",
            fmt_float(spawn_random.0),
            fmt_float(spawn_random.1)
        ));
    }
    if start_extras.is_empty() {
        parts.push(format!("[({start_col},{start_row})]"));
    } else {
        parts.push(format!(
            "[({start_col},{start_row}),{}]",
            start_extras.join(",")
        ));
    }

    // 中间路径点
    for cp in checkpoints {
        let cp_arr = match cp.as_array() {
            Some(a) => a,
            None => continue,
        };
        let cp_type = cp_arr.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
        let cp_time = cp_arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let (cp_row, cp_col) = pos_from_json(cp_arr.get(2));
        let cp_offset = offset_from_json(cp_arr.get(3));

        let is_default =
            cp_type == 0 && (cp_time - 0.0).abs() < f64::EPSILON && is_zero_offset(&cp_offset);

        if is_default {
            parts.push(format!("({cp_col},{cp_row})"));
        } else {
            // 坐标无意义的事件类型（非移动、非传送落点）：WAIT 类 / DISAPPEAR / ALERT
            // 此类只输出类型名/时间/偏移，不输出坐标，避免填充值误导
            let omit_coord = matches!(cp_type, 1 | 2 | 3 | 4 | 5 | 7 | 9);
            let mut extras = Vec::new();
            if cp_type != 0 {
                extras.push(checkpoint_type_name(cp_type));
            }
            if (cp_time - 0.0).abs() > f64::EPSILON {
                extras.push(format!("{}s", fmt_float(cp_time)));
            }
            if !is_zero_offset(&cp_offset) {
                extras.push(format!(
                    "offset=({},{})",
                    fmt_float(cp_offset.0),
                    fmt_float(cp_offset.1)
                ));
            }
            let cp_str = if omit_coord {
                // 纯语义形式，如 [停驻·计时,20.0s]
                format!("[{}]", extras.join(","))
            } else if extras.is_empty() {
                format!("[({cp_col},{cp_row})]")
            } else {
                format!("[({cp_col},{cp_row}),{}]", extras.join(","))
            };
            parts.push(cp_str);
        }
    }

    // 终点
    parts.push(format!("[({end_col},{end_row})]"));

    let mut line = format!("{idx}({mode_str}):{}", parts.join("→"));
    // visitEveryCheckPoint：第 7 位仅在 true 时存在（数组长度 ≥ 7）
    if arr.get(6).and_then(|v| v.as_bool()).unwrap_or(false) {
        line.push_str(" [visitEveryCheckPoint]");
    }
    line
}

fn pos_from_json(v: Option<&serde_json::Value>) -> (i64, i64) {
    v.and_then(|x| x.as_array())
        .map(|a| {
            let row = a.first().and_then(|v| v.as_i64()).unwrap_or(0);
            let col = a.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
            (row, col)
        })
        .unwrap_or((0, 0))
}

fn offset_from_json(v: Option<&serde_json::Value>) -> (f64, f64) {
    v.and_then(|x| x.as_array())
        .map(|a| {
            let x = a.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = a.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
            (x, y)
        })
        .unwrap_or((0.0, 0.0))
}

fn is_zero_offset(off: &(f64, f64)) -> bool {
    (off.0 - 0.0).abs() < f64::EPSILON && (off.1 - 0.0).abs() < f64::EPSILON
}

fn checkpoint_type_name(t: i64) -> String {
    let map: &[(i64, &str)] = &[
        (0, "移动"),
        (1, "停驻·计时"),
        (2, "停驻·全局计时"),
        (3, "停驻·段计时"),
        (4, "停驻·波计时"),
        (5, "进入传送"),
        (6, "离开传送"),
        (7, "警报"),
        (8, "巡逻"),
        (9, "引航者试炼休整"),
        (10, "地图偏移移动"),
    ];
    for (k, v) in map {
        if *k == t {
            return v.to_string();
        }
    }
    format!("type{t}")
}

// ═══════════════════════════════════════════════════════
// 三、波次信息 (spec 6.7.4)
// ═══════════════════════════════════════════════════════

fn format_waves_section(
    row: &HashMap<String, String>,
    key_cache: Option<&HashMap<String, String>>,
) -> Result<String, ToolError> {
    let key_cache = key_cache.ok_or_else(|| ToolError::Execution("缓存未初始化".into()))?;
    let waves: Vec<WaveData> = parse_json_field(row, "波次")?;

    let mut lines = Vec::new();
    lines.push("══════ 波次信息 ══════".to_string());
    lines.push(String::new());

    for (wi, wave) in waves.iter().enumerate() {
        let mut header = format!("波 {wi}");
        let mut post_info = Vec::new();
        if (wave.preDelay - 0.0).abs() > f64::EPSILON {
            post_info.push(format!("[preDelay: {}s]", fmt_float(wave.preDelay)));
        }
        if (wave.postDelay - 0.0).abs() > f64::EPSILON {
            post_info.push(format!("[postDelay: {}s]", fmt_float(wave.postDelay)));
        }
        if !post_info.is_empty() {
            header.push_str(&format!("  {}", post_info.join("  ")));
        }
        lines.push(header);
        lines.push(String::new());

        for (fi, frag) in wave.fragments.iter().enumerate() {
            let mut fh = format!("  段 {fi}");
            if (frag.preDelay - 0.0).abs() > f64::EPSILON {
                fh.push_str(&format!("  [preDelay: {}s]", fmt_float(frag.preDelay)));
            }
            lines.push(fh);

            format_fragment_actions(&frag.actions, key_cache, &mut lines);
        }
        lines.push(String::new());
    }

    // 分支信息
    let branches_str = row
        .get("分支")
        .filter(|s| *s != "null" && !s.trim().is_empty());
    if let Some(raw) = branches_str {
        let branches: HashMap<String, BranchValue> = serde_json::from_str(raw)
            .map_err(|e| ToolError::Execution(format!("解析分支数据: {e}")))?;
        if !branches.is_empty() {
            lines.push("══════ 分支信息 ══════".to_string());
            lines.push(String::new());
            for (branch_key, branch_val) in &branches {
                lines.push(branch_key.clone());
                for (fi, phase) in branch_val.phases.iter().enumerate() {
                    let mut fh = format!("  段 {fi}");
                    if (phase.preDelay - 0.0).abs() > f64::EPSILON {
                        fh.push_str(&format!("  [preDelay: {}s]", fmt_float(phase.preDelay)));
                    }
                    lines.push(fh);
                    format_fragment_actions(&phase.actions, key_cache, &mut lines);
                }
                lines.push(String::new());
            }
        }
    }

    Ok(lines.join("\n"))
}

fn format_fragment_actions(
    actions: &[serde_json::Value],
    key_cache: &HashMap<String, String>,
    lines: &mut Vec<String>,
) {
    if actions.is_empty() {
        return;
    }

    // ── 收集随机组 ──
    // group_key → Vec<action_index>
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (ai, act_val) in actions.iter().enumerate() {
        let arr = match act_val.as_array() {
            Some(a) => a,
            None => continue,
        };
        if let Some(grp) = act_str(arr, 6) {
            if !grp.is_empty() {
                groups.entry(grp.to_string()).or_default().push(ai);
            }
        }
    }

    // 对有 packKey 但无 groupKey 的 action，拉入同 packKey 的组
    for (ai, act_val) in actions.iter().enumerate() {
        let arr = match act_val.as_array() {
            Some(a) => a,
            None => continue,
        };
        let pck = act_str(arr, 7);
        let grp = act_str(arr, 6);
        if let Some(pck) = pck {
            if !pck.is_empty() && grp.map_or(true, |g| g.is_empty()) {
                // 找同 packKey 且有 groupKey 的 action
                for (aj, act_val2) in actions.iter().enumerate() {
                    if ai == aj {
                        continue;
                    }
                    let arr2 = match act_val2.as_array() {
                        Some(a) => a,
                        None => continue,
                    };
                    if act_str(arr2, 7) == Some(pck) {
                        if let Some(g2) = act_str(arr2, 6) {
                            if !g2.is_empty() {
                                groups.entry(g2.to_string()).or_default().push(ai);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    // 收集已被组覆盖的 action 索引
    let mut grouped_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for indices in groups.values() {
        for &ai in indices {
            grouped_indices.insert(ai);
        }
    }

    // 输出：逐个 action，grouped 的在 random group 中输出
    let mut outputted_groups: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (ai, act_val) in actions.iter().enumerate() {
        let arr = match act_val.as_array() {
            Some(a) => a,
            None => continue,
        };
        let grp = act_str(arr, 6).filter(|s| !s.is_empty());

        if let Some(g) = grp {
            if !outputted_groups.contains(g) {
                format_random_group(g, actions, &groups, key_cache, lines);
                outputted_groups.insert(g.to_string());
            }
            continue;
        }

        if grouped_indices.contains(&ai) {
            continue; // 被拉入组的，已在 random group 内输出
        }

        // 固定刷出
        format_action_line(arr, key_cache, lines, 4);
    }

    // 确保所有 group 都输出（处理边界情况）
    for g in groups.keys() {
        if !outputted_groups.contains(g) {
            format_random_group(g, actions, &groups, key_cache, lines);
            outputted_groups.insert(g.clone());
        }
    }
}

fn format_random_group(
    group_key: &str,
    actions: &[serde_json::Value],
    groups: &HashMap<String, Vec<usize>>,
    key_cache: &HashMap<String, String>,
    lines: &mut Vec<String>,
) {
    let indices = match groups.get(group_key) {
        Some(v) => v.clone(),
        None => return,
    };

    // 按 packKey 分组
    let mut pack_dict: HashMap<Option<String>, Vec<&[serde_json::Value]>> = HashMap::new();
    for &ai in &indices {
        let arr = match actions[ai].as_array() {
            Some(a) => a,
            None => continue,
        };
        let pck = act_str(arr, 7)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        pack_dict.entry(pck).or_default().push(arr);
    }

    let distinct_keys: Vec<&Option<String>> = pack_dict.keys().collect();
    let has_multiple_packs = distinct_keys.iter().filter(|k| k.is_some()).count() > 1;

    lines.push(format!("    随机组 {group_key}"));

    if has_multiple_packs {
        // 每个 packKey 是一个 option（包）
        for pck_opt in distinct_keys {
            if pck_opt.is_none() {
                continue;
            }
            let pck = pck_opt.as_ref().unwrap();
            let pack_acts = &pack_dict[pck_opt];
            // 找该包内带 groupKey 的 action 的 weight
            let mut wt = 0;
            for act in pack_acts {
                if act_str(*act, 6) == Some(group_key) {
                    let w = act_i64(*act, 8);
                    if w > 0 {
                        wt = w;
                        break;
                    }
                }
            }
            lines.push(format!("      权重 {wt}  包 {pck}"));
            for act in pack_acts {
                format_action_line(act, key_cache, lines, 8);
            }
        }
        // 处理 null packKey
        if let Some(null_acts) = pack_dict.get(&None) {
            for act in null_acts {
                if act_str(*act, 6) == Some(group_key) {
                    let wt = act_i64(*act, 8);
                    if wt > 0 {
                        lines.push(format!("      权重 {wt}"));
                        format_action_line(act, key_cache, lines, 8);
                        // 跟随的 packKey=null 无 groupKey 的 action
                        for act2 in null_acts {
                            if act2.len() > 7 && act_str(*act2, 6).map_or(true, |s| s.is_empty()) {
                                format_action_line(act2, key_cache, lines, 8);
                            }
                        }
                    }
                }
            }
        }
    } else {
        // 所有 action 的 packKey 相同（全部 None 或全同一值）
        let all_acts: Vec<&&[serde_json::Value]> = pack_dict.values().flatten().collect();
        for act in all_acts {
            if act_str(*act, 6) != Some(group_key) {
                continue;
            }
            let wt = act_i64(*act, 8);
            if wt > 0 {
                lines.push(format!("      权重 {wt}"));
                format_action_line(act, key_cache, lines, 8);
                // 跟随的同 packKey 无 groupKey 的 action
                let pck = act_str(*act, 7);
                for act2 in &indices {
                    let arr2 = match actions[*act2].as_array() {
                        Some(a) => a,
                        None => continue,
                    };
                    if act_str(arr2, 6).map_or(true, |s| s.is_empty()) && act_str(arr2, 7) == pck {
                        format_action_line(arr2, key_cache, lines, 8);
                    }
                }
            }
        }
    }
}

fn format_action_line(
    act: &[serde_json::Value],
    key_cache: &HashMap<String, String>,
    lines: &mut Vec<String>,
    indent: usize,
) {
    let prefix = " ".repeat(indent);
    let action_type = act_i64(act, 0);
    let key = act_str(act, 1).unwrap_or("");
    let count = act_i64(act, 2);
    let pre_delay = act_f64(act, 3);
    let interval = act_f64(act, 4);
    let route_idx = act_i64(act, 5);
    let hidden_group = act_str(act, 9);

    let type_name = action_type_name(action_type);

    // ── 特殊类型：仅 [hiddenGroup] [preDelay] 类型名 ──
    if is_special_action_type(action_type) {
        let mut parts = Vec::new();
        if let Some(hg) = hidden_group {
            if !hg.is_empty() {
                parts.push(format!("[hiddenGroup: {hg}]"));
            }
        }
        parts.push(format!("[preDelay: {}s]", fmt_float(pre_delay)));
        parts.push(type_name);
        lines.push(format!("{prefix}{}", parts.join("  ")));
        return;
    }

    // ── 未知类型（3/12 等）：按特殊处理 ──
    if !is_default_action_type(action_type) {
        let mut parts = Vec::new();
        if let Some(hg) = hidden_group {
            if !hg.is_empty() {
                parts.push(format!("[hiddenGroup: {hg}]"));
            }
        }
        parts.push(format!("[preDelay: {}s]", fmt_float(pre_delay)));
        parts.push(type_name);
        lines.push(format!("{prefix}{}", parts.join("  ")));
        return;
    }

    // ── 默认类型：全字段展开 ──
    let mut parts = Vec::new();

    // 1. hiddenGroup
    if let Some(hg) = hidden_group {
        if !hg.is_empty() {
            parts.push(format!("[hiddenGroup: {hg}]"));
        }
    }

    // 2. preDelay（强制标注）
    parts.push(format!("[preDelay: {}s]", fmt_float(pre_delay)));

    // 3. 类型名
    parts.push(type_name);

    // 4. 目标 → 翻译。key 为空时输出（无）
    if key.is_empty() {
        parts.push("（无）".to_string());
    } else {
        let translated = translate_key(key, key_cache);
        parts.push(translated);

        // 5. ×N
        if count > 1 || action_type != 1 {
            parts.push(format!("×{count}"));
        }
    }

    // 6. [间隔]
    if count > 1 && (interval - 0.0).abs() > f64::EPSILON {
        parts.push(format!("[间隔: {}s]", fmt_float(interval)));
    }

    // 7. 路线N
    parts.push(format!("路线{route_idx}"));

    lines.push(format!("{prefix}{}", parts.join("  ")));
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn db_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arknights.db")
    }

    fn make_ctx() -> ToolContext {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        ToolContext::with_db(Arc::new(db))
    }

    // ── 地图信息 ──

    #[test]
    fn query_map_basic_1_7() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(&ctx, r#"{"levelId":"level_main_01-07","fields":["地图"]}"#)
            .unwrap()
            .summary;

        // 地图：尺寸 + 坐标分组 + 属性表
        assert!(
            result.contains("【地图】"),
            "应包含地图尺寸\n结果: {result}"
        );
        assert!(
            result.contains("【地块坐标】"),
            "应包含地块坐标\n结果: {result}"
        );
        assert!(
            result.contains("【地块属性表】"),
            "应包含地块属性表\n结果: {result}"
        );

        // 坐标显式输出（非网格），区间压缩用 ~ 连接
        assert!(result.contains("(0,0)"), "坐标应显式输出\n结果: {result}");
        assert!(result.contains("~"), "应包含区间压缩符~\n结果: {result}");

        // 地图独立于预置单位与符文（拆分后不再夹带）
        assert!(
            !result.contains("预置单位"),
            "地图不应含预置单位\n结果: {result}"
        );
        assert!(
            !result.contains("关卡符文"),
            "地图不应含关卡符文\n结果: {result}"
        );
    }

    #[test]
    fn query_predefines_null_outputs_none() {
        // level_act6d5_04 预置单位为 NULL（DB Null→""），应输出"无"而非解析报错
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(
                &ctx,
                r#"{"levelId":"level_act6d5_04","fields":["预置单位"]}"#,
            )
            .unwrap()
            .summary;

        assert!(result.contains("预置单位"), "应包含预置单位标题");
        assert!(
            result.contains("无"),
            "NULL 预置单位应输出无，不应报解析错误\n结果: {result}"
        );
        assert!(
            !result.contains("解析预置单位失败"),
            "不应出现解析错误\n结果: {result}"
        );
    }

    #[test]
    fn query_predefines_1_7() {
        // 1-7 预置单位含障碍物等预设装置
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(
                &ctx,
                r#"{"levelId":"level_main_01-07","fields":["预置单位"]}"#,
            )
            .unwrap()
            .summary;

        assert!(result.contains("预置单位"), "应包含预置单位标题");
        assert!(
            !result.contains("【地块坐标】"),
            "预置单位不应含地图内容\n结果: {result}"
        );
    }

    #[test]
    fn coord_group_compresses_runs() {
        // 行内连续区间压缩、单格保留、跨行不合并；组内按 (y, x) 排序
        let map = MapData {
            width: 6,
            height: 3,
            grid: vec![
                vec![0, 0, 0, 0, 1, 1], // y=2（顶部行）
                vec![0, 1, 0, 0, 1, 1], // y=1
                vec![0, 0, 0, 0, 0, 0], // y=0（底部行）
            ],
            tiles: vec![
                TileEntry("tile_road".into(), 0, 1, 3),
                TileEntry("tile_hole".into(), 0, 0, 3),
            ],
        };
        let groups = group_tile_coords(&map);
        assert_eq!(groups.len(), 2);
        let road = groups.iter().find(|g| g.name == "平地").unwrap();
        assert_eq!(
            road.coords,
            vec![
                (0, 0),
                (1, 0),
                (2, 0),
                (3, 0),
                (4, 0),
                (5, 0),
                (0, 1),
                (2, 1),
                (3, 1),
                (0, 2),
                (1, 2),
                (2, 2),
                (3, 2),
            ]
        );
        assert_eq!(
            fmt_coord_group(&road.coords),
            "(0,0)~(5,0), (0,1), (2,1)~(3,1), (0,2)~(3,2)"
        );
        // 地穴：y2 x4-5, y1 x1 + x4-5（跨行不合并）
        let hole = groups.iter().find(|g| g.name == "地穴").unwrap();
        assert_eq!(
            fmt_coord_group(&hole.coords),
            "(1,1), (4,1)~(5,1), (4,2)~(5,2)"
        );
    }

    #[test]
    fn same_name_groups_get_attr_anchor() {
        // 同名不同属性（化境地块高台/低地两形态）：坐标行附加属性消歧锚，
        // 属性表保持同名不带锚（属性段即匹配键）
        let map = MapData {
            width: 2,
            height: 1,
            grid: vec![vec![0, 1]],
            tiles: vec![
                TileEntry("tile_dygmny_1".into(), 1, 2, 3), // 化境地块 高台
                TileEntry("tile_dygmny_3".into(), 0, 1, 3), // 化境地块 低地
            ],
        };
        let out = format_map(&map);
        assert!(
            out.contains("化境地块(高台·全通行，远程部署): (0,0)"),
            "同名坐标行应附属性锚\n{out}"
        );
        assert!(
            out.contains("化境地块(低地·全通行，近战部署): (1,0)"),
            "同名坐标行应附属性锚\n{out}"
        );
        assert!(
            out.contains("化境地块: 高台·全通行，远程部署，会有更高的概率"),
            "属性表保持原格式\n{out}"
        );
        assert!(
            out.contains("化境地块: 低地·全通行，近战部署，会有更高的概率"),
            "属性表保持原格式\n{out}"
        );
    }

    #[test]
    fn attrs_table_whitelist_and_desc() {
        // 白名单地块不附描述文本；非白名单附 tile_info.json 描述
        let map = MapData {
            width: 3,
            height: 1,
            grid: vec![vec![0, 1, 2]],
            tiles: vec![
                TileEntry("tile_road".into(), 0, 1, 3),  // 平地（白名单）
                TileEntry("tile_hole".into(), 0, 0, 3),  // 地穴（非白名单）
                TileEntry("tile_floor".into(), 0, 0, 3), // 不可放置位（白名单）
            ],
        };
        let out = format_map(&map);
        assert!(out.contains("【地块坐标】"), "应包含坐标分组\n{out}");
        assert!(out.contains("【地块属性表】"), "应包含属性表\n{out}");
        assert!(
            out.contains("平地: 低地·全通行，近战部署"),
            "白名单不附描述\n{out}"
        );
        assert!(
            out.contains("不可放置位: 低地·全通行，不可部署"),
            "白名单不附描述\n{out}"
        );
        assert!(
            out.contains("地穴: 低地·全通行，不可部署，危险的凹陷地形"),
            "非白名单附描述\n{out}"
        );
    }

    // ── 路线信息 ──

    #[test]
    fn query_routes_1_7() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(&ctx, r#"{"levelId":"level_main_01-07","fields":["路线"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("路线列表"), "应包含路线列表");
        assert!(result.contains("WALK"), "应包含 WALK 模式");
        assert!(result.contains("E_NUM"), "1-7 路线0 应为 E_NUM");
        assert!(result.contains("→"), "应包含箭头链");
    }

    #[test]
    fn query_routes_visit_every_checkpoint() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        // level_act12mini_04 routes[2] 为 visitEveryCheckPoint=true
        let result = tool
            .execute(
                &ctx,
                r#"{"levelId":"level_act12mini_04","fields":["路线"]}"#,
            )
            .unwrap()
            .summary;

        assert!(result.contains("路线列表"), "应包含路线列表");
        assert!(
            result.contains("[visitEveryCheckPoint]"),
            "应标注 visitEveryCheckPoint\n结果: {result}"
        );
    }

    #[test]
    fn query_routes_without_visit_flag() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        // level_main_01-07 无 visitEveryCheckPoint=true 路线
        let result = tool
            .execute(&ctx, r#"{"levelId":"level_main_01-07","fields":["路线"]}"#)
            .unwrap()
            .summary;

        assert!(
            !result.contains("visitEveryCheckPoint"),
            "不应出现 visit 标注\n结果: {result}"
        );
    }

    // ── 波次信息 ──

    #[test]
    fn query_waves_1_7() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(&ctx, r#"{"levelId":"level_main_01-07","fields":["波次"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("波次信息"), "应包含波次标题");
        assert!(result.contains("波 0"), "应包含波次编号");
        assert!(result.contains("段"), "应包含段");
        assert!(result.contains("生成"), "SPAWN 应为生成");
        assert!(result.contains("源石虫"), "应翻译敌人 key\n结果: {result}");
        assert!(result.contains("路线"), "应包含 routeIndex");
    }

    // ── 全输出 ──

    #[test]
    fn query_all_1_7() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(
                &ctx,
                r#"{"levelId":"level_main_01-07","fields":["地图","预置单位","关卡符文","路线","波次"]}"#,
            )
            .unwrap().summary;

        assert!(result.contains("【地图】"));
        assert!(result.contains("预置单位"));
        assert!(result.contains("关卡符文"));
        assert!(result.contains("路线信息"));
        assert!(result.contains("波次信息"));
    }

    #[test]
    fn empty_fields_returns_error() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool.execute(&ctx, r#"{"levelId":"level_main_01-07","fields":[]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不能为空"));
    }

    // ── 错误处理 ──

    #[test]
    fn nonexistent_level() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool.execute(&ctx, r#"{"levelId":"not_a_real_level","fields":["地图"]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("未找到"));
    }

    #[test]
    fn missing_levelid() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool.execute(&ctx, r#"{}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("levelId"));
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new();
        let tool = QueryLevel;
        let result = tool.execute(&ctx, r#"{"levelId":"level_main_01-07","fields":["地图"]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("数据库未连接"));
    }

    // ── levelId 不同写法 ──

    #[test]
    fn query_level_with_random_groups() {
        // 1-7 没有随机组，用有随机组的关卡测试
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(
                &ctx,
                r#"{"levelId":"level_act1arcade_m1_01","fields":["波次"]}"#,
            )
            .unwrap()
            .summary;

        assert!(result.contains("随机组"), "应包含随机组\n结果: {result}");
    }

    // ── 分支信息 ──

    #[test]
    fn query_level_with_branches() {
        let ctx = make_ctx();
        let tool = QueryLevel;
        let result = tool
            .execute(
                &ctx,
                r#"{"levelId":"level_act12mini_06","fields":["波次"]}"#,
            )
            .unwrap()
            .summary;

        assert!(result.contains("分支信息"), "应包含分支\n结果: {result}");
        assert!(result.contains("line_"), "应包含分支名\n结果: {result}");
    }

    // ── 手动测试入口 ──

    #[test]
    fn show_output() {
        fn call(ctx: &ToolContext, params: &str) -> String {
            let tool = QueryLevel;
            match tool.execute(ctx, params) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        }

        let ctx = make_ctx();

        // 环境变量驱动：
        //   PRTS_LEVEL_ID="level_main_01-07"
        //   PRTS_LEVEL_FIELDS="地图,路线,波次"（可选，逗号分隔）
        if let Ok(level_id) = std::env::var("PRTS_LEVEL_ID") {
            let mut params = serde_json::json!({ "levelId": level_id });
            if let Ok(fstr) = std::env::var("PRTS_LEVEL_FIELDS") {
                let fv: Vec<String> = if fstr.is_empty() {
                    vec![]
                } else {
                    fstr.split(',').map(|s| s.trim().to_string()).collect()
                };
                params["fields"] = serde_json::json!(fv);
            }
            println!("{}", call(&ctx, &params.to_string()));
            return;
        }

        for id in ["level_main_01-07", "level_rogue5_4-3"] {
            println!("========== {id} ==========");
            println!(
                "{}",
                call(&ctx, &serde_json::json!({ "levelId": id }).to_string())
            );
            println!();
        }
    }
}
