//! query_table —— 通用键值表查询工具
//!
//! 语义：输入 table（表名）+ keys（键列表）→ 返回每个键对应条目的内容。
//! 五张表共享"按键列表查询 + 去重 + 逐条格式化"的处理骨架，合并为单工具，
//! 避免工具数量膨胀增加 LLM 的选择成本；渲染逻辑按表独立，保留各表原有语义。
//!
//! 支持的表：
//! - range：攻击范围表（resources/ranges.json），键为 rangeID，如 "2-3"、"x-4"
//! - tile：地块信息表（resources/tile_info.json），键为 tileKey，如 "tile_hole"
//! - term：战斗术语表（resources/term.json），键为术语名，如 "可充能"、"阻挡"
//! - abnormal：异常效果表（resources/abnormal_effects.json），键为异常中文名，如 "晕眩"、"沉默"
//! - slang：黑话表（resources/slang.json），键为社区称呼/别名，如 "42"、"小羊"
//!
//! 五种数据源均为编译期嵌入，构造时一次解析，之后每次 execute 只查内存，
//! 无文件 I/O、无数据库依赖。

use crate::tools::display::array_summary;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

// ═══════════════════════════════════════════════════════
// 数据模型（对应五个 json 文件）
// ═══════════════════════════════════════════════════════

/// 攻击范围条目（对应 ranges.json）。
#[derive(Debug, Deserialize)]
struct RangeEntry {
    /// 格点相对坐标列表 [row, col]，(0,0) 为干员所在格
    grids: Vec<[i32; 2]>,
    /// 可读别名（人工补充），可为空字符串
    alias: String,
}

/// 地块条目（对应 tile_info.json）。
#[derive(Debug, Deserialize)]
struct TileEntry {
    /// 中文名（官方或人工补充），可为空字符串
    名称: String,
    /// 语义描述（官方或人工补充），可为空字符串
    描述: String,
    /// 是否为功能性地块（如伤害、传送、增益等），非功能地块为纯地形
    #[serde(default)]
    功能: bool,
    /// 人工补充的机制备注（wiki 细节，可选；重建时原样保留）
    #[serde(default)]
    备注: String,
}

/// 异常效果条目（对应 abnormal_effects.json）。
#[derive(Debug, Deserialize)]
struct AbnormalEntry {
    /// 现象描述（中文）
    效果: String,
    /// 所属统一检查分类（无法选择/阻止攻击/阻止能力/阻止移动/对友方隐藏/对非友方隐藏），可为空数组
    #[serde(default)]
    分类: Vec<String>,
    /// 该异常效果是否通常由可抵抗 Buff 携带
    可抵抗: bool,
}

/// 黑话条目（对应 slang.json）。
#[derive(Debug, Deserialize)]
struct SlangEntry {
    /// 指向的规范名称（与各表实体名一致）
    指向: String,
    /// 实体类型：干员 / 敌人 / 其他实体
    类型: String,
    /// 补充说明（可选，如多义说明）
    #[serde(default)]
    说明: String,
}

// ═══════════════════════════════════════════════════════
// 表名枚举
// ═══════════════════════════════════════════════════════

/// 支持的表名。
#[derive(Debug, Clone, Copy)]
enum Table {
    Range,
    Tile,
    Term,
    Abnormal,
    Slang,
}

impl Table {
    /// 解析表名字符串；未知表名报错。
    fn parse(s: &str) -> Result<Self, ToolError> {
        match s {
            "range" => Ok(Self::Range),
            "tile" => Ok(Self::Tile),
            "term" => Ok(Self::Term),
            "abnormal" => Ok(Self::Abnormal),
            "slang" => Ok(Self::Slang),
            other => Err(ToolError::Parse(format!(
                "table 必须是 range/tile/term/abnormal/slang 之一，收到: {other}"
            ))),
        }
    }

    /// 表的标识名（用于未命中提示）。
    fn label(self) -> &'static str {
        match self {
            Self::Range => "range",
            Self::Tile => "tile",
            Self::Term => "term",
            Self::Abnormal => "abnormal",
            Self::Slang => "slang",
        }
    }
}

// ═══════════════════════════════════════════════════════
// QueryTable
// ═══════════════════════════════════════════════════════

/// 通用键值表查询工具。
///
/// 构造时解析四份 json（编译期嵌入），之后每次 execute 只查内存，
/// 无文件 I/O、无数据库依赖。
pub struct QueryTable {
    ranges: HashMap<String, RangeEntry>,
    tiles: HashMap<String, TileEntry>,
    /// 中文名 → tileKey 列表（反向索引；重名中文名全部收录，查询时批量返回）。
    /// 地块图（query_level 视图一）已改输出中文名，LLM 拿中文名即可反查详情。
    tile_by_name: HashMap<String, Vec<String>>,
    terms: HashMap<String, String>,
    abnormals: HashMap<String, AbnormalEntry>,
    slangs: HashMap<String, SlangEntry>,
}

impl QueryTable {
    /// 从四份 json 的文本构建工具。
    ///
    /// 格式错误时 panic：四个 json 均为编译期嵌入的生成物/人工维护数据，
    /// 数据与代码同源，格式错误属于构建期问题而非运行期异常。
    pub fn new(
        ranges_json: &str,
        tiles_json: &str,
        terms_json: &str,
        abnormals_json: &str,
        slangs_json: &str,
    ) -> Self {
        let ranges = serde_json::from_str(ranges_json).expect(
            "ranges.json 格式错误（src-tauri/resources/ranges.json，由 data-source/_gen_ranges_json.py 生成）",
        );
        let tiles: HashMap<String, TileEntry> = serde_json::from_str(tiles_json).expect(
            "tile_info.json 格式错误（src-tauri/resources/tile_info.json，由 data-pipeline/maintenance/rebuild_tile_info.py 生成）",
        );
        let terms = serde_json::from_str(terms_json)
            .expect("term.json 格式错误（src-tauri/resources/term.json，人工维护）");
        let abnormals = serde_json::from_str(abnormals_json).expect(
            "abnormal_effects.json 格式错误（src-tauri/resources/abnormal_effects.json，人工维护）",
        );
        let slangs = serde_json::from_str(slangs_json).expect(
            "slang.json 格式错误（src-tauri/resources/slang.json，由 scripts/slang_scan.py 生成、人工维护）",
        );
        let mut tile_by_name: HashMap<String, Vec<String>> = HashMap::new();
        for (tk, entry) in tiles.iter() {
            if !entry.名称.is_empty() {
                tile_by_name
                    .entry(entry.名称.clone())
                    .or_default()
                    .push(tk.clone());
            }
        }
        Self {
            ranges,
            tiles,
            tile_by_name,
            terms,
            abnormals,
            slangs,
        }
    }

    // ── range 表 ──

    /// 渲染格点为字符画。
    ///
    /// 裁剪边界 = 格点 ∪ 干员格 (0,0)，保证干员标记恒在图中；
    /// 干员与范围之间的间隙用空位标记填充，距离信息由此表达。
    fn render_grid(grids: &[[i32; 2]]) -> Vec<String> {
        let mut cells: HashSet<(i32, i32)> = grids.iter().map(|g| (g[0], g[1])).collect();
        // cells 至少含 (0,0)（下方插入），以下四个 min/max 必然有值，不会 panic
        cells.insert((0, 0));

        let rmin = cells.iter().map(|c| c.0).min().unwrap();
        let rmax = cells.iter().map(|c| c.0).max().unwrap();
        let cmin = cells.iter().map(|c| c.1).min().unwrap();
        let cmax = cells.iter().map(|c| c.1).max().unwrap();

        let mut lines = Vec::new();
        for r in rmin..=rmax {
            let mut line = String::new();
            for c in cmin..=cmax {
                if (r, c) == (0, 0) {
                    line.push('Ｏ');
                } else if cells.contains(&(r, c)) {
                    line.push('▣');
                } else {
                    line.push('・');
                }
            }
            lines.push(line);
        }
        lines
    }

    /// 渲染单个 range 条目：标题（格数 + 干员格标注 + 别名）+ 图形或原始坐标。
    fn render_range(&self, rid: &str, entry: &RangeEntry, raw: bool, out: &mut String) {
        let has_origin = entry.grids.iter().any(|g| g[0] == 0 && g[1] == 0);
        let mut meta: Vec<String> = Vec::new();
        if !has_origin {
            meta.push("干员格不在范围内".into());
        }
        if !entry.alias.is_empty() {
            meta.push(format!("别名：{}", entry.alias));
        }
        let meta_part = if meta.is_empty() {
            String::new()
        } else {
            format!(" · {}", meta.join(" · "))
        };
        out.push_str(&format!(
            "━━━ {rid}（{} 格{meta_part}）━━━\n",
            entry.grids.len()
        ));

        if raw {
            let grids_str: Vec<String> = entry
                .grids
                .iter()
                .map(|g| format!("[{},{}]", g[0], g[1]))
                .collect();
            out.push_str(&format!("grids: [{}]\n", grids_str.join(",")));
        } else {
            for line in Self::render_grid(&entry.grids) {
                out.push_str(&line);
                out.push('\n');
            }
        }
    }

    fn query_ranges(&self, table: Table, keys: &[String], raw: bool) -> String {
        let mut output = String::new();
        for (i, rid) in keys.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }
            match self.ranges.get(rid) {
                Some(entry) => self.render_range(rid, entry, raw, &mut output),
                None => output.push_str(&format!("✗ {rid} 未收录于 {} 表\n", table.label())),
            }
        }
        output.trim_end().to_string()
    }

    // ── tile 表 ──

    /// 渲染单个 tile 条目：标题（名称 + 功能标记）+ 名称/描述行 + 备注行（如有）。
    ///
    /// 区分三种结果：信息完整 / 已收录但名称或描述缺失（标注待补充）/
    /// 条目不存在（由调用方处理）。
    fn render_tile(&self, tk: &str, entry: &TileEntry, out: &mut String) {
        let missing_name = entry.名称.is_empty();
        let missing_desc = entry.描述.is_empty();
        let info_missing = missing_name || missing_desc;
        let mut meta: Vec<String> = Vec::new();
        if !info_missing {
            meta.push(
                if entry.功能 {
                    "功能地块"
                } else {
                    "非功能地块"
                }
                .into(),
            );
        } else {
            // 标题标注与实际缺失维度一致，避免标题与正文自相矛盾（全完整情况已由外层排除）
            let label = if missing_name && missing_desc {
                "信息待补充"
            } else if missing_name {
                "名称待补充"
            } else {
                "描述待补充"
            };
            meta.push(label.into());
        }
        let meta_part = format!(" · {}", meta.join(" · "));

        // 信息缺失：名称/描述至少一项为空
        if missing_name && missing_desc {
            out.push_str(&format!(
                "━━━ {tk}（已收录{meta_part}）━━━\n地块存在但信息尚未补充\n"
            ));
        } else if missing_name {
            out.push_str(&format!(
                "━━━ {tk}（已收录{meta_part}）━━━\n描述：{}\n名称：待补充\n",
                entry.描述
            ));
        } else if missing_desc {
            out.push_str(&format!(
                "━━━ {tk}（已收录{meta_part}）━━━\n名称：{}\n描述：待补充\n",
                entry.名称
            ));
        } else {
            let name = &entry.名称;
            let desc = &entry.描述;
            out.push_str(&format!(
                "━━━ {tk}（{name}{meta_part}）━━━\n名称：{name}\n描述：{desc}\n"
            ));
        }
        if !entry.备注.is_empty() {
            out.push_str(&format!("备注：{}\n", entry.备注));
        }
    }

    fn query_tiles(&self, table: Table, keys: &[String]) -> String {
        let mut output = String::new();
        for (i, key) in keys.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }
            // 先按 tileKey 精确查（兼容既有用法），未命中按中文名反查
            // （地块图已改输出中文名，LLM 拿中文名即可查详情；重名批量返回）
            if let Some(entry) = self.tiles.get(key) {
                self.render_tile(key, entry, &mut output);
                continue;
            }
            match self.tile_by_name.get(key) {
                Some(tks) if !tks.is_empty() => {
                    for (j, tk) in tks.iter().enumerate() {
                        if j > 0 {
                            output.push('\n');
                        }
                        self.render_tile(tk, &self.tiles[tk], &mut output);
                    }
                }
                _ => output.push_str(&format!("✗ {key} 未收录于 {} 表\n", table.label())),
            }
        }
        output.trim_end().to_string()
    }

    // ── slang 表 ──

    /// 渲染单个 slang 条目：别名标题 + 指向/类型（+ 说明）。
    fn render_slang(&self, alias: &str, entry: &SlangEntry, out: &mut String) {
        out.push_str(&format!("━━━ {alias} ━━━\n"));
        out.push_str(&format!("指向：{}（{}）\n", entry.指向, entry.类型));
        if !entry.说明.is_empty() {
            out.push_str(&format!("说明：{}\n", entry.说明));
        }
    }

    fn query_slangs(&self, table: Table, keys: &[String]) -> String {
        let mut output = String::new();
        for (i, alias) in keys.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }
            match self.slangs.get(alias) {
                Some(entry) => self.render_slang(alias, entry, &mut output),
                None => output.push_str(&format!("✗ {alias} 未收录于 {} 表\n", table.label())),
            }
        }
        output.trim_end().to_string()
    }

    // ── term 表 ──

    /// 渲染单个 term 条目：术语名标题 + 定义文本。
    fn render_term(&self, name: &str, desc: &str, out: &mut String) {
        out.push_str(&format!("━━━ {name} ━━━\n{desc}\n"));
    }

    fn query_terms(&self, table: Table, keys: &[String]) -> String {
        let mut output = String::new();
        for (i, name) in keys.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }
            match self.terms.get(name) {
                Some(desc) => self.render_term(name, desc, &mut output),
                None => output.push_str(&format!("✗ {name} 未收录于 {} 表\n", table.label())),
            }
        }
        output.trim_end().to_string()
    }

    // ── abnormal 表 ──

    /// 渲染单个异常效果条目：标题 + 效果/分类/可抵抗行。
    ///
    /// 分类为空数组时不输出分类行；可抵抗为布尔值，总是输出是/否，
    /// 避免 LLM 将省略误判为未标注。
    fn render_abnormal(&self, name: &str, entry: &AbnormalEntry, out: &mut String) {
        out.push_str(&format!("━━━ {name} ━━━\n"));
        out.push_str(&format!("效果：{}\n", entry.效果));
        if !entry.分类.is_empty() {
            out.push_str(&format!("分类：{}\n", entry.分类.join("、")));
        }
        out.push_str(&format!(
            "可抵抗：{}\n",
            if entry.可抵抗 { "是" } else { "否" }
        ));
    }

    fn query_abnormals(&self, table: Table, keys: &[String]) -> String {
        let mut output = String::new();
        for (i, name) in keys.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }
            match self.abnormals.get(name) {
                Some(entry) => self.render_abnormal(name, entry, &mut output),
                None => output.push_str(&format!("✗ {name} 未收录于 {} 表\n", table.label())),
            }
        }
        output.trim_end().to_string()
    }
}

impl Tool for QueryTable {
    fn name(&self) -> &str {
        "query_table"
    }

    fn description(&self) -> &str {
        "按表查询条目：table 指定表名，keys 为键列表，逐个返回结果。range=攻击范围表，按 rangeID 查范围形状（字符画：Ｏ=干员所在格、▣=范围格、・=空位标记）与别名；tile=地块表，按中文名查地块名称、描述、功能标记与机制备注（如有），重名地块批量返回；term=战斗术语表，按术语名查定义；abnormal=异常效果表，按异常中文名查效果、所属检查分类与可抵抗标记；slang=黑话表，按社区称呼/别名查其所指实体与类型（如 42→史尔特尔）。信息缺失标注待补充，未收录键明确提示。raw=true 时仅 range 表返回原始格点坐标"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "table": {
                    "type": "string",
                    "enum": ["range", "tile", "term", "abnormal", "slang"],
                    "description": "要查询的表"
                },
                "keys": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要查询的键列表：range 表为 rangeID（如 \"2-3\"），tile 表为中文名（如 \"地穴\"），term 表为术语名（如 \"可充能\"），abnormal 表为异常中文名（如 \"晕眩\"），slang 表为社区称呼/别名（如 \"42\"）"
                },
                "raw": {
                    "type": "boolean",
                    "description": "仅 range 表生效：true 时返回原始格点坐标而非范围图形，其他表忽略。默认 false"
                }
            },
            "required": ["table", "keys"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let table = v.get("table").and_then(|x| x.as_str()).unwrap_or("");
        let table_cn = match table {
            "range" => "攻击范围",
            "tile" => "地块",
            "term" => "术语",
            "abnormal" => "异常效果",
            "slang" => "黑话",
            other => other,
        };
        match array_summary(&v, "keys") {
            Some(keys) => ToolDisplay::info(format!("{table_cn} · {keys}")),
            None if !table_cn.is_empty() => ToolDisplay::info(table_cn),
            None => ToolDisplay::default(),
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        // ── 1. 解析参数 ──
        let table = Table::parse(v["table"].as_str().ok_or_else(|| {
            ToolError::Parse("table 必须为字符串（range/tile/term/abnormal/slang 之一）".into())
        })?)?;

        let keys: Vec<String> = v["keys"]
            .as_array()
            .ok_or_else(|| ToolError::Parse("缺少 keys 参数（键字符串数组）".into()))?
            .iter()
            .map(|x| x.as_str().map(|s| s.to_string()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| ToolError::Parse("keys 元素必须为字符串".into()))?;

        if keys.is_empty() {
            return Err(ToolError::Parse("keys 不能为空".into()));
        }

        // 去重（保留首次出现顺序）：LLM 可能重复传入同一键，重复输出浪费上下文
        let mut seen: HashSet<String> = HashSet::new();
        let keys: Vec<String> = keys
            .into_iter()
            .filter(|k| seen.insert(k.clone()))
            .collect();

        let raw = match v.get("raw") {
            None => false,
            Some(b) => b
                .as_bool()
                .ok_or_else(|| ToolError::Parse("raw 必须为布尔值".into()))?,
        };

        // ── 2. 按表分发查询 ──
        let output = match table {
            Table::Range => self.query_ranges(table, &keys, raw),
            Table::Tile => self.query_tiles(table, &keys),
            Table::Term => self.query_terms(table, &keys),
            Table::Abnormal => self.query_abnormals(table, &keys),
            Table::Slang => self.query_slangs(table, &keys),
        };

        // 内嵌 JSON 表查询：结构化行数据在分发方法内部消费，此处退化为文本输出
        Ok(ToolOutput::text(output))
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立于真实数据的小样本工具（覆盖各表的典型情况）。
    fn make_tool() -> QueryTable {
        let ranges = r#"{
            "2-3": { "grids": [[-1,0],[-1,1],[0,0],[0,1],[0,2],[1,0],[1,1]], "alias": "领主远程" },
            "1-5": { "grids": [[0,1]], "alias": "测试单格" }
        }"#;
        let tiles = r#"{
            "tile_hole": { "名称": "地穴", "描述": "经过的敌人会摔落至底部直接死亡", "功能": true, "备注": "补充说明样例" },
            "tile_empty": { "名称": "", "描述": "", "功能": false },
            "tile_noname": { "名称": "", "描述": "只有描述没有名称", "功能": false },
            "tile_volcano": { "名称": "热泵通道", "描述": "", "功能": true },
            "tile_reed_a": { "名称": "芦苇丛", "描述": "隐藏我方单位", "功能": true },
            "tile_reed_b": { "名称": "芦苇丛", "描述": "使敌人隐匿", "功能": true }
        }"#;
        let terms = r#"{
            "停顿": "移动速度降低80%",
            "束缚": "无法移动",
            "可充能": "一些技能中存在“可充能X次”的描述，其实际效果为当前技力上限等于该技能技力需求的X倍"
        }"#;
        let abnormals = r#"{
            "晕眩": { "效果": "无法移动、阻挡、攻击及使用技能", "分类": ["阻止攻击", "阻止能力"], "可抵抗": true },
            "沉默": { "效果": "无法触发可沉默的技能与能力", "分类": [], "可抵抗": false },
            "束缚": { "效果": "无法移动", "分类": ["阻止移动"], "可抵抗": false }
        }"#;
        let slangs = r#"{
            "42": { "指向": "史尔特尔", "类型": "干员" },
            "大爹": { "指向": "博卓卡斯替", "类型": "敌人", "说明": "可能指多个实体" }
        }"#;
        QueryTable::new(ranges, tiles, terms, abnormals, slangs)
    }

    #[test]
    fn tool_metadata() {
        let tool = make_tool();
        assert_eq!(tool.name(), "query_table");
        let desc = tool.description();
        assert!(desc.contains("range"));
        assert!(desc.contains("tile"));
        assert!(desc.contains("term"));
        assert!(desc.contains("abnormal"));
        assert!(desc.contains("slang"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        let required = params["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "table"));
        assert!(required.iter().any(|r| r == "keys"));
        // table 枚举包含五种表
        let table_props = &params["properties"]["table"];
        assert_eq!(table_props["enum"][0], "range");
        assert_eq!(table_props["enum"][1], "tile");
        assert_eq!(table_props["enum"][2], "term");
        assert_eq!(table_props["enum"][3], "abnormal");
        assert_eq!(table_props["enum"][4], "slang");
    }

    // ── range 表 ──

    #[test]
    fn range_with_operator_and_alias() {
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"range","keys":["2-3"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("2-3（7 格"));
        assert!(out.contains("别名：领主远程"));
        assert!(out.contains("Ｏ"));
        assert!(out.contains("▣▣・"));
        assert!(!out.contains("干员格不在范围内"));
    }

    #[test]
    fn range_without_operator() {
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"range","keys":["1-5"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("1-5（1 格"));
        assert!(out.contains("干员格不在范围内"));
        assert!(out.contains("Ｏ▣"));
    }

    #[test]
    fn range_raw_mode() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"range","keys":["1-5"],"raw":true}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("grids: [[0,1]]"));
        assert!(!out.contains("Ｏ"));
        assert!(!out.contains("▣"));
    }

    #[test]
    fn range_unknown_key() {
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"range","keys":["zz-9"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("✗ zz-9 未收录于 range 表"));
    }

    #[test]
    fn range_batch_partial_missing() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"range","keys":["2-3","zz-9","1-5"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("2-3"));
        assert!(out.contains("1-5"));
        assert!(out.contains("✗ zz-9"));
    }

    // ── tile 表 ──

    #[test]
    fn tile_full_entry() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_hole"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("地穴"));
        assert!(out.contains("摔落至底部直接死亡"));
        assert!(out.contains("功能地块"));
        assert!(
            !out.contains("待补充"),
            "完整条目不应标注待补充\n结果: {out}"
        );
    }

    #[test]
    fn tile_note_shown() {
        // 备注非空：正文末尾追加备注行
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_hole"]}"#,
            )
            .unwrap()
            .summary;
        assert!(
            out.contains("备注：补充说明样例"),
            "备注应输出\n结果: {out}"
        );
    }

    #[test]
    fn tile_note_absent() {
        // 备注缺省（未写）：不输出备注行
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_reed_a"]}"#,
            )
            .unwrap()
            .summary;
        assert!(!out.contains("备注："), "无备注不应输出备注行\n结果: {out}");
    }

    #[test]
    fn tile_missing_all_info() {
        // 条目存在但名称描述都空：应提示已收录、信息未补充，而非不存在
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_empty"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("已收录"), "应标注已收录\n结果: {out}");
        assert!(
            out.contains("信息尚未补充"),
            "应标注信息未补充\n结果: {out}"
        );
        assert!(!out.contains("不存在"), "不应提示不存在\n结果: {out}");
        // 信息缺失的条目：功能标记无依据（存储默认 false），不应输出非功能地块
        assert!(
            !out.contains("非功能地块"),
            "缺失条目不应输出非功能标记\n结果: {out}"
        );
    }

    #[test]
    fn tile_missing_name() {
        // 有描述缺名称：标题标注名称待补充（与正文一致），保留已有描述
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_noname"]}"#,
            )
            .unwrap()
            .summary;
        assert!(
            out.contains("名称待补充"),
            "标题应标注名称待补充\n结果: {out}"
        );
        assert!(out.contains("只有描述没有名称"));
        assert!(out.contains("名称：待补充"));
        assert!(
            !out.contains("描述待补充"),
            "标题不应误标描述待补充\n结果: {out}"
        );
    }

    #[test]
    fn tile_missing_description() {
        // 有条目但有名称缺描述：提示描述待补充，保留已有名称
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_volcano"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("热泵通道"));
        assert!(
            out.contains("描述：待补充"),
            "应标注描述待补充\n结果: {out}"
        );
        assert!(!out.contains("不存在"), "不应提示不存在\n结果: {out}");
    }

    #[test]
    fn tile_unknown_key() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_zzzz"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("✗ tile_zzzz 未收录于 tile 表"));
    }

    #[test]
    fn tile_by_chinese_name() {
        // 地块图已改输出中文名：中文名反查详情（标题带 tileKey）
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"tile","keys":["地穴"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("tile_hole"), "标题应带 tileKey\n结果: {out}");
        assert!(out.contains("地穴"));
        assert!(out.contains("摔落至底部直接死亡"));
    }

    #[test]
    fn tile_by_chinese_name_duplicate_returns_all() {
        // 重名中文名（真实数据有 17 组）：批量返回全部匹配
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"tile","keys":["芦苇丛"]}"#)
            .unwrap()
            .summary;
        assert!(
            out.contains("tile_reed_a"),
            "应含第一个重名条目\n结果: {out}"
        );
        assert!(
            out.contains("tile_reed_b"),
            "应含第二个重名条目\n结果: {out}"
        );
        assert!(out.contains("隐藏我方单位"));
        assert!(out.contains("使敌人隐匿"));
    }

    // ── term 表 ──

    #[test]
    fn term_hit() {
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"term","keys":["停顿"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("━━━ 停顿 ━━━"));
        assert!(out.contains("移动速度降低80%"));
    }

    #[test]
    fn term_long_entry() {
        // 长描述完整输出，不被截断
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"term","keys":["可充能"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("可充能X次"));
        assert!(out.contains("技力上限等于该技能技力需求的X倍"));
    }

    #[test]
    fn term_unknown_key() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"term","keys":["不存在的术语"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("✗ 不存在的术语 未收录于 term 表"));
    }

    #[test]
    fn term_batch_and_dedup() {
        // 重复键只输出一次，保留首次出现顺序
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"term","keys":["停顿","停顿","束缚"]}"#,
            )
            .unwrap()
            .summary;
        assert_eq!(
            out.matches("━━━ 停顿 ━━━").count(),
            1,
            "重复键应只输出一次\n结果: {out}"
        );
        assert!(out.contains("━━━ 束缚 ━━━"));
    }

    // ── abnormal 表 ──

    #[test]
    fn abnormal_full_entry() {
        // 命中：效果 + 分类 + 可抵抗全部输出
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"abnormal","keys":["晕眩"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("━━━ 晕眩 ━━━"));
        assert!(out.contains("效果：无法移动、阻挡、攻击及使用技能"));
        assert!(out.contains("分类：阻止攻击、阻止能力"));
        assert!(out.contains("可抵抗：是"));
    }

    #[test]
    fn abnormal_not_resistable_and_no_category() {
        // 不可抵抗 + 无分类：不输出分类行，可抵抗明确标注否
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"abnormal","keys":["沉默"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("可抵抗：否"));
        assert!(!out.contains("分类："), "无分类不应输出分类行\n结果: {out}");
    }

    #[test]
    fn abnormal_unknown_key() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"abnormal","keys":["不存在的异常"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("✗ 不存在的异常 未收录于 abnormal 表"));
    }

    #[test]
    fn abnormal_batch() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"abnormal","keys":["晕眩","束缚","晕眩"]}"#,
            )
            .unwrap()
            .summary;
        assert_eq!(
            out.matches("━━━ 晕眩 ━━━").count(),
            1,
            "重复键应只输出一次\n结果: {out}"
        );
        assert!(out.contains("━━━ 束缚 ━━━"));
    }

    // ── slang 表 ──

    #[test]
    fn slang_hit() {
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"slang","keys":["42"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("━━━ 42 ━━━"));
        assert!(out.contains("指向：史尔特尔（干员）"));
    }

    #[test]
    fn slang_with_explain() {
        // 带说明的条目：追加说明行
        let tool = make_tool();
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"slang","keys":["大爹"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("指向：博卓卡斯替（敌人）"));
        assert!(out.contains("说明：可能指多个实体"));
    }

    #[test]
    fn slang_unknown_key() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"slang","keys":["不存在的黑话"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("✗ 不存在的黑话 未收录于 slang 表"));
    }

    // ── 跨表去重 ──

    #[test]
    fn range_duplicate_keys_deduplicated() {
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"range","keys":["2-3","2-3","1-5","2-3"]}"#,
            )
            .unwrap()
            .summary;
        assert_eq!(
            out.matches("━━━ 2-3（7 格").count(),
            1,
            "重复键应只输出一次\n结果: {out}"
        );
        assert!(out.contains("1-5"));
    }

    // ── 参数错误 ──

    #[test]
    fn missing_table_param() {
        let tool = make_tool();
        let result = tool.execute(&ToolContext::new(), r#"{"keys":["2-3"]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_table() {
        let tool = make_tool();
        let result = tool.execute(&ToolContext::new(), r#"{"table":"foo","keys":["2-3"]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("range/tile/term"));
    }

    #[test]
    fn missing_keys_param() {
        let tool = make_tool();
        let result = tool.execute(&ToolContext::new(), r#"{"table":"range"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn empty_keys() {
        let tool = make_tool();
        let result = tool.execute(&ToolContext::new(), r#"{"table":"range","keys":[]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn non_string_key() {
        let tool = make_tool();
        let result = tool.execute(&ToolContext::new(), r#"{"table":"range","keys":[123]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_raw_type() {
        // raw 传非布尔值：严格报错而非静默降级
        let tool = make_tool();
        let result = tool.execute(
            &ToolContext::new(),
            r#"{"table":"range","keys":["2-3"],"raw":"true"}"#,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("布尔"));
    }

    #[test]
    fn raw_ignored_on_other_tables() {
        // raw 仅 range 表生效，term 表传入 raw 不影响输出
        let tool = make_tool();
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"term","keys":["停顿"],"raw":true}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("━━━ 停顿 ━━━"));
        assert!(out.contains("移动速度降低80%"));
    }

    // ── 真实数据 ──

    #[test]
    fn real_data_all_tables() {
        let tool = QueryTable::new(
            include_str!("../../resources/ranges.json"),
            include_str!("../../resources/tile_info.json"),
            include_str!("../../resources/term.json"),
            include_str!("../../resources/abnormal_effects.json"),
            include_str!("../../resources/slang.json"),
        );
        // range：4-3 不含干员格，0-1 仅含干员格，3-10 别名穿透
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"range","keys":["4-3"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("4-3（13 格"));
        assert!(out.contains("干员格不在范围内"));
        assert!(out.contains("Ｏ・▣▣▣"));
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"range","keys":["3-10"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("别名：炮手攻击范围"));
        // tile：官方条目与非功能地块
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_hole"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("地穴"));
        assert!(out.contains("功能地块"));
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"tile","keys":["tile_road"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("平地"));
        assert!(out.contains("非功能地块"));
        // term：真实术语
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"term","keys":["可充能"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("可充能X次"));
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"term","keys":["移动"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("不退场，以当前血量在目标位置部署"));
        // abnormal：真实异常效果
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"abnormal","keys":["晕眩"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("可抵抗：是"));
        assert!(out.contains("分类：阻止攻击、阻止能力"));
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"abnormal","keys":["战栗"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("无法触发普通攻击"));
        assert!(out.contains("可抵抗：否"));
        // slang：真实黑话
        let out = tool
            .execute(&ToolContext::new(), r#"{"table":"slang","keys":["42"]}"#)
            .unwrap()
            .summary;
        assert!(out.contains("史尔特尔（干员）"));
        // 不存在的键（跨表确认提示格式）
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"term","keys":["绝对不存在的术语"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("未收录于 term 表"));
        let out = tool
            .execute(
                &ToolContext::new(),
                r#"{"table":"abnormal","keys":["绝对不存在的异常"]}"#,
            )
            .unwrap()
            .summary;
        assert!(out.contains("未收录于 abnormal 表"));
    }

    // ── 手动测试入口 ──

    #[test]
    fn show_output() {
        // 环境变量驱动：PRTS_TABLE="range|tile|term|abnormal|slang"  PRTS_KEYS="a,b,c"  PRTS_RAW="1"
        let tool = QueryTable::new(
            include_str!("../../resources/ranges.json"),
            include_str!("../../resources/tile_info.json"),
            include_str!("../../resources/term.json"),
            include_str!("../../resources/abnormal_effects.json"),
            include_str!("../../resources/slang.json"),
        );
        let call = |table: &str, keys: &str, raw: bool| -> String {
            let mut params = serde_json::json!({
                "table": table,
                "keys": keys.split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>()
            });
            if raw {
                params["raw"] = serde_json::json!(true);
            }
            match tool.execute(&ToolContext::new(), &params.to_string()) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        };

        if let Ok(table) = std::env::var("PRTS_TABLE") {
            if let Ok(keys) = std::env::var("PRTS_KEYS") {
                let raw = std::env::var("PRTS_RAW").map(|v| v == "1").unwrap_or(false);
                println!("{}", call(&table, &keys, raw));
                return;
            }
        }

        println!("========== range: 2-3 ==========");
        println!("{}", call("range", "2-3", false));
        println!("\n========== range: x-4 ==========");
        println!("{}", call("range", "x-4", false));
        println!("\n========== tile: tile_hole ==========");
        println!("{}", call("tile", "tile_hole", false));
        println!("\n========== tile: tile_poison（含备注） ==========");
        println!("{}", call("tile", "tile_poison", false));
        println!("\n========== term: 可充能, 束缚 ==========");
        println!("{}", call("term", "可充能,束缚", false));
        println!("\n========== abnormal: 晕眩, 战栗 ==========");
        println!("{}", call("abnormal", "晕眩,战栗", false));
        println!("\n========== slang: 42, 大爹 ==========");
        println!("{}", call("slang", "42,大爹", false));
        println!("\n========== term: 不存在的术语 ==========");
        println!("{}", call("term", "绝对不存在的术语", false));
    }
}
