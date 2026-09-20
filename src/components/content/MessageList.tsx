// src/components/content/MessageList.tsx
// 消息列表 · 核心展示组件（架构升级 · 模块 1：投影缓存渲染）
//
// 职责：将 turns（已归档轮次的 SessionEvent 序列）+ currentEvents
//       （进行中轮次的实时投影）按时间线交错排列为统一的展示项列表，
//       渲染消息气泡和工具事件卡片。
//       智能滚动：流式输出期间尊重用户手动滚动位置。
//       右键菜单：拦截浏览器默认菜单，仅保留复制功能。
//
// 消息气泡/事件卡片全部由投影函数派生（src/projection.ts），
// 不再维护独立的 messages 数组。

import { memo, useMemo, useRef, useEffect, useCallback, useState } from "react";
import type { SessionEvent } from "../../types";
import type { CompactionMeta } from "../../types";
import { deriveTextSegments, isMetaEvent, splitTurnByUserBoundary, stripReminderBlock } from "../../projection";
import type { TextSegment } from "../../projection";
import { EventBadge } from "./EventBadge";
import { Markdown } from "./Markdown";
import { registerMessageScrollHost } from "./messageScroll";
import styles from "./MessageList.module.css";

// ── 展示项类型 ──

type DisplayItem =
  | { type: "message"; role: "user" | "error" | "aborted"; content: string }
  | {
      type: "message";
      role: "assistant";
      segments: TextSegment[];
      isStreaming: boolean;
    }
  | { type: "events"; events: SessionEvent[]; interactive: boolean }
  | { type: "welcome" }
  // 插话 pending（待注入气泡；点击取消）
  | { type: "pending"; id: string; text: string }
  // 模块 D：压缩点本地提示卡（非 LLM 消息——derive/展示派生忽略，仅列表呈现）
  | { type: "compaction"; summary: string; foldedCount: number };

/**
 * 压缩点提示卡（模块 D · 上下文衰减）。
 *
 * 本地条目，非 LLM 消息：turns 全量保留（回看自由），本卡标记「从这一位置起
 * 之前的对话已压缩归档」——压缩点之后为模型当前可见的保留对话。
 * 展开可读压缩摘要（LLM checkpoint 正文）。
 */
function CompactionNotice({
  foldedCount,
  summary,
}: {
  foldedCount: number;
  summary: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={styles.compactionCard} data-compaction-card>
      <button
        type="button"
        className={styles.compactionHeader}
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span>上下文已压缩 · {foldedCount} 轮并入摘要</span>
        <span className={styles.compactionChevron} aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      <div className={styles.compactionDesc}>
        被压缩的对话已并入上方摘要，不再进入模型上下文（原历史保留，仅供回看）；
        从下一条消息起为当前对话。
      </div>
      {open && (
        <pre className={styles.compactionSummary} data-compaction-summary>
          {summary}
        </pre>
      )}
    </div>
  );
}

/** 钉底收敛窗口（ms）：items 变化后在这段时间内逐帧压到底，
 * 覆盖输入区/上下文环/大段文本布局随后落定的情况。 */
const PIN_SETTLE_MS = 400;

/** 贴底判定阈值（px）：离底距离不超过它就当成“贴底”。 */
const STICK_PX = 5;

// ── 时间线算法（投影派生） ──

/**
 * 将一组轮次事件追加为展示项（按消息边界切分——插话设计定稿）。
 *
 * 轮内注入后一轮可含多问答段：
 * - user/message 各成用户气泡（不再是整轮只取第一条）；
 * - assistant 段独立闭合：段内 meta（推理/工具卡）→ 段文本（段内最后一个
 *   assistant/message 为权威聚合；无则从 chunks 累积 + tool/call 回溯分段）；
 * - error → 错误气泡（其后内容不渲染；错误轮语义不变）；
 * - turn/aborted 轮中止标记 → 全部段渲染后轮尾追加（不占段）。
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

/** 归档部分（turns + 压缩点卡）。只在 turns / compaction / isFirstUse 变化时重算。 */
function buildArchivedItems(
  turns: SessionEvent[][],
  isFirstUse: boolean,
  compaction: CompactionMeta | undefined
): DisplayItem[] {
  if (isFirstUse) return [{ type: "welcome" }];

  const items: DisplayItem[] = [];
  for (let i = 0; i < turns.length; i++) {
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

/** 进行中部分（当前轮 + 插话 pending）。随每次流式刷新重算，规模限于当前轮。 */
function buildLiveItems(
  currentEvents: SessionEvent[],
  pending: readonly { id: string; text: string }[] | undefined
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
  return items;
}

// ── 单行渲染 ──

/**
 * 一行展示项（消息气泡 / 事件卡 / pending / 压缩卡）。
 *
 * memo 的前提是归档 item 引用跨刷新稳定（buildArchivedItems 按 turns 缓存 +
 * 单轮投影缓存）：流式刷新时归档行整段跳过重渲染，只有当前轮的行会更新。
 */
const MessageRow = memo(function MessageRow({
  item,
  onCancelPending,
}: {
  item: DisplayItem;
  onCancelPending?: (entryId: string) => void;
}) {
  switch (item.type) {
    case "welcome":
      return (
        <div className={styles.welcome}>
          <div className={styles.welcomeLogo}>PRTS</div>
          <div className={styles.welcomeTagline}>明日方舟AI辅助终端</div>
        </div>
      );
    case "events":
      return (
        <EventBadge
          events={item.events}
          interactive={item.interactive}
          className={styles.row}
        />
      );
    case "message":
      if (item.role === "assistant") {
        return (
          <div
            data-message-area
            className={`${styles.bubble} ${styles.assistant} ${styles.row}`}
          >
            {item.segments.map((seg, segIdx) => (
              <div
                key={segIdx}
                className={seg.transition ? styles.streaming : undefined}
              >
                <Markdown content={seg.content} />
                {item.isStreaming && segIdx === item.segments.length - 1 && (
                  <span className={styles.cursor} />
                )}
              </div>
            ))}
          </div>
        );
      }
      // M3-B：轮中止标记条（灰/斜体小字，独立样式——不走 error 分支）
      if (item.role === "aborted") {
        return (
          <div
            data-message-area
            className={`${styles.bubble} ${styles.aborted} ${styles.row}`}
          >
            {item.content}
          </div>
        );
      }
      return (
        <div
          data-message-area
          className={`${styles.bubble} ${styles[item.role]} ${styles.row}`}
        >
          <Markdown content={item.content} />
        </div>
      );
    case "pending":
      return (
        <button
          type="button"
          className={`${styles.steerPending} ${styles.row}`}
          title="点击取消"
          data-steer-pending
          onClick={() => onCancelPending?.(item.id)}
        >
          {item.text}
        </button>
      );
    case "compaction":
      return (
        <CompactionNotice
          foldedCount={item.foldedCount}
          summary={item.summary}
        />
      );
  }
});

// ── 组件 ──

interface Props {
  turns: SessionEvent[][];
  currentEvents: SessionEvent[];
  isFirstUse: boolean;
  /** 压缩记录（模块 D；undefined = 未压缩过）。决定压缩点提示卡位置与摘要 */
  compaction?: CompactionMeta;
  /** 当前进行中轮的插话 pending（待注入气泡；undefined/空 = 无） */
  pending?: readonly { id: string; text: string }[];
  /** pending 气泡点击取消（cancel_steer 成功移除后消失） */
  onCancelPending?: (entryId: string) => void;
  /** 当前对话 id：变化即视为切换对话（重置滑底标志，保证新对话能自动沉底） */
  conversationId?: string | null;
}

export function MessageList({ turns, currentEvents, isFirstUse, compaction, pending, onCancelPending, conversationId }: Props) {
  const listRef = useRef<HTMLDivElement>(null);

  /*
   * 用户手动上滑标志位。
   *
   * true  = 用户向上滚动了（距离底部 > 5px），停止自动滚底
   * false = 用户在最底部，允许自动滚底
   *
   * 用 useRef 而非 useState 的原因：
   * 滚动事件回调里不需要触发重渲染——这个值只在 useEffect 的
   * 自动滚底逻辑中被读取，ref 的变更不会导致额外渲染。
   */
  const userScrolledUpRef = useRef(false);

  /* 钉底原语：直接写 scrollTop = scrollHeight（浏览器自行钳到最大）。 */
  const pinNow = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, []);

  /* 提供给输入框的“稳定测量窗口”：测量期间接管滚动行为（见 messageScroll.ts）。
   * 贴底则测量后仍贴底；否则原位不动——避免输入框复位测量把滚动位置钳下去。 */
  useEffect(() => {
    registerMessageScrollHost({
      measure: (fn) => {
        const el = listRef.current;
        if (!el) return fn();
        const atBottom =
          el.scrollHeight - el.scrollTop - el.clientHeight <= STICK_PX;
        const saved = el.scrollTop;
        try {
          return fn();
        } finally {
          el.scrollTop = atBottom ? el.scrollHeight : saved;
        }
      },
    });
    return () => registerMessageScrollHost(null);
  }, []);

  // ── 展示项派生 ──
  //
  // 归档部分只在 turns / compaction / isFirstUse 变化时重算（内部再按轮缓存
  // 投影），流式刷新的每次 currentEvents 变更不再重算整段历史；进行中部分
  // 每次刷新重算，但规模限于当前轮。
  const archivedItems = useMemo(
    () => buildArchivedItems(turns, isFirstUse, compaction),
    [turns, isFirstUse, compaction]
  );
  const liveItems = useMemo(
    () => buildLiveItems(currentEvents, pending),
    [currentEvents, pending]
  );
  // 无进行中内容时直接复用归档数组（引用稳定 → 不触发多余的滚底副作用）
  const items = useMemo(
    () =>
      isFirstUse
        ? archivedItems
        : liveItems.length > 0
          ? [...archivedItems, ...liveItems]
          : archivedItems,
    [archivedItems, liveItems, isFirstUse]
  );

  /*
   * 用户发送新消息时强制重置滚底标志。
   *
   * 判断依据：新轮次归档（turns 增长），或进行中轮次的
   * 第一条事件（合成 user/message）出现。无论用户之前在哪个
   * 历史位置，发送新消息意味着"我想看刚才发的和接下来的回复"，
   * 此时自动滚底不应该被之前的 scrollUp 标志阻塞。
   */
  const prevTurnCountRef = useRef(turns.length);
  useEffect(() => {
    const turnsGrew = turns.length > prevTurnCountRef.current;
    const last = currentEvents[currentEvents.length - 1];
    if (turnsGrew || last?.type === "user/message") {
      userScrolledUpRef.current = false;
    }
    prevTurnCountRef.current = turns.length;
  }, [turns, currentEvents]);

  /*
   * 滚动事件处理。
   *
   * 用户向上滚动（离底 > 5px）→ 停止自动跟底；滑回距底 ≤ 5px → 恢复跟底。
   * 判据用“离底距离”而非“位置是否变小”：浏览器因布局变化钳位滚动位置时，
   * 落点就是当时的最大滚动量，即仍然贴底——不该被当成用户上滑。
   */
  const handleScroll = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    userScrolledUpRef.current =
      el.scrollHeight - el.scrollTop - el.clientHeight > STICK_PX;
  }, []);

  /*
   * 切换对话：重置滑底标志。
   *
   * 上一段对话“上滑过”的状态会随 ref 带到下一段，导致新对话完全不自动沉底。
   * 本副作用必须声明在钉底副作用之前：React 按声明顺序执行，否则钉底会先读到
   * 尚未重置的标志位而直接跳过。
   */
  useEffect(() => {
    userScrolledUpRef.current = false;
  }, [conversationId]);

  /*
   * 自动滚底（钉底）。
   *
   * 刻意不做“一次性”：切换对话/恢复时，输入区、上下文环、大段文本的布局会在
   * 随后几帧才落定，只钉一次会差一截——表现为“比较接近于底部但不在底部”。
   * 这里在 items 变化后跟一段收敛窗口，期间逐帧把滚动位置压到底（用户一上滑
   * 即停）。
   */
  useEffect(() => {
    if (userScrolledUpRef.current) return;
    let raf = 0;
    const deadline = performance.now() + PIN_SETTLE_MS;
    const tick = () => {
      if (userScrolledUpRef.current) return;
      pinNow();
      if (performance.now() < deadline) raf = requestAnimationFrame(tick);
    };
    tick();
    return () => cancelAnimationFrame(raf);
  }, [items, conversationId, pinNow]);

  /*
   * 对话区自身尺寸变化时保持沉底。
   *
   * 输入框随内容变长展开（高度是动画）会逐帧压缩对话区高度：此时 items 没变，
   * 上面的钉底副作用不会触发，最后一屏内容就被输入框挤出视口。这里盯列表自身
   * 尺寸，变化即重钉；用户已上滑（翻历史）时不动。
   */
  useEffect(() => {
    const el = listRef.current;
    if (!el || typeof ResizeObserver !== "function") return;
    const ro = new ResizeObserver(() => {
      if (userScrolledUpRef.current) return;
      pinNow();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [pinNow]);

  return (
    <div className={styles.list} ref={listRef} onScroll={handleScroll} data-message-list>
      {items.map((item, i) => (
        // 回调只传给 pending 行：归档行拿到恒为 undefined 的稳定属性，
        // 不会因为 ChatView 里 handleCancelSteer 换引用而整段重渲染
        <MessageRow
          key={i}
          item={item}
          onCancelPending={item.type === "pending" ? onCancelPending : undefined}
        />
      ))}
    </div>
  );
}
