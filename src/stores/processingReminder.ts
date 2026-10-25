// 处理 store 的提醒装配：把 get_reminder_data 的载荷转换成
// 可注入到 user 消息前的 [prts_reminder] 块，并给出本次游标推进量。
//
// 新对话（游标为空）：值维度快照，仅 kind === "value" 的维度；
// 进行中对话：stamp > 游标的维度在前，seq > 游标的事件在后。

import type { PreviousTurnStatus, ReminderData } from "../types";
import { buildReminderBlock, type ReminderLine } from "../projection";

const PREVIOUS_TURN_STATUS_TEXT: Record<PreviousTurnStatus, string> = {
  aborted:
    "上一轮被手动中止，已产生的可见内容可能不完整；此前成功完成的工具结果仍然有效。",
  "app-shutdown":
    "上一轮因应用关闭而中断，已产生的可见内容可能不完整；此前成功完成的工具结果仍然有效。",
  interrupted:
    "上一轮未正常结束（应用异常退出或进程被终止），已产生的可见内容可能不完整；结果未记录的工具调用状态未知，涉及副作用的操作不要盲目重试。",
  error:
    "上一轮请求失败，没有产生可提交的助手回复；此前成功完成的工具结果仍然有效。",
  stagnation:
    "上一轮因工具调用重复而提前终止，已产生的可见内容可能不完整。",
  "max-tokens": "上一轮因达到输出上限而截断，已产生的可见内容不完整。",
};

/** 上一轮状态 → reminder 行；unknown/undefined → null。 */
export function previousTurnStatusLine(
  status: PreviousTurnStatus | undefined
): ReminderLine | null {
  if (!status) return null;
  const detail = PREVIOUS_TURN_STATUS_TEXT[status];
  return detail ? { title: "上一轮状态", detail } : null;
}

export interface ComposedReminder {
  /** 注入 user 消息前的提醒块；无内容为空串 */
  block: string;
  /** 本次应推进到的游标（payload.seq） */
  advanceTo: number;
  /** 推进前的游标（回滚用；新对话为 0） */
  prevSeq: number;
  /** 本次是否新对话（新对话失败不回滚为 0，避免重放） */
  isNewConversation: boolean;
}

export function composeReminderNotice(
  data: ReminderData,
  cursor: number | null | undefined,
  previousTurnStatus?: PreviousTurnStatus
): ComposedReminder {
  const lines: ReminderLine[] = [];
  const statusLine = previousTurnStatusLine(previousTurnStatus);
  if (statusLine) lines.push(statusLine);
  let prevSeq = 0;
  const isNewConversation = cursor === undefined || cursor === null;

  if (isNewConversation) {
    for (const d of data.dims) {
      if (d.kind === "value" && d.value !== "") {
        lines.push({ title: d.title, detail: d.value });
      }
    }
  } else {
    prevSeq = cursor;
    // 维度行（stamp > 游标）在上
    for (const d of data.dims) {
      if (d.stamp > cursor) lines.push({ title: d.title, detail: d.note });
    }
    // 事件行（seq > 游标）在下
    for (const e of data.events) {
      if (e.seq > cursor) lines.push({ title: e.title, detail: e.detail });
    }
  }

  return {
    block: lines.length > 0 ? buildReminderBlock(lines) : "",
    advanceTo: data.seq,
    prevSeq,
    isNewConversation,
  };
}
