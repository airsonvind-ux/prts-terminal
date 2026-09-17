//! tools::cache —— 缓存区工具（模块 C 第二批：工作记忆读写通道）
//!
//! 缓存区 = 单对话工作记忆（静态桶常驻 + 动态桶会话级），四个工具是 LLM
//! 的操作通道。每次操作既改桶（效果）又在 session log 落账（事实）：
//!
//! - **效果侧**：经 ToolContext 的 cache_stores + conversation_id 通道
//!   访问当前对话分区（批次 1 已建）；写/读命中同时携带 ContextOp 的
//!   ReferenceLoad/Remove 标记（当轮上下文载入/卸下，由中枢统一应用）
//! - **落账侧**：返回 CacheEventMark（Store/Load/Remove），orchestrator
//!   统一转为 session log 的 cache/* 事件（工具不改 log，同 ContextOp 哲学）
//!
//! 全部工具 is_concurrency_safe = false：缓存操作顺序必须确定
//! （先写后读、读后删等时序语义），串行执行。

use std::sync::{Arc, Mutex};

use crate::cache::{CacheEntry, CacheStores};
use crate::session::CacheBucket;
use crate::tools::{CacheEventMark, ContextOp, Tool, ToolContext, ToolError, ToolOutput};

// ═══════════════════════════════════════════════════════
// 辅助：参数解析 + 缓存通道提取
// ═══════════════════════════════════════════════════════

/// 解析 bucket 参数（enum ["static","dynamic"]）。
fn parse_bucket(v: &serde_json::Value) -> Result<CacheBucket, ToolError> {
    match v.as_str() {
        Some("static") => Ok(CacheBucket::Static),
        Some("dynamic") => Ok(CacheBucket::Dynamic),
        _ => Err(ToolError::Parse(
            "参数 bucket 必须是 \"static\" 或 \"dynamic\"".into(),
        )),
    }
}

/// 桶的展示名（summary 用）。
fn bucket_label(bucket: &CacheBucket) -> &'static str {
    match bucket {
        CacheBucket::Static => "static",
        CacheBucket::Dynamic => "dynamic",
    }
}

/// 从 ToolContext 提取缓存区通道；缺失（Mock 场景未注入）时返回错误。
fn cache_channel(ctx: &ToolContext) -> Result<(Arc<Mutex<CacheStores>>, String), ToolError> {
    match (&ctx.cache_stores, &ctx.conversation_id) {
        (Some(stores), Some(conversation_id)) => {
            Ok((Arc::clone(stores), conversation_id.clone()))
        }
        _ => Err(ToolError::Execution("缓存区不可用".into())),
    }
}

// ═══════════════════════════════════════════════════════
// CacheStoreTool —— 写入条目
// ═══════════════════════════════════════════════════════

/// 向缓存区写入一条工作记忆条目。同名 key 覆盖更新；存储后当轮自动载入上下文。
pub struct CacheStoreTool;

impl Tool for CacheStoreTool {
    fn name(&self) -> &str {
        "cache_store"
    }

    fn description(&self) -> &str {
        "向缓存区写入一条工作记忆条目。同名 key 覆盖更新。静态缓存为再学习后的总结，动态缓存为重要数据的备份。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "minLength": 1,
                    "description": "条目名。对象 + 信息维度、可互相区分，如 敌人.源石虫属性；静态总结用「条目名.总结」如 buff.总结"
                },
                "content": {
                    "type": "string",
                    "minLength": 1,
                    "description": "条目内容"
                },
                "bucket": {
                    "type": "string",
                    "enum": ["static", "dynamic"],
                    "description": "static=知识总结（对话内常驻）；dynamic=工作数据（每轮结束自动卸下，桶内保留）"
                },
                "source_entry": {
                    "type": "string",
                    "description": "仅 static 使用，来源知识条目名"
                }
            },
            "required": ["key", "content", "bucket"]
        })
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let key = v["key"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 key 必须是字符串".into()))?;
        let content = v["content"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 content 必须是字符串".into()))?;
        let bucket = parse_bucket(&v["bucket"])?;
        // source_entry 仅 static 使用；dynamic 传入属参数误用，拒收而非静默忽略
        // （宽容会掩盖 LLM 的参数错误，且造成落账与桶内内容分叉）
        let source_entry = v
            .get("source_entry")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        if bucket == CacheBucket::Dynamic && source_entry.is_some() {
            return Err(ToolError::Parse("source_entry 仅 static 使用".into()));
        }

        let (stores, conversation_id) = cache_channel(ctx)?;
        let key = key.to_string();
        let content = content.to_string();

        // 锁 CacheStores 同步段内写桶（std Mutex，不跨 await）
        let mut stores = stores.lock().unwrap();
        let partition = stores.partition(&conversation_id);
        // CacheEntry 字段 pub 直接构造：static 可携带 source_entry，
        // dynamic 恒为 None（与 dynamic() 构造器等价，但不限 source 形态）
        let entry = match bucket {
            CacheBucket::Static => CacheEntry {
                key: key.clone(),
                content: content.clone(),
                source_entry: source_entry.clone(),
            },
            CacheBucket::Dynamic => CacheEntry {
                key: key.clone(),
                content: content.clone(),
                source_entry: None,
            },
        };
        match bucket {
            CacheBucket::Static => partition.store_static(entry),
            CacheBucket::Dynamic => partition.store_dynamic(entry),
        }

        // 载入语义分桶：静态=创建即常驻（ReferenceLoad）；动态=创建后不自动
        // 载入（后续需要时 cache_retrieve 取回，见系统提示词「缓存区使用」）
        let summary = format!("已写入缓存条目「{key}」（{}）", bucket_label(&bucket));
        let mut output = ToolOutput::text(summary);
        if bucket == CacheBucket::Static {
            output.context_ops = vec![ContextOp::ReferenceLoad {
                key: key.clone(),
                content,
            }];
        }
        output.cache_events = vec![CacheEventMark::Store {
            key,
            bucket,
            source_entry,
        }];
        Ok(output)
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════
// CacheListTool —— 条目清单（查看动作不入 log）
// ═══════════════════════════════════════════════════════

/// 查看缓存区条目清单。动态桶列出条目名；静态桶条目名附内容预览。
/// 省略 bucket 时两桶都列出。
pub struct CacheListTool;

impl Tool for CacheListTool {
    fn name(&self) -> &str {
        "cache_list"
    }

    fn description(&self) -> &str {
        "查看缓存区条目清单。动态桶列出条目名；静态桶条目名附内容预览。省略 bucket 时两桶都列出。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "bucket": {
                    "type": "string",
                    "enum": ["static", "dynamic"],
                    "description": "省略=两桶都列出"
                }
            }
        })
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let bucket_opt = match v.get("bucket") {
            None | Some(serde_json::Value::Null) => None,
            Some(b) => Some(parse_bucket(b)?),
        };

        let (stores, conversation_id) = cache_channel(ctx)?;
        let stores = stores.lock().unwrap();
        let summary = match stores.get(&conversation_id) {
            // 无分区（从未写入）→ 空
            None => "缓存区为空".to_string(),
            Some(partition) => {
                let dyn_keys = partition.dynamic_keys();
                let stat = partition.static_previews();
                match bucket_opt {
                    Some(CacheBucket::Dynamic) => dynamic_listing(&dyn_keys),
                    Some(CacheBucket::Static) => static_listing(&stat),
                    // 省略：两段拼接（先动态后静态，中间空行）；两桶都空 → 空
                    None => {
                        if dyn_keys.is_empty() && stat.is_empty() {
                            "缓存区为空".to_string()
                        } else {
                            format!("{}\n\n{}", dynamic_listing(&dyn_keys), static_listing(&stat))
                        }
                    }
                }
            }
        };
        // 查看动作不入 log：context_ops / cache_events 恒为空（text() 默认）
        Ok(ToolOutput::text(summary))
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

/// 动态桶清单段：纯 key 列表；空桶 → "动态桶为空"。
fn dynamic_listing(keys: &[String]) -> String {
    if keys.is_empty() {
        "动态桶为空".to_string()
    } else {
        let lines = keys
            .iter()
            .map(|k| format!("- {k}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("缓存清单（动态桶，共 {} 条）：\n{lines}", keys.len())
    }
}

/// 静态桶清单段：key 附（字符数 + 60 字预览）；空桶 → "静态桶为空"。
fn static_listing(entries: &[(String, usize, String)]) -> String {
    if entries.is_empty() {
        "静态桶为空".to_string()
    } else {
        let lines = entries
            .iter()
            .map(|(k, len, preview)| format!("- {k}（{len} 字）：{preview}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("缓存清单（静态桶，共 {} 条）：\n{lines}", entries.len())
    }
}

// ═══════════════════════════════════════════════════════
// CacheRetrieveTool —— 按 key 读取
// ═══════════════════════════════════════════════════════

/// 按 key 从缓存区取出条目。命中时返回内容并载入上下文；未命中明确告知。
pub struct CacheRetrieveTool;

impl Tool for CacheRetrieveTool {
    fn name(&self) -> &str {
        "cache_retrieve"
    }

    fn description(&self) -> &str {
        "按 key 从缓存区取出条目。命中时返回内容并载入上下文；未命中明确告知。静态桶条目常驻上下文，无需取出；本工具主要用于取回每轮自动卸下的动态条目。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "minLength": 1,
                    "description": "条目名"
                },
                "bucket": {
                    "type": "string",
                    "enum": ["static", "dynamic"],
                    "description": "static=知识总结（常驻上下文，一般无需取出）；dynamic=工作数据（每轮卸下，跨轮需要时取出）"
                }
            },
            "required": ["key", "bucket"]
        })
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let key = v["key"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 key 必须是字符串".into()))?;
        let bucket = parse_bucket(&v["bucket"])?;

        let (stores, conversation_id) = cache_channel(ctx)?;
        let stores = stores.lock().unwrap();
        // 无分区视为 miss（cache miss 语义：LLM 决定重载还是放弃）
        let hit = stores.get(&conversation_id).and_then(|p| match bucket {
            CacheBucket::Static => p.load_static(key),
            CacheBucket::Dynamic => p.load_dynamic(key),
        });

        match hit {
            Some(entry) => {
                let summary = format!("缓存条目「{key}」内容：\n{}", entry.content);
                let mut output = ToolOutput::text(summary);
                output.context_ops = vec![ContextOp::ReferenceLoad {
                    key: key.to_string(),
                    content: entry.content.clone(),
                }];
                output.cache_events = vec![CacheEventMark::Load {
                    key: key.to_string(),
                    bucket,
                    hit: true,
                }];
                Ok(output)
            }
            None => {
                let summary = format!("缓存区中没有条目「{key}」，可用 cache_list 查看现有条目");
                let mut output = ToolOutput::text(summary);
                output.cache_events = vec![CacheEventMark::Load {
                    key: key.to_string(),
                    bucket,
                    hit: false,
                }];
                Ok(output)
            }
        }
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════
// CacheRemoveTool —— 删除条目
// ═══════════════════════════════════════════════════════

/// 从缓存区删除条目（从上下文中同步移除）。静态条目删除后不再常驻。
pub struct CacheRemoveTool;

impl Tool for CacheRemoveTool {
    fn name(&self) -> &str {
        "cache_remove"
    }

    fn description(&self) -> &str {
        "从缓存区删除条目（从上下文中同步移除）。静态条目删除后不再常驻。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "minLength": 1,
                    "description": "条目名"
                },
                "bucket": {
                    "type": "string",
                    "enum": ["static", "dynamic"]
                }
            },
            "required": ["key", "bucket"]
        })
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let key = v["key"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("参数 key 必须是字符串".into()))?;
        let bucket = parse_bucket(&v["bucket"])?;

        let (stores, conversation_id) = cache_channel(ctx)?;
        let key = key.to_string();

        // 先判分区存在再取 &mut（partition 是 get_or_create——只在有分区时
        // 调用，避免删除动作凭空创建空分区）
        let mut stores = stores.lock().unwrap();
        let removed = if stores.get(&conversation_id).is_some() {
            let partition = stores.partition(&conversation_id);
            match bucket {
                CacheBucket::Static => partition.remove_static(&key),
                CacheBucket::Dynamic => partition.remove_dynamic(&key),
            }
        } else {
            None
        };

        match removed {
            Some(_) => {
                let summary = format!("已删除缓存条目「{key}」");
                let mut output = ToolOutput::text(summary);
                output.context_ops = vec![ContextOp::ReferenceRemove {
                    key: key.clone(),
                }];
                output.cache_events = vec![CacheEventMark::Remove {
                    key,
                    bucket,
                    existed: true,
                }];
                Ok(output)
            }
            None => {
                let summary = format!("缓存区中没有条目「{key}」");
                let mut output = ToolOutput::text(summary);
                output.cache_events = vec![CacheEventMark::Remove {
                    key,
                    bucket,
                    existed: false,
                }];
                Ok(output)
            }
        }
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheStores;

    /// 带缓存通道的 ToolContext。
    fn cache_ctx(stores: Arc<Mutex<CacheStores>>, conversation_id: &str) -> ToolContext {
        ToolContext::with_cache(stores, conversation_id.to_string())
    }

    /// 预写分区 helper：向指定对话分区写入条目（工具测试的种子数据）。
    fn seed(
        stores: &Arc<Mutex<CacheStores>>,
        conv: &str,
        bucket: CacheBucket,
        key: &str,
        content: &str,
        source: Option<&str>,
    ) {
        let mut s = stores.lock().unwrap();
        let p = s.partition(conv);
        match bucket {
            CacheBucket::Static => p.store_static(CacheEntry {
                key: key.into(),
                content: content.into(),
                source_entry: source.map(str::to_string),
            }),
            CacheBucket::Dynamic => p.store_dynamic(CacheEntry {
                key: key.into(),
                content: content.into(),
                source_entry: None,
            }),
        }
    }

    // ── cache_store ──

    #[test]
    fn store_dynamic_writes_bucket_without_load_mark() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheStoreTool;

        let out = tool
            .execute(&ctx, r#"{"key":"敌人.源石虫属性","content":"HP 550","bucket":"dynamic"}"#)
            .unwrap();
        assert_eq!(out.summary, "已写入缓存条目「敌人.源石虫属性」（dynamic）");
        // 动态桶：创建后不自动载入（后续需要时 cache_retrieve 取回）
        assert!(out.context_ops.is_empty(), "dynamic store 不应携带载入标记");
        // 事件标记：供中枢落账 cache/store
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Store {
                key: "敌人.源石虫属性".into(),
                bucket: CacheBucket::Dynamic,
                source_entry: None,
            }]
        );
        // 桶内容已写入（对话分区隔离：读同一 conv）
        let s = stores.lock().unwrap();
        let p = s.get("conv-1").unwrap();
        assert_eq!(p.load_dynamic("敌人.源石虫属性").unwrap().content, "HP 550");
    }

    #[test]
    fn store_static_carries_load_mark() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheStoreTool;

        let out = tool
            .execute(
                &ctx,
                r#"{"key":"buff.总结","content":"同类取最高","bucket":"static","source_entry":"Buff机制基础"}"#,
            )
            .unwrap();
        assert_eq!(out.summary, "已写入缓存条目「buff.总结」（static）");
        // 静态桶：创建即载入并常驻（ReferenceLoad 由中枢统一应用）
        assert_eq!(
            out.context_ops,
            vec![ContextOp::ReferenceLoad {
                key: "buff.总结".into(),
                content: "同类取最高".into(),
            }]
        );
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Store {
                key: "buff.总结".into(),
                bucket: CacheBucket::Static,
                source_entry: Some("Buff机制基础".into()),
            }]
        );
    }

    #[test]
    fn store_dynamic_with_source_entry_rejected() {
        // source_entry 仅 static 使用：dynamic 传入属参数误用，拒收而非静默忽略
        // （宽容会掩盖 LLM 的参数错误，且造成落账与桶内内容分叉）
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheStoreTool;

        let err = tool
            .execute(
                &ctx,
                r#"{"key":"k","content":"v","bucket":"dynamic","source_entry":"某条目"}"#,
            )
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("source_entry 仅 static 使用")),
            "got: {err}"
        );
        // 拒收不产生任何写入
        let s = stores.lock().unwrap();
        assert!(s.get("conv-1").is_none(), "拒收路径不应创建分区");
    }

    #[test]
    fn store_static_carries_source_entry() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheStoreTool;

        let out = tool
            .execute(
                &ctx,
                r#"{"key":"buff.总结","content":"同类取最高","bucket":"static","source_entry":"Buff机制基础"}"#,
            )
            .unwrap();
        assert_eq!(out.summary, "已写入缓存条目「buff.总结」（static）");
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Store {
                key: "buff.总结".into(),
                bucket: CacheBucket::Static,
                source_entry: Some("Buff机制基础".into()),
            }]
        );
        let s = stores.lock().unwrap();
        let e = s.get("conv-1").unwrap().load_static("buff.总结").unwrap();
        assert_eq!(e.source_entry.as_deref(), Some("Buff机制基础"));
    }

    #[test]
    fn store_overwrites_same_key() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheStoreTool;

        tool.execute(&ctx, r#"{"key":"buff.总结","content":"v1","bucket":"static"}"#)
            .unwrap();
        let out = tool
            .execute(&ctx, r#"{"key":"buff.总结","content":"v2","bucket":"static"}"#)
            .unwrap();
        assert_eq!(out.summary, "已写入缓存条目「buff.总结」（static）");
        let s = stores.lock().unwrap();
        let p = s.get("conv-1").unwrap();
        assert_eq!(p.load_static("buff.总结").unwrap().content, "v2");
        assert_eq!(p.static_len(), 1);
    }

    // ── cache_retrieve ──

    #[test]
    fn retrieve_hit_returns_content_and_loads_reference() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        seed(
            &stores,
            "conv-1",
            CacheBucket::Static,
            "buff.总结",
            "同类取最高",
            Some("Buff机制基础"),
        );
        let ctx = cache_ctx(stores, "conv-1");
        let tool = CacheRetrieveTool;

        let out = tool
            .execute(&ctx, r#"{"key":"buff.总结","bucket":"static"}"#)
            .unwrap();
        assert_eq!(out.summary, "缓存条目「buff.总结」内容：\n同类取最高");
        assert_eq!(
            out.context_ops,
            vec![ContextOp::ReferenceLoad {
                key: "buff.总结".into(),
                content: "同类取最高".into(),
            }]
        );
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Load {
                key: "buff.总结".into(),
                bucket: CacheBucket::Static,
                hit: true,
            }]
        );
    }

    #[test]
    fn retrieve_miss_reports_absent_without_load() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        // 有分区但 key 在另一桶 → miss
        seed(&stores, "conv-1", CacheBucket::Dynamic, "k", "v", None);
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheRetrieveTool;

        let out = tool
            .execute(&ctx, r#"{"key":"k","bucket":"static"}"#)
            .unwrap();
        assert_eq!(out.summary, "缓存区中没有条目「k」，可用 cache_list 查看现有条目");
        assert!(out.context_ops.is_empty(), "未命中不载入 Reference");
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Load {
                key: "k".into(),
                bucket: CacheBucket::Static,
                hit: false,
            }]
        );

        // 无分区 → miss（不创建分区）
        let ctx2 = cache_ctx(stores.clone(), "conv-不存在");
        let out = tool
            .execute(&ctx2, r#"{"key":"k","bucket":"dynamic"}"#)
            .unwrap();
        assert!(out.summary.contains("缓存区中没有条目「k」"));
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Load {
                key: "k".into(),
                bucket: CacheBucket::Dynamic,
                hit: false,
            }]
        );
        assert!(stores.lock().unwrap().get("conv-不存在").is_none());
    }

    // ── cache_remove ──

    #[test]
    fn remove_existing_returns_removed_mark_and_reference_remove() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        seed(&stores, "conv-1", CacheBucket::Dynamic, "地图", "二维结构", None);
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheRemoveTool;

        let out = tool
            .execute(&ctx, r#"{"key":"地图","bucket":"dynamic"}"#)
            .unwrap();
        assert_eq!(out.summary, "已删除缓存条目「地图」");
        assert_eq!(
            out.context_ops,
            vec![ContextOp::ReferenceRemove { key: "地图".into() }]
        );
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Remove {
                key: "地图".into(),
                bucket: CacheBucket::Dynamic,
                existed: true,
            }]
        );
        // 桶内已删除
        let s = stores.lock().unwrap();
        assert!(s.get("conv-1").unwrap().load_dynamic("地图").is_none());
    }

    #[test]
    fn remove_missing_reports_absent_without_reference_remove() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        // 无分区 → existed=false，且不创建空分区
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheRemoveTool;

        let out = tool
            .execute(&ctx, r#"{"key":"k","bucket":"static"}"#)
            .unwrap();
        assert_eq!(out.summary, "缓存区中没有条目「k」");
        assert!(out.context_ops.is_empty(), "未删除不产生 ReferenceRemove");
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Remove {
                key: "k".into(),
                bucket: CacheBucket::Static,
                existed: false,
            }]
        );
        assert!(stores.lock().unwrap().get("conv-1").is_none());

        // 有分区但 key 不存在 → existed=false
        seed(&stores, "conv-2", CacheBucket::Dynamic, "别的", "v", None);
        let ctx2 = cache_ctx(stores, "conv-2");
        let out = tool
            .execute(&ctx2, r#"{"key":"k","bucket":"dynamic"}"#)
            .unwrap();
        assert!(out.summary.contains("缓存区中没有条目「k」"));
        assert_eq!(
            out.cache_events,
            vec![CacheEventMark::Remove {
                key: "k".into(),
                bucket: CacheBucket::Dynamic,
                existed: false,
            }]
        );
    }

    // ── cache_list ──

    #[test]
    fn list_shows_both_buckets_when_omitted() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        seed(&stores, "conv-1", CacheBucket::Dynamic, "地图", "二维结构", None);
        seed(
            &stores,
            "conv-1",
            CacheBucket::Static,
            "buff.总结",
            "同类取最高",
            Some("Buff机制基础"),
        );
        let ctx = cache_ctx(stores, "conv-1");
        let tool = CacheListTool;

        let out = tool.execute(&ctx, "{}").unwrap();
        assert_eq!(
            out.summary,
            "缓存清单（动态桶，共 1 条）：\n- 地图\n\n缓存清单（静态桶，共 1 条）：\n- buff.总结（5 字）：同类取最高"
        );
        // 查看动作不入 log
        assert!(out.context_ops.is_empty());
        assert!(out.cache_events.is_empty());
    }

    #[test]
    fn list_single_bucket_and_empty_states() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        seed(&stores, "conv-1", CacheBucket::Dynamic, "地图", "二维结构", None);
        let ctx = cache_ctx(stores.clone(), "conv-1");
        let tool = CacheListTool;

        // 指定 dynamic：纯 key 清单
        let out = tool.execute(&ctx, r#"{"bucket":"dynamic"}"#).unwrap();
        assert_eq!(out.summary, "缓存清单（动态桶，共 1 条）：\n- 地图");
        // 指定 static（空桶）
        let out = tool.execute(&ctx, r#"{"bucket":"static"}"#).unwrap();
        assert_eq!(out.summary, "静态桶为空");

        // 省略：动态有、静态空 → 两段拼接（空桶也给出提示）
        let out = tool.execute(&ctx, "{}").unwrap();
        assert_eq!(out.summary, "缓存清单（动态桶，共 1 条）：\n- 地图\n\n静态桶为空");

        // 有分区但两桶都空 → 缓存区为空
        {
            let mut s = stores.lock().unwrap();
            s.partition("conv-空");
        }
        let ctx2 = cache_ctx(stores.clone(), "conv-空");
        let out = tool.execute(&ctx2, "{}").unwrap();
        assert_eq!(out.summary, "缓存区为空");

        // 无分区 → 缓存区为空
        let ctx3 = cache_ctx(stores, "conv-不存在");
        let out = tool.execute(&ctx3, "{}").unwrap();
        assert_eq!(out.summary, "缓存区为空");
    }

    #[test]
    fn list_dynamic_keys_sorted_pure_keys() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        seed(&stores, "conv-1", CacheBucket::Dynamic, "b", "v", None);
        seed(&stores, "conv-1", CacheBucket::Dynamic, "a", "v", None);
        let ctx = cache_ctx(stores, "conv-1");
        let tool = CacheListTool;

        let out = tool.execute(&ctx, r#"{"bucket":"dynamic"}"#).unwrap();
        // 动态桶只列 key（无内容/预览），按 key 排序
        assert_eq!(out.summary, "缓存清单（动态桶，共 2 条）：\n- a\n- b");
    }

    #[test]
    fn list_static_preview_truncates_at_60_chars() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        let long = "长".repeat(70);
        seed(&stores, "conv-1", CacheBucket::Static, "b", &long, Some("条目"));
        seed(&stores, "conv-1", CacheBucket::Static, "a", "短内容", None);
        let ctx = cache_ctx(stores, "conv-1");
        let tool = CacheListTool;

        let out = tool.execute(&ctx, r#"{"bucket":"static"}"#).unwrap();
        assert_eq!(
            out.summary,
            format!(
                "缓存清单（静态桶，共 2 条）：\n- a（{} 字）：短内容\n- b（{} 字）：{}…",
                3,
                70,
                "长".repeat(60)
            )
        );
    }

    // ── 通道缺失 / 参数校验 ──

    #[test]
    fn tools_without_cache_channel_error() {
        // Mock 场景（ToolContext::new()）未注入缓存通道 → 明确报错。
        // 各工具传合法参数（参数校验在通道检查之前，非法参数会先报 Parse）
        let ctx = ToolContext::new();
        let cases: Vec<(&dyn Tool, &str)> = vec![
            (&CacheStoreTool, r#"{"key":"k","content":"v","bucket":"static"}"#),
            (&CacheListTool, "{}"),
            (&CacheRetrieveTool, r#"{"key":"k","bucket":"dynamic"}"#),
            (&CacheRemoveTool, r#"{"key":"k","bucket":"dynamic"}"#),
        ];
        for (tool, params) in cases {
            let err = tool.execute(&ctx, params).unwrap_err();
            assert!(
                matches!(&err, ToolError::Execution(m) if m == "缓存区不可用"),
                "got: {err}"
            );
        }
    }

    #[test]
    fn invalid_bucket_is_parse_error() {
        let stores = Arc::new(Mutex::new(CacheStores::new()));
        let ctx = cache_ctx(stores, "conv-1");
        let err = CacheStoreTool
            .execute(&ctx, r#"{"key":"k","content":"v","bucket":"bogus"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(m) if m.contains("bucket")),
            "got: {err}"
        );
    }
}
