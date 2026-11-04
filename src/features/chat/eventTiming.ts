// 事件到达时刻（毫秒）。
//
// 只用于展示层的耗时：过程卡标题行的「3.4s」、工具行右侧的耗时。
// 后端事件没有时间戳，归档读回的事件也没有——这条通道只服务实时到达的事件，
// 拿不到时刻时展示层就不显示耗时，不猜一个数出来。
//
// 用 WeakMap 挂在事件对象上：事件对象在 store 里就是最终对象，块与行持有的
// 也是同一批引用，不需要往协议里加字段，也不需要额外的状态树。

import type { SessionEvent } from "../../types";

const arrival = new WeakMap<SessionEvent, number>();

/** 记录一条事件到达前端的时刻；同一对象只记第一次。 */
export function stampEvent(event: SessionEvent, at: number = Date.now()): void {
  if (!arrival.has(event)) arrival.set(event, at);
}

/** 事件到达时刻；归档回放、测试构造的事件没有 → undefined。 */
export function eventTime(event: SessionEvent): number | undefined {
  return arrival.get(event);
}

/** 两个时刻之间的秒数文案（一位小数；不足 0.1s 也写 0.1s，避免「0.0s」）。 */
export function formatElapsed(startedAt?: number, endedAt?: number): string | null {
  if (startedAt === undefined || endedAt === undefined) return null;
  const ms = endedAt - startedAt;
  if (ms < 0) return null;
  if (ms < 100) return "0.1s";
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.round(ms / 1000)}s`;
}
