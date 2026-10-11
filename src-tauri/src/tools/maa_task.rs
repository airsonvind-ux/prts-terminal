//! maa_task —— 把 MAA 任务交给本机 MAA（桥 ↔ PRTS 工具层的对外面）。
//!
//! 工具本身很薄：校验参数 → 调 `maa::manager` → 把结果与事件翻译成人能读、
//! 模型能用的形状。进程、连接、重试全在 `maa` 模块里。
//!
//! 三个 action：
//! - `run`（默认）：把 `tasks` 整条队列派给 MAA；
//! - `stop`：中止当前任务；
//! - `status`：报当前状态与「最近一次」的结果文件（**不起桥、不改状态**）。
//!
//! 事件分流（见 `_maa-接入模块-设计.md` §5）：`recognition`（识别结果）落盘
//! 并把路径交给模型；`maa`（进度）只做计数与最新一条的摘要——整份进度不进
//! 上下文，那是烧 token。

use std::collections::BTreeMap;

use serde_json::{json, Value};

#[cfg(test)]
use crate::config;
use crate::maa::manager;
use crate::maa::records;

use super::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

pub struct MaaTaskTool;

impl Tool for MaaTaskTool {
    fn name(&self) -> &str {
        "run_maa_task"
    }

    fn description(&self) -> &str {
        concat!(
            "在本机《明日方舟》中，通过 MAA 执行游戏操作：刷关、公招、基建、领奖、肉鸽、抄作业、识别等。仅支持模拟器。\n",
            "\n参数：\n",
            "  action：\"run\"（默认）| \"stop\" | \"status\"\n",
            "  tasks：action=\"run\" 时要执行的任务序列，按顺序依次执行；每项 { \"type\": 任务类型, \"params\": 参数对象 }，params 可省。\n",
            "  wait：最多等多少秒。0 = 派发：发出去就返回，任务在后台跑，不阻塞当前对话；大于 0 = 等待：等到结果再返回。\n",
            "    识别这类短任务可以等待；长任务（Fight / Infrast / Roguelike / Copilot 等）默认派发，派下去跟博士说一声即可。\n",
            "    想等一个可能很久的任务跑完时（估着超过三分钟，或估不出要多久），先向博士确认执行方式与等待时间。\n",
            "\n返回：派发即回一句确认；等就给该次的结论；失败给 MAA 的原话。\n",
            "\n示例：\n",
            "  · 识别持有的干员：{ \"action\": \"run\", \"tasks\": [ { \"type\": \"OperBox\" } ] }\n",
            "  · 启动游戏并把 1-7 刷到理智耗尽：{ \"action\": \"run\", \"tasks\": [ { \"type\": \"StartUp\" }, { \"type\": \"Fight\", \"params\": { \"stage\": \"1-7\", \"series\": 0 } } ] }\n",
            "  · 中止任务：{ \"action\": \"stop\" }\n",
            "  · 查看状态：{ \"action\": \"status\" }\n",
            "\n任务类型、常用参数、注意事项与官方文档，见 skill「MAA 任务」。"
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "stop", "screenshot", "status"],
                    "description": "省略时按 run。"
                },
                "tasks": {
                    "type": "array",
                    "description": "要执行的任务序列。",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "description": "任务类型。" },
                            "params": { "type": "object", "description": "该任务的参数。" }
                        },
                        "required": ["type"]
                    }
                },
                "wait": {
                    "type": "integer",
                    "description": "0 = 派发，大于 0 = 等待。"
                }
            }
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<Value>(params) else {
            return ToolDisplay::default();
        };
        match v.get("action").and_then(Value::as_str).unwrap_or("run") {
            "stop" => ToolDisplay::info("中止当前任务"),
            "status" => ToolDisplay::info("查询 MAA 状态"),
            _ => match v.get("tasks").and_then(Value::as_array) {
                Some(tasks) if !tasks.is_empty() => ToolDisplay::info(summarize_tasks(tasks)),
                _ => ToolDisplay::default(),
            },
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        match v.get("action").and_then(Value::as_str).unwrap_or("run") {
            "stop" => {
                // 桥不在就没有队列，也就没有在跑的任务——给结论，不给错误，也不起桥。
                let view = manager::status_view();
                if !view.bridge_available {
                    return Ok(ToolOutput::text("当前没有在跑的任务。"));
                }
                let queued = manager::queue()
                    .ok()
                    .flatten()
                    .and_then(|q| q.get("tasks").and_then(Value::as_array).map(Vec::len))
                    .unwrap_or(0);
                if let Err(e) = manager::stop() {
                    return Ok(ToolOutput::text(format!("未能执行：{e}")));
                }
                // 「已中止」只在真有东西被中止时才说——空队列上叫停，不能报成一次中止。
                Ok(ToolOutput::text(if view.running || queued > 0 {
                    "已中止并清空队列；正在执行的动作会在下个检查点停下。"
                } else {
                    "当前没有在跑的任务。"
                }))
            }
            "status" => {
                let (canonical, text) = render_status();
                Ok(ToolOutput::json(canonical, text))
            }
            "run" => {
                let tasks = v
                    .get("tasks")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if tasks.is_empty() {
                    return Err(ToolError::Execution(
                        "action=run 需要 tasks：至少给一个任务".to_string(),
                    ));
                }
                let wait_secs = v
                    .get("wait")
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| manager::queue_default_wait_secs(&tasks));
                let outcome =
                    manager::submit(&tasks, wait_secs).map_err(ToolError::Execution)?;
                // 等待期间取走的帧在 `outcome.events` 里；缓冲区剩下的只是收尾之后到的。
                // 两边都要——少了一边，等待越成功摘要就越空（2026-10-06 真机逮到）。
                let mut events = outcome.events.clone();
                events.extend(manager::drain_events());
                let (canonical, text) = render_run(&outcome, &tasks, &events, wait_secs);
                Ok(ToolOutput::json(canonical, text))
            }
            other => Err(ToolError::Execution(format!(
                "未知的 action：{other}（只有 run / stop / status）"
            ))),
        }
    }

    /// 这个工具是**有状态且带副作用**的（桥连接、任务队列、截图落盘），
    /// 必须串行执行。
    fn is_concurrency_safe(&self) -> bool {
        false
    }
}

/// 一组的类型归属（按 skill 既有分类，决定结论怎么给）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Recognition,
    Routine,
    Roguelike,
    Copilot,
    Lifecycle,
    Other,
}

fn group_of(type_: &str) -> Group {
    match type_ {
        "Depot" | "OperBox" => Group::Recognition,
        "Fight" | "Recruit" | "Infrast" | "Mall" | "Award" | "SwitchTheme" => Group::Routine,
        "Roguelike" => Group::Roguelike,
        "Copilot" | "SSSCopilot" | "ParadoxCopilot" | "VideoRecognition" => Group::Copilot,
        "StartUp" | "CloseDown" => Group::Lifecycle,
        _ => Group::Other,
    }
}

/// 一条任务链的结局（`TaskChain*` 信号按 `taskid` 归并）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainState {
    Start,
    Completed,
    Error,
    Stopped,
}

fn chain_outcomes(progress: &[Value]) -> BTreeMap<i64, ChainState> {
    let mut map = BTreeMap::new();
    for frame in progress {
        let state = match frame.get("name").and_then(Value::as_str) {
            Some("TaskChainStart") => ChainState::Start,
            Some("TaskChainCompleted") => ChainState::Completed,
            Some("TaskChainError") => ChainState::Error,
            Some("TaskChainStopped") => ChainState::Stopped,
            _ => continue,
        };
        if let Some(id) = frame
            .get("details")
            .and_then(|d| d.get("taskid"))
            .and_then(Value::as_i64)
        {
            map.insert(id, state);
        }
    }
    map
}

/// 把一次 `run` 的结果与事件翻译成（结构化值，面向模型的文本）。
///
/// 形态按 `_maa-返回设计.md`：**状态行 + 结论**（需要接手时单独成行）。
/// 详略由**模式**决定：派发只报派发；等待报主结论；到点仍在跑要声明结果会变。
fn render_run(
    outcome: &manager::SubmitOutcome,
    tasks: &[Value],
    events: &[Value],
    wait_secs: u64,
) -> (Value, String) {
    let (recognition, progress) = split_events(events.to_vec());
    // 结果文件由事件侧的运行记录负责写（`wait=0` 也一样），这里只取它写下的。
    let saved = manager::run_files();
    let chains = chain_outcomes(&progress);
    let ids = outcome.task_ids.clone();
    let elapsed = outcome.elapsed_ms as f64 / 1000.0;

    // 声明顺序与 task_id 一一对应（`task_add` 按序发出）。
    let state_of = |i: usize| ids.get(i).and_then(|id| chains.get(id)).copied();
    let failed = (0..tasks.len())
        .filter(|i| state_of(*i) == Some(ChainState::Error))
        .count();

    let mut lines: Vec<String> = Vec::new();

    // ── 状态行 ──
    if wait_secs == 0 {
        lines.push(format!(
            "已派出：{} 个任务在后台执行（task_id {}）。",
            ids.len(),
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join("、")
        ));
        // 派发即返回：不给结果、不给路径、不给回执。
        return (
            canonical(&outcome.connected, &ids, &saved, progress.len()),
            lines.join("\n"),
        );
    }
    if !outcome.completed {
        lines.push(format!(
            "仍在跑：等待上限 {wait_secs}s 已到，任务在后台继续；以下结果还会变。"
        ));
    } else if tasks.len() > 1 {
        lines.push(if failed > 0 {
            format!(
                "完成：队列 {} 个任务中 {} 个失败，耗时 {elapsed:.1}s。",
                tasks.len(),
                failed
            )
        } else {
            format!("完成：队列 {} 个任务，耗时 {elapsed:.1}s。", tasks.len())
        });
    } else if failed == 0 {
        // 单任务：类型自己的那句话（含耗时）；给不出来就回通用状态行。
        let only = &tasks[0];
        let type_ = only.get("type").and_then(Value::as_str).unwrap_or("");
        let params = only.get("params").cloned().unwrap_or_else(|| json!({}));
        let mine: Vec<Value> = recognition
            .iter()
            .filter(|f| f.get("taskid").and_then(Value::as_i64) == ids.first().copied())
            .cloned()
            .collect();
        lines.push(
            headline(type_, &params, &mine, elapsed)
                .unwrap_or_else(|| format!("完成：已跑完，耗时 {elapsed:.1}s。")),
        );
    }

    // ── 结论 ──
    let multi = tasks.len() > 1;
    for (i, task) in tasks.iter().enumerate() {
        let type_ = task.get("type").and_then(Value::as_str).unwrap_or("?");
        let id = ids.get(i).copied();
        let mine: Vec<Value> = recognition
            .iter()
            .filter(|f| f.get("taskid").and_then(Value::as_i64) == id)
            .cloned()
            .collect();

        let params = task.get("params").cloned().unwrap_or_else(|| json!({}));
        let mut block = match state_of(i) {
            Some(ChainState::Error) => vec!["失败。".to_string()],
            Some(ChainState::Stopped) => vec!["已中止。".to_string()],
            _ => conclusions_for(type_, &params, &mine),
        };
        // 识别组在单任务时用「结果：」起头；没跑完就把这件事写在结论上。
        if !multi && group_of(type_) == Group::Recognition {
            if let Some(first) = block.first_mut() {
                let head = first.trim_end_matches('。').to_string();
                *first = if outcome.completed {
                    format!("结果：{head}。")
                } else {
                    format!("结果：{head}（未完成）。")
                };
            }
        }
        // 只有识别类把「没产出」当一件要说的事；别的类型有没有帧都不影响那句话。
        if block.is_empty() && group_of(type_) == Group::Recognition {
            block.push("本次没有产出。".to_string());
        }
        if multi {
            // 队列行：该类型自己的结论；没有帧就给它的「那一句」核心。
            let body = if !block.is_empty() {
                block.join(" ")
            } else if let Some(core) = headline_core(type_, &params, &mine) {
                format!("{core}。")
            } else {
                "（无结果）".to_string()
            };
            lines.push(format!("- {type_}：{body}"));
        } else {
            lines.extend(block);
        }
    }

    // 界园主动重开：`TaskChainExtraInfo{what: RoutingRestart}`，不进识别路由，单独看一眼。
    if progress.iter().any(|f| {
        f.get("details").and_then(|d| d.get("what")).and_then(Value::as_str)
            == Some("RoutingRestart")
    }) {
        lines.push("界园里主动重开了一次。".to_string());
    }

    // 出错时才给取证位置——常态不给路径。
    if let Some(i) = (0..tasks.len()).find(|i| state_of(*i) == Some(ChainState::Error)) {
        let type_ = tasks[i].get("type").and_then(Value::as_str).unwrap_or("");
        if let Some(kept) = saved
            .iter()
            .find(|k| k.chain.as_deref() == Some(type_) || k.kind.starts_with(type_))
        {
            lines.push(format!("出错任务的记录留存于 {}。", kept.path.display()));
        }
    }

    (
        canonical(&outcome.connected, &ids, &saved, progress.len()),
        lines.join("\n"),
    )
}

/// 结构化收尾（不给模型看，供日志与后续消费者重建）。
fn canonical(
    connected: &manager::ConnectedInfo,
    ids: &[i64],
    saved: &[crate::maa::records::Kept],
    progress: usize,
) -> Value {
    json!({
        "connected": {
            "emulator": connected.emulator,
            "address": connected.address,
            "config": connected.config,
            "extras": connected.extras,
        },
        "task_ids": ids,
        "recognition": saved
            .iter()
            .map(|k| json!({ "kind": k.kind, "path": k.path.to_string_lossy() }))
            .collect::<Vec<_>>(),
        "progress_events": progress,
    })
}

/// 事件分流：`recognition`（识别结果）与其余（`maa` 进度）分开。
fn split_events(frames: Vec<Value>) -> (Vec<Value>, Vec<Value>) {
    let mut recognition = Vec::new();
    let mut progress = Vec::new();
    for frame in frames {
        if frame.get("event").and_then(Value::as_str) == Some("recognition") {
            recognition.push(frame);
        } else {
            progress.push(frame);
        }
    }
    (recognition, progress)
}

/// `status`：如实报当前状态与「最近一次」的结果文件。
///
/// **不起桥、不改状态**——查询不该把 MAA 拉起来；没桥就如实报未启动。
fn render_status() -> (Value, String) {
    let view = manager::status_view();
    let queue = manager::queue().ok().flatten();
    let last = manager::last_run();

    let mut lines = Vec::new();
    lines.push(format!(
        "MAA ：{}｜桥：{}｜连接：{}",
        if view.maa_installed { "已装" } else { "未装" },
        if view.bridge_available { "已启动" } else { "未启动" },
        if view.connected { "已连接" } else { "未连接" }
    ));
    if let Some(err) = &view.last_error {
        lines.push(format!("最近错误：{err}"));
    }

    let tasks = queue
        .as_ref()
        .and_then(|q| q.get("tasks"))
        .and_then(Value::as_array);
    let running = if view.running { "在跑" } else { "不在跑" };
    match tasks {
        Some(tasks) if !tasks.is_empty() => {
            let names: Vec<String> = tasks
                .iter()
                .map(|t| {
                    t.get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string()
                })
                .collect();
            lines.push(format!(
                "任务：{running}｜队列 {} 项：{}",
                names.len(),
                names.join("、")
            ));
        }
        _ => lines.push(format!("任务：{running}｜队列空")),
    }

    // 「当前状态」只在真的在跑时写：它回答的是「派出去的那批活干到哪了」。
    // 帧**只读**（peek），不动待读队列。
    let current = if view.running {
        current_state(
            &manager::peek_events(STATUS_FRAME_WINDOW),
            tasks.map(Vec::as_slice),
        )
    } else {
        None
    };
    if let Some(current) = &current {
        lines.push(format!("当前状态：{current}"));
    }

    match (last.ended_at_ms, last.files.first()) {
        (Some(ended), Some(_)) => {
            let secs = records::now_ms().saturating_sub(ended) / 1000;
            // 来自磁盘的留痕只能看出“写过”，看不出那次有没有跑完——如实说。
            let state = if last.from_disk {
                "磁盘记录，完成状态未知"
            } else if last.completed {
                "已完成"
            } else {
                "未完成"
            };
            // 报的是**任务类型**（读者问的是“跑了哪个任务”），不是 MAA 的节点名。
            lines.push(format!(
                "最近一次：{}（{state}，{}）",
                run_label(&last.files),
                human_ago(secs)
            ));
            if let Some(err) = &last.error {
                lines.push(format!("落盘异常：{err}"));
            }
        }
        _ => lines.push("最近一次：还没跑过任务".to_string()),
    }

    let canonical = json!({
        "maa_installed": view.maa_installed,
        "bridge_available": view.bridge_available,
        "connected": view.connected,
        "running": view.running,
        "stale": view.stale,
        "last_error": view.last_error,
        "emulator": view.emulator,
        "address": view.address,
        "queue": queue,
        "current_state": current,
        "last_run": {
            "completed": last.completed,
            "ended_at_ms": last.ended_at_ms,
            "files": last
                .files
                .iter()
                .map(|k| json!({ "kind": k.kind, "chain": k.chain }))
                .collect::<Vec<_>>(),
            "error": last.error,
        },
    });
    (canonical, lines.join("\n"))
}

/// `status` 的当前状态从最近这么多帧里提炼。
const STATUS_FRAME_WINDOW: usize = 20;

/// `status` 的「当前状态」：从最近的事件帧里提炼一句动作名。
///
/// 帧形（桥侧归一化）：`maa` 帧的动作名在 `details.what`，识别结果帧在 `kind`。
/// 从最新往回找第一条有动作名的帧；能对上队列项就带上任务类型。
/// **取不到就返回 `None`**——调用方不写这行，也不写「未知」。
fn current_state(frames: &[Value], tasks: Option<&[Value]>) -> Option<String> {
    for frame in frames.iter().rev() {
        if frame.get("event").and_then(Value::as_str) == Some("recognition") {
            if let Some(kind) = frame.get("kind").and_then(Value::as_str) {
                return Some(kind.to_string());
            }
            continue;
        }
        let Some(details) = frame.get("details") else {
            continue;
        };
        let what = details.get("what").and_then(Value::as_str).unwrap_or("");
        if what.is_empty() {
            continue;
        }
        let type_ = tasks
            .and_then(|tasks| {
                let id = details.get("taskid").and_then(Value::as_i64)?;
                tasks
                    .iter()
                    .find(|t| t.get("task_id").and_then(Value::as_i64) == Some(id))
            })
            .and_then(|t| t.get("type").and_then(Value::as_str));
        return Some(match type_ {
            Some(t) => format!("{t}（{what}）"),
            None => what.to_string(),
        });
    }
    None
}

/// 「最近一次」跑的是哪些任务：取各文件的 `chain`（任务类型），去重保序。
/// 磁盘上的老记录可能没有 `chain`，那就退回识别种类。
fn run_label(files: &[crate::maa::records::Kept]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for kept in files {
        let name = kept.chain.as_deref().unwrap_or(kept.kind.as_str());
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        "未知".to_string()
    } else {
        names.join("、")
    }
}

/// 秒数说人话。
fn human_ago(secs: u128) -> String {
    if secs < 60 {
        format!("{secs} 秒前")
    } else if secs < 3600 {
        format!("{} 分钟前", secs / 60)
    } else {
        format!("{} 小时前", secs / 3600)
    }
}

/// 明确不进的种类：过程帧与连接态，明细走结果文件。
const SILENT_KINDS: &[&str] = &[
    "ProductOfFacility",
    "ProductChanged",
    "ProductChangeFail",
    "ProductIncorrect",
    "StageInfo",
    "StageQueueMissionCompleted",
    "RecruitTagsDetected",
    "RecruitTagsSelected",
    "RecruitSpecialTag",
    "RecruitPreservedTag",
    "CopilotAction",
    "CopilotListLoadTaskFileSuccess",
    "BattleFormation",
    "BattleFormationSelected",
    "OcrStage",
    "OcrFormation",
    "ClassifyDirection",
    "DetectOperators",
    "Slice",
    "MatchDeployment",
    "Reconnected",
    "ResolutionInfo",
    "UnsupportedResolution",
    "UnsupportedPlayTools",
    "BlackFlowInventoryCleanup",
    "BlackFlowStrategyResult",
    "BlackFlowStrategyStarted",
    "BoskyPassageNode",
    "RoguelikeCollapsalParadigms",
    "RoguelikeEvent",
    "RoguelikeEventSelected",
    "RoguelikeEncounterOptions",
    "RoguelikeJieGardenTargetFound",
    "RoguelikeCoppersExchangeInfo",
    "RoguelikeCombatEnd",
    "FoldartalGainOcrNextLevel",
    "SSSStage",
    "AutoRaisePotentialProgress",
    "AutoRaisePotentialTotal",
    "CompSkill",
    "DeepExplorationCompleted",
    "MonthlySquadCompleted",
    "InfrastTrainingCompleted",
    "InfrastTrainingIdle",
    "InfrastTrainingTimeLeft",
    "CustomInfrastRoomGroupsMatch",
    "CustomInfrastRoomGroupsMatchFailed",
    "CustomInfrastRoomOperators",
    "FacilityLayoutRecognitionFailed",
    "OperProgressDetail",
    "OperProgressSummary",
    "PixelPaintProgress",
];

/// 一个任务的结论行。
///
/// 需要「数一数」的（设施、刷新、位子）在这里聚合；单个字段的走 [`extract_known`]。
/// 帧里没有可靠字段就不写这一行——宁可不写，也不编。
fn conclusions_for(type_: &str, params: &Value, frames: &[Value]) -> Vec<String> {
    let mut by_kind: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for frame in frames {
        if let Some(kind) = frame.get("kind").and_then(Value::as_str) {
            by_kind.entry(kind).or_default().push(frame);
        }
    }

    let mut lines: Vec<String> = Vec::new();

    // 障碍是「需要你接手」的东西，优先级最高，先摆出来。
    for (kind, group) in &by_kind {
        if let Some(text) = obstacle_text(kind, group.last().copied()) {
            lines.push(text);
        }
    }

    // 各组的主结论（各自取走与自己有关的种类）。
    match type_ {
        "Fight" => fight_lines(&mut by_kind, params, &mut lines),
        "Recruit" => lines.extend(recruit_lines(&mut by_kind, params)),
        "Infrast" => lines.extend(infrast_lines(&mut by_kind, params)),
        "Award" => lines.extend(award_lines(params)),
        "Roguelike" => roguelike_lines(&mut by_kind, &mut lines),
        _ => {}
    }

    // 剩下的走通用提取；过程帧不进，剩下来的种类只报「有几样」。
    let mut reasons: Vec<String> = Vec::new();
    let mut unknown = 0usize;
    for (kind, group) in &by_kind {
        let last = group.last().copied();
        if obstacle_text(kind, last).is_some() {
            continue;
        }
        match *kind {
            // 结束原因收在末尾，不占结论行。
            "ExceededLimit" => {
                if let Some(text) = exceeded_limit_text(last) {
                    reasons.push(text);
                }
            }
            // 过程性帧，不进返回。
            k if SILENT_KINDS.contains(&k) => {}
            k => match extract_known(k, last) {
                Some(body) => lines.push(body),
                None => unknown += 1,
            },
        }
    }
    if unknown > 0 {
        lines.push(format!("另有 {unknown} 种未识别的结果。"));
    }
    lines.extend(reasons);
    lines
}

/// 「需要你接手」的障碍：MAA 停手、把决定交回用户的那些时刻。
fn obstacle_text(kind: &str, frame: Option<&Value>) -> Option<String> {
    let details = frame.and_then(|f| f.get("details"));
    let field = |name: &str| details.and_then(|d| d.get(name)).and_then(Value::as_str);
    match kind {
        "UnsupportedLevel" => Some(match field("level") {
            Some(level) => format!("障碍：关卡「{level}」不支持。"),
            None => "障碍：有不支持的关卡。".to_string(),
        }),
        "StageInfoError" => Some("障碍：关卡信息出错。".to_string()),
        "BattleFormationParseFailed" => Some("障碍：作业文件解析失败。".to_string()),
        "BattleFormationOperUnavailable" => {
            let oper = field("oper_name").unwrap_or("?");
            Some(format!("障碍：干员「{oper}」不满足作业要求。"))
        }
        "UserAdditionalOperInvalid" => {
            let name = field("name").unwrap_or("?");
            Some(format!("障碍：指定使用的干员「{name}」无效。"))
        }
        "SSSSettlement" => Some(match field("why") {
            Some(why) => format!("障碍：保全派驻以「{why}」结束。"),
            None => "障碍：保全派驻没有打通关。".to_string(),
        }),
        "SideStoryReopenTaskNotExists" => Some("障碍：复刻关卡不存在。".to_string()),
        "StageQueueUnableToAgent" => Some("障碍：关卡不能代理。".to_string()),
        "StageQueueStageNotFound" => Some("障碍：关卡没找到。".to_string()),
        _ => None,
    }
}

/// `ExceededLimit{task, exec_times, max_times}`：某个动作到次数上限，任务链就此收尾。
fn exceeded_limit_text(frame: Option<&Value>) -> Option<String> {
    let task = frame?.get("details")?.get("task")?.as_str()?;
    Some(format!("结束原因：{task} 到次数上限，MAA 停手。"))
}

/// 基建设施的中文名（键取自 MAA `InfrastFacilityImageAnalyzer`）。
fn facility_name(key: &str) -> &str {
    match key {
        "Mfg" => "制造站",
        "Trade" => "贸易站",
        "Power" => "发电站",
        "Control" => "控制中枢",
        "Dorm" => "宿舍",
        "Reception" => "会客室",
        "Office" => "办公室",
        "Processing" => "加工站",
        "Training" => "训练室",
        other => other,
    }
}

/// `EnterFacility{facility, index}` 聚合成「换了哪些设施」，换得多的在前。
fn facility_text(group: &[&Value]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for frame in group {
        if let Some(key) = frame
            .get("details")
            .and_then(|d| d.get("facility"))
            .and_then(Value::as_str)
        {
            *counts.entry(key).or_default() += 1;
        }
    }
    let mut items: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(key, n)| (facility_name(key).to_string(), n))
        .collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let parts: Vec<String> = items.iter().map(|(name, n)| format!("{name} ×{n}")).collect();
    format!("已换班：{}。", parts.join("、"))
}

/// 一组同种帧里，最后一帧的这个字段。
fn last_field<'a>(group: &[&'a Value], field: &str) -> Option<&'a Value> {
    group
        .iter()
        .rev()
        .find_map(|f| f.get("details").and_then(|d| d.get(field)))
}

/// 长任务的「那一句」核心（不含耗时）：类型自己的说法。给不出来就回通用状态行。
fn headline_core(type_: &str, params: &Value, frames: &[Value]) -> Option<String> {
    match type_ {
        "Fight" => {
            let stage = params.get("stage").and_then(Value::as_str);
            let n = fight_times(frames);
            Some(match (stage, n) {
                (Some(stage), Some(n)) => format!("{stage} 打完了 {n} 场"),
                (Some(stage), None) => format!("{stage} 打完了"),
                (None, Some(n)) => format!("打完了 {n} 场"),
                (None, None) => "刷关完成".to_string(),
            })
        }
        "Recruit" => Some("公开招募完成".to_string()),
        "Infrast" => Some("基建换班完成".to_string()),
        "Mall" => Some("信用购物完成".to_string()),
        "Award" => Some("奖励领取完成".to_string()),
        "Roguelike" => Some(if params.get("mode").and_then(Value::as_i64) == Some(1) {
            "肉鸽存钱跑完了".to_string()
        } else {
            "肉鸽跑完了".to_string()
        }),
        "StartUp" => Some("游戏已启动".to_string()),
        "CloseDown" => Some("游戏已关闭".to_string()),
        "Custom" => Some("自定义任务完成".to_string()),
        "Reclamation" => Some("生息演算完成".to_string()),
        "SingleStep" => step_core(params),
        _ => None,
    }
}

/// 单任务首行 = 核心 + 耗时。单步不加耗时——一步的耗时是噪音。
fn headline(type_: &str, params: &Value, frames: &[Value], elapsed: f64) -> Option<String> {
    let core = headline_core(type_, params, frames)?;
    Some(if type_ == "SingleStep" {
        format!("{core}。")
    } else {
        format!("{core}，耗时 {elapsed:.1}s。")
    })
}

/// 单步任务：一步一句，全部从入参来（默认等待，这句就是常态返回）。
fn step_core(params: &Value) -> Option<String> {
    let details = params.get("details");
    let field = |name: &str| details.and_then(|d| d.get(name)).and_then(Value::as_str);
    match params.get("subtype").and_then(Value::as_str)? {
        "stage" => Some(format!("单步：已设置关卡 {}", field("stage_name")?)),
        "start" => Some("单步：已开始".to_string()),
        "action" => Some(match (field("name"), field("direction")) {
            (Some(name), Some(dir)) => format!("单步：已执行动作（{name} → {dir}）"),
            (Some(name), None) => format!("单步：已执行动作（{name}）"),
            _ => "单步：已执行动作".to_string(),
        }),
        other => Some(format!("单步：已执行 {other}")),
    }
}

/// 这一趟打了几场：`FightTimes.times_finished` 的末值。
fn fight_times(frames: &[Value]) -> Option<i64> {
    frames
        .iter()
        .filter(|f| f.get("kind").and_then(Value::as_str) == Some("FightTimes"))
        .filter_map(|f| f.get("details")?.get("times_finished")?.as_i64())
        .last()
}

/// Fight：掉落 + （剿灭进度）+ 理智账 + 用药。没有帧的行不写。
fn fight_lines(by_kind: &mut BTreeMap<&str, Vec<&Value>>, params: &Value, lines: &mut Vec<String>) {
    let drops = by_kind.remove("StageDrops");
    let times = by_kind.remove("FightTimes");
    let sanity = by_kind.remove("SanityBeforeStage");
    let medicine = by_kind.remove("UseMedicine");

    if let Some(group) = &drops {
        lines.extend(drops_lines(group));
        if let Some(progress) = annihilation_text(group) {
            lines.push(progress);
        }
    }

    let series = times.as_ref().and_then(|g| last_field(g, "series")).and_then(Value::as_i64);
    let cost = times.as_ref().and_then(|g| last_field(g, "sanity_cost")).and_then(Value::as_i64);
    let before = sanity
        .as_ref()
        .and_then(|g| last_field(g, "current_sanity"))
        .and_then(Value::as_i64);
    if let Some(cost) = cost {
        let lead = match series {
            Some(s) if s > 1 => format!("按 {s} 倍代理执行，"),
            _ => String::new(),
        };
        let tail = match before {
            Some(b) => format!("（开打前 {b}）"),
            None => String::new(),
        };
        lines.push(format!("{lead}消耗理智 {cost}{tail}。"));
    }
    if let Some(group) = &medicine {
        lines.push(format!("用了 {} 瓶理智药。", group.len()));
    }

    // 次数与倍率参数只在帧里没有时，拿入参报一次（设定可能落空的那类）。
    if times.is_none() {
        if let Some(series) = params.get("series").and_then(Value::as_i64) {
            if series > 1 {
                lines.push(format!("按 {series} 倍代理执行。"));
            }
        }
    }
}

/// 掉落：单关卡用累计 `stats`；多关卡按 `stageCode` 分组（复刻一次刷多关）。
fn drops_lines(group: &[&Value]) -> Vec<String> {
    let mut per_stage: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for frame in group {
        let Some(details) = frame.get("details") else {
            continue;
        };
        let Some(code) = details
            .get("stage")
            .and_then(|s| s.get("stageCode"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(drops) = details.get("drops").and_then(Value::as_array) else {
            continue;
        };
        let bucket = per_stage.entry(code.to_string()).or_default();
        for item in drops {
            if let (Some(name), Some(n)) = (
                item.get("itemName").and_then(Value::as_str),
                item.get("quantity").and_then(Value::as_i64),
            ) {
                *bucket.entry(name.to_string()).or_default() += n;
            }
        }
    }

    if per_stage.len() <= 1 {
        // 单一关卡：末帧的 `stats` 就是整趟总账。
        let stats = group
            .last()
            .and_then(|f| f.get("details"))
            .and_then(|d| d.get("stats"))
            .and_then(Value::as_array);
        let items = stats.map(|s| stats_text(s)).unwrap_or_default();
        return if items.is_empty() {
            Vec::new()
        } else {
            vec![format!("掉落（累计）：{items}。")]
        };
    }

    per_stage
        .into_iter()
        .map(|(code, items)| {
            let body: Vec<String> = items.iter().map(|(n, q)| format!("{n} ×{q}")).collect();
            format!("{code}：{body}。", body = body.join("、"))
        })
        .collect()
}

/// `stats` 数组 → 「名字 ×数量」列表。
fn stats_text(stats: &[Value]) -> String {
    stats
        .iter()
        .filter_map(|it| {
            let name = it.get("itemName")?.as_str()?;
            let n = it.get("quantity").and_then(Value::as_i64)?;
            Some(format!("{name} ×{n}"))
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 剿灭：`annihilation_weekly_process` 是 `[已完成数, 上限]`，只有剿灭关卡有。
fn annihilation_text(group: &[&Value]) -> Option<String> {
    let progress = group
        .iter()
        .rev()
        .find_map(|f| f.get("details")?.get("annihilation_weekly_process"))?;
    let arr = progress.as_array()?;
    Some(format!(
        "本周剿灭进度：{}/{cap}。",
        arr.first()?.as_i64()?,
        cap = arr.get(1)?.as_i64()?
    ))
}

/// Recruit：两个数合成一句，高星单独列；一个位子都没处理时按入参分两种说法。
fn recruit_lines(by_kind: &mut BTreeMap<&str, Vec<&Value>>, params: &Value) -> Vec<String> {
    let results = by_kind.remove("RecruitResult").unwrap_or_default();
    let refreshed = by_kind.remove("RecruitTagsRefreshed").unwrap_or_default();
    by_kind.remove("RecruitTagsDetected");
    by_kind.remove("RecruitTagsSelected");

    let mut lines = Vec::new();
    if results.is_empty() {
        let times = params.get("times").and_then(Value::as_i64).unwrap_or(0);
        lines.push(if times > 0 {
            format!("本次要开 {times} 个位子，但没有可招募的空位。")
        } else {
            "本次只领取已完成的招募（times 为 0）。".to_string()
        });
        return lines;
    }

    let mut head = format!("共招募 {} 次", results.len());
    if !refreshed.is_empty() {
        head.push_str(&format!("；刷新标签 {} 次", refreshed.len()));
    }
    head.push('。');
    lines.push(head);

    // 「已招」看入参的确认星级门槛；门槛没给就不加这个标记，不替 MAA 猜。
    let confirm: Vec<i64> = params
        .get("confirm")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let mut high: Vec<String> = Vec::new();
    for frame in &results {
        let Some(details) = frame.get("details") else {
            continue;
        };
        let Some(level) = details.get("level").and_then(Value::as_i64) else {
            continue;
        };
        if level < 4 {
            continue;
        }
        let tags = details
            .get("result")
            .and_then(Value::as_array)
            .and_then(|result| {
                result
                    .iter()
                    .find(|r| r.get("level").and_then(Value::as_i64) == Some(level))
            })
            .and_then(|best| best.get("tags"))
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" + ")
            })
            .unwrap_or_default();
        let mut item = if tags.is_empty() {
            format!("{level} 星")
        } else {
            format!("{level} 星「{tags}」")
        };
        if !confirm.is_empty() {
            item.push_str(if confirm.contains(&level) {
                "（已招）"
            } else {
                "（未招）"
            });
        }
        high.push(item);
    }
    if !high.is_empty() {
        lines.push(format!("高星：{}。", high.join("、")));
    }
    lines
}

/// Infrast：模式声明 + 换了哪些设施 + 进驻确认。
fn infrast_lines(by_kind: &mut BTreeMap<&str, Vec<&Value>>, params: &Value) -> Vec<String> {
    let facilities = by_kind.remove("EnterFacility");
    let confirms = by_kind.remove("InfrastConfirmButton");
    let short = by_kind.remove("NotEnoughStaff");

    let mut lines = Vec::new();
    match params.get("mode").and_then(Value::as_i64) {
        Some(10000) => lines.push("按自定义排班执行。".to_string()),
        Some(20000) => lines.push("按一键轮换执行。".to_string()),
        _ => {}
    }
    if let Some(group) = &facilities {
        lines.push(facility_text(group));
    }
    if let Some(group) = &confirms {
        lines.push(format!(
            "期间有 {} 次进驻确认（目标干员已在其他设施，MAA 已自动确认）。",
            group.len()
        ));
    }
    if let Some(group) = &short {
        lines.push(format!("人手不足 {} 次。", group.len()));
    }
    lines
}

/// Award：只报**明确打开**的子项；一个都没写就不报（默认项是 MAA 的事）。
fn award_lines(params: &Value) -> Vec<String> {
    const ITEMS: &[(&str, &str)] = &[
        ("award", "每日/每周奖励"),
        ("mail", "邮件"),
        ("recruit", "公招"),
        ("orundum", "源石"),
        ("mining", "采矿"),
        ("specialaccess", "特别访问"),
        ("signinevent", "签到活动"),
    ];
    let on: Vec<&str> = ITEMS
        .iter()
        .filter(|(key, _)| params.get(*key).and_then(Value::as_bool) == Some(true))
        .map(|(_, label)| *label)
        .collect();
    if on.is_empty() {
        return Vec::new();
    }
    vec![format!("本次执行：{}。", on.join("、"))]
}

/// Roguelike：存钱投入与存款 / 结算结局 / 停止原因（不报上限数字）。
fn roguelike_lines(by_kind: &mut BTreeMap<&str, Vec<&Value>>, lines: &mut Vec<String>) {
    let investments = by_kind.remove("RoguelikeInvestment");
    let settlement = by_kind.remove("RoguelikeSettlement");
    let full = by_kind.remove("RoguelikeInvestmentReachFull");
    let limit = by_kind.remove("RoguelikeInvestmentReachLimit");

    if let Some(group) = &investments {
        if let Some(text) = extract_known("RoguelikeInvestment", group.last().copied()) {
            lines.push(text);
        }
    }
    if let Some(group) = &settlement {
        if let Some(pass) = last_field(group, "game_pass").and_then(Value::as_bool) {
            lines.push(if pass {
                "这一局通关了。".to_string()
            } else {
                "这一局没通关。".to_string()
            });
        }
    }
    if full.is_some() || limit.is_some() {
        lines.push("结束原因：存款已到上限，MAA 停手。".to_string());
    }
}

/// 已知类型的字段提取。只读文档写明、且真机见过的字段；缺失就给 `None`。
fn extract_known(kind: &str, frame: Option<&Value>) -> Option<String> {
    let details = frame?.get("details")?;
    match kind {
        // `all_opers` 含当前客户端拿不到的条目（未实装），只报 `own_opers`——那才是「拥有」的真话。
        "OperBoxInfo" => {
            let own = details.get("own_opers")?.as_array()?.len();
            Some(format!("持有 {own} 名干员。"))
        }
        // 仓库结果在 `data` 这个 JSON 字符串里：`{"物品ID": 数量, ...}`。
        "DepotInfo" => {
            let data = details.get("data")?.as_str()?;
            let parsed: Value = serde_json::from_str(data).ok()?;
            Some(format!("仓库 {} 种物品。", parsed.as_object()?.len()))
        }
        // 掉落：`stats` 是整趟累计（末帧即总账）。`stars` 不进返回（默认 3 星无信息量）。
        "StageDrops" => {
            let stats = details.get("stats")?.as_array()?;
            let items: Vec<String> = stats
                .iter()
                .filter_map(|it| {
                    let name = it.get("itemName")?.as_str()?;
                    let n = it.get("quantity").and_then(Value::as_i64)?;
                    Some(format!("{name} ×{n}"))
                })
                .collect();
            Some(format!("掉落（累计）：{}。", items.join("、")))
        }
        "SwitchThemeNotFound" => {
            let theme = details.get("theme")?.as_str()?;
            Some(format!("没找到这个主题：「{theme}」。"))
        }
        "SwitchThemeSkipped" => Some("本次没有候选主题，已跳过。".to_string()),
        "AccountSwitch" => {
            let name = details.get("account_name")?.as_str()?;
            Some(format!("已切换到账号「{name}」。"))
        }
        // `count` 是本次、`total` 是跨局累计、`deposit` 是当前存款。
        "RoguelikeInvestment" => {
            let count = details.get("count").and_then(Value::as_i64)?;
            let deposit = details.get("deposit").and_then(Value::as_i64)?;
            Some(format!("本次投入 {count} 枚源石锭，当前存款 {deposit}。"))
        }
        // 视频识别的产物：`Finished{filename}` 直接给路径（它是产物，不是存档）。
        "Finished" => {
            let file = details.get("filename")?.as_str()?;
            Some(format!("作业已生成：{file}。"))
        }
        "CreditFullOnlyBuyDiscount" => Some("信用点已满，只买了折扣商品。".to_string()),
        _ => None,
    }
}

/// 任务序列的短摘要（展示切片用，最多列前三个）。
fn summarize_tasks(tasks: &[Value]) -> String {
    let names: Vec<String> = tasks
        .iter()
        .map(|task| {
            let type_ = task.get("type").and_then(Value::as_str).unwrap_or("?");
            match task
                .get("params")
                .and_then(|p| p.get("stage"))
                .and_then(Value::as_str)
            {
                Some(stage) => format!("{type_}({stage})"),
                None => type_.to_string(),
            }
        })
        .collect();
    if names.len() <= 3 {
        names.join(" → ")
    } else {
        format!("{} 等 {} 个任务", names[..3].join(" → "), names.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_state_picks_the_newest_action() {
        let tasks = vec![json!({ "task_id": 7, "type": "Fight" })];
        // 最新一条没有动作名 → 往回找，找到 AutoFight，并带上队列里的任务类型。
        let frames = vec![
            json!({ "event": "maa", "details": { "what": "AutoFight", "taskid": 7 } }),
            json!({ "event": "maa", "details": { "taskchain": 1 } }),
        ];
        assert_eq!(
            current_state(&frames, Some(tasks.as_slice())).as_deref(),
            Some("Fight（AutoFight）")
        );
    }

    #[test]
    fn current_state_reports_a_recognition_frame_by_kind() {
        let frames = vec![json!({ "event": "recognition", "kind": "StageDrops" })];
        assert_eq!(current_state(&frames, None).as_deref(), Some("StageDrops"));
    }

    #[test]
    fn current_state_says_nothing_when_the_window_has_no_action() {
        let frames = vec![json!({ "event": "maa", "details": {} })];
        assert_eq!(current_state(&frames, None), None);
        assert_eq!(current_state(&[], None), None);
    }

    #[test]
    fn split_events_separates_recognition_from_progress() {
        let frames = vec![
            json!({ "event": "maa", "code": 3, "name": "AllTasksCompleted" }),
            json!({ "event": "recognition", "kind": "OperBoxInfo", "details": {} }),
            json!({ "event": "recognition", "kind": "DepotInfo", "details": {} }),
        ];
        let (recognition, progress) = split_events(frames);
        assert_eq!(recognition.len(), 2);
        assert_eq!(progress.len(), 1);
        assert_eq!(recognition[0]["kind"], json!("OperBoxInfo"));
    }

    #[test]
    fn summarize_tasks_lists_short_and_counts_long() {
        let short = vec![json!({ "type": "StartUp" }), json!({ "type": "Fight", "params": { "stage": "1-7" } })];
        assert_eq!(summarize_tasks(&short), "StartUp → Fight(1-7)");

        let long = vec![
            json!({ "type": "StartUp" }),
            json!({ "type": "Fight" }),
            json!({ "type": "Recruit" }),
            json!({ "type": "Mall" }),
        ];
        assert_eq!(summarize_tasks(&long), "StartUp → Fight → Recruit 等 4 个任务");
    }

    #[test]
    fn stage_drops_summary_uses_cumulative_stats() {
        // 取自 2026-10-06 真机第一帧（Fight 1-7 ×1），字段就是实际回来的样子。
        let frame = json!({
            "event": "recognition",
            "kind": "StageDrops",
            "details": {
                "drops": [{"dropType":"NORMAL_DROP","itemId":"30012","itemName":"固源岩","quantity":1}],
                "stage": {"stageCode":"1-7","stageId":"main_01-07"},
                "stars": 3,
                "stats": [
                    {"addQuantity":72,"itemId":"4001","itemName":"龙门币","quantity":72},
                    {"addQuantity":1,"itemId":"30041","itemName":"异铁碎片","quantity":1}
                ]
            }
        });
        assert_eq!(
            extract_known("StageDrops", Some(&frame)).unwrap(),
            "掉落（累计）：龙门币 ×72、异铁碎片 ×1。"
        );
    }

    #[test]
    fn operbox_summary_reports_only_owned() {
        let frame = json!({
            "event": "recognition",
            "kind": "OperBoxInfo",
            "details": { "done": true, "all_opers": vec![0; 460], "own_opers": vec![0; 429] }
        });
        let out = extract_known("OperBoxInfo", Some(&frame)).unwrap();
        assert_eq!(out, "持有 429 名干员。");
        assert!(!out.contains("460"), "不把 MAA 表内的行数当总数");
    }

    #[test]
    fn depot_summary_counts_items_from_the_json_string() {
        let frame = json!({
            "event": "recognition",
            "kind": "DepotInfo",
            "details": { "done": true, "data": "{\"2001\":18000,\"31043\":317}" }
        });
        assert_eq!(
            extract_known("DepotInfo", Some(&frame)).unwrap(),
            "仓库 2 种物品。"
        );
    }

    fn fake_outcome(ids: Vec<i64>, completed: bool, wait: u64) -> manager::SubmitOutcome {
        manager::SubmitOutcome {
            task_ids: ids,
            connected: manager::ConnectedInfo {
                emulator: "MuMu".into(),
                address: "127.0.0.1:16384".into(),
                config: "{}".into(),
                extras: true,
                screencap: None,
                events: Vec::new(),
            },
            events: Vec::new(),
            completed,
            waited_secs: wait,
            elapsed_ms: 42_400,
        }
    }

    #[test]
    fn dispatch_returns_only_the_dispatch_line() {
        let tasks = vec![json!({ "type": "Fight" })];
        let out = fake_outcome(vec![7], false, 0);
        let (_, text) = render_run(&out, &tasks, &[], 0);
        assert_eq!(text, "已派出：1 个任务在后台执行（task_id 7）。");
    }

    #[test]
    fn recognition_single_task_puts_the_count_under_a_result_line() {
        let tasks = vec![json!({ "type": "OperBox" })];
        let out = fake_outcome(vec![1], true, 180);
        let events = vec![json!({
            "event": "recognition", "kind": "OperBoxInfo",
            "details": { "own_opers": vec![0; 3] }, "taskid": 1
        })];
        let (_, text) = render_run(&out, &tasks, &events, 180);
        assert_eq!(text, "完成：已跑完，耗时 42.4s。\n结果：持有 3 名干员。");
    }

    #[test]
    fn hitting_the_deadline_says_the_result_will_still_change() {
        let tasks = vec![json!({ "type": "OperBox" })];
        let out = fake_outcome(vec![1], false, 180);
        let events = vec![json!({
            "event": "recognition", "kind": "OperBoxInfo",
            "details": { "own_opers": vec![0; 2] }, "taskid": 1
        })];
        let (_, text) = render_run(&out, &tasks, &events, 180);
        assert!(text.starts_with("仍在跑："), "{text}");
        assert!(text.contains("结果：持有 2 名干员（未完成）。"), "{text}");
    }

    #[test]
    fn a_queue_gives_one_row_per_task() {
        let tasks = vec![json!({ "type": "Depot" }), json!({ "type": "OperBox" })];
        let out = fake_outcome(vec![1, 2], true, 300);
        let events = vec![
            json!({ "event": "recognition", "kind": "DepotInfo",
                    "details": { "data": "{\"2001\":18000}" }, "taskid": 1 }),
            json!({ "event": "recognition", "kind": "OperBoxInfo",
                    "details": { "own_opers": vec![0; 3] }, "taskid": 2 }),
        ];
        let (_, text) = render_run(&out, &tasks, &events, 300);
        assert!(text.starts_with("完成：队列 2 个任务，耗时 42.4s。"), "{text}");
        assert!(text.contains("- Depot：仓库 1 种物品。"), "{text}");
        assert!(text.contains("- OperBox：持有 3 名干员。"), "{text}");
    }

    #[test]
    fn a_failed_task_is_counted_in_the_status_line() {
        let tasks = vec![json!({ "type": "Depot" }), json!({ "type": "OperBox" })];
        let out = fake_outcome(vec![1, 2], true, 300);
        let events = vec![
            json!({ "event": "maa", "name": "TaskChainError", "details": { "taskid": 2 } }),
            json!({ "event": "recognition", "kind": "DepotInfo",
                    "details": { "data": "{\"2001\":18000}" }, "taskid": 1 }),
        ];
        let (_, text) = render_run(&out, &tasks, &events, 300);
        assert!(text.contains("完成：队列 2 个任务中 1 个失败"), "{text}");
        assert!(text.contains("- OperBox：失败。"), "{text}");
    }

    #[test]
    fn chain_outcomes_merge_by_taskid() {
        let frames = vec![
            json!({ "event": "maa", "name": "TaskChainStart", "details": { "taskid": 3 } }),
            json!({ "event": "maa", "name": "TaskChainCompleted", "details": { "taskid": 3 } }),
            json!({ "event": "maa", "name": "TaskChainError", "details": { "taskid": 4 } }),
            json!({ "event": "maa", "name": "SubTaskStart", "details": { "taskid": 5 } }),
        ];
        let map = chain_outcomes(&frames);
        assert_eq!(map.get(&3), Some(&ChainState::Completed));
        assert_eq!(map.get(&4), Some(&ChainState::Error));
        assert_eq!(map.get(&5), None);
    }

    #[test]
    fn infrast_facilities_are_summed_in_chinese() {
        let frames = vec![
            json!({ "kind": "EnterFacility", "details": { "facility": "Mfg", "index": 0 } }),
            json!({ "kind": "EnterFacility", "details": { "facility": "Mfg", "index": 1 } }),
            json!({ "kind": "EnterFacility", "details": { "facility": "Trade", "index": 0 } }),
        ];
        assert_eq!(
            conclusions_for("Infrast", &json!({}), &frames),
            vec!["已换班：制造站 ×2、贸易站 ×1。".to_string()]
        );
    }

    /// 真帧（`latest-RecruitResult.jsonl`）里，保底星级就是 `details.level`，
    /// 对应那一档的 `tags` 就叫得出高星的来路。
    #[test]
    fn recruit_reports_the_high_star_combination() {
        let frames = vec![json!({
            "event": "recognition", "kind": "RecruitResult",
            "details": {
                "level": 4,
                "result": [
                    { "level": 3, "tags": ["近战位"], "opers": [] },
                    { "level": 4, "tags": ["爆发"], "opers": [] },
                ],
            },
        })];
        let lines = conclusions_for("Recruit", &json!({}), &frames);
        assert!(lines.contains(&"共招募 1 次。".to_string()), "{lines:?}");
        assert!(lines.contains(&"高星：4 星「爆发」。".to_string()), "{lines:?}");
    }

    #[test]
    fn a_process_frame_does_not_show_up_as_a_conclusion() {
        let frames = vec![json!({
            "event": "recognition", "kind": "ProductOfFacility",
            "details": { "facility": "Trade", "index": 1, "product": "Money" },
        })];
        assert!(conclusions_for("Infrast", &json!({}), &frames).is_empty());
    }

    #[test]
    fn last_run_is_labelled_by_task_type_not_node_name() {
        let kept = |kind: &str, chain: Option<&str>| crate::maa::records::Kept {
            kind: kind.to_string(),
            chain: chain.map(str::to_string),
            path: std::path::PathBuf::from("x"),
            frames: 1,
            seen: 1,
        };
        // 同一条链下的多个识别种类 → 只报一次类型名。
        assert_eq!(
            run_label(&[
                kept("EnterFacility", Some("Infrast")),
                kept("ProductOfFacility", Some("Infrast")),
            ]),
            "Infrast"
        );
        assert_eq!(
            run_label(&[
                kept("StageDrops", Some("Fight")),
                kept("OperBoxInfo", Some("OperBox")),
            ]),
            "Fight、OperBox"
        );
        // 老记录没有 chain → 退回识别种类。
        assert_eq!(run_label(&[kept("StageDrops", None)]), "StageDrops");
    }

    #[test]
    fn a_single_step_reports_what_the_params_asked_for() {
        let tasks = vec![json!({
            "type": "SingleStep",
            "params": { "type": "copilot", "subtype": "stage", "details": { "stage_name": "1-7" } }
        })];
        let out = fake_outcome(vec![1], true, 300);
        let (_, text) = render_run(&out, &tasks, &[], 300);
        assert_eq!(text, "单步：已设置关卡 1-7。");
    }

    #[test]
    fn a_single_step_action_names_the_operator_and_direction() {
        let params = json!({
            "type": "copilot", "subtype": "action",
            "details": { "name": "史尔特尔", "location": [4, 5], "direction": "左" }
        });
        assert_eq!(step_core(&params).unwrap(), "单步：已执行动作（史尔特尔 → 左）");
    }

    #[test]
    fn a_queue_row_falls_back_to_its_headline_core() {
        // 启动/关闭没有自己的结果帧，队列行就靠「那一句」撑着。
        let tasks = vec![json!({ "type": "StartUp" }), json!({ "type": "CloseDown" })];
        let out = fake_outcome(vec![1, 2], true, 150);
        let (_, text) = render_run(&out, &tasks, &[], 150);
        assert!(text.contains("- StartUp：游戏已启动。"), "{text}");
        assert!(text.contains("- CloseDown：游戏已关闭。"), "{text}");
    }

    #[test]
    fn fight_headline_carries_stage_and_count_with_the_sanity_line() {
        let tasks = vec![json!({ "type": "Fight", "params": { "stage": "1-7" } })];
        let out = fake_outcome(vec![1], true, 300);
        let events = vec![
            json!({ "event": "recognition", "kind": "FightTimes", "taskid": 1,
                    "details": { "times_finished": 2, "series": 2, "sanity_cost": 12 } }),
            json!({ "event": "recognition", "kind": "SanityBeforeStage", "taskid": 1,
                    "details": { "current_sanity": 210, "max_sanity": 135 } }),
        ];
        let (_, text) = render_run(&out, &tasks, &events, 300);
        assert_eq!(
            text,
            "1-7 打完了 2 场，耗时 42.4s。\n按 2 倍代理执行，消耗理智 12（开打前 210）。"
        );
    }

    #[test]
    fn multi_stage_drops_are_grouped_by_stage_code() {
        let frames = vec![
            json!({ "kind": "StageDrops", "details": {
                "stage": { "stageCode": "SS-1" },
                "drops": [ { "itemName": "龙门币", "quantity": 10 } ] } }),
            json!({ "kind": "StageDrops", "details": {
                "stage": { "stageCode": "SS-2" },
                "drops": [ { "itemName": "固源岩", "quantity": 1 } ] } }),
        ];
        assert_eq!(
            conclusions_for("Fight", &json!({}), &frames),
            vec!["SS-1：龙门币 ×10。".to_string(), "SS-2：固源岩 ×1。".to_string()]
        );
    }

    #[test]
    fn recruit_without_any_slot_says_which_case_it_was() {
        assert_eq!(
            conclusions_for("Recruit", &json!({ "times": 4 }), &[]),
            vec!["本次要开 4 个位子，但没有可招募的空位。".to_string()]
        );
        assert_eq!(
            conclusions_for("Recruit", &json!({ "times": 0 }), &[]),
            vec!["本次只领取已完成的招募（times 为 0）。".to_string()]
        );
    }

    #[test]
    fn recruit_marks_whether_the_confirm_threshold_took_it() {
        let frames = vec![json!({
            "event": "recognition", "kind": "RecruitResult",
            "details": {
                "level": 5,
                "result": [ { "level": 5, "tags": ["资深干员"], "opers": [] } ],
            },
        })];
        // confirm 只含 4/3 → 5 星不进门槛，所以是「未招」。
        let lines = conclusions_for("Recruit", &json!({ "confirm": [4, 3] }), &frames);
        assert!(
            lines.contains(&"高星：5 星「资深干员」（未招）。".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn award_receipt_only_lists_what_is_explicitly_on() {
        assert_eq!(
            conclusions_for("Award", &json!({ "award": true, "mail": true, "mining": false }), &[]),
            vec!["本次执行：每日/每周奖励、邮件。".to_string()]
        );
        assert!(conclusions_for("Award", &json!({}), &[]).is_empty());
    }

    #[test]
    fn results_path_lives_under_data_dir() {
        let base = config::data_dir();
        assert!(records::results_dir().starts_with(&base));
    }

    /// 真机验证：刷关的识别产出（Fight 1-7 ×1）+ 等待路径。
    ///
    /// **会消耗 6 点理智**，会操作游戏。验三件：
    /// 1. `wait > 0` 这条等待路径（至今没在真机跑过）；
    /// 2. `StageDrops` 的真实结构；
    /// 3. 跑完之后任务队列是否清空。
    #[test]
    #[ignore = "真机验证：Fight 1-7 ×1（花 6 理智；会操作游戏）"]
    fn fight_drops_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"Fight","params":{"stage":"1-7","times":1,"medicine":0,"stone":0}}],"wait":360}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => {
                println!("\n=== 工具返回 ===\n{}", out.summary);
                println!("canonical = {}", out.canonical);
            }
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());

        // 结果文件里存的就是帧本身：一行一帧。
        let file = records::results_dir().join("latest-StageDrops.jsonl");
        match std::fs::read_to_string(&file) {
            Ok(body) => {
                let lines: Vec<&str> = body.lines().collect();
                println!("\n=== {}：{} 帧 ===", file.display(), lines.len());
                for (i, line) in lines.iter().enumerate() {
                    let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
                    let details = v.get("details").cloned().unwrap_or(Value::Null);
                    println!("--- 第 {i} 帧（{} 字符）", line.chars().count());
                    match details.as_object() {
                        Some(map) => {
                            for (k, val) in map {
                                match val {
                                    Value::Array(a) => println!(
                                        "  {k}: 数组 {} 项，首项 = {}",
                                        a.len(),
                                        a.first().map(|x| x.to_string()).unwrap_or_default()
                                    ),
                                    other => println!("  {k}: {other}"),
                                }
                            }
                        }
                        None => println!("  details = {details}"),
                    }
                    if i == 0 {
                        let head: String = line.chars().take(600).collect();
                        println!("  首帧原文（前 600 字）= {head}");
                    }
                }
            }
            Err(e) => println!("\n结果文件读不到（{e}）——这一趟没有 StageDrops 产出"),
        }

        // 收尾后可能还有零星帧。
        println!("\n收尾后再取到 {} 帧", crate::maa::manager::drain_events().len());

        // 跑完之后队列清不清空？（一直没验过的未知）
        match crate::maa::manager::queue() {
            Ok(Some(q)) => println!("task_list = {q}"),
            Ok(None) => println!("task_list：没有桥"),
            Err(e) => println!("task_list 失败：{e}"),
        }

        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：`series`（代理倍率）与 `times` 的关系。
    ///
    /// `{ "series": 2, "times": 2 }` 有两种读法，它们在**用时**与**作战次数**上分岔：
    /// - `times` 是**总把数**（本轮判断）→ 一次 2 连战后停：**2 把、12 理智**；
    /// - `times` 是**连战轮数** → 2 轮 × 2 倍 = **4 把、24 理智**。
    ///
    /// 所以：把 `StageDrops` 的帧数与总用时对着看，就能判定是哪种。
    #[test]
    #[ignore = "真机验证：Fight 1-7 series=2 times=2（花 12 理智；会操作游戏）"]
    fn series_times_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"Fight","params":{"stage":"1-7","series":2,"times":2}}],"wait":600}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());

        // StageDrops 是作战结束的产物：帧数与时用一起看，就能判定刷了几把。
        let file = records::results_dir().join("latest-StageDrops.jsonl");
        match std::fs::read_to_string(&file) {
            Ok(body) => {
                let n = body.lines().count();
                println!("\n=== StageDrops 共 {n} 帧 ===");
                for (i, line) in body.lines().enumerate() {
                    let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
                    let stars = v.pointer("/details/stars").cloned().unwrap_or(Value::Null);
                    let drops = v
                        .pointer("/details/drops")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    println!("--- 第 {i} 帧：stars={stars} drops={drops} 项");
                }
            }
            Err(e) => println!("\n结果文件读不到（{e}）"),
        }

        // 直接证据：FightTimes 的 times_finished / finished。
        let events = crate::maa::manager::drain_events();
        println!("\n=== 回调里的进展帧（共 drain 到 {} 帧）===", events.len());
        for ev in &events {
            let s = format!("{ev:?}");
            if s.contains("FightTimes") || s.contains("SanityBeforeStage") {
                let head: String = s.chars().take(300).collect();
                println!("{head}");
            }
        }

        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：公招的日常填法（`select: [5,4]`、`confirm: [4,3]`、`times: 4`、`refresh: true`）。
    ///
    /// 看四件事：是否先领取已完成的；每个槽怎么处理（3★ 空选直接招 / 4★ 点标签再招 /
    /// 5★·6★ 跳过）；只出 3★ 的槽是否刷新；最后开了几个槽。
    #[test]
    #[ignore = "真机验证：Recruit 日常填法（花招聘许可、会刷新标签）"]
    fn recruit_daily_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"Recruit","params":{"select":[5,4],"confirm":[4,3],"times":4,"refresh":true}}],"wait":600}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());
        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：公招「先收菜、再种菜」。
    ///
    /// 前提：4 个槽都是已完成的招募。参数**不带 `refresh`**（把变量降到最少）。
    /// 看三件事：① 是否先领取已完成的；② 领取后是否对新空出的槽识别并确认（`RecruitConfirm`）；
    /// ③ 这一趟到底发了什么（看落盘行数，动作序列事后到 `asst.log` 里看）。
    #[test]
    #[ignore = "真机验证：Recruit 收菜 + 种菜（会消耗招聘许可）"]
    fn recruit_harvest_sow_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"Recruit","params":{"select":[5,4],"confirm":[4,3],"times":4}}],"wait":600}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());

        // 落盘：本趟各类结果文件的行数（动作序列不在文件里，事后到 asst.log 看）。
        let dir = crate::maa::records::results_dir();
        for kind in [
            "RecruitResult",
            "RecruitTagsDetected",
            "RecruitTagsSelected",
            "RecruitTagsRefreshed",
        ] {
            let p = dir.join(format!("latest-{kind}.jsonl"));
            let lines = std::fs::read_to_string(&p)
                .map(|b| b.lines().count())
                .unwrap_or(0);
            println!("  文件 {kind}: {lines} 行");
        }

        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：基建换班的基本填法（`facility` + `drones`）。
    ///
    /// 队列是 StartUp + Infrast：先把游戏唤到主界面，再对制造站、贸易站、会客室换班，
    /// 无人机用在贸易站。
    #[test]
    #[ignore = "真机验证：Infrast 换班（会调动干员、用无人机、领会客室信用）"]
    fn infrast_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"StartUp"},{"type":"Infrast","params":{"facility":["Mfg","Trade","Reception"],"drones":"Money"}}],"wait":600}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());
        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：商城的基本填法（`buy_first` + `blacklist`）。
    ///
    /// 队列是 StartUp + Mall：先把游戏唤到主界面，再拜访好友拿信用，然后按三遍买法花信用。
    #[test]
    #[ignore = "真机验证：Mall（会花信用、拜访好友）"]
    fn mall_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"StartUp"},{"type":"Mall","params":{"buy_first":["龙门币"],"blacklist":[]}}],"wait":600}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());
        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：Award 的默认填法（不填参数，只领每日/每周任务奖励）。
    ///
    /// 队列是 StartUp + Award：先把游戏唤到主界面，再领任务奖励。不动任何消耗。
    #[test]
    #[ignore = "真机验证：Award（只领任务奖励，不动消耗）"]
    fn award_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"StartUp"},{"type":"Award"}],"wait":600}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());
        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：肉鸽存钱（`mode: 1`，萨米主题，后勤分队，存一次）。
    ///
    /// 队列是 StartUp + Roguelike：先把游戏唤到主界面，再开一局萨米、第一层投资完退出。
    #[test]
    #[ignore = "真机验证：Roguelike 存钱（会开一局萨米肉鸽并投资源石锭）"]
    fn roguelike_invest_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"StartUp"},{"type":"Roguelike","params":{"theme":"Sami","mode":1,"squad":"后勤分队","starts_count":1}}],"wait":900}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());
        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：公招的识别产出（`Recruit` 仅计算）。
    ///
    /// `confirm: [-1]` 是**仅计算**（不点击、不领取）。
    /// 注意：`confirm: []` 不是——那会走完整流程，先领取所有已完成的招募。
    /// 空槽里没有标签，所以这一步在空槽上依旧拿不到 `RecruitResult`；
    /// 它验的是“仅计算这条路径确实不碰账号”。
    /// 验的是：`RecruitResult` 等公招标签任务的真实结构。
    #[test]
    #[ignore = "真机验证：Recruit 仅计算（会进公招页并点 tags）"]
    fn recruit_result_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let t0 = std::time::Instant::now();
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"Recruit","params":{"select":[4],"confirm":[-1],"times":0,"set_time":false}}],"wait":240}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("\n=== 工具返回 ===\n{}", out.summary),
        }
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());

        // 这一趟留下了哪些种类（公招可能同时冒出好几种 kind）。
        let kept = crate::maa::manager::run_files();
        println!("\n=== 本次产出 {} 种 ===", kept.len());
        for k in &kept {
            println!(
                "{}：写 {} 帧（见过 {} 帧）→ {}",
                k.kind,
                k.frames,
                k.seen,
                k.path.display()
            );
        }

        for k in &kept {
            if k.kind == "ExceededLimit" {
                continue; // 内部计数通知，Fight 那趟已看过
            }
            let Ok(body) = std::fs::read_to_string(&k.path) else {
                continue;
            };
            let lines: Vec<&str> = body.lines().collect();
            println!("\n--- {}：{} 帧 ---", k.kind, lines.len());
            for (i, line) in lines.iter().enumerate() {
                let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
                let details = v.get("details").cloned().unwrap_or(Value::Null);
                match details.as_object() {
                    Some(map) => {
                        for (key, val) in map {
                            match val {
                                Value::Array(a) => println!(
                                    "  {key}: 数组 {} 项，首项 = {}",
                                    a.len(),
                                    a.first().map(|x| x.to_string()).unwrap_or_default()
                                ),
                                other => println!("  {key}: {other}"),
                            }
                        }
                    }
                    None => println!("  details = {details}"),
                }
                if i == 0 {
                    let head: String = line.chars().take(700).collect();
                    println!("  首帧原文 = {head}");
                }
            }
        }

        match crate::maa::manager::queue() {
            Ok(Some(q)) => println!("\ntask_list = {q}"),
            Ok(None) => println!("\ntask_list：没有桥"),
            Err(e) => println!("\ntask_list 失败：{e}"),
        }

        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：`status` 的真机路径。
    ///
    /// 跑一次**只读的 Depot**（不碰账号，约 5 秒）→ 调 `status` 看它报的“最近一次”；
    /// 再关桥 → 再调一次，确认它**不起桥**。
    #[test]
    #[ignore = "真机验证：status（跑一次只读 Depot，不碰账号）"]
    fn status_report_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let ctx = ToolContext::new();

        // 先跑一次 Depot，让“最近一次”有留痕。
        match MaaTaskTool.execute(
            &ctx,
            r#"{"action":"run","tasks":[{"type":"Depot"}],"wait":120}"#,
        ) {
            Err(e) => println!("Depot 执行失败：{e}"),
            Ok(out) => println!("=== Depot 返回 ===\n{}", out.summary),
        }

        // status：真机路径。
        match MaaTaskTool.execute(&ctx, r#"{"action":"status"}"#) {
            Err(e) => println!("\nstatus 失败：{e}"),
            Ok(out) => {
                println!("\n=== status 返回 ===\n{}", out.summary);
                println!("canonical = {}", out.canonical);
            }
        }

        // 关桥之后再问一次：查询不该把桥拉起来。
        crate::maa::manager::shutdown();
        let view = crate::maa::manager::status_view();
        println!(
            "\n关桥后：bridge_available = {}，connected = {}",
            view.bridge_available, view.connected
        );
        match MaaTaskTool.execute(&ctx, r#"{"action":"status"}"#) {
            Ok(out) => println!("关桥后 status：\n{}", out.summary),
            Err(e) => println!("关桥后 status 失败：{e}"),
        }
        let after = crate::maa::manager::status_view();
        println!(
            "再问之后：bridge_available = {}（仍是 false 就说明查询没起桥）",
            after.bridge_available
        );

        crate::maa::manager::shutdown();
        println!("已关桥");
    }

    /// 真机验证：B 服下 `CloseDown` + `StartUp` 的端到端。
    ///
    /// **会关掉再打开游戏**。验的是 `client_type` 注入是否真的落到客户端选择上：
    /// 日志里该出现 `com.hypergryph.arknights.bilibili`（B 服包名），而非官服包名。
    #[test]
    #[ignore = "真机验证：CloseDown + StartUp（会关掉再打开游戏）"]
    fn client_type_end_to_end_probe() {
        crate::maa::manager::set_settings(crate::maa::settings::MaaSettings {
            server: crate::maa::settings::Server::Bilibili,
            ..Default::default()
        });
        let ctx = ToolContext::new();
        let params = r#"{"action":"run","tasks":[{"type":"CloseDown"},{"type":"StartUp","params":{"start_game_enabled":true}}],"wait":300}"#;
        match MaaTaskTool.execute(&ctx, params) {
            Err(e) => println!("执行失败：{e}"),
            Ok(out) => println!("=== 工具返回 ===\n{}", out.summary),
        }
        crate::maa::manager::shutdown();
        println!("已关桥");
    }
}
