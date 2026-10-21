// 首轮未归档时暂存的标题用量，随下一次 appendTurn 提交。

import type { UsageDelta } from "../types";

const pendingTitleUsage = new Map<string, UsageDelta>();

/** 暂存标题用量（供 useAgentEvents 在会话未归档时调用）。 */
export function stashTitleUsage(conversationId: string, delta: UsageDelta): void {
  pendingTitleUsage.set(conversationId, delta);
}

/** 取出并清除标题用量（轮末归档时消费）。 */
export function takeTitleUsage(conversationId: string): UsageDelta | undefined {
  const delta = pendingTitleUsage.get(conversationId);
  if (delta) pendingTitleUsage.delete(conversationId);
  return delta;
}
