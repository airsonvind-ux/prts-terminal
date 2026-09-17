//! tools::memory —— 记忆工具（批 2：两把）
//!
//! - [`MemoryExperienceTool`]（`memory_experience`）：经历记忆**只读**通道
//!   （检索 / 读细节）。经历由后台管线维护，本工具无写入口——单写者边界在
//!   代码层强制（无写 action；显式写意图报错）。
//! - [`MemoryGrowthTool`]（`memory_growth`）：成长页**读写**通道
//!   （读页 / 建页 / 改页 / 删页）。「关于博士」与专题页同构。
//!
//! 写入一律 `is_concurrency_safe = false`。成长写成功后经提醒数据源
//! （`Notices`）追加一条记忆事件（记忆变化属于"资源变化"，进通知家族）。

use std::sync::{Arc, Mutex};

use crate::memory::prompt::{ymd_hms_local, ymd_local};
use crate::memory::schema::PROFILE_NAME;
use crate::memory::store::MemoryStore;
use crate::notices::Notices;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// search 缺省返回上限。
const DEFAULT_SEARCH_LIMIT: usize = 20;
/// search 硬上限（防超大载荷）。
const MAX_SEARCH_LIMIT: usize = 50;

// ═══════════════════════════════════════════════════════
// memory_experience —— 经历（只读）
// ═══════════════════════════════════════════════════════

pub struct MemoryExperienceTool {
    store: Arc<Mutex<MemoryStore>>,
}

impl MemoryExperienceTool {
    pub fn new(store: Arc<Mutex<MemoryStore>>) -> Self {
        Self { store }
    }
}

impl Tool for MemoryExperienceTool {
    fn name(&self) -> &str {
        "memory_experience"
    }

    fn description(&self) -> &str {
        "经历记忆（只读）：过去工作的记录——更早的部分用这里；近况与长期梗概随对话呈现，对话内的更新会随提醒带出。默认不查；三种时刻必查：涉及过去（\"上次\"\"之前\"）；说\"不记得\"之前——先查再开口；接手旧工作前——先看上次到哪。经历由后台写入、你改不了；发现记错，把更正写进成长页或提醒博士。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search", "read"],
                    "description": "search=检索经历；read=按 id 或 title 读取细节"
                },
                "query": { "type": "string", "description": "search：关键词（大小写不敏感子串）" },
                "from": { "type": "integer", "description": "search：起始时间（unix 毫秒，闭区间）" },
                "to": { "type": "integer", "description": "search：结束时间（unix 毫秒，闭区间）" },
                "limit": { "type": "integer", "description": "search：返回条数上限（默认 20，最大 50）" },
                "id": { "type": "string", "description": "read：经历条目 id（来自 search 结果）" },
                "title": { "type": "string", "description": "read：条目标题（id 缺省时的唯一匹配回退）" }
            },
            "required": ["action"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        match v.get("action").and_then(|x| x.as_str()) {
            Some("search") => {
                let q = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
                ToolDisplay::info(if q.is_empty() {
                    "检索经历".to_string()
                } else {
                    format!("检索经历：{q}")
                })
            }
            Some("read") => ToolDisplay::info("读取经历细节"),
            _ => ToolDisplay::default(),
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let action = v
            .get("action")
            .and_then(|x| x.as_str())
            .ok_or_else(|| ToolError::Parse("参数 action 必须是字符串".into()))?;

        let store = self
            .store
            .lock()
            .map_err(|e| ToolError::Execution(format!("记忆存储锁获取失败: {e}")))?;

        match action {
            "search" => {
                let query = v.get("query").and_then(|x| x.as_str());
                let from = v.get("from").and_then(|x| x.as_u64());
                let to = v.get("to").and_then(|x| x.as_u64());
                let limit = v
                    .get("limit")
                    .and_then(|x| x.as_u64())
                    .map(|n| (n as usize).clamp(1, MAX_SEARCH_LIMIT))
                    .unwrap_or(DEFAULT_SEARCH_LIMIT);
                let hits = store.experience_search(query, from, to, limit);
                if hits.is_empty() {
                    return Ok(ToolOutput::text("没有匹配的经历条目"));
                }
                let lines: Vec<String> = hits
                    .iter()
                    .map(|e| {
                        format!(
                            "{} | {} | {}：{}",
                            e.id,
                            ymd_local(e.ts),
                            e.title.trim(),
                            e.summary.trim()
                        )
                    })
                    .collect();
                let summary = format!("经历检索结果（共 {} 条）：\n{}", hits.len(), lines.join("\n"));
                Ok(ToolOutput::json(
                    serde_json::json!({
                        "count": hits.len(),
                        "items": hits.iter().map(|e| serde_json::json!({
                            "id": e.id, "date": ymd_local(e.ts), "title": e.title, "summary": e.summary,
                        })).collect::<Vec<_>>()
                    }),
                    summary,
                ))
            }
            "read" => {
                let id = v.get("id").and_then(|x| x.as_str()).filter(|s| !s.trim().is_empty());
                let title = v
                    .get("title")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.trim().is_empty());
                // id 优先；id 缺省 → 标题唯一匹配回退（让「近期」行一步直达细节）
                let entry = match (id, title) {
                    (Some(id), _) => store
                        .experience_get(id)
                        .ok_or_else(|| ToolError::Execution(format!("没有经历条目「{id}」")))?,
                    (None, Some(title)) => {
                        let matches = store.experience_by_title(title);
                        match matches.len() {
                            0 => {
                                return Err(ToolError::Execution(format!(
                                    "没有标题为「{title}」的经历条目"
                                )))
                            }
                            1 => matches[0],
                            n => {
                                let ids: Vec<String> =
                                    matches.iter().map(|e| e.id.clone()).collect();
                                return Err(ToolError::Execution(format!(
                                    "标题「{title}」匹配 {n} 条经历，请改用 id：{}",
                                    ids.join("、")
                                )));
                            }
                        }
                    }
                    (None, None) => {
                        return Err(ToolError::Parse("read 需要参数 id 或 title".into()))
                    }
                };
                let summary = format!(
                    "{}\n{} {}：{}\n来源：对话 {} · 轮次 {}-{}",
                    entry.id,
                    ymd_local(entry.ts),
                    entry.title.trim(),
                    entry.summary.trim(),
                    entry.conversation_id,
                    entry.turn_from,
                    entry.turn_to
                );
                Ok(ToolOutput::text(summary))
            }
            // 单写者边界：经历层只由后台管线写
            "write" | "add" | "create" | "update" | "delete" | "remove" => Err(
                ToolError::Execution("经历层只由管线写：memory_experience 是只读工具".into()),
            ),
            other => Err(ToolError::Parse(format!(
                "不支持的 action「{other}」，可用：search / read"
            ))),
        }
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════
// memory_growth —— 成长（读写）
// ═══════════════════════════════════════════════════════

pub struct MemoryGrowthTool {
    store: Arc<Mutex<MemoryStore>>,
    ledger: Arc<Mutex<Notices>>,
}

impl MemoryGrowthTool {
    pub fn new(store: Arc<Mutex<MemoryStore>>, ledger: Arc<Mutex<Notices>>) -> Self {
        Self { store, ledger }
    }
}

impl Tool for MemoryGrowthTool {
    fn name(&self) -> &str {
        "memory_growth"
    }

    fn description(&self) -> &str {
        "成长记忆（读写）：维护成长页——你对博士与共同工作的理解（「关于博士」常驻）。宁少勿滥——被纠正、\"记住\"、关键结论当刻写；零散认识收尾合并写；判据：会改变你未来的判断或选择吗？不会就是复述，不写。读—并—写：新的／被纠正的覆盖旧的、留博士用词、只动相关内容不整篇重写；重要改判留轨迹（\"原以为 X，现为 Y，因为 Z\"）；补充进现有页、独立才新建；碎页并、杂页拆、上限约 12；存疑标\"待验证\"。格式：标题＋一句话简述＋正文；时间与来源由系统记录，不用你写；正文不必带日期（时间本身是判断的一部分时除外）。删除仅限：被取代／终结无参照／复核不成立；想不起用途不算——博士的要求除外。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "create", "update", "delete"],
                    "description": "read=读页；create=建页；update=改页；delete=删页"
                },
                "name": { "type": "string", "description": "页名。「关于博士」为画像与协作约定页；其余为专题页名" },
                "brief": { "type": "string", "description": "一句话简述（进主题目录）" },
                "body": { "type": "string", "description": "正文" },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "标签（可选）"
                }
            },
            "required": ["action", "name"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
        match v.get("action").and_then(|x| x.as_str()) {
            Some("read") if !name.is_empty() => ToolDisplay::info(format!("读取记忆页：{name}")),
            Some("create") if !name.is_empty() => ToolDisplay::info(format!("新建记忆页：{name}")),
            Some("update") if !name.is_empty() => ToolDisplay::info(format!("更新记忆页：{name}")),
            Some("delete") if !name.is_empty() => ToolDisplay::info(format!("删除记忆页：{name}")),
            _ => ToolDisplay::default(),
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let action = v
            .get("action")
            .and_then(|x| x.as_str())
            .ok_or_else(|| ToolError::Parse("参数 action 必须是字符串".into()))?;
        let name = v
            .get("name")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::Parse("参数 name 不能为空".into()))?;

        let mut store = self
            .store
            .lock()
            .map_err(|e| ToolError::Execution(format!("记忆存储锁获取失败: {e}")))?;

        // 写路径产出 reminder 摘要（store 锁释放后再入台账，避免嵌套持锁）
        let mut reminder: Option<String> = None;
        let result = match action {
            "read" => {
                let page = store
                    .growth_get(name)
                    .ok_or_else(|| ToolError::Execution(format!("成长页「{name}」不存在")))?;
                let body = if page.body.trim().is_empty() {
                    "（尚无内容）".to_string()
                } else {
                    page.body.trim().to_string()
                };
                // 头行：brief 为空时省略括号（不出现「（）」）
                let brief = page.brief.trim();
                let head = if brief.is_empty() {
                    page.name.clone()
                } else {
                    format!("{}（{brief}）", page.name)
                };
                // 元数据行：本地日期＋时分秒（系统记录的 createdAt/updatedAt/source）
                let meta = format!(
                    "创建 {} · 更新 {} · 来源 {}",
                    ymd_hms_local(page.created_at),
                    ymd_hms_local(page.updated_at),
                    page.source
                );
                Ok(ToolOutput::text(format!("{head}\n{meta}\n{body}")))
            }
            "create" => {
                let body = v
                    .get("body")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| ToolError::Parse("create 需要参数 body".into()))?;
                let brief = v.get("brief").and_then(|x| x.as_str()).unwrap_or("");
                let tags = parse_tags(&v)?;
                store
                    .growth_create(name, brief, body, tags, "手动")
                    .map_err(ToolError::Execution)?;
                let b = brief.trim();
                reminder = Some(if b.is_empty() {
                    format!("新增成长页「{name}」")
                } else {
                    format!("新增成长页「{name}：{b}」")
                });
                Ok(ToolOutput::text(format!("已新建成长页「{name}」")))
            }
            "update" => {
                let body = v.get("body").and_then(|x| x.as_str());
                let brief = v.get("brief").and_then(|x| x.as_str());
                let tags = match v.get("tags") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(_) => Some(parse_tags(&v)?),
                };
                if body.is_none() && brief.is_none() && tags.is_none() {
                    return Err(ToolError::Parse("update 至少需要 body / brief / tags 之一".into()));
                }
                store
                    .growth_update(name, body, brief, tags)
                    .map_err(ToolError::Execution)?;
                if name == PROFILE_NAME {
                    // 「关于博士」：提醒带新版全文（快照冻结不再更新，靠提醒同步）
                    let full = store
                        .growth_get(name)
                        .map(|p| p.body.trim().to_string())
                        .unwrap_or_default();
                    reminder = Some(format!("更新成长页「{name}」\n{full}"));
                } else {
                    let b = store
                        .growth_get(name)
                        .map(|p| p.brief.trim().to_string())
                        .unwrap_or_default();
                    reminder = Some(if b.is_empty() {
                        format!("更新成长页「{name}」")
                    } else {
                        format!("更新成长页「{name}：{b}」")
                    });
                }
                Ok(ToolOutput::text(format!("已更新成长页「{name}」")))
            }
            "delete" => {
                store.growth_delete(name).map_err(ToolError::Execution)?;
                reminder = Some(format!("删除成长页「{name}」"));
                Ok(ToolOutput::text(format!("已删除成长页「{name}」")))
            }
            other => Err(ToolError::Parse(format!(
                "不支持的 action「{other}」，可用：read / create / update / delete"
            ))),
        };
        drop(store);

        // 成长写 → MemoryFacts（reminder；台账失败仅告警不影响工具结果）
        if let Some(detail) = reminder {
            match self.ledger.lock() {
                Ok(mut l) => l.note_memory_change("记忆", &detail),
                Err(e) => eprintln!("[memory] 台账锁获取失败（记忆变化未通知）: {e}"),
            }
        }
        result
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

/// 解析可选 tags 数组（缺失/非数组 → 空 Vec；元素非字符串 → Parse 错）。
fn parse_tags(v: &serde_json::Value) -> Result<Vec<String>, ToolError> {
    match v.get("tags") {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(arr) => {
            let items = arr
                .as_array()
                .ok_or_else(|| ToolError::Parse("参数 tags 必须是字符串数组".into()))?;
            items
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ToolError::Parse("参数 tags 的元素必须是字符串".into()))
                })
                .collect()
        }
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notices::Notices;

    fn temp_store(tag: &str) -> (Arc<Mutex<MemoryStore>>, Arc<Mutex<Notices>>, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-mem-tool-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = MemoryStore::load(&dir).unwrap();
        let ledger = Notices::load(&dir);
        (
            Arc::new(Mutex::new(store)),
            Arc::new(Mutex::new(ledger)),
            dir,
        )
    }

    #[test]
    fn growth_read_create_update_delete_flow_and_reminder() {
        let (store, ledger, dir) = temp_store("growth");
        let tool = MemoryGrowthTool::new(store.clone(), ledger.clone());
        let ctx = ToolContext::new();

        let out = tool.execute(&ctx, r#"{"action":"read","name":"关于博士"}"#).unwrap();
        assert!(out.summary.contains("（尚无内容）"));

        let out = tool
            .execute(
                &ctx,
                r##"{"action":"create","name":"伤害计算","brief":"暴击与减伤","body":"正文内容","tags":["数值"]}"##,
            )
            .unwrap();
        assert!(out.summary.contains("已新建"));

        let err = tool
            .execute(&ctx, r#"{"action":"create","name":"伤害计算","body":"x"}"#)
            .unwrap_err();
        assert!(matches!(&err, ToolError::Execution(m) if m.contains("已存在")), "got: {err}");

        tool.execute(&ctx, r#"{"action":"update","name":"伤害计算","body":"新正文内容"}"#)
            .unwrap();
        assert_eq!(store.lock().unwrap().growth_get("伤害计算").unwrap().body, "新正文内容");

        let err = tool
            .execute(&ctx, r#"{"action":"update","name":"不存在","body":"x"}"#)
            .unwrap_err();
        assert!(matches!(&err, ToolError::Execution(m) if m.contains("不存在")), "got: {err}");

        // 成长写已入提醒事件（作用域内取锁，断言后立即释放——防后续重锁死锁）
        {
            let guard = ledger.lock().unwrap();
            let events = guard.events();
            assert_eq!(events.len(), 2, "create + update 各一条（删除尚未发生）");
            assert_eq!(events[1].title, "记忆");
            assert_eq!(events[1].detail, "更新成长页「伤害计算：暴击与减伤」");
        }

        tool.execute(&ctx, r#"{"action":"delete","name":"伤害计算"}"#).unwrap();
        assert!(store.lock().unwrap().growth_get("伤害计算").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn experience_tool_search_read_by_id_and_title_and_boundary() {
        let (store, ledger, dir) = temp_store("exp");
        {
            let mut s = store.lock().unwrap();
            s.inject_experience_for_test(crate::memory::schema::ExperienceEntry {
                id: "e1".into(),
                ts: 1_700_000_000_000,
                conversation_id: "c1".into(),
                line_from: String::new(),
                turn_from: 0,
                turn_to: 1,
                level: "short".into(),
                title: "伤害公式".into(),
                summary: "确认暴击与减伤相乘".into(),
                tags: vec![],
                revision_count: 0,
                absorbed_in: None,
            });
        }
        let tool = MemoryExperienceTool::new(store.clone());
        let ctx = ToolContext::new();

        let out = tool.execute(&ctx, r#"{"action":"search","query":"暴击"}"#).unwrap();
        assert!(out.summary.contains("e1"));
        assert!(out
            .summary
            .contains(&crate::memory::prompt::ymd_local(1_700_000_000_000)));

        // read by id（输出＝id＋本地日期 标题：状态句＋来源行）
        let out = tool.execute(&ctx, r#"{"action":"read","id":"e1"}"#).unwrap();
        assert!(out.summary.contains("确认暴击与减伤相乘"));
        assert!(out.summary.contains("来源：对话 c1 · 轮次 0-1"), "got: {}", out.summary);
        // read by unique title fallback
        let out = tool.execute(&ctx, r#"{"action":"read","title":"伤害公式"}"#).unwrap();
        assert!(out.summary.contains("确认暴击与减伤相乘"));
        // read by unknown title
        let err = tool.execute(&ctx, r#"{"action":"read","title":"nope"}"#).unwrap_err();
        assert!(matches!(&err, ToolError::Execution(m) if m.contains("没有标题")), "got: {err}");

        let out = tool.execute(&ctx, r#"{"action":"search","query":"不存在xyz"}"#).unwrap();
        assert!(out.summary.contains("没有匹配"));

        for act in ["write", "add", "delete", "update"] {
            let err = tool.execute(&ctx, &format!(r#"{{"action":"{act}","id":"e1"}}"#)).unwrap_err();
            assert!(
                matches!(&err, ToolError::Execution(m) if m.contains("只由管线写")),
                "action {act}: got {err}"
            );
        }
        let err = tool.execute(&ctx, r#"{"action":"frobnicate"}"#).unwrap_err();
        assert!(matches!(err, ToolError::Parse(_)));
        let _ = std::fs::remove_dir_all(&dir);
        drop(ledger);
    }

    #[test]
    fn growth_read_includes_metadata_line() {
        let (store, ledger, dir) = temp_store("meta");
        let tool = MemoryGrowthTool::new(store.clone(), ledger);
        let ctx = ToolContext::new();
        tool.execute(
            &ctx,
            r##"{"action":"create","name":"伤害计算","brief":"暴击与减伤","body":"正文内容"}"##,
        )
        .unwrap();
        let out = tool
            .execute(&ctx, r#"{"action":"read","name":"伤害计算"}"#)
            .unwrap();
        // 头行（含 brief）+ 元数据行 + 正文
        assert!(out.summary.starts_with("伤害计算（暴击与减伤）\n创建 "), "got: {}", out.summary);
        assert!(out.summary.contains(" · 更新 "), "got: {}", out.summary);
        assert!(out.summary.contains(" · 来源 手动\n"), "got: {}", out.summary);
        assert!(out.summary.ends_with("\n正文内容"), "got: {}", out.summary);
        // 空 brief：头行不带括号
        tool.execute(&ctx, r#"{"action":"create","name":"无简述","body":"x"}"#)
            .unwrap();
        let out = tool
            .execute(&ctx, r#"{"action":"read","name":"无简述"}"#)
            .unwrap();
        assert!(out.summary.starts_with("无简述\n创建 "), "got: {}", out.summary);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tools_schema_declares_action_enum() {
        let (store, ledger, dir) = temp_store("schema");
        let exp = MemoryExperienceTool::new(store.clone());
        let gro = MemoryGrowthTool::new(store.clone(), ledger);
        assert_eq!(exp.name(), "memory_experience");
        assert_eq!(gro.name(), "memory_growth");
        assert!(!exp.is_concurrency_safe());
        assert!(!gro.is_concurrency_safe());
        assert_eq!(exp.parameters()["properties"]["action"]["enum"][0], "search");
        assert_eq!(gro.parameters()["properties"]["action"]["enum"][0], "read");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
