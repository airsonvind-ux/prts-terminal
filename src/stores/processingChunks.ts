// processingChunks · 流式 chunk 的 33ms 节流缓冲
//
// 模块级 Map（不进 Zustand 状态）：StrictMode 双挂载下 hook 层缓冲会在
// cleanup 时丢未 flush 事件，store 模块级缓冲没有这个问题。
//
// 与 store 解耦：写入/追加由 configureChunkBuffer 注入的 sink 完成，
// 避免 processingChunks ↔ useProcessingStore 的循环依赖。

import type { AssistantChunkEvent, SessionEvent } from "../types";

const PENDING_FLUSH_MS = 33;

interface ChunkBuffer {
  /** 窗口内累积的 assistant/chunk 事件（FIFO） */
  events: SessionEvent[];
  /** 上次 flush 的时间戳（Date.now()，用于计算距 33ms 窗口的剩余时间） */
  lastFlush: number;
  /** 补足窗口剩余时间的定时器；null = 无挂起定时器 */
  timer: ReturnType<typeof setTimeout> | null;
}

interface ChunkSink {
  /** 把一批事件一次性追加到指定 session 的快照 */
  appendMany(sessionId: string, events: SessionEvent[]): void;
  /** 把单条非 chunk 事件追加到快照 */
  appendOne(event: SessionEvent): void;
}

let sink: ChunkSink | null = null;

/** 由 useProcessingStore 在创建后注入写入实现。 */
export function configureChunkBuffer(next: ChunkSink): void {
  sink = next;
}

/** sessionId → chunk 缓冲（模块级，不进入 Zustand 状态） */
const chunkBuffers = new Map<string, ChunkBuffer>();

/**
 * 把指定 session 的缓冲 chunk 一次性追加进 currentEvents（强 flush）。
 *
 * 导出的目的：测试可直接显式 flush 验证落位与顺序（processing.throttle.test.ts），
 * 生产路径由 scheduleFlush（33ms 窗口）与 finishProcessing（turn/end 强 flush）驱动。
 *
 * 缓冲为空或无缓冲 → 无操作（幂等）。
 * 快照已被清理（failProcessing 先执行）→ sink 丢弃缓冲，不追加。
 */
export function flushPending(sessionId: string): void {
  const buf = chunkBuffers.get(sessionId);
  if (!buf || buf.events.length === 0) return;

  const events = buf.events;
  if (buf.timer) clearTimeout(buf.timer);
  buf.timer = null;
  buf.events = [];
  // 保留 buffer 并推进 lastFlush：节流窗口跨批次持续（Hanako 语义——
  // 「距上次 flush ≥ 33ms → 立即 flush」以真实的上次 flush 时间为基准）。
  // 若在此处删除 buffer，下次 append 会重建 lastFlush=now，窗口重置为 0，
  // 永远等满 33ms，与设计不符。
  buf.lastFlush = Date.now();
  sink?.appendMany(sessionId, events);
}

/**
 * 调度 33ms 节流 flush（Hanako scheduleFlush 语义）：
 * 距上次 flush ≥ 33ms → 立即 flush；否则按剩余时间安排 setTimeout 补足。
 */
function scheduleFlush(sessionId: string): void {
  const buf = chunkBuffers.get(sessionId);
  if (!buf) return;

  const now = Date.now();
  if (now - buf.lastFlush >= PENDING_FLUSH_MS) {
    flushPending(sessionId);
  } else if (!buf.timer) {
    buf.timer = setTimeout(() => {
      const b = chunkBuffers.get(sessionId);
      if (b) b.timer = null;
      flushPending(sessionId);
    }, PENDING_FLUSH_MS - (now - buf.lastFlush));
  }
}

/**
 * 立即追加非 chunk 事件；先 flush 该 session 的缓冲（chunk 先落位），
 * 再追加本事件——两次同步 set 在 React 批处理下合并为一次渲染，
 * 事件顺序不变，且无需额外等待 timer。
 */
export function appendImmediate(event: SessionEvent): void {
  flushPending(event.sessionId);
  sink?.appendOne(event);
}

/** 清理指定 session 的缓冲与定时器（failProcessing 路径，丢弃未 flush chunk） */
export function clearChunkBuffer(sessionId: string): void {
  const pending = chunkBuffers.get(sessionId);
  if (!pending) return;
  if (pending.timer) clearTimeout(pending.timer);
  chunkBuffers.delete(sessionId);
}

/** 把一条 assistant/chunk 事件推入该 session 的 33ms 节流缓冲 */
export function pushChunk(event: AssistantChunkEvent): void {
  let buf = chunkBuffers.get(event.sessionId);
  if (!buf) {
    buf = { events: [], lastFlush: Date.now(), timer: null };
    chunkBuffers.set(event.sessionId, buf);
  }
  buf.events.push(event);
  scheduleFlush(event.sessionId);
}

/** 把一段文本以 assistant/chunk(text) 合成事件推入缓冲（扫描器放行的扣留文本） */
export function pushTextChunk(sessionId: string, content: string): void {
  if (content.length === 0) return;
  pushChunk({ type: "assistant/chunk", sessionId, kind: "text", content });
}
