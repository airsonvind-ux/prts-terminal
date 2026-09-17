//! 可观测性模块 —— 开发期调试宏 + 会话日志遥测投影
//!
//! ## 用法
//!
//! ```text
//! # PowerShell
//! $env:PRTS_DEBUG = "1"
//! npx tauri dev
//! ```
//!
//! 开启后：
//! - `debug!` 宏：每次调度循环、工具调用、数据库查询输出单行 JSON 到 stderr。
//! - `log_session`：每次 turn 结束后，把 session log（模块 1 事实来源）的
//!   遥测相关事件以 JSON Lines 输出到 stderr——遥测 = log 的投影，
//!   调试开关是过滤/采样器（架构升级 · 模块 8 P0：遥测挂 log）。
//!
//! 每条日志自动附加 `+` 字段（自进程启动的已过毫秒数），用于快速定位耗时来源。
//!
//! `trace!` 宏由 `PRTS_TRACE=1` 控制，本期仅定义，不调用。
//! 留待上下文衰减实现时补消息级日志。

use std::sync::OnceLock;
use std::time::Instant;

use crate::session::{SessionEvent, SessionLog};

/// 进程启动时间。所有 debug!/trace! 的 `+` 字段相对此基准。
pub(crate) fn start_time() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// 遥测事件投影：从 session log 中过滤出遥测相关事件（JSON Lines）。
///
/// 纯函数（不读环境变量），便于测试。log 是事实来源，投影只取子集：
/// turn/step 边界（step/start 含请求前估算、step/end 含 provider usage——
/// 双锚对照）、工具调用/结果、最终聚合、错误。
/// chunk 级事件不投影（量大，可从 assistant/message 聚合重建）。
fn project_telemetry(log: &SessionLog) -> Vec<String> {
    log.events()
        .iter()
        .filter_map(|event| {
            let include = matches!(
                event,
                SessionEvent::TurnStart { .. }
                    | SessionEvent::TurnEnd { .. }
                    | SessionEvent::StepStart { .. }
                    | SessionEvent::StepEnd { .. }
                    | SessionEvent::ToolCall { .. }
                    | SessionEvent::ToolResult { .. }
                    | SessionEvent::AssistantMessage { .. }
                    | SessionEvent::Error { .. }
            );
            if include {
                serde_json::to_string(event).ok()
            } else {
                None
            }
        })
        .collect()
}

/// 会话日志遥测投影：PRTS_DEBUG=1 时输出到 stderr（模块 8 P0：遥测挂 log）。
///
/// 调试开关在这里的角色是**过滤/采样**——log 本身始终完整记录，
/// 开关只决定投影是否输出。turn 结束后由调度中枢调用。
pub fn log_session(session_id: &str, log: &SessionLog) {
    if std::env::var("PRTS_DEBUG").as_deref() != Ok("1") {
        return;
    }
    let lines = project_telemetry(log);
    if lines.is_empty() {
        return;
    }
    eprintln!("[session:{session_id}] telemetry:");
    for line in lines {
        eprintln!("  {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::TokenUsage;
    use crate::session::{ChunkKind, TurnEndReason};

    #[test]
    fn project_telemetry_filters_chunks_keeps_boundaries() {
        let mut log = SessionLog::new();
        log.append(SessionEvent::turn_start("s1"));
        log.append(SessionEvent::user_message("s1", "你好"));
        log.append(SessionEvent::step_start("s1", 1, 100));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "片段"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Reasoning, "思考"));
        log.append(SessionEvent::step_end(
            "s1",
            1,
            Some(TokenUsage { input_tokens: 10, output_tokens: 5 }),
            12,
        ));
        log.append(SessionEvent::turn_end("s1", TurnEndReason::Completed));

        let lines = project_telemetry(&log);
        // turn/start、step/start、step/end、turn/end；chunk/user 被过滤
        assert_eq!(lines.len(), 4, "{lines:#?}");
        // 全部为合法 JSON
        for l in &lines {
            serde_json::from_str::<serde_json::Value>(l).unwrap();
        }
        assert!(lines.iter().all(|l| {
            !l.contains("assistant/chunk") && !l.contains("user/message")
        }));
        // step/start 携带请求前估算（双锚 estimate 兜底锚）
        assert!(lines
            .iter()
            .any(|l| l.contains("\"type\":\"step/start\"") && l.contains("\"estimatedTokens\":100")));
        // step/end 携带 provider usage（基线锚）
        assert!(lines
            .iter()
            .any(|l| l.contains("\"type\":\"step/end\"") && l.contains("\"inputTokens\":10")));
    }
}

/// 调试日志宏。
///
/// `PRTS_DEBUG=1` 时输出单行 JSON 到 stderr。
/// 环境变量未设置时展开为空语句块，运行时仅一次 env var 检查。
///
/// # 输出格式
///
/// ```text
/// {"+":"2657ms","round":1,"action":"llm_start"}
/// ```
///
/// `+` 键始终存在，值为自进程启动的毫秒数。
/// 其余键值对由调用方传入。
#[macro_export]
macro_rules! debug {
    ($($key:expr => $val:expr),* $(,)?) => {
        if std::env::var("PRTS_DEBUG").as_deref() == Ok("1") {
            let elapsed_ms = $crate::observability::start_time().elapsed().as_millis();
            let mut obj = serde_json::Map::new();
            obj.insert("+".into(), serde_json::Value::String(format!("{}ms", elapsed_ms)));
            $(
                obj.insert($key.into(), serde_json::json!($val));
            )*
            eprintln!("{}", serde_json::Value::Object(obj));
        }
    };
}

/// 消息级调试宏（预留）。
///
/// 语义与 `debug!` 相同，由 `PRTS_TRACE=1` 控制。
/// 本期不设置调用点。留待上下文衰减实现时用于消息级别的细粒度日志。
#[macro_export]
macro_rules! trace {
    ($($key:expr => $val:expr),* $(,)?) => {
        if std::env::var("PRTS_TRACE").as_deref() == Ok("1") {
            let elapsed_ms = $crate::observability::start_time().elapsed().as_millis();
            let mut obj = serde_json::Map::new();
            obj.insert("+".into(), serde_json::Value::String(format!("{}ms", elapsed_ms)));
            $(
                obj.insert($key.into(), serde_json::json!($val));
            )*
            eprintln!("{}", serde_json::Value::Object(obj));
        }
    };
}
