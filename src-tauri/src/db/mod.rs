//! 数据库访问层 —— SQLite 查询封装
//!
//! 职责：管理 rusqlite 连接，提供白名单过滤的只读查询能力。
//! `query_filtered()` 返回原始结构化数据（`Vec<HashMap<String, String>>`），
//! 不做格式化——格式化是工具层的职责。
//!
//! ## 安全设计
//!
//! 列名来自 LLM 输出（不可信输入），SQL 列名无法通过参数化绑定。
//! 初始化时从 PRAGMA table_info 动态获取合法列名白名单，
//! LLM 请求的字段和过滤字段均与白名单取交集。
//! 未命中的 fields 静默丢弃，未命中的 filter fields 返回错误。
//!
//! ## 操作符安全
//!
//! FilterOp 枚举白名单限制可用操作符，外部无法构造任意 SQL 运算符。
//! 所有值通过参数化绑定（`?1`, `?2`...）传递，LIKE 的 `%` 包裹在 Rust 侧完成。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::debug;
use rusqlite::Connection;

// ═══════════════════════════════════════════════════════
// 常量
// ═══════════════════════════════════════════════════════

/// 所有可用表。Database::open 对每张表执行 PRAGMA table_info 构建白名单。
const ALL_TABLES: &[&str] = &[
    "enemies",
    "operators",
    "stages",
    "levels",
    "tokens",
    "traps",
    "rogue_1_entries",
    "rogue_2_entries",
    "rogue_3_entries",
    "rogue_4_entries",
    "rogue_5_entries",
];

// ═══════════════════════════════════════════════════════
// FilterOp · Filter — 查询条件
// ═══════════════════════════════════════════════════════

/// 过滤操作符白名单。
///
/// 枚举而非字符串——外部无法构造任意 SQL 运算符。
/// 各变体映射到标准 SQL 运算符（`as_sql()`）。
#[derive(Debug, Clone, PartialEq)]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Gte,
    Lte,
    Like,
}

impl FilterOp {
    fn as_sql(&self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "!=",
            FilterOp::Gt => ">",
            FilterOp::Lt => "<",
            FilterOp::Gte => ">=",
            FilterOp::Lte => "<=",
            FilterOp::Like => "LIKE",
        }
    }
}

impl std::str::FromStr for FilterOp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "=" => Ok(FilterOp::Eq),
            "!=" => Ok(FilterOp::Ne),
            ">" => Ok(FilterOp::Gt),
            "<" => Ok(FilterOp::Lt),
            ">=" => Ok(FilterOp::Gte),
            "<=" => Ok(FilterOp::Lte),
            "LIKE" => Ok(FilterOp::Like),
            _ => Err(format!("不支持的操作符: {s}。可用: =, >, <, >=, <=, !=, LIKE")),
        }
    }
}

/// 一个过滤条件：`字段 操作符 值`。
///
/// `field` 必须通过表级白名单校验。
/// `value` 在构建 SQL 时以参数化绑定（?N）注入，`Like` 时自动包裹 `%value%`。
#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
    pub value: String,
}

impl Filter {
    pub fn new(field: impl Into<String>, op: FilterOp, value: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            op,
            value: value.into(),
        }
    }
}

// ═══════════════════════════════════════════════════════
// QueryResult —— 查询返回值
// ═══════════════════════════════════════════════════════

/// 一次查询的完整结果。
///
/// `rows` 是命中的数据行，`dropped_fields` 是请求了但不在白名单中被丢弃的字段名。
/// 工具层可据此告知 LLM 哪些字段无效，而非静默忽略。
#[derive(Debug)]
pub struct QueryResult {
    pub rows: Vec<HashMap<String, String>>,
    pub dropped_fields: Vec<String>,
}

// ═══════════════════════════════════════════════════════
// Database
// ═══════════════════════════════════════════════════════

/// 数据库访问句柄。
///
/// # 线程安全
///
/// `rusqlite::Connection` 不实现 `Sync`，用 `Mutex` 包裹。
/// 所有查询方法内部短暂持锁，不跨 `.await` 持有。
///
/// # 列名白名单
///
/// `valid_columns` 在 `open()` 时通过 PRAGMA table_info 动态填充，
/// 数据管线新增列时无需修改 Rust 代码。
#[derive(Debug)]
pub struct Database {
    conn: Mutex<Connection>,
    /// 表名 → 该表的所有合法列名
    valid_columns: HashMap<String, HashSet<String>>,
}

impl Database {
    /// 打开数据库文件，对所有表构建列名白名单。
    ///
    /// 仅当文件无法打开时返回错误。某张表不存在不会导致整体失败，
    /// 该表的列集合为空，后续查询时返回明确的"表不存在"错误。
    pub fn open(path: &str) -> Result<Self, String> {
        let conn =
            Connection::open(path).map_err(|e| format!("无法打开数据库 {}: {e}", path))?;

        let mut valid_columns: HashMap<String, HashSet<String>> = HashMap::new();

        for table in ALL_TABLES {
            let mut cols = HashSet::new();
            let sql = format!("PRAGMA table_info(\"{table}\")");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(1)) {
                    for name in rows.flatten() {
                        cols.insert(name);
                    }
                }
            }
            valid_columns.insert(table.to_string(), cols);
        }

        Ok(Self {
            conn: Mutex::new(conn),
            valid_columns,
        })
    }

    /// 执行白名单过滤的只读查询，返回原始结构化数据。
    ///
    /// # 参数
    ///
    /// * `table` — 目标表名
    /// * `fields` — LLM 请求的返回字段列表。与白名单取交集，
    ///   不存在的列静默丢弃并记 warn 日志，全部无效则返回错误
    /// * `filters` — 过滤条件列表，以 AND 连接。每个 `Filter.field`
    ///   必须通过白名单校验（失败返回错误）。空数组表示无条件（返回全部行）
    /// * `limit` — 最大返回行数。0 表示不设上限
    ///
    /// # 返回
    ///
    /// `QueryResult { rows, dropped_fields }`。`rows` 每行是一个 列名→值 的映射，
    /// `dropped_fields` 记录了被白名单丢弃的字段名（供工具层告知 LLM）。
    /// 空结果返回 `rows: []`（不返回错误）。
    /// 值类型映射：Null→""，Integer/Real→数字字符串，Text→原字符串，Blob→"[BLOB]"。
    pub fn query_filtered(
        &self,
        table: &str,
        fields: &[String],
        filters: &[Filter],
        limit: u32,
    ) -> Result<QueryResult, String> {
        // ── 1. 校验表存在 ──
        let valid = self
            .valid_columns
            .get(table)
            .ok_or_else(|| format!("未知数据表: {table}"))?;

        if valid.is_empty() {
            return Err(format!("数据表 {table} 的列为空（表不存在或结构异常）"));
        }

        // ── 2. 字段白名单过滤（返回字段）──
        let mut safe_fields: Vec<&str> = Vec::new();
        let mut dropped_fields: Vec<String> = Vec::new();
        for f in fields {
            if valid.contains(f.as_str()) {
                safe_fields.push(f.as_str());
            } else {
                dropped_fields.push(f.clone());
                log::warn!("[db] 表 {table} 忽略无效请求字段: {f}");
            }
        }

        if safe_fields.is_empty() {
            let available = self.list_columns(table);
            return Err(format!(
                "请求的字段均不在表 {table} 的合法列名中。可用列（部分）: {available}"
            ));
        }

        // ── 3. 校验过滤字段 ──
        for f in filters {
            if !valid.contains(f.field.as_str()) {
                return Err(format!(
                    "过滤字段 \"{}\" 不在表 {table} 的合法列名中",
                    f.field
                ));
            }
        }

        // ── 4. 构建参数化 SQL ──
        let cols_str = safe_fields
            .iter()
            .map(|c| format!("\"{}\"", c))
            .collect::<Vec<_>>()
            .join(", ");

        let mut sql = format!("SELECT {cols_str} FROM \"{table}\"");

        // 参数值列表（按 ?1, ?2... 顺序），对应 filters 的值。
        // limit 不参数化——它是 u32，不可能含 SQL，直接拼入字符串更简洁。
        let mut param_values: Vec<String> = Vec::new();

        if !filters.is_empty() {
            let clauses: Vec<String> = filters
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let idx = i + 1; // SQL 参数从 ?1 开始
                    let val = if f.op == FilterOp::Like {
                        format!("%{}%", f.value)
                    } else {
                        f.value.clone()
                    };
                    param_values.push(val);
                    format!("\"{}\" {} ?{}", f.field, f.op.as_sql(), idx)
                })
                .collect();
            sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
        }

        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        // ── 5. 执行查询 ──
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("数据库锁获取失败: {e}"))?;

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("SQL 准备失败: {e}"))?;

        let column_names: Vec<String> = safe_fields.iter().map(|s| s.to_string()).collect();

        let param_refs: Vec<&str> = param_values.iter().map(|s| s.as_str()).collect();

        let rows: Vec<Vec<rusqlite::types::Value>> = if param_refs.is_empty() {
            // 无条件查询：无 WHERE 子句，不需要参数
            stmt.query_map([], |row| {
                let mut values = Vec::with_capacity(column_names.len());
                for i in 0..column_names.len() {
                    values.push(row.get::<_, rusqlite::types::Value>(i)?);
                }
                Ok(values)
            })
            .map_err(|e| format!("查询执行失败: {e}"))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(rusqlite::params_from_iter(param_refs), |row| {
                let mut values = Vec::with_capacity(column_names.len());
                for i in 0..column_names.len() {
                    values.push(row.get::<_, rusqlite::types::Value>(i)?);
                }
                Ok(values)
            })
            .map_err(|e| format!("查询执行失败: {e}"))?
            .filter_map(|r| r.ok())
            .collect()
        };

        drop(stmt);
        drop(conn);

        // ── 6. 转换为 Vec<HashMap<String, String>> ──
        let result = QueryResult {
            rows: rows_to_maps(&column_names, &rows),
            dropped_fields,
        };
        debug!(
            "db" => table,
            "action" => "query",
            "fields" => safe_fields.len(),
            "filters" => filters.len(),
            "rows" => result.rows.len()
        );
        Ok(result)
    }

    /// 对过滤条件做 COUNT(*)，返回匹配行数。
    ///
    /// 表级和字段级白名单校验与 `query_filtered` 一致。
    /// 无条件（filters 为空）返回全表行数。
    pub fn query_count(
        &self,
        table: &str,
        filters: &[Filter],
    ) -> Result<u32, String> {
        // ── 1. 校验表存在 ──
        let valid = self
            .valid_columns
            .get(table)
            .ok_or_else(|| format!("未知数据表: {table}"))?;

        if valid.is_empty() {
            return Err(format!("数据表 {table} 的列为空（表不存在或结构异常）"));
        }

        // ── 2. 校验过滤字段 ──
        for f in filters {
            if !valid.contains(f.field.as_str()) {
                return Err(format!(
                    "过滤字段 \"{}\" 不在表 {table} 的合法列名中",
                    f.field
                ));
            }
        }

        // ── 3. 构建 COUNT SQL ──
        let mut sql = format!("SELECT COUNT(*) FROM \"{table}\"");
        let mut param_values: Vec<String> = Vec::new();

        if !filters.is_empty() {
            let clauses: Vec<String> = filters
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let idx = i + 1;
                    let val = if f.op == FilterOp::Like {
                        format!("%{}%", f.value)
                    } else {
                        f.value.clone()
                    };
                    param_values.push(val);
                    format!("\"{}\" {} ?{}", f.field, f.op.as_sql(), idx)
                })
                .collect();
            sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
        }

        // ── 4. 执行 ──
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("数据库锁获取失败: {e}"))?;

        let param_refs: Vec<&str> = param_values.iter().map(|s| s.as_str()).collect();

        let count: u32 = if param_refs.is_empty() {
            conn.query_row(&sql, [], |row| row.get(0))
        } else {
            conn.query_row(&sql, rusqlite::params_from_iter(param_refs), |row| row.get(0))
        }
        .map_err(|e| format!("COUNT 查询失败: {e}"))?;

        drop(conn);
        Ok(count)
    }

    /// 列出某表的所有合法列名（最多 20 个，用于错误提示）。
    fn list_columns(&self, table: &str) -> String {
        self.valid_columns
            .get(table)
            .map(|cols| {
                let mut names: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
                names.sort();
                if names.len() <= 20 {
                    names.join("、")
                } else {
                    format!("{}…（共 {} 列）", names[..20].join("、"), names.len())
                }
            })
            .unwrap_or_default()
    }
}

// ═══════════════════════════════════════════════════════
// 辅助函数
// ═══════════════════════════════════════════════════════

/// 将原始行数据转换为 `Vec<HashMap<String, String>>`。
fn rows_to_maps(
    column_names: &[String],
    rows: &[Vec<rusqlite::types::Value>],
) -> Vec<HashMap<String, String>> {
    rows.iter()
        .map(|row| {
            column_names
                .iter()
                .enumerate()
                .map(|(i, col)| {
                    let val = value_to_string(&row[i]);
                    (col.clone(), val)
                })
                .collect()
        })
        .collect()
}

/// rusqlite::Value → String。
fn value_to_string(v: &rusqlite::types::Value) -> String {
    match v {
        rusqlite::types::Value::Null => String::new(),
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Real(f) => f.to_string(),
        rusqlite::types::Value::Text(s) => s.clone(),
        rusqlite::types::Value::Blob(_) => "[BLOB]".into(),
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 开发期数据库路径。
    fn db_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arknights.db")
    }

    /// 构造一个 LIKE 名称过滤器的快捷方式。
    fn name_like(_table: &str, name_column: &str, keyword: &str) -> Filter {
        // 验证 name_column 在目标表存在（调用方已知表结构）
        Filter::new(name_column, FilterOp::Like, keyword)
    }

    // ── 基础功能 ──

    #[test]
    fn open_database_succeeds() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        for table in ALL_TABLES {
            let cols = db.valid_columns.get(*table).unwrap();
            assert!(!cols.is_empty(), "表 {table} 的列集合不应为空");
        }
    }

    #[test]
    fn query_enemy_by_name() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into(), "级别".into(), "最大生命值".into()],
                &[name_like("enemies", "名称", "源石虫")],
                5,
            ).map(|r| r.rows)
            .unwrap();

        assert!(!rows.is_empty(), "至少应返回一条结果");
        let first = &rows[0];
        assert!(first.contains_key("名称"), "应包含'名称'列");
        assert!(first["名称"].contains("源石虫"), "名称应包含搜索词");
        assert!(first.contains_key("最大生命值"), "应包含'最大生命值'列");
    }

    #[test]
    fn query_enemy_no_result() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into()],
                &[name_like("enemies", "名称", "这个敌人绝对不存在xyz123")],
                5,
            ).map(|r| r.rows)
            .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn query_operator_multiple_results() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "operators",
                &["名称".into(), "职业".into(), "稀有度".into()],
                &[name_like("operators", "名称", "阿米娅")],
                5,
            ).map(|r| r.rows)
            .unwrap();

        // 阿米娅有三条（原版+近卫+医疗），受 LIMIT 5 保护
        assert!(rows.len() >= 2, "至少应有两条结果，实际: {}", rows.len());
        assert!(rows[0].contains_key("职业"), "应包含'职业'列");
    }

    #[test]
    fn query_stage_by_name() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "stages",
                &["关卡名".into(), "关卡ID".into(), "关卡编号".into(), "理智消耗".into()],
                &[name_like("stages", "关卡名", "暴君")],
                5,
            ).map(|r| r.rows)
            .unwrap();

        assert!(!rows.is_empty());
        assert_eq!(rows[0]["关卡名"], "暴君");
        assert_eq!(rows[0]["关卡编号"], "1-7");
    }

    #[test]
    fn query_level_by_levelid() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "levels",
                &["levelId".into()],
                &[Filter::new("levelId", FilterOp::Like, "main_01-07")],
                5,
            ).map(|r| r.rows)
            .unwrap();
        assert!(!rows.is_empty());
        assert_eq!(rows[0]["levelId"], "level_main_01-07");
    }

    #[test]
    fn query_single_field() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into()],
                &[name_like("enemies", "名称", "源石虫")],
                5,
            ).map(|r| r.rows)
            .unwrap();

        // LIMIT 5 生效
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].len(), 1); // 只有一个字段
        assert!(rows[0].contains_key("名称"));
    }

    // ── 错误处理 ──

    #[test]
    fn query_nonexistent_table() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let result = db.query_filtered(
            "nonexistent",
            &["名称".into()],
            &[name_like("nonexistent", "名称", "test")],
            5,
        ).map(|r| r.rows);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("未知数据表"));
    }

    #[test]
    fn query_invalid_filter_field() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let result = db.query_filtered(
            "enemies",
            &["名称".into()],
            &[Filter::new("这列肯定不存在", FilterOp::Eq, "test")],
            5,
        ).map(|r| r.rows);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("过滤字段"));
        assert!(msg.contains("不存在"));
    }

    // ── 白名单过滤（返回字段）──

    #[test]
    fn invalid_fields_are_silently_dropped() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into(), "不存在的列xyz".into(), "级别".into()],
                &[name_like("enemies", "名称", "源石虫")],
                5,
            ).map(|r| r.rows)
            .unwrap();

        // 有效字段存在
        assert!(rows[0].contains_key("名称"));
        assert!(rows[0].contains_key("级别"));
        // 无效字段不在 map 中（被静默丢弃）
        assert!(!rows[0].contains_key("不存在的列xyz"));
    }

    #[test]
    fn all_invalid_fields_returns_error() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let result = db.query_filtered(
            "enemies",
            &["不存在的列".into()],
            &[name_like("enemies", "名称", "test")],
            5,
        ).map(|r| r.rows);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("均不在"));
        assert!(msg.contains("可用列"));
    }

    // ── 其他表 ──

    #[test]
    fn query_token_by_name() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "tokens",
                &["名称".into(), "持有者".into()],
                &[name_like("tokens", "名称", "触手")],
                5,
            ).map(|r| r.rows)
            .unwrap();
        assert!(!rows.is_empty());
        assert_eq!(rows[0]["名称"], "触手");
    }

    #[test]
    fn query_trap_by_name() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "traps",
                &["名称".into(), "代号".into()],
                &[name_like("traps", "名称", "障碍物")],
                5,
            ).map(|r| r.rows)
            .unwrap();
        assert!(!rows.is_empty());
        assert_eq!(rows[0]["名称"], "障碍物");
    }

    #[test]
    fn query_empty_fields_array() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let result = db.query_filtered(
            "enemies",
            &[],
            &[name_like("enemies", "名称", "源石虫")],
            5,
        ).map(|r| r.rows);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("均不在"));
    }

    // ── query_filtered 新增能力 ──

    #[test]
    fn filtered_by_numeric_comparison() {
        // 法抗 > 90 的敌人
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into(), "法术抗性".into()],
                &[Filter::new("法术抗性", FilterOp::Gt, "90")],
                0, // 不设上限
            ).map(|r| r.rows)
            .unwrap();

        assert!(!rows.is_empty(), "至少应有法抗>90的敌人");
        for row in &rows {
            let res: f64 = row["法术抗性"].parse().unwrap();
            assert!(res > 90.0, "{} 法抗={} 不满足 >90", row["名称"], res);
        }
    }

    #[test]
    fn filtered_by_equality() {
        // 地位 = 领袖 的敌人
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into(), "地位".into()],
                &[Filter::new("地位", FilterOp::Eq, "领袖")],
                20,
            ).map(|r| r.rows)
            .unwrap();

        assert!(!rows.is_empty(), "至少应有一个领袖敌人");
        for row in &rows {
            assert_eq!(row["地位"], "领袖");
        }
    }

    #[test]
    fn filtered_multiple_conditions() {
        // 地位 = 精英 AND 级别 = 0
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into(), "地位".into(), "级别".into()],
                &[
                    Filter::new("地位", FilterOp::Eq, "精英"),
                    Filter::new("级别", FilterOp::Eq, "0"),
                ],
                10,
            ).map(|r| r.rows)
            .unwrap();

        for row in &rows {
            assert_eq!(row["地位"], "精英");
            assert_eq!(row["级别"], "0");
        }
    }

    #[test]
    fn filtered_no_conditions_returns_all() {
        // 无条件查询 + LIMIT 3
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into()],
                &[],
                3,
            ).map(|r| r.rows)
            .unwrap();

        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn query_count_by_condition() {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        // 法抗 > 90 的数量
        let count = db
            .query_count(
                "enemies",
                &[Filter::new("法术抗性", FilterOp::Gt, "90")],
            )
            .unwrap();
        assert!(count > 0);

        // 无条件 = 全表
        let total = db.query_count("enemies", &[]).unwrap();
        assert!(total > 1000, "enemies 表应有超过 1000 条记录，实际: {total}");
    }

    #[test]
    fn filtered_no_limit() {
        // 没有 LIMIT（limit=0），name_like 限制范围，不会爆炸
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        let rows = db
            .query_filtered(
                "enemies",
                &["名称".into()],
                &[name_like("enemies", "名称", "源石虫")],
                0,
            ).map(|r| r.rows)
            .unwrap();

        // 源石虫变种有 40+ 条，无 LIMIT 应全部返回
        assert!(rows.len() > 5, "无 LIMIT 应返回 > 5 条，实际: {}", rows.len());
    }

    // ── FilterOp ──

    #[test]
    fn filterop_as_sql() {
        assert_eq!(FilterOp::Eq.as_sql(), "=");
        assert_eq!(FilterOp::Ne.as_sql(), "!=");
        assert_eq!(FilterOp::Gt.as_sql(), ">");
        assert_eq!(FilterOp::Lt.as_sql(), "<");
        assert_eq!(FilterOp::Gte.as_sql(), ">=");
        assert_eq!(FilterOp::Lte.as_sql(), "<=");
        assert_eq!(FilterOp::Like.as_sql(), "LIKE");
    }

    // ── 值类型转换 ──

    #[test]
    fn value_to_string_null() {
        assert_eq!(value_to_string(&rusqlite::types::Value::Null), "");
    }

    #[test]
    fn value_to_string_integer() {
        assert_eq!(value_to_string(&rusqlite::types::Value::Integer(42)), "42");
    }

    #[test]
    fn value_to_string_real() {
        assert_eq!(value_to_string(&rusqlite::types::Value::Real(3.14)), "3.14");
    }

    #[test]
    fn value_to_string_text() {
        assert_eq!(
            value_to_string(&rusqlite::types::Value::Text("hello".into())),
            "hello"
        );
    }

    #[test]
    fn value_to_string_blob() {
        assert_eq!(
            value_to_string(&rusqlite::types::Value::Blob(vec![0, 1, 2])),
            "[BLOB]"
        );
    }

    #[test]
    fn rows_to_maps_empty() {
        let result = rows_to_maps(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn rows_to_maps_single_row() {
        let cols = vec!["名称".to_string(), "级别".to_string()];
        let rows = vec![vec![
            rusqlite::types::Value::Text("源石虫".into()),
            rusqlite::types::Value::Text("普通".into()),
        ]];
        let result = rows_to_maps(&cols, &rows);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["名称"], "源石虫");
        assert_eq!(result[0]["级别"], "普通");
    }
}
