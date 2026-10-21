// 上下文压缩与窗口预算的纯函数
//
// 估算口径（轮/文本 token，0.3 ascii + 0.6 cjk）与后端 estimate_tokens 同构，
// 仅供 cut 点选择、noop 预判与发送前硬拦；权威用量以 provider usage 为准。

import type { ChatMessage, CompactionMeta, SessionEvent } from "../types";

/** 摘要消息定界块起始行（模型可见；固定前缀利于未来展示路径剥离） */
export const SUMMARY_BLOCK_START = "[对话压缩摘要——非用户输入]";
/** 摘要消息定界块结束行 */
export const SUMMARY_BLOCK_END = "[/对话压缩摘要]";

/** 保留段 token 预算（定稿裁决 1：常数 20K，沿用既有成熟参数，可配常量） */
export const RETAIN_BUDGET_TOKENS = 20_000;
/** noop 预判：折叠区轮数下限（少于则「对话太短」） */
export const NOOP_MIN_FOLD_TURNS = 2;
/** noop 预判：折叠区估算 token 下限（低于则「新内容太少」） */
export const NOOP_MIN_FOLD_TOKENS = 4_000;

/**
 * 逐字符估算 token 数（与后端 `estimate_tokens` 同构：ASCII 0.3 / 非 ASCII 0.6，
 * 跳过空白）。仅用于 cut 点选择与 noop 预判——每次请求的真实用量以
 * provider usage（assistant/message.usage）为准，本估算不参与水位显示。
 */
export function estimateTextTokens(text: string): number {
  let ascii = 0;
  let nonAscii = 0;
  for (const ch of text) {
    if (/\s/.test(ch)) continue;
    if (/[\x00-\x7f]/.test(ch)) ascii += 1;
    else nonAscii += 1;
  }
  return Math.max(1, Math.ceil(ascii * 0.3 + nonAscii * 0.6));
}

/**
 * 估算单轮事件的文本 token（近似：只统计会派生进 LLM 消息的文本字段；
 * 结构事件不计）。轮粒度误差可接受——cut 只决定保留多少，无正确性影响。
 */
export function estimateTurnTokens(events: SessionEvent[]): number {
  let total = 0;
  for (const e of events) {
    switch (e.type) {
      case "user/message":
        total += estimateTextTokens(e.content);
        break;
      case "assistant/chunk":
        total += estimateTextTokens(e.content);
        break;
      case "assistant/message":
        total += estimateTextTokens(e.content);
        if (e.reasoningContent) total += estimateTextTokens(e.reasoningContent);
        break;
      case "tool/call":
        total += estimateTextTokens(e.toolName) + estimateTextTokens(e.params);
        break;
      case "tool/result":
        total += estimateTextTokens(e.summary);
        break;
      case "error":
        total += estimateTextTokens(e.message);
        break;
      default:
        break;
    }
  }
  return total;
}

/**
 * 计算保留段起点（保留段第一轮在 turns 的下标）。
 *
 * 语义：从尾部向前累计最近轮 token，保留最近且总估 ≤ budget 的轮；首个被
 * 挤出的轮的下标即 `retainedFrom`。全部轮 ≤ budget → 返回 0（无可折叠，
 * noop「nothing」）。轮边界对齐——绝不拆轮（不拆工具事务）。
 */
export function computeRetainedFrom(
  turns: SessionEvent[][],
  budget: number = RETAIN_BUDGET_TOKENS
): number {
  let total = 0;
  for (let i = turns.length - 1; i >= 0; i--) {
    total += estimateTurnTokens(turns[i]);
    if (total > budget) return i + 1;
  }
  return 0;
}

/**
 * 构造摘要 user 消息（定界块内声明非用户输入 + 归档语义 + checkpoint 正文）。
 *
 * LLM 侧角色：user（seed 已识别形态，后端零改动）；位置 = 派生序列首条
 * （system 之后、真实历史之前）。措辞明示「非用户输入 / 已归档 / 细节可重查」
 * ——避免模型把它当本轮指令或精确事实源（PRTS 查询结果可随时重查）。
 * 格式固定后摘要消息冻结——压缩后前缀重新稳定（付一次未命中为接受的代价）。
 */
export function buildSummaryUserMessage(compaction: CompactionMeta): ChatMessage {
  const body = [
    SUMMARY_BLOCK_START,
    "以下是你（PRTS）与博士的对话概要。这段对话已被压缩归档，仅在本消息中呈现，用于保持任务连续性；其细节（含工具查询结果）已不再可用，需要时可重新查询。压缩点之后的消息是完整保留的近期对话，请以它们为准继续。",
    "",
    compaction.summary,
    SUMMARY_BLOCK_END,
  ].join("\n");
  return { role: "user", content: body };
}

/**
 * 是否为“只有 user + error、没有完成 assistant”的错误轮。
 *
 * failProcessing 归档的错误轮固定为 [user/message, error]；不会含
 * assistant/message。用它区分“启动/鉴权失败”与真正完成过的轮次。
 */
export function isErrorOnlyTurn(turn: SessionEvent[]): boolean {
  let hasError = false;
  let hasAssistantMessage = false;
  for (const event of turn) {
    if (event.type === "error") hasError = true;
    if (event.type === "assistant/message") hasAssistantMessage = true;
  }
  return hasError && !hasAssistantMessage;
}

/**
 * 去掉对话首部连续的错误轮。
 *
 * 语义：首轮 401 / 网络失败不应成为模型的“第一段对话历史”；第一次正常轮
 * 才是事实上的首轮。后续轮次中出现的错误轮不受影响（那是真实对话中的失败）。
 */
export function stripLeadingErrorTurns(
  turns: SessionEvent[][]
): SessionEvent[][] {
  let firstKept = 0;
  while (firstKept < turns.length && isErrorOnlyTurn(turns[firstKept])) {
    firstKept += 1;
  }
  return firstKept === 0 ? turns : turns.slice(firstKept);
}

// ═══════════════════════════════════════════════════════
// 上下文窗口检查（发送前硬拦 + 环预警比例）——纯函数，UI 消费
// ═══════════════════════════════════════════════════════
// 定稿裁决 2：纯比例双线（不加绝对值）。水位口径 = usage.inputTokens
// （provider 权威；output 是成本信息不是水位——ContextIndicator 显示侧修正）。

/** 环预警比例（≥ 此值 tooltip 追加「接近上限，点击圆环压缩」） */
export const CONTEXT_WARNING_RATIO = 0.7;
/** 发送硬拦比例（下一轮预估占用 ≥ 此值 → 阻止发送，提示先压缩） */
export const CONTEXT_HARD_RATIO = 0.9;

/**
 * output 预留 = max(4096, 5% 窗口)：避免回答中途被 provider 截断
 * （对齐后端 max_tokens 默认 4096 与窗口 5% 经验值）。
 */
export function outputReserveTokens(window: number): number {
  return Math.max(4096, Math.ceil(window * 0.05));
}

/**
 * 发送前硬拦判定（模块 D）：used + 本轮 user 文本估 + output 预留 ≥ 0.9×窗口
 * → 应阻止发送（提示先压缩）。无窗口信息（window <= 0）→ false（不拦）。
 */
export function willNextTurnOverflow(
  usedInputTokens: number,
  nextText: string,
  window: number
): boolean {
  if (window <= 0) return false;
  const projected =
    usedInputTokens + estimateTextTokens(nextText) + outputReserveTokens(window);
  return projected >= window * CONTEXT_HARD_RATIO;
}
