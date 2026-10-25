// 发送前的请求准备：上一轮状态与上下文窗口预估。
// 这里只做纯计算，不读写 store。

import type { Conversation, PreviousTurnStatus, SessionEvent } from "../types";
import { willNextTurnOverflow } from "../projection";

/**
 * 读取最近一轮的非正常结束状态。
 *
 * 只取最近一轮，作为 `[prts_reminder]` 行随 user 消息发送，不修改
 * 冻结的 system 前缀；轮成功完成后自然返回 undefined。具体错误详情
 * 仍留在 UI/日志，模型只拿到安全的状态摘要。
 */
type TurnStatus = PreviousTurnStatus | "completed";

/** 单轮终止状态；无法判断 → undefined。 */
function deriveTurnStatus(turn: SessionEvent[]): TurnStatus | undefined {
  // 先读后端 turn/end(reason)：primary 归档可能没有前端私有标记；
  // interrupted 也只能从这里区分，不能先被 turn/aborted 标记截走。
  const terminal = turn.find((e) => e.type === "turn/end");
  if (terminal?.type === "turn/end") {
    if (terminal.reason === "interrupted") return "interrupted";
    if (terminal.reason === "app-shutdown") return "app-shutdown";
    if (terminal.reason === "error") return "error";
    if (terminal.reason === "aborted") return "aborted";
    if (terminal.reason === "stagnation") return "stagnation";
    if (terminal.reason === "max-tokens") return "max-tokens";
    return "completed";
  }
  if (turn.some((e) => e.type === "turn/aborted")) return "aborted";
  if (turn.some((e) => e.type === "turn/truncated")) return "max-tokens";
  if (turn.some((e) => e.type === "turn/stagnated")) return "stagnation";
  if (turn.some((e) => e.type === "error" && !e.recoverable)) return "error";
  if (turn.some((e) => e.type === "assistant/message")) return "completed";
  // 归档轮没有终止标记，但有可见输出：只可能是关闭/强杀留下的半截轮。
  if (
    turn.some(
      (e) =>
        e.type === "assistant/chunk" ||
        e.type === "tool/call" ||
        e.type === "tool/result"
    )
  ) {
    return "interrupted";
  }
  return undefined;
}

export function derivePreviousTurnStatus(
  conv: Conversation | undefined
): PreviousTurnStatus | undefined {
  const turns = conv?.turns;
  if (!turns || turns.length === 0) return undefined;
  // 从尾向前找最近一个非空轮：恢复路径可能留下空占位轮，不能让
  // 它把真正的中止轮遮住。遇到正常完成轮就停止，不把更早的状态带进来。
  for (let i = turns.length - 1; i >= 0; i--) {
    const turn = turns[i];
    if (turn.length === 0) continue;
    const status = deriveTurnStatus(turn);
    if (status === "completed") return undefined;
    if (status) return status;
    return undefined;
  }
  return undefined;
}

/**
 * 下一轮是否会超出模型上下文窗口。
 *
 * 水位 = 最近 usage 的 contextInputTokens ?? inputTokens（Anthropic 缓存
 * 命中时用完整 prompt）；下一轮预估 = user 文本（含 reminder 块）+ output
 * 预留。无窗口信息（未知模型）→ 不拦。
 */
export function willOverflowContext(
  conv: Conversation | undefined,
  displayContent: string,
  windowSize: number
): boolean {
  if (!conv) return false;
  const usedInput =
    conv.contextTokens?.contextInputTokens ?? conv.contextTokens?.inputTokens ?? 0;
  return willNextTurnOverflow(usedInput, displayContent, windowSize);
}
