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

import { useMemo, useRef, useEffect, useCallback, useState } from "react";
import type { SessionEvent } from "../../types";
import type { CompactionMeta } from "../../types";
import { deriveTextSegments, isMetaEvent, splitTurnByUserBoundary, stripReminderBlock } from "../../projection";
import type { TextSegment } from "../../projection";
import { EventBadge } from "./EventBadge";
import { Markdown } from "./Markdown";
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

function buildDisplayItems(
  turns: SessionEvent[][],
  currentEvents: SessionEvent[],
  isFirstUse: boolean,
  compaction: CompactionMeta | undefined,
  pending: readonly { id: string; text: string }[] | undefined
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
    appendTurnItems(items, turns[i], false);
  }
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
}

export function MessageList({ turns, currentEvents, isFirstUse, compaction, pending, onCancelPending }: Props) {
  const listRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

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

  const items = useMemo(
    () => buildDisplayItems(turns, currentEvents, isFirstUse, compaction, pending),
    [turns, currentEvents, isFirstUse, compaction, pending]
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
   * 用户向上滚动（距离底部 > 5px）→ 立即停止自动跟底。
   * 用户滑回最底部（距离 ≤ 5px）→ 恢复自动跟底。
   *
   * 5px 的阈值仅用于过滤浏览器子像素滚动抖动，不做"滚多远才停"的
   * 距离判断。任何有意图的向上滚动都会在第一个像素就触发暂停。
   */
  const handleScroll = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    const distanceFromBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight;
    userScrolledUpRef.current = distanceFromBottom > 5;
  }, []);

  /*
   * 自动滚底。
   *
   * items 每次变化都检查标志位：用户未上滑 → 滚底；用户上滑了 → 不滚。
   * behavior: "instant" 而非 "smooth"——之前已验证 smooth 在 WebView2
   * 透明窗口下会导致持续闪烁。
   */
  useEffect(() => {
    if (!userScrolledUpRef.current) {
      bottomRef.current?.scrollIntoView({ behavior: "instant" });
    }
  }, [items]);

  return (
    <div className={styles.list} ref={listRef} onScroll={handleScroll} data-message-list>
      {items.map((item, i) => {
        switch (item.type) {
          case "welcome":
            return (
              <div key="welcome" className={styles.welcome}>
                <div className={styles.welcomeLogo}>PRTS</div>
                <div className={styles.welcomeTagline}>明日方舟AI辅助终端</div>
              </div>
            );
          case "events":
            return (
              <EventBadge
                key={`events-${i}`}
                events={item.events}
                interactive={item.interactive}
              />
            );
          case "message":
            if (item.role === "assistant") {
              return (
                <div
                  key={`msg-${i}`}
                  data-message-area
                  className={`${styles.bubble} ${styles.assistant}`}
                >
                  {item.segments.map((seg, segIdx) => (
                    <div
                      key={segIdx}
                      className={
                        seg.transition ? styles.streaming : undefined
                      }
                    >
                      <Markdown content={seg.content} />
                      {item.isStreaming &&
                        segIdx === item.segments.length - 1 && (
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
                  key={`msg-${i}`}
                  data-message-area
                  className={`${styles.bubble} ${styles.aborted}`}
                >
                  {item.content}
                </div>
              );
            }
            return (
              <div
                key={`msg-${i}`}
                data-message-area
                className={`${styles.bubble} ${styles[item.role]}`}
              >
                <Markdown content={item.content} />
              </div>
            );
          case "pending":
            return (
              <button
                key={`pending-${item.id}`}
                type="button"
                className={styles.steerPending}
                title="点击取消"
                data-steer-pending
                onClick={() => onCancelPending?.(item.id)}
              >
                {item.text}
              </button>
            );
            // 模块 D：压缩点提示卡（本地条目，非 LLM 消息）
            case "compaction":
              return (
                <CompactionNotice
                  key={`compaction-${i}`}
                  foldedCount={item.foldedCount}
                  summary={item.summary}
                />
              );
        }
      })}
      <div ref={bottomRef} />
    </div>
  );
}
