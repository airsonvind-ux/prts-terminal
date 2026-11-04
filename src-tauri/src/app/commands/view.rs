//! 视图交互：沙箱文档的源与它发回来的消息。
//!
//! 沙箱 iframe 与宿主是跨源的（`prts-view` 协议是独立源），消息只能走
//! `postMessage`；而沙箱不给 `allow-same-origin`，`event.origin` 恒为字符串
//! `"null"`——校验必须比 `event.source === iframe.contentWindow`，用 origin
//! 白名单会把所有消息都拒掉（spike 结论）。

use super::prelude::*;
use crate::runtime::steer_queue::SteerQueue;
use crate::runtime::turn_registry::TurnRegistry;
use std::sync::Arc;

/// 视图文档的源。Windows 的 WebView2 把自定义协议映射成 `http://<scheme>.localhost`。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewOrigin {
    pub scheme: &'static str,
    pub host: &'static str,
    pub origin: &'static str,
}

/// 前端拼 iframe `src` 用。平台差异只在 Rust 这一处，前端不再自己判断。
#[tauri::command]
pub fn view_origin() -> ViewOrigin {
    #[cfg(target_os = "windows")]
    {
        ViewOrigin {
            scheme: "prts-view",
            host: "prts-view.localhost",
            origin: "http://prts-view.localhost",
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        ViewOrigin {
            scheme: "prts-view",
            host: "localhost",
            origin: "prts-view://localhost",
        }
    }
}

/// `prompt` 原语的落点。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPromptOutcome {
    /// true = 已作为插话投进活动轮次；false = 没人在跑，前端填进输入框当草稿。
    pub delivered: bool,
}

/// 视图发起的 `prompt`：活动轮次里作为插话进对话；空闲时不替用户发言。
///
/// 空闲与「不替用户花钱」同一条理由——只把文字放进输入框，发不发由用户按。
#[tauri::command]
pub fn view_prompt(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    steer_queue: State<'_, Arc<SteerQueue>>,
    conversation_id: String,
    text: String,
) -> Result<ViewPromptOutcome, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("视图发来的内容是空的".into());
    }
    let Some(session_id) = turn_registry.active_session_for(&conversation_id) else {
        return Ok(ViewPromptOutcome { delivered: false });
    };
    steer_queue
        .enqueue(&session_id, trimmed)
        .ok_or_else(|| "插话入队失败（内容为空）".to_string())?;
    Ok(ViewPromptOutcome { delivered: true })
}
