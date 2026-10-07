//! MaaCore 回调的接收与归一化。
//!
//! C 回调可能从**任意线程**进入。这里只做两件事：立即把 `details_json`
//! 拷成 Rust 字符串、塞进无界 channel。**绝不在回调线程里调用 MaaCore**（会死锁）。

use std::ffi::{c_char, c_void, CStr};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use serde_json::Value;

use crate::ffi::AsstMsgId;
use crate::ipc::Sink;

/// 随 `custom_arg` 传出的回调上下文（本桥里只承载一个发送端）。
pub struct CallbackCtx {
    tx: Sender<RawMsg>,
}

pub struct RawMsg {
    pub msg: AsstMsgId,
    pub details: Option<String>,
}

/// 与 `ASST_CALL`（Windows 上 `__stdcall`）对齐，故用 `extern "system"`。
pub unsafe extern "system" fn on_msg(
    msg: AsstMsgId,
    details_json: *const c_char,
    custom_arg: *mut c_void,
) {
    if custom_arg.is_null() {
        return;
    }
    let ctx = &*(custom_arg as *const CallbackCtx);
    let details = if details_json.is_null() {
        None
    } else {
        Some(CStr::from_ptr(details_json).to_string_lossy().into_owned())
    };
    // 无界 channel：send 不阻塞，回调线程立即返回。
    let _ = ctx.tx.send(RawMsg { msg, details });
}

pub fn channel() -> (CallbackCtx, Receiver<RawMsg>) {
    let (tx, rx) = mpsc::channel();
    (CallbackCtx { tx }, rx)
}

/// 消费回调并转成 `maa` 事件推给 PRTS。原始 `details` 保真透传为唯一事实来源。
///
/// 另做一层**薄的识别路由**：当回调是结构化识别结果（`SubTaskExtraInfo` 且 `what` 命中
/// 已知列表）时，额外发一条 `recognition` 事件。只做路由、不解释 payload，
/// 以降低随 MAA 版本漂移的风险。
pub fn spawn_forwarder(rx: Receiver<RawMsg>, sink: Arc<Sink>) {
    std::thread::spawn(move || {
        for m in rx {
            let parsed: Value = m
                .details
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            let payload = serde_json::json!({
                "event": "maa",
                "code": m.msg,
                "name": msg_name(m.msg),
                "details": &parsed,
                "raw": m.details,
            });
            let _ = sink.emit(&payload);

            if m.msg == SUBTASK_EXTRA_INFO {
                if let Some(what) = parsed.get("what").and_then(Value::as_str) {
                    if RECOGNITION_KINDS.contains(&what) {
                        let _ = sink.emit(&serde_json::json!({
                            "event": "recognition",
                            "kind": what,
                            "details": parsed.get("details").cloned().unwrap_or(Value::Null),
                            "taskchain": parsed.get("taskchain").cloned().unwrap_or(Value::Null),
                            "taskid": parsed.get("taskid").cloned().unwrap_or(Value::Null),
                            "uuid": parsed.get("uuid").cloned().unwrap_or(Value::Null),
                        }));
                    }
                }
            }
        }
    });
}

/// 承载「结构化识别结果」的回调码（`SubTaskExtraInfo`）。
const SUBTASK_EXTRA_INFO: AsstMsgId = 20003;

/// 已知的结构化结果 `what`。
///
/// **按源码重建**：MaaCore 里两种写法——`basic_info_with_what("X")` 75 种与
/// `cb_info["what"] = "X"` 14 种，并集 89 种。升级 MAA 时用同两条 grep 复核：
/// `basic_info_with_what\("([A-Za-z0-9_]+)"\)` 与 `\["what"\]\s*=\s*"([A-Za-z0-9_]+)"`。
/// 仅作路由；官方明言回调飞速迭代，`maa` 原始事件才是事实来源。
const RECOGNITION_KINDS: &[&str] = &[
    "AccountSwitch",
    "AutoRaisePotentialProgress",
    "AutoRaisePotentialTotal",
    "BattleFormation",
    "BattleFormationOperUnavailable",
    "BattleFormationParseFailed",
    "BattleFormationSelected",
    "BlackFlowInventoryCleanup",
    "BlackFlowStrategyResult",
    "BlackFlowStrategyStarted",
    "BoskyPassageNode",
    "ClassifyDirection",
    "CompSkill",
    "CopilotAction",
    "CopilotListLoadTaskFileSuccess",
    "CreditFullOnlyBuyDiscount",
    "CustomInfrastRoomGroupsMatch",
    "CustomInfrastRoomGroupsMatchFailed",
    "CustomInfrastRoomOperators",
    "DeepExplorationCompleted",
    "DepotInfo",
    "DetectOperators",
    "EncounterOcrError",
    "EnterFacility",
    "ExceededLimit",
    "FacilityLayoutRecognitionFailed",
    "FightTimes",
    "Finished",
    "FoldartalGainOcrNextLevel",
    "InfrastConfirmButton",
    "InfrastTrainingCompleted",
    "InfrastTrainingIdle",
    "InfrastTrainingTimeLeft",
    "MatchDeployment",
    "MonthlySquadCompleted",
    "NotEnoughStaff",
    "OcrFormation",
    "OcrStage",
    "OperBoxInfo",
    "OperProgressDetail",
    "OperProgressSummary",
    "PixelPaintProgress",
    "ProductChanged",
    "ProductChangeFail",
    "ProductIncorrect",
    "ProductOfFacility",
    "Reconnected",
    "RecruitError",
    "RecruitNoPermit",
    "RecruitPermitCountRecognitionFailed",
    "RecruitPermitReserved",
    "RecruitPreservedTag",
    "RecruitResult",
    "RecruitSpecialTag",
    "RecruitTagsDetected",
    "RecruitTagsRefreshed",
    "RecruitTagsSelected",
    "ResolutionInfo",
    "RoguelikeCollapsalParadigms",
    "RoguelikeCombatEnd",
    "RoguelikeCoppersExchangeInfo",
    "RoguelikeCoppersRecognitionError",
    "RoguelikeEncounterOptions",
    "RoguelikeEvent",
    "RoguelikeEventSelected",
    "RoguelikeInvestment",
    "RoguelikeInvestmentReachFull",
    "RoguelikeInvestmentReachLimit",
    "RoguelikeJieGardenTargetFound",
    "RoguelikeSettlement",
    "SanityBeforeStage",
    "SideStoryReopenTaskNotExists",
    "Slice",
    "SSSGamePass",
    "SSSSettlement",
    "SSSStage",
    "StageDrops",
    "StageInfo",
    "StageInfoError",
    "StageQueueMissionCompleted",
    "StageQueueStageNotFound",
    "StageQueueUnableToAgent",
    "SwitchThemeNotFound",
    "SwitchThemeSkipped",
    "UnsupportedLevel",
    "UnsupportedPlayTools",
    "UnsupportedResolution",
    "UseMedicine",
    "UserAdditionalOperInvalid",
];

/// 消息码 → 名称。取自 `docs/zh-cn/protocol/callback-schema.md` 的 `AsstMsg` 枚举。
///
/// 官方明言「回调随版本飞速迭代」，故此表仅作**提示**用途；`code` 与 `raw`
/// 才是事实来源，PRTS 侧不应依赖此名称做逻辑分支。
pub fn msg_name(code: AsstMsgId) -> &'static str {
    match code {
        0 => "InternalError",
        1 => "InitFailed",
        2 => "ConnectionInfo",
        3 => "AllTasksCompleted",
        4 => "AsyncCallInfo",
        5 => "Destroyed",
        10000 => "TaskChainError",
        10001 => "TaskChainStart",
        10002 => "TaskChainCompleted",
        10003 => "TaskChainExtraInfo",
        10004 => "TaskChainStopped",
        20000 => "SubTaskError",
        20001 => "SubTaskStart",
        20002 => "SubTaskCompleted",
        20003 => "SubTaskExtraInfo",
        20004 => "SubTaskStopped",
        30000 => "ReportRequest",
        _ => "Unknown",
    }
}
