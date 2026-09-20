//! orchestrator::memory —— 经历记忆管线（Phase 4 任务二 · 批 2）
//!
//! 后台旁路：对某对话"水位线之后的对话增量"做一次**单次、无工具**的静默 LLM
//! 提炼（与标题生成 `title.rs` 同款范式），产出**操作清单**并写入经历层；
//! 随后必要时做一次**滚动压缩**（短期 → 长期，读—并—写 `longterm.md`）。
//!
//! ## 静默语义
//!
//! 与主对话共享 provider/模型（命令层传入已路由 client），但：无工具、
//! 非流式、不 emit IPC、不写 session log（session.rs 零改动）。请求级覆盖：
//! `thinking_level="off"`、`max_tokens` 压小。**注意**：`LlmConfig` 无 temperature
//! 字段（适配器不透出）——"低温"以"无工具 + 强约束输出 + thinking off"近似，
//! 待 LlmConfig 扩展后补 temperature。
//!
//! ## 提炼输出＝操作清单
//!
//! `[{"target": 编号|null, "title", "summary", "tags"}]`（无正文层）
//! - `target=编号`：指向材料中**可重写**条目＝重写（**覆盖**标题/状态句/标签、计数 +1、延长时间）；
//! - 已封存（revisionCount ≥ 2）不可重写，同题新进展另起新条目。
//! - `target=null`：新建；判别＝同线未完结且是进展→续写，同题已完结或不同目标→新建，拿不准→新建。
//! - 范围限未吸收；跨对话同线可续写（编号由本管线按当前未吸收集合给出）。
//!
//! ## 失败链
//!
//! 预处理（剥围栏/思考块）→ 校验 → **修复一轮** → 整轮报废（水位不前移、记错、
//! 冷却）；个别坏件丢弃、好的入账、水位照常推进；无效编号修复失败→降级新建。
//! 续写写入前以**指纹**校验目标未被改动，冲突降级为新建（保内容、防丢更新）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::debug;
use crate::notices::Notices;
use crate::llm::{ChatMessage, LlmClient, LlmConfig};
use crate::memory::schema::now_ms;
use crate::memory::store::{Fingerprint, MemoryOp, MemoryStore};
use crate::turn_registry::AbortSignal;

// ── 参数常量（成本纪律：小模型 / 增量 / 小输出）──

/// 攒够多少轮才提炼（轮末/切换/补偿触发时，增量不足此数直接 no-op）。
pub const INCREMENTAL_TURNS: usize = 6;
/// 提炼材料最大字符数（防超长对话）。
const MAX_MATERIAL_CHARS: usize = 24_000;
/// 提炼输出 token 上限。
const EXTRACT_MAX_TOKENS: u32 = 1_500;
/// 单次提炼超时。
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(90);
/// 字段长度上限（校验用；提示词目标 title ≤20 / summary ≤150，此处为宽松硬闸）。
const MAX_TITLE_CHARS: usize = 100;
const MAX_SUMMARY_CHARS: usize = 500;
/// 压缩触发条数 / 每次取最老条数（"到 10 压 5"）。
pub const COMPRESS_AFTER: usize = 10;
pub const COMPRESS_TAKE: usize = 5;
/// 压缩输入上限 / 输出 token 上限 / 超时。
const MAX_COMPRESS_INPUT_CHARS: usize = 24_000;
/// 压缩输出 token 预算：与字数上限对齐（目标 400 字 ≈ 400~550 tokens）。
const COMPRESS_MAX_TOKENS: u32 = 600;
const COMPRESS_TIMEOUT: Duration = Duration::from_secs(90);
/// 长期记忆字数：提示词目标 [LONGTERM_TARGET_CHARS]，代码硬闸 [MAX_LONGTERM_CHARS]。
/// 硬闸超限 → 先让模型缩短重写一次，仍超限则拒收（保留旧版，条目不吸收）。
const LONGTERM_TARGET_CHARS: usize = 400;
pub const MAX_LONGTERM_CHARS: usize = 600;
/// 整轮报废后的冷却时长（下次不再立刻重试）。
const FAILURE_COOLDOWN_MS: u64 = 5 * 60 * 1000;

/// 管线输入（命令层组装）。
pub struct PipelineInput {
    pub conversation_id: String,
    /// 水位线之后的轮行原文（`SessionStore` 的 jsonl 行）
    pub delta_turns: Vec<String>,
    /// 提炼前水位（turnFrom）
    pub turn_from: u64,
    /// 提炼后水位（turnTo）
    pub new_watermark: u64,
}

/// 管线结果（命令层日志/诊断；批 3 任务四总览消费）。
#[derive(Debug, Default)]
#[allow(dead_code)] // 字段供日志/统计/任务四消费（当前命令层仅取 Ok/Err）
pub struct PipelineOutcome {
    /// 是否真正执行了提炼（false = 跳过）
    pub ran: bool,
    pub created: usize,
    pub rewritten: usize,
    pub degraded: usize,
    pub compressed: bool,
    pub note: Option<String>,
    /// 跳过原因（cooldown / no_delta / below_threshold）；ran=false 时用于界面反馈
    pub skipped: Option<String>,
}

/// 执行一次经历管线。Err = 整轮报废（命令层记录；水位不前移已在内部落盘）。
pub async fn run_experience_pipeline(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    store: &Arc<Mutex<MemoryStore>>,
    ledger: &Arc<Mutex<Notices>>,
    abort: &AbortSignal,
    input: PipelineInput,
) -> Result<PipelineOutcome, String> {
    run_experience_pipeline_with(llm, config, store, ledger, abort, input, false).await
}

/// 同 [`run_experience_pipeline`]，可**忽略增量阈值**（设置页「整理当下记忆」用；
/// 冷却仍生效）。管线逻辑单份——不复制。
#[allow(clippy::too_many_arguments)]
pub async fn run_experience_pipeline_with(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    store: &Arc<Mutex<MemoryStore>>,
    ledger: &Arc<Mutex<Notices>>,
    abort: &AbortSignal,
    input: PipelineInput,
    ignore_threshold: bool,
) -> Result<PipelineOutcome, String> {
    let now = now_ms();

    // ── 0. 冷却检查 ──
    {
        let s = store.lock().map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        if let Some(until) = s.cooldown_until(&input.conversation_id) {
            if now < until {
                let mut o = PipelineOutcome::default();
                o.skipped = Some("cooldown".into());
                return Ok(o);
            }
        }
    }

    // ── 1. 增量（空 / 不足阈值）──
    if input.delta_turns.is_empty() {
        let mut o = PipelineOutcome::default();
        o.skipped = Some("no_delta".into());
        return Ok(o);
    }
    if !ignore_threshold && input.delta_turns.len() < INCREMENTAL_TURNS {
        let mut o = PipelineOutcome::default();
        o.skipped = Some("below_threshold".into());
        return Ok(o);
    }

    // ── 2. 读料（可重写组带编号 + 已封存组 + 指纹快照）──
    let delta_text = render_delta(&input.delta_turns);
    if delta_text.trim().is_empty() {
        let mut o = PipelineOutcome::default();
        o.skipped = Some("no_delta".into());
        return Ok(o);
    }
    let (recent, rewritable_text, sealed_text) = {
        let s = store.lock().map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        collect_material(&s)
    };

    // ── 3. 提炼 run（含修复一轮）──
    let ops_raw = match extract_ops(
        llm,
        config,
        abort,
        &delta_text,
        &rewritable_text,
        &sealed_text,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            // 整轮报废：水位不前移 + 记错 + 冷却
            record_failure(store, &input.conversation_id, &e, now)?;
            return Err(e);
        }
    };
    let raw_ops = ops_raw.ops;

    // 编号 → id/fp 映射
    let mut ops: Vec<MemoryOp> = Vec::new();
    let mut dropped = 0usize;
    for op in raw_ops {
        match resolve_op(op, &recent) {
            Some(o) => ops.push(o),
            None => dropped += 1,
        }
    }
    if !ops.is_empty() {
        // 正常
    } else if dropped > 0 {
        // 全部坏件 → 记为整轮报废（下一步 mark_failure），避免静默丢内容
        let err = format!("提炼产出全部坏件（{dropped} 条），本轮报废");
        record_failure(store, &input.conversation_id, &err, now)?;
        return Err(err);
    } else if ops_raw.empty_input_valid {
        // 模型合法地产出空清单（无需记录）→ 推进水位
    }

    // ── 4. 应用 + 推进水位 ──
    let report = {
        let mut s = store.lock().map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        let r = s.apply_ops(
            &ops,
            &input.conversation_id,
            input.turn_from,
            input.new_watermark,
            now,
        )?;
        s.advance_watermark(&input.conversation_id, input.new_watermark, now)?;
        r
    };

    let mut outcome = PipelineOutcome {
        ran: true,
        created: report.created,
        rewritten: report.rewritten,
        degraded: report.degraded,
        compressed: false,
        note: None,
        skipped: None,
    };
    if !report.affected.is_empty() {
        // 提醒带内容：本次所涉条目行（日期·标题：摘要），正文细节不搬
        let lines: Vec<String> = {
            let s = store.lock().map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
            report
                .affected
                .iter()
                .filter_map(|id| s.experience_get(id))
                .map(|e| {
                    format!(
                        "{}·{}：{}",
                        crate::memory::prompt::ymd_local(e.ts),
                        e.title.trim(),
                        e.summary.trim()
                    )
                })
                .collect()
        };
        let header = format!(
            "经历更新：新建 {} 条、重写 {} 条",
            report.created, report.rewritten
        );
        let detail = if lines.is_empty() {
            header
        } else {
            format!("{header}\n{}", lines.join("\n"))
        };
        note(ledger, &detail);
        outcome.note = Some(detail);
    }

    // ── 5. 滚动压缩（到 10 压 5）──
    let compress_plan = {
        let s = store.lock().map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        if s.recent_line_count() as usize >= COMPRESS_AFTER {
            let oldest = s.oldest_unabsorbed(COMPRESS_TAKE);
            Some((
                oldest.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
                oldest
                    .iter()
                    .map(|e| format!("{}（{}）", e.title.trim(), e.summary.trim()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                oldest.iter().map(|e| e.ts).max().unwrap_or(now),
                s.longterm().to_string(),
            ))
        } else {
            None
        }
    };

    if let Some((ids, entries_text, through_ts, current_long)) = compress_plan {
        match compress_longterm(llm, config, abort, &current_long, &entries_text).await {
            Ok(text) if !text.trim().is_empty() => {
                match fit_longterm(llm, config, abort, text.trim()).await {
                    Some(text) => {
                        let version = {
                            let mut s = store.lock().map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
                            let v = s.set_longterm(text.clone(), through_ts, now)?;
                            s.mark_absorbed(&ids, v)?;
                            v
                        };
                        outcome.compressed = true;
                        // 提醒带内容：附新版长期梗概全文（快照冻结不再更新，靠提醒同步）
                        let detail = format!("长期经历已整理（版本 {version}）\n{}", text.trim());
                        note(ledger, &detail);
                        outcome.note = Some(detail);
                    }
                    None => {
                        // 超限且缩短重写后仍超限：拒收——保留旧版，条目不吸收（下轮再试）。
                        // 人工编辑入口（设置页长期记忆）是此时的出口。
                        debug!(
                            "action" => "memory_compress_over_limit_kept_old",
                            "limit" => MAX_LONGTERM_CHARS
                        );
                    }
                }
            }
            Ok(_) => debug!("action" => "memory_compress_empty"),
            Err(e) => {
                // 失败静默：条目不丢，下次再试
                debug!("action" => "memory_compress_failed", "error" => e);
            }
        }
    }

    if report.affected.is_empty() && !outcome.compressed {
        outcome.skipped = Some("no_delta".into());
    }

    Ok(outcome)
}

// ═══════════════════════════════════════════════════════
// 提炼 run
// ═══════════════════════════════════════════════════════

/// 已校验的原始操作（target 为 1-based 编号或 None）。无正文层。
struct RawOp {
    target: Option<i64>,
    title: String,
    summary: String,
    tags: Vec<String>,
}

struct ExtractOutcome {
    ops: Vec<RawOp>,
    /// 输入合法且为空数组（非坏件）
    empty_input_valid: bool,
}

async fn extract_ops(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    abort: &AbortSignal,
    delta_text: &str,
    rewritable_text: &str,
    sealed_text: &str,
) -> Result<ExtractOutcome, String> {
    let system = build_extract_system();
    let user = build_extract_user(delta_text, rewritable_text, sealed_text);
    let messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let count = numbered_count(rewritable_text);

    let first = call(llm, config, &messages, EXTRACT_MAX_TOKENS, EXTRACT_TIMEOUT, abort).await?;
    match parse_ops(&first, count) {
        Ok(outcome) => Ok(outcome),
        Err(err) => {
            // 修复一轮
            debug!("action" => "memory_extract_repair", "error" => &err);
            let repair_msgs = vec![
                ChatMessage::system(build_extract_system()),
                ChatMessage::user(build_repair_user(&first, &err)),
            ];
            let fixed = call(llm, config, &repair_msgs, EXTRACT_MAX_TOKENS, EXTRACT_TIMEOUT, abort).await?;
            parse_ops(&fixed, count).map_err(|e2| format!("修复后仍无法解析：{e2}"))
        }
    }
}

/// 材料分组：可重写组（**带编号**）/ 已封存组（不编号），附指纹快照。
/// 返回 (`编号表[id, fp, 行]`, 可重写文本, 已封存文本)。
fn collect_material(s: &MemoryStore) -> (Vec<(String, Fingerprint, String)>, String, String) {
    let rewritable = s.unabsorbed_rewritable();
    let sealed = s.unabsorbed_sealed();
    let numbered: Vec<(String, Fingerprint, String)> = rewritable
        .iter()
        .enumerate()
        .map(|(i, e)| {
            (
                e.id.clone(),
                s.experience_fingerprint(&e.id)
                    .unwrap_or(Fingerprint { ts: 0, digest: 0 }),
                format!("[{}] {}：{}", i + 1, e.title.trim(), e.summary.trim()),
            )
        })
        .collect();
    let rewritable_text = if numbered.is_empty() {
        "（无）".to_string()
    } else {
        numbered
            .iter()
            .map(|(_, _, line)| line.clone())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let sealed_text = if sealed.is_empty() {
        "（无）".to_string()
    } else {
        sealed
            .iter()
            .map(|e| format!("{}：{}", e.title.trim(), e.summary.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    (numbered, rewritable_text, sealed_text)
}

/// 材料中编号条目数（供编号有效性校验）。
fn numbered_count(numbered_text: &str) -> usize {
    numbered_text.lines().filter(|l| l.trim_start().starts_with('[')).count()
}

fn build_extract_system() -> String {
    r#"你是记忆整理器。你会收到两部分输入：
① 增量——自上次整理以来新增的对话记录；
② 近期条目——已记录的工作单元（分「可重写」与「已封存」两组）。

请把增量整理为操作清单，只输出一个 JSON 数组，不要任何解释、思考过程或其他文字。

每个元素：
{"target": null 或 编号, "title": "短标题", "summary": "状态句", "tags": ["标签"]}

## 切分：一条一个单元

- 每个条目只记一个可以独立回顾的工作单元：一次讨论的结论、一个决定、一段推进、一个阶段变化、一个未完成项。
- 增量中有几个独立事项，就产出几条；不得把不同事项糅进一条。
  反例（禁止）：一条写"方案定稿、开始实现、卡在数据源、其后修复完成"。
  正例：拆为"方案定稿"、"实现完成"、"数据源：卡点定位"、"数据源：修复完成"。
- 同一件事的细碎步骤合为一条；没有实质进展的内容不产出条目。

## 字段

- title：≤20 字。
- summary：单行、一句话（不得包含两个完整句子），≤150 字；直陈当前状态——结论、决定、卡点、未落项；不要重复标题。
- tags：2~5 个有辨识度的关键词。

## 重写与新建

- 重写：当增量内容与「可重写」组中某条内容相近（讲的是同一件事的延续）时，可重写该条——target 填该编号：summary 覆盖为最新状态（并入新进展），title、tags 可微调。
- 其余（含拿不准的）一律新建（target 填 null）。
- 「已封存」组：一律不可重写；同题再有新进展，一律另起新条目。
- 重写只做状态更新与必要合并：不得加入材料里没有的信息，不得凭空扩写。

## 其他

- 只能引用给定编号；不要编造编号。
- 涉及游戏机制与数值的内容：只记「做过什么／结论」，不复制具体数值。
- 若增量没有任何值得保留的工作内容，输出 []。"#
        .to_string()
}

fn build_extract_user(delta_text: &str, rewritable_text: &str, sealed_text: &str) -> String {
    let delta: String = delta_text.chars().take(MAX_MATERIAL_CHARS).collect();
    format!(
        "增量：\n{delta}\n\n近期条目（可重写）：\n{rewritable_text}\n\n近期条目（已封存）：\n{sealed_text}\n\n请输出操作清单 JSON 数组："
    )
}

fn build_repair_user(bad: &str, err: &str) -> String {
    format!(
        "你上一步的输出无法解析为要求的 JSON 数组。\n\n错误：{err}\n\n上一步输出：\n{bad}\n\n请只输出修正后的 JSON 数组（结构同上），不要任何其它文字："
    )
}

/// 解析并校验模型输出为操作清单。
fn parse_ops(raw: &str, recent_count: usize) -> Result<ExtractOutcome, String> {
    let text = preprocess(raw);
    if text.is_empty() {
        return Err("输出为空".into());
    }
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| format!("不是合法 JSON 数组: {e}"))?;
    let mut ops = Vec::new();
    let mut hard_errors: Vec<String> = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        match validate_item(item, recent_count) {
            Ok(op) => ops.push(op),
            Err(ItemIssue::InvalidTarget) => {
                // 无效编号：降级为新建（保留字段）。字段本身非法则记坏件。
                match validate_fields(item) {
                    Some((title, summary, tags)) => ops.push(RawOp {
                        target: None,
                        title,
                        summary,
                        tags,
                    }),
                    None => hard_errors.push(format!("第 {} 条字段非法", i + 1)),
                }
            }
            Err(ItemIssue::BadFields(msg)) => hard_errors.push(format!("第 {} 条：{msg}", i + 1)),
        }
    }
    // 结构/必填严重错误 → 交给修复轮（整轮报废路径）
    if !hard_errors.is_empty() {
        return Err(hard_errors.join("；"));
    }
    Ok(ExtractOutcome {
        empty_input_valid: arr.is_empty(),
        ops,
    })
}

enum ItemIssue {
    InvalidTarget,
    BadFields(String),
}

fn validate_item(v: &serde_json::Value, recent_count: usize) -> Result<RawOp, ItemIssue> {
    let target = match v.get("target") {
        None | Some(serde_json::Value::Null) => None,
        Some(t) => {
            let n = t.as_i64().ok_or(ItemIssue::InvalidTarget)?;
            if n < 1 || (n as usize) > recent_count {
                return Err(ItemIssue::InvalidTarget);
            }
            Some(n)
        }
    };
    let (title, summary, tags) = validate_fields(v).ok_or_else(|| {
        ItemIssue::BadFields("title/summary 缺失或超长".into())
    })?;
    Ok(RawOp {
        target,
        title,
        summary,
        tags,
    })
}

fn validate_fields(v: &serde_json::Value) -> Option<(String, String, Vec<String>)> {
    let title = v.get("title")?.as_str()?.trim().to_string();
    let summary = v.get("summary")?.as_str()?.trim().to_string();
    if title.is_empty() || summary.is_empty() {
        return None;
    }
    if title.chars().count() > MAX_TITLE_CHARS || summary.chars().count() > MAX_SUMMARY_CHARS {
        return None;
    }
    let tags = v
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some((title, summary, tags))
}

/// 把已校验原始操作解析为存储指令（编号 → id + 指纹）。
fn resolve_op(op: RawOp, recent: &[(String, Fingerprint, String)]) -> Option<MemoryOp> {
    match op.target {
        Some(n) => {
            let (id, fp, _) = recent.get((n - 1) as usize)?;
            Some(MemoryOp::Rewrite {
                target_id: id.clone(),
                expected: *fp,
                title: op.title,
                summary: op.summary,
                tags: op.tags,
            })
        }
        None => Some(MemoryOp::Create {
            title: op.title,
            summary: op.summary,
            tags: op.tags,
        }),
    }
}

// ═══════════════════════════════════════════════════════
// 压缩 run
// ═══════════════════════════════════════════════════════

/// 压缩系统提示词。
///
/// 写法参照 Hanako 的 longterm 编译提示词（硬上限放首句、保留/剔除双层清单、
/// 超长时优先概括旧内容）；内容判据换成 PRTS 的工作经历口径。
fn compress_system_prompt() -> String {
    format!(
        r#"请综合「上一份长期情况」和「新沉淀内容」，重写成一份新的长期情况。必须控制在 {target} 字以内。

长期情况不是工作日志。这一层只保留「以后再回看仍然有助于理解这段长期工作」的内容：
- 在推进的长期方向与它们当前的状态
- 已经稳定下来的结论、约定与偏好
- 反复出现的问题、已知的坑

去掉这些单次性内容：
- 某次任务的具体步骤、参数、数值、命令与工具调用
- 某次的失败与重试、来回修改、被打断又恢复
- 逐条任务的流水账

处理方式：
- 不要追加，不要把旧内容和新内容分开复述
- 必须做取舍、抽象、合并：同一方向的多次往返归并为一件事，过细信息压成更高层概括
- 如果上一份长期情况已经很长，优先概括旧内容，再吸收真正重要的新内容
- 细节不必留在这一层——经历库可检索

只输出正文本身，不要 Markdown 标题，不要以 #、##、### 开头，不要解释或代码围栏。"#,
        target = LONGTERM_TARGET_CHARS
    )
}

async fn compress_longterm(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    abort: &AbortSignal,
    current_long: &str,
    entries_text: &str,
) -> Result<String, String> {
    let system = compress_system_prompt();

    let input = {
        let cur = if current_long.trim().is_empty() {
            "（暂无长期记忆）"
        } else {
            current_long
        };
        let text = format!("现有长期记忆：\n{cur}\n\n需并入的旧条目：\n{entries_text}");
        text.chars().take(MAX_COMPRESS_INPUT_CHARS).collect::<String>()
    };
    let messages = vec![ChatMessage::system(system), ChatMessage::user(input)];
    let raw = call(llm, config, &messages, COMPRESS_MAX_TOKENS, COMPRESS_TIMEOUT, abort).await?;
    Ok(strip_fences(&raw))
}

/// 长度硬闸：超限时让模型缩短重写一次；返回 None = 仍超限（调用方拒收、保留旧版）。
async fn fit_longterm(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    abort: &AbortSignal,
    text: &str,
) -> Option<String> {
    let len = text.chars().count();
    if len <= MAX_LONGTERM_CHARS {
        return Some(text.to_string());
    }
    let system = "你是长期记忆压缩器。上一步输出的长期记忆超长，请在不丢失要点（方向、结论、约定、\
坑）的前提下重写得更短。只输出正文本身，不要解释、代码围栏或其他文字。";
    let user = format!(
        "上一步输出（{len} 字，超出上限 {max} 字）：\n{text}\n\n请在 {max} 字以内重写（目标 {target} 字）：",
        len = len,
        max = MAX_LONGTERM_CHARS,
        target = LONGTERM_TARGET_CHARS,
        text = text
    );
    let messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let raw = call(llm, config, &messages, COMPRESS_MAX_TOKENS, COMPRESS_TIMEOUT, abort)
        .await
        .ok()?;
    let out = strip_fences(&raw);
    let out = out.trim();
    if out.is_empty() || out.chars().count() > MAX_LONGTERM_CHARS {
        None
    } else {
        Some(out.to_string())
    }
}

// ═══════════════════════════════════════════════════════
// 共享辅助
// ═══════════════════════════════════════════════════════

/// 单次无工具 LLM 调用（请求级覆盖：thinking off + 限额），可取消 + 超时。
async fn call(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    messages: &[ChatMessage],
    max_tokens: u32,
    timeout: Duration,
    abort: &AbortSignal,
) -> Result<String, String> {
    let mut cfg = config.clone();
    cfg.thinking_level = Some("off".to_string());
    let capped = cfg.max_tokens.min(max_tokens);
    cfg.max_tokens = if capped == 0 { max_tokens } else { capped };

    let run = llm.chat(&cfg, messages, &[]);
    let res = tokio::select! {
        biased;
        _ = abort.wait_abort() => return Err("记忆管线已取消".into()),
        r = tokio::time::timeout(timeout, run) => r,
    };
    match res {
        Ok(Ok(resp)) => Ok(resp.text.unwrap_or_default()),
        Ok(Err(e)) => Err(format!("LLM 调用失败: {e}")),
        Err(_) => Err("LLM 调用超时".into()),
    }
}

fn note(ledger: &Arc<Mutex<Notices>>, detail: &str) {
    match ledger.lock() {
        Ok(mut l) => l.note_memory_change("记忆", detail),
        Err(e) => eprintln!("[memory] 台账锁获取失败（记忆变化未通知）: {e}"),
    }
}

fn record_failure(
    store: &Arc<Mutex<MemoryStore>>,
    conversation_id: &str,
    err: &str,
    now: u64,
) -> Result<(), String> {
    let mut s = store.lock().map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
    s.mark_failure(conversation_id, err, now + FAILURE_COOLDOWN_MS, now)
}

/// 预处理：剥代码围栏 / 思考块，抽取 JSON 数组主体。
fn preprocess(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after
            .strip_prefix("json")
            .or_else(|| after.strip_prefix("JSON"))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            s = after[..end].to_string();
        } else {
            s = after.to_string();
        }
    }
    // 去思考块（若模型误带）
    for (open, close) in [("<thinking>", "</thinking>")] {
        while let (Some(a), Some(b)) = (s.find(open), s.find(close)) {
            if b > a {
                s.replace_range(a..b + close.len(), "");
            } else {
                break;
            }
        }
    }
    if let (Some(a), Some(b)) = (s.find('['), s.rfind(']')) {
        if b > a {
            s = s[a..=b].to_string();
        }
    }
    s.trim().to_string()
}

/// 剥代码围栏（压缩输出用；保留正文）。
fn strip_fences(raw: &str) -> String {
    let s = raw.trim();
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after
            .strip_prefix("markdown")
            .or_else(|| after.strip_prefix("md"))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_string();
        }
        return after.trim().to_string();
    }
    s.to_string()
}

/// 剥 `[prts_reminder]` 块（提炼材料里不携带环境噪声）。
fn strip_reminder(content: &str) -> String {
    const START: &str = "[prts_reminder]";
    const END: &str = "[/prts_reminder]";
    let Some(a) = content.find(START) else {
        return content.to_string();
    };
    let Some(b) = content[a..].find(END) else {
        return content[..a].to_string();
    };
    let rest = &content[a + b + END.len()..];
    format!("{}{}", &content[..a], rest.trim_start_matches(['\r', '\n']))
}

/// 把 jsonl 轮行渲染为可读增量文本（只取 user/assistant 可见文本）。
fn render_delta(lines: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let Ok(events) = serde_json::from_str::<Vec<serde_json::Value>>(line) else {
            continue;
        };
        for ev in &events {
            let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ty != "user/message" && ty != "assistant/message" {
                continue;
            }
            let content = ev.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let text = if ty == "user/message" {
                strip_reminder(content)
            } else {
                content.to_string()
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let who = if ty == "user/message" { "用户" } else { "助手" };
            out.push(format!("{who}：{text}"));
        }
    }
    out.join("\n\n")
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::{MockLlmClient, MockResponse};
    use crate::llm::{LlmError, LlmResponse, StreamEvent, StreamResult, ToolDef};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex as StdMutex;

    // ── 预处理 / 解析 ──

    #[test]
    fn preprocess_strips_fence_and_extracts_array() {
        let raw = "```json\n[{\"a\":1}]\n```";
        assert_eq!(preprocess(raw), "[{\"a\":1}]");
        let raw2 = "说明文字\n[{\"a\":1}]\n后面的话";
        assert_eq!(preprocess(raw2), "[{\"a\":1}]");
        assert_eq!(preprocess("<thinking>x</thinking>[[]]"), "[[]]");
    }

    #[test]
    fn parse_ops_validates_fields_and_targets() {
        // 合法：新建 + 续写
        let text = r#"[{"target":null,"title":"A","summary":"s","detail":"d","tags":["t"]},
                       {"target":1,"title":"B","summary":"s2","detail":"d2","tags":[]}]"#;
        let out = parse_ops(text, 2).unwrap();
        assert_eq!(out.ops.len(), 2);
        assert!(out.ops[0].target.is_none());
        assert_eq!(out.ops[1].target, Some(1));

        // 空数组合法
        let out = parse_ops("[]", 2).unwrap();
        assert!(out.ops.is_empty() && out.empty_input_valid);

        // 非数组 → 硬错（修复）
        assert!(parse_ops("{\"a\":1}", 2).is_err());
        // 缺 summary → 硬错
        assert!(parse_ops(r#"[{"title":"A","detail":"d"}]"#, 2).is_err());
        // 超长 title → 硬错
        let long = "标".repeat(MAX_TITLE_CHARS + 1);
        assert!(parse_ops(&format!(r#"[{{"title":"{long}","summary":"s"}}]"#), 2).is_err());
    }

    #[test]
    fn parse_ops_invalid_target_degrades_to_create() {
        // 编号越界 → 保留字段、降级新建
        let text = r#"[{"target":9,"title":"A","summary":"s","detail":"d"}]"#;
        let out = parse_ops(text, 3).unwrap();
        assert_eq!(out.ops.len(), 1);
        assert!(out.ops[0].target.is_none(), "无效编号降级为新建");
    }

    #[test]
    fn render_delta_strips_reminder_and_keeps_visible_text() {
        let line = r#"[{"type":"user/message","content":"[prts_reminder]\n- 模型：x\n[/prts_reminder]\n\n帮我算伤害"},
                       {"type":"assistant/chunk","content":"忽略"},
                       {"type":"assistant/message","content":"算好了"}]"#;
        let out = render_delta(&[line.to_string()]);
        assert!(out.contains("用户：帮我算伤害"));
        assert!(!out.contains("prts_reminder"));
        assert!(out.contains("助手：算好了"));
    }

    #[test]
    fn numbered_count_counts_bracket_lines() {
        let text = "[1] A：s\n[2] B：s2";
        assert_eq!(numbered_count(text), 2);
        assert_eq!(numbered_count("（无）"), 0);
    }

    #[test]
    fn collect_material_groups_rewritable_and_sealed() {
        let (store, ledger, dir) = temp_store("material");
        {
            let mut s = store.lock().unwrap();
            let mk = |id: &str, ts: u64, title: &str, summary: &str, rc: u32| {
                crate::memory::schema::ExperienceEntry {
                    id: id.into(),
                    ts,
                    conversation_id: "c1".into(),
                    line_from: String::new(),
                    turn_from: 0,
                    turn_to: 1,
                    level: "short".into(),
                    title: title.into(),
                    summary: summary.into(),
                    tags: vec![],
                    revision_count: rc,
                    absorbed_in: None,
                }
            };
            s.inject_experience_for_test(mk("rw0", 100, "甲", "s0", 0));
            s.inject_experience_for_test(mk("rw1", 101, "乙", "s1", 1));
            s.inject_experience_for_test(mk("sealed", 200, "丙", "s2", 2));
        }
        let s = store.lock().unwrap();
        let (numbered, rewritable_text, sealed_text) = collect_material(&s);
        assert_eq!(numbered.len(), 2, "仅可重写组带编号");
        assert_eq!(rewritable_text, "[1] 甲：s0\n[2] 乙：s1");
        assert_eq!(sealed_text, "丙：s2");
        assert!(!sealed_text.contains('['), "已封存组不编号");
        drop(s);
        drop(ledger);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 端到端（mock LLM）──

    fn mock_client(replies: Vec<&str>) -> MockLlmClient {
        MockLlmClient::new(
            replies
                .into_iter()
                .map(|t| MockResponse {
                    events: vec![],
                    result: StreamResult {
                        text: t.to_string(),
                        tool_calls: vec![],
                        reasoning: None,
                        usage: None,
                    },
                })
                .collect(),
        )
    }

    struct CaptureConfigClient {
        last: StdMutex<Option<LlmConfig>>,
        reply: String,
    }
    impl LlmClient for CaptureConfigClient {
        fn chat<'a>(
            &'a self,
            config: &'a LlmConfig,
            _m: &'a [ChatMessage],
            _t: &'a [ToolDef],
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                *self.last.lock().unwrap() = Some(config.clone());
                Ok(LlmResponse {
                    text: Some(self.reply.clone()),
                    tool_calls: None,
                })
            })
        }
        fn chat_stream<'a>(
            &'a self,
            _c: &'a LlmConfig,
            _m: &'a [ChatMessage],
            _t: &'a [ToolDef],
            _e: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(StreamResult {
                    text: self.reply.clone(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                })
            })
        }
    }

    struct FailClient;
    impl LlmClient for FailClient {
        fn chat<'a>(
            &'a self,
            _c: &'a LlmConfig,
            _m: &'a [ChatMessage],
            _t: &'a [ToolDef],
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
            Box::pin(async move { Err(LlmError::Config("boom".into())) })
        }
        fn chat_stream<'a>(
            &'a self,
            _c: &'a LlmConfig,
            _m: &'a [ChatMessage],
            _t: &'a [ToolDef],
            _e: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
            Box::pin(async move { Err(LlmError::Config("boom".into())) })
        }
    }

    fn temp_store(tag: &str) -> (Arc<Mutex<MemoryStore>>, Arc<Mutex<Notices>>, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-mem-pipe-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = MemoryStore::load(&dir).unwrap();
        let ledger = Notices::load(&dir);
        (Arc::new(Mutex::new(store)), Arc::new(Mutex::new(ledger)), dir)
    }

    fn delta(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                format!(
                    r#"[{{"type":"user/message","content":"问题{i}"}},{{"type":"assistant/message","content":"回答{i}"}}]"#
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn pipeline_creates_and_advances_watermark() {
        let (store, ledger, dir) = temp_store("create");
        let client = mock_client(vec![r#"[{"target":null,"title":"伤害","summary":"确认了公式","detail":"细节","tags":["数值"]}]"#]);
        let out = run_experience_pipeline(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: INCREMENTAL_TURNS as u64,
            },
        )
        .await
        .unwrap();
        assert!(out.ran && out.created == 1);
        assert_eq!(store.lock().unwrap().watermark("c1"), INCREMENTAL_TURNS as u64);
        assert_eq!(store.lock().unwrap().recent_line_count(), 1);
        // 提醒数据源已有事件，且**带内容**（条目行「日期·标题：摘要」）
        let guard = ledger.lock().unwrap();
        let detail = guard
            .events()
            .iter()
            .rev()
            .map(|e| e.detail.clone())
            .next()
            .expect("应有记忆事件");
        assert!(detail.contains("经历更新：新建 1 条"), "got {detail}");
        assert!(detail.contains("·伤害：确认了公式"), "got {detail}");
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pipeline_below_threshold_is_noop() {
        let (store, ledger, dir) = temp_store("below");
        let client = mock_client(vec![]); // 不应被调用
        let out = run_experience_pipeline(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(2),
                turn_from: 0,
                new_watermark: 2,
            },
        )
        .await
        .unwrap();
        assert!(!out.ran);
        assert_eq!(store.lock().unwrap().watermark("c1"), 0, "水位不动");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pipeline_repairs_once_then_succeeds() {
        let (store, ledger, dir) = temp_store("repair");
        // 第一次输出坏 JSON，第二次（修复）输出合法
        let client = mock_client(vec![
            "这不是 JSON",
            r#"[{"target":null,"title":"修好了","summary":"s","detail":"d"}]"#,
        ]);
        let out = run_experience_pipeline(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: 6,
            },
        )
        .await
        .unwrap();
        assert!(out.ran && out.created == 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pipeline_whole_round_abort_sets_cooldown_and_keeps_watermark() {
        let (store, ledger, dir) = temp_store("abort");
        // 两次都坏 → 整轮报废
        let client = mock_client(vec!["坏一", "还是坏"]);
        let err = run_experience_pipeline(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: 6,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("修复"), "got {err}");
        let s = store.lock().unwrap();
        assert_eq!(s.watermark("c1"), 0, "整轮报废水位不前移");
        assert!(s.cooldown_until("c1").is_some(), "设冷却");
        drop(s);
        // 冷却期内再次调用 → 直接跳过（不消耗 mock 响应）
        let client2 = mock_client(vec![]);
        let out = run_experience_pipeline(
            &client2,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: 6,
            },
        )
        .await
        .unwrap();
        assert!(!out.ran, "冷却期内 no-op");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pipeline_llm_error_is_whole_round_failure() {
        let (store, ledger, dir) = temp_store("llmerr");
        let err = run_experience_pipeline(
            &FailClient,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: 6,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("LLM"));
        assert_eq!(store.lock().unwrap().watermark("c1"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pipeline_compresses_at_threshold() {
        let (store, ledger, dir) = temp_store("compress");
        // 预置 10 条未吸收（直接注入）→ 触发压缩
        {
            let mut s = store.lock().unwrap();
            for i in 0..COMPRESS_AFTER {
                s.inject_experience_for_test(crate::memory::schema::ExperienceEntry {
                    id: format!("old{i}"),
                    ts: 100 + i as u64,
                    conversation_id: "c1".into(),
                    line_from: String::new(),
                    turn_from: 0,
                    turn_to: 1,
                    level: "short".into(),
                    title: format!("旧事{i}"),
                    summary: "s".into(),
                    tags: vec![],
                    revision_count: 0,
                    absorbed_in: None,
                });
            }
        }
        // 第一次响应＝提炼（空清单，推进水位）；第二次＝压缩正文
        let client = mock_client(vec!["[]", "## 先前\n旧事概要"]);
        let out = run_experience_pipeline(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: 6,
            },
        )
        .await
        .unwrap();
        assert!(out.compressed, "达到 10 条应压缩");
        // 压缩提醒带新版长期梗概全文
        {
            let guard = ledger.lock().unwrap();
            assert!(
                guard.events().iter().any(|e| {
                    e.detail.contains("已整理（版本 1）")
                        && e.detail.contains("旧事概要")
                }),
                "压缩提醒应附长期梗概全文"
            );
        }
        let s = store.lock().unwrap();
        assert_eq!(s.longterm_version(), 1);
        assert_eq!(s.recent_line_count(), (COMPRESS_AFTER - COMPRESS_TAKE) as u64);
        // 被吸收条目（level=long、absorbedIn 已置）
        assert!(s.experience_get("old0").unwrap().is_absorbed());
        assert_eq!(s.experience_get("old0").unwrap().level, "long");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预置 n 条未吸收短期条目（压缩触发条件）。
    fn inject_unabsorbed(store: &Arc<Mutex<MemoryStore>>, n: usize) {
        let mut s = store.lock().unwrap();
        for i in 0..n {
            s.inject_experience_for_test(crate::memory::schema::ExperienceEntry {
                id: format!("old{i}"),
                ts: 100 + i as u64,
                conversation_id: "c1".into(),
                line_from: String::new(),
                turn_from: 0,
                turn_to: 0,
                level: "short".into(),
                title: format!("题{i}"),
                summary: "s".into(),
                tags: vec![],
                revision_count: 0,
                absorbed_in: None,
            });
        }
    }

    #[test]
    fn compress_prompt_states_target_and_scope() {
        let p = compress_system_prompt();
        assert!(
            p.contains("必须控制在 400 字以内"),
            "硬上限应写在首句且不留豁免口: {p}"
        );
        assert!(p.contains("长期情况不是工作日志"));
        assert!(p.contains("去掉这些单次性内容"));
        assert!(p.contains("如果上一份长期情况已经很长，优先概括旧内容"));
        assert!(p.contains("经历库可检索"), "应给模型一个“可以忘”的许可");
    }

    #[tokio::test]
    async fn overlong_longterm_shortened_once_then_accepted() {
        let (store, ledger, dir) = temp_store("lt-fit");
        inject_unabsorbed(&store, COMPRESS_AFTER);
        let long: &'static str = Box::leak("长".repeat(MAX_LONGTERM_CHARS + 50).into_boxed_str());
        let short: &'static str = Box::leak("短".repeat(120).into_boxed_str());
        // 第一次＝提炼（空清单）；第二次＝超长 longterm；第三次＝缩短重写
        let client = mock_client(vec!["[]", long, short]);
        let out = run_experience_pipeline(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: 6,
            },
        )
        .await
        .unwrap();
        assert!(out.compressed, "缩短重写后应接受并落盘");
        let s = store.lock().unwrap();
        assert_eq!(s.longterm().chars().count(), 120);
        assert_eq!(s.longterm_version(), 1);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overlong_longterm_kept_old_when_still_too_long() {
        let (store, ledger, dir) = temp_store("lt-reject");
        {
            let mut s = store.lock().unwrap();
            s.set_longterm("## 旧版\n一句话".into(), 0, 0).unwrap();
        }
        inject_unabsorbed(&store, COMPRESS_AFTER);
        let long: &'static str = Box::leak("长".repeat(MAX_LONGTERM_CHARS + 50).into_boxed_str());
        let still: &'static str = Box::leak("长".repeat(MAX_LONGTERM_CHARS + 10).into_boxed_str());
        let client = mock_client(vec!["[]", long, still]);
        let out = run_experience_pipeline(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(INCREMENTAL_TURNS),
                turn_from: 0,
                new_watermark: 6,
            },
        )
        .await
        .unwrap();
        assert!(!out.compressed, "仍超限应拒收");
        let s = store.lock().unwrap();
        assert_eq!(s.longterm(), "## 旧版\n一句话", "拒收应保留旧版");
        assert_eq!(s.longterm_version(), 1, "拒收不推进版本");
        assert_eq!(s.recent_line_count(), COMPRESS_AFTER as u64, "拒收不吸收条目");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn call_overrides_thinking_and_caps_tokens() {
        let client = CaptureConfigClient {
            last: StdMutex::new(None),
            reply: "[]".into(),
        };
        let cfg = LlmConfig {
            max_tokens: 20_000,
            ..LlmConfig::default()
        };
        let _ = call(
            &client,
            &cfg,
            &[ChatMessage::user("x")],
            EXTRACT_MAX_TOKENS,
            EXTRACT_TIMEOUT,
            &AbortSignal::new(),
        )
        .await;
        let captured = client.last.lock().unwrap().clone().unwrap();
        assert_eq!(captured.thinking_level.as_deref(), Some("off"));
        assert_eq!(captured.max_tokens, EXTRACT_MAX_TOKENS);
    }

    #[tokio::test]
    async fn pipeline_ignore_threshold_runs_below_six() {
        let (store, ledger, dir) = temp_store("ignore");
        let client = mock_client(vec![
            r#"[{"target":null,"title":"小增量","summary":"s","detail":"d"}]"#,
        ]);
        let out = run_experience_pipeline_with(
            &client,
            &LlmConfig::default(),
            &store,
            &ledger,
            &AbortSignal::new(),
            PipelineInput {
                conversation_id: "c1".into(),
                delta_turns: delta(2),
                turn_from: 0,
                new_watermark: 2,
            },
            true,
        )
        .await
        .unwrap();
        assert!(out.ran && out.created == 1, "忽略阈值：2 轮也提炼");
        assert_eq!(store.lock().unwrap().watermark("c1"), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
