import type { Attachment, CompactionMeta, SessionEvent } from "../../types";
import {
  deriveTextSegments,
  isErrorOnlyTurn,
  isMetaEvent,
  splitTurnByUserBoundary,
  stripReminderBlock,
} from "../../projection";
import type { TextSegment } from "../../projection";

// ── 展示项类型 ──

export type DisplayItem =
  | { type: "message"; role: "user" | "error" | "aborted" | "aborting" | "truncated"; content: string; attachments?: Attachment[] }
  | {
      type: "message";
      role: "assistant";
      segments: TextSegment[];
      isStreaming: boolean;
    }
  | { type: "events"; events: SessionEvent[]; interactive: boolean }
  | { type: "welcome" }
  // 首部连续错误轮的折叠记录（数据保留；首次正常轮后只显露一条摘要）
  | { type: "preflight"; count: number; errors: string[] }
  // 插话 pending（待注入气泡；点击取消）
  | { type: "pending"; id: string; text: string }
  // 模块 D：压缩点本地提示卡（非 LLM 消息——derive/展示派生忽略，仅列表呈现）
  | { type: "compaction"; summary: string; foldedCount: number };

/**
 * 将一组轮次事件追加为展示项（按消息边界切分——插话设计定稿）。
 *
 * 轮内注入后一轮可含多问答段：
 * - user/message 各成用户气泡（不再是整轮只取第一条）；
 * - assistant 段独立闭合：段内 meta（推理/工具卡）→ 段文本（段内最后一个
 *   assistant/message 为权威聚合；无则从 chunks 累积 + tool/call 回溯分段）；
 * - error → 错误气泡（其后内容不渲染；错误轮语义不变）；
 * - turn/aborted 轮中止标记 → 全部段渲染后轮尾追加（不占段）。
 * - turn/truncated 轮截断标记 → 全部段渲染后轮尾追加（不占段）。
 *
 * 单段轮（现状形态）渲染序列与历史完全一致：user 气泡 → 事件卡 → assistant
 * 气泡（cursor/interactive 仅在正在流的末段）。
 */
function appendTurnItems(
  items: DisplayItem[],
  events: SessionEvent[],
  isStreaming: boolean
): void {
  const segments = splitTurnByUserBoundary(events);

  // 末段 assistant（正在流的段）才携带 streaming 光标 / 审批交互；
  // 其后的段（注入的下一条 user 及其回答）接管 streaming 态。
  let lastAssistantIdx = -1;
  for (let i = segments.length - 1; i >= 0; i--) {
    if (segments[i].kind === "assistant") {
      lastAssistantIdx = i;
      break;
    }
  }

  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    if (seg.kind === "user") {
      // 防御性剥离 [prts_reminder] 块（当前合成 user 消息不含块；
      // 防未来后端回传 user/message 后块漏到用户可见界面）
      items.push({
        type: "message",
        role: "user",
        content: stripReminderBlock(seg.content),
        attachments: seg.attachments,
      });
      continue;
    }

    // assistant 段
    const err = seg.events.find((e) => e.type === "error");
    if (err) {
      items.push({ type: "message", role: "error", content: err.message });
      return; // 错误轮/错误终止段：其后内容不渲染（既有语义）
    }

    const meta = seg.events.filter(isMetaEvent);
    if (meta.length > 0) {
      // interactive = 进行中轮（currentEvents）才允许审批按钮渲染；
      // 归档重放（turns）不渲染交互。仅末段可能在流。
      items.push({
        type: "events",
        events: meta,
        interactive: isStreaming && i === lastAssistantIdx,
      });
    }

    const segIsStreaming = isStreaming && i === lastAssistantIdx;
    const textSegments = deriveTextSegments(seg.events);
    if (textSegments) {
      items.push({
        type: "message",
        role: "assistant",
        segments: textSegments,
        isStreaming: segIsStreaming,
      });
    }
  }

  // M3-B：轮尾中止标记（半截 chunk 已在上述分支照常展示；标记只追加轮尾）
  if (events.some((e) => e.type === "turn/aborted")) {
    items.push({ type: "message", role: "aborted", content: "该轮已中止" });
  }
  if (events.some((e) => e.type === "turn/truncated")) {
    items.push({
      type: "message",
      role: "truncated",
      content: "回答未完整（达到长度上限）",
    });
  }
}

/**
 * 单轮归档展示项缓存（键 = 该轮事件数组的身份）。
 *
 * 归档轮次的 events 数组在 store 中身份稳定（归档只 push/替换外层数组），
 * 因此一次投影可长期复用：流式刷新期间不再重算整段历史的文本聚合。
 */
const turnItemsCache = new WeakMap<SessionEvent[], DisplayItem[]>();

/** 单轮 → 归档展示项（带缓存）。 */
function archivedItemsForTurn(events: SessionEvent[]): DisplayItem[] {
  const cached = turnItemsCache.get(events);
  if (cached) return cached;
  const items: DisplayItem[] = [];
  appendTurnItems(items, events, false);
  turnItemsCache.set(events, items);
  return items;
}

/** 首部连续错误轮：只有后面出现过完成轮时才折叠；全是错误则继续原样显示。 */
function leadingErrorSummary(
  turns: SessionEvent[][]
): { count: number; errors: string[] } | null {
  const errors: string[] = [];
  let firstKept = 0;
  while (firstKept < turns.length && isErrorOnlyTurn(turns[firstKept])) {
    const errorEvent = turns[firstKept].find((event) => event.type === "error");
    if (errorEvent?.type === "error") errors.push(errorEvent.message);
    firstKept += 1;
  }
  if (firstKept === 0) return null;
  const hasCompletedTurn = turns
    .slice(firstKept)
    .some((turn) => turn.some((event) => event.type === "assistant/message"));
  return hasCompletedTurn ? { count: firstKept, errors } : null;
}

/** 归档部分（turns + 压缩点卡）。只在 turns / compaction / showWelcome 变化时重算。 */
export function buildArchivedItems(
  turns: SessionEvent[][],
  showWelcome: boolean,
  compaction: CompactionMeta | undefined
): DisplayItem[] {
  if (showWelcome) return [{ type: "welcome" }];

  const items: DisplayItem[] = [];
  const preflight = leadingErrorSummary(turns);
  if (preflight) {
    items.push({
      type: "preflight",
      count: preflight.count,
      errors: preflight.errors,
    });
  }
  const firstKept = preflight?.count ?? 0;
  for (let i = firstKept; i < turns.length; i++) {
    // 模块 D：压缩点提示卡插在保留段第一轮之前（折叠轮与新轮之间）——
    // retainedFrom=0（无可折叠/异常）不插卡
    if (compaction && compaction.retainedFrom > 0 && i === compaction.retainedFrom) {
      items.push({
        type: "compaction",
        summary: compaction.summary,
        foldedCount: compaction.retainedFrom,
      });
    }
    items.push(...archivedItemsForTurn(turns[i]));
  }
  return items;
}

/** 进行中部分（当前轮 + 插话 pending；pending 含跨轮未交付项）。随每次流式刷新重算，规模限于当前轮/当前对话。 */
export function buildLiveItems(
  currentEvents: SessionEvent[],
  pending: readonly { id: string; text: string }[] | undefined,
  aborting: boolean
): DisplayItem[] {
  const items: DisplayItem[] = [];
  if (currentEvents.length > 0) {
    appendTurnItems(items, currentEvents, true);
  }
  // 插话 pending：追加在对话底部（当前输出段之后；未注入前一直是最后一条）
  if (pending && pending.length > 0) {
    for (const p of pending) {
      items.push({ type: "pending", id: p.id, text: p.text });
    }
  }
  // 中止已发出、轮未收尾：轮尾一条「正在中止…」（收尾后由 turn/aborted 标记接替）
  if (aborting) {
    items.push({ type: "message", role: "aborting", content: "正在中止…" });
  }
  return items;
}
