// 会话自动标题 · 触发层（前端编排）
//
// 职责：素材累积（首答可见文本）+ 阈值触发（500 字）+ 轮末兜底 + 幂等 +
// 默认标题哨兵 + 生成前预检。LLM 生成与写盘在后端（generate_conversation_title
// 命令）；本层只负责“何时发起”。
//
// 幂等标志放模块级 Map（仿 chunkBuffers：跨快照删除存活）。key=conversationId
// ——自动标题仅针对“新对话首轮”，注册于 sendMessage 建对话处，轮末清理。

import { invoke } from "@tauri-apps/api/core";
import type { Conversation, SessionEvent, UserMessageEvent } from "../types";
import { stripReminderBlock } from "../projection";
import { useConversationStore } from "./useConversationStore";

/** 首答可见文本触发阈值（字符数） */
const TITLE_TRIGGER_CHARS = 500;
/** 尝试次数上限（阈值一次 + 轮末兜底一次） */
const TITLE_MAX_ATTEMPTS = 2;
/** 首答文本累积上限（仅防无界；实际传后端时再截 500） */
const TITLE_ASSISTANT_CAP = 2000;

interface TitleGenState {
  /** 默认标题哨兵（首问 trim + 前 12 字符；创建时快照） */
  defaultTitle: string;
  /** 首答可见文本累积（text kind；已剥离情绪标签后的可见文本） */
  text: string;
  /** 阈值是否已触发（幂等） */
  thresholdFired: boolean;
  /** 已发起尝试次数（上限 TITLE_MAX_ATTEMPTS） */
  attempts: number;
}

const titleGenState = new Map<string, TitleGenState>();

/** 注册自动标题现场（仅新对话首轮；sendMessage 调用）。 */
export function registerTitleGen(conversationId: string, defaultTitle: string): void {
  titleGenState.set(conversationId, {
    defaultTitle,
    text: "",
    thresholdFired: false,
    attempts: 0,
  });
}

/** 清理自动标题现场（轮末 / 错误轮）。 */
export function clearTitleGenState(conversationId: string): void {
  titleGenState.delete(conversationId);
}

/** 该对话是否仍挂着自动标题现场。 */
export function hasTitleGen(conversationId: string): boolean {
  return titleGenState.has(conversationId);
}

/**
 * 生成请求的默认标题哨兵：事件回来时用它做“用户是否已改名”的复检。
 * 与 titleGenState 分开保存——标题事件可能晚于轮末清理到达。
 */
const titleExpected = new Map<string, string>();

/** 读取某对话最近一次标题生成使用的哨兵（事件通道复检用）。 */
export function titleExpectedFor(conversationId: string): string | undefined {
  return titleExpected.get(conversationId);
}

/** 该对话是否已有成功轮（以 assistant/message 为完成标志）。 */
function hasSuccessfulTurn(conv: Conversation): boolean {
  return conv.turns.some((turn) =>
    turn.some((event) => event.type === "assistant/message")
  );
}

/**
 * 是否应在本次发送时重新启动自动标题。
 *
 * 条件（三者同时满足）：
 * - 尚未生成过真标题（用户改名也视为真标题）；
 * - 还没有成功轮（前几轮可能全是 401/网络错误）；
 *
 * 不再要求“标题等于首条 user 的默认哨兵”：首个正常轮可能已经把占位标题
 * 替换成自己的截断名（见 replaceDefaultTitle），仍然要继续挂标题现场。
 */
export function conversationNeedsTitle(conv: Conversation | undefined): boolean {
  if (!conv || conv.titleGenerated === true) return false;
  return !hasSuccessfulTurn(conv);
}

/**
 * 收集生成素材（首问剥离 [prts_reminder] + 首答可见文本）。
 * 素材在触发时刻从当前处理快照的 currentEvents 取（首条 user 事件为前端
 * 合成的含块版本）。
 */
function collectTitleMaterial(
  conversationId: string,
  events: SessionEvent[]
): {
  userText: string;
  assistantText: string;
  expectedTitle: string;
} | null {
  const st = titleGenState.get(conversationId);
  if (!st) return null;
  const userEv = events.find(
    (ev): ev is UserMessageEvent => ev.type === "user/message"
  );
  if (!userEv) return null;
  return {
    userText: stripReminderBlock(userEv.content),
    assistantText: st.text.slice(0, 500),
    expectedTitle: st.defaultTitle,
  };
}

/**
 * 发起一次标题生成（fire-and-forget；结果经 conversation/title 事件回推）。
 *
 * 预检（写盘侧另有 CAS 兜底）：当前标题 !== 默认哨兵（用户已改名）→ 跳过。
 */
function triggerTitleGeneration(
  conversationId: string,
  source: "threshold" | "fallback",
  events: SessionEvent[]
): void {
  const st = titleGenState.get(conversationId);
  if (!st) return;
  if (st.attempts >= TITLE_MAX_ATTEMPTS) return;
  if (source === "threshold") {
    if (st.thresholdFired) return;
    st.thresholdFired = true;
  }
  const conv = useConversationStore
    .getState()
    .conversations.find((c) => c.id === conversationId);
  if (!conv) return;
  if (conv.title !== st.defaultTitle) return; // 已被改/已生成 → 跳过
  const material = collectTitleMaterial(conversationId, events);
  if (!material || !material.assistantText.trim()) return;
  st.attempts += 1;
  titleExpected.set(conversationId, material.expectedTitle);
  invoke("generate_conversation_title", {
    conversationId,
    userText: material.userText,
    assistantText: material.assistantText,
    expectedTitle: material.expectedTitle,
    provider: conv.provider ?? null,
    model: conv.model ?? null,
  }).catch(() => {});
}

/** 累积首答可见文本（appendCurrentEvent 的 text chunk 路径调用）。 */
export function accumulateTitleText(
  conversationId: string,
  content: string,
  events: SessionEvent[]
): void {
  const st = titleGenState.get(conversationId);
  if (!st || st.thresholdFired) return;
  if (st.text.length < TITLE_ASSISTANT_CAP) {
    st.text += content;
  }
  if (st.text.length >= TITLE_TRIGGER_CHARS) {
    triggerTitleGeneration(conversationId, "threshold", events);
  }
}

/**
 * 轮末兜底（仅首轮正常完成时；finishProcessing 调用）。
 * 条件：尚未成功获得标题（标题仍为默认）+ 尝试次数未达上限。
 */
export function maybeTriggerTitleFallback(
  conversationId: string,
  events: SessionEvent[]
): void {
  const st = titleGenState.get(conversationId);
  if (!st) return;
  if (st.attempts >= TITLE_MAX_ATTEMPTS) return;
  const conv = useConversationStore
    .getState()
    .conversations.find((c) => c.id === conversationId);
  if (!conv || conv.title !== st.defaultTitle) return;
  triggerTitleGeneration(conversationId, "fallback", events);
}
