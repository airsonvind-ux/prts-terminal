// 发送前的请求准备：从对话投影派生 LLM 历史，并做上下文窗口预估。
// 这里只做纯计算，不读写 store。

import type { Attachment, ChatMessage, Conversation } from "../types";
import { deriveActiveHistory, willNextTurnOverflow } from "../projection";

/**
 * 组装本次请求的完整历史（含本轮 user 消息）。
 *
 * 已归档轮次从 SessionEvent 投影重建，error 事件天然被过滤；user 消息保留
 * [prts_reminder] 块——块随 history 发送并归档冻结，是 LLM 实际所见。
 */
export function buildSendHistory(
  conv: Conversation | undefined,
  displayContent: string,
  attachments: Attachment[]
): ChatMessage[] {
  const prior = conv ? deriveActiveHistory(conv.turns, conv.compaction) : [];
  const lastUser: ChatMessage = { role: "user", content: displayContent };
  if (attachments.length > 0) lastUser.attachments = attachments;
  return [...prior, lastUser];
}

/**
 * 下一轮是否会超出模型上下文窗口。
 *
 * 水位 = 最近 usage 的 inputTokens（provider 权威口径）；下一轮预估 =
 * user 文本（含 reminder 块）+ output 预留。无窗口信息（未知模型）→ 不拦。
 */
export function willOverflowContext(
  conv: Conversation | undefined,
  displayContent: string,
  windowSize: number
): boolean {
  if (!conv) return false;
  const usedInput = conv.contextTokens?.inputTokens ?? 0;
  return willNextTurnOverflow(usedInput, displayContent, windowSize);
}
