// 视图与宿主之间的那一层：源、URL、消息校验与限流。
//
// 沙箱 iframe 是独立源，宿主与它只靠 postMessage 说话。三件容易做错的事：
//   1. 平台的 URL 形态由 Rust 给（view_origin），前端不再自己猜；
//   2. 沙箱不给 allow-same-origin，event.origin 恒为 "null"，只能用
//      event.source === iframe.contentWindow 校验来源；
//   3. 每张视图每分钟最多 10 次动作，键是视图本身（callId 级别的实例）。

import { invoke } from "@tauri-apps/api/core";

export interface ViewOrigin {
  scheme: string;
  host: string;
  origin: string;
}

let originPromise: Promise<ViewOrigin> | null = null;

const FALLBACK: ViewOrigin = {
  scheme: "prts-view",
  host: "prts-view.localhost",
  origin: "http://prts-view.localhost",
};

/** 取平台的视图源（结果进程内只取一次）。取不到时按 Windows 形态兜底，宁可打不开也不白屏。 */
export function getViewOrigin(): Promise<ViewOrigin> {
  if (!originPromise) {
    originPromise = invoke<ViewOrigin>("view_origin")
      .then((value) => (value?.origin ? value : FALLBACK))
      .catch(() => FALLBACK);
  }
  return originPromise;
}

/** 测试用：清掉进程内缓存。 */
export function resetViewOrigin(): void {
  originPromise = null;
}

/** 拼沙箱文档 URL：会话 id 与路径都按段转义，避免 `..` 之类的东西变形。 */
export function viewUrl(origin: string, conversationId: string, relative: string): string {
  const encoded = relative
    .split("/")
    .map((segment) => encodeURIComponent(segment))
    .join("/");
  return `${origin}/${encodeURIComponent(conversationId)}/${encoded}`;
}

export const VIEW_MESSAGE_SOURCE = "prts-view";
/** 一条交互文字的上限；超了拒绝并提示，不截断。 */
export const VIEW_TEXT_LIMIT = 2000;

export type ViewPrimitive = "local" | "prompt" | "call" | "toast";

export interface ViewMessage {
  type: ViewPrimitive;
  text?: string;
  tool?: string;
  args?: unknown;
}

/** 解析并校验沙箱发来的消息；不是本协议的载荷一律返回 null。 */
export function parseViewMessage(data: unknown): ViewMessage | null {
  if (!data || typeof data !== "object") return null;
  const raw = data as Record<string, unknown>;
  if (raw.source !== VIEW_MESSAGE_SOURCE) return null;
  const type = raw.type;
  if (type !== "local" && type !== "prompt" && type !== "call" && type !== "toast") {
    return null;
  }
  const text = typeof raw.text === "string" ? raw.text : undefined;
  const tool = typeof raw.tool === "string" ? raw.tool : undefined;
  return { type, text, tool, args: raw.args };
}

/** 滑动窗口限流：返回 true 表示放行。 */
export function createViewRateLimiter(
  limit = 10,
  windowMs = 60_000
): () => boolean {
  const stamps: number[] = [];
  return () => {
    const now = Date.now();
    while (stamps.length > 0 && now - stamps[0] >= windowMs) stamps.shift();
    if (stamps.length >= limit) return false;
    stamps.push(now);
    return true;
  };
}

/** 来源标注：进上下文之前加一句，让模型知道这话不是用户从键盘打的。 */
export function withViewSource(title: string, text: string): string {
  return `来自《${title}》的交互：${text}`;
}
