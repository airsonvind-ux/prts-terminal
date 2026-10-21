// 处理 store 的提醒装配：把 get_reminder_data 的载荷转换成
// 可注入到 user 消息前的 [prts_reminder] 块，并给出本次游标推进量。
//
// 新对话（游标为空）：值维度快照，仅 kind === "value" 的维度；
// 进行中对话：stamp > 游标的维度在前，seq > 游标的事件在后。

import type { ReminderData } from "../types";
import { buildReminderBlock, type ReminderLine } from "../projection";

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
  cursor: number | null | undefined
): ComposedReminder {
  const lines: ReminderLine[] = [];
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
