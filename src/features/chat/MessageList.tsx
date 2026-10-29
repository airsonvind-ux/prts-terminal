// src/components/content/MessageList.tsx
// 消息列表 · 核心展示组件（架构升级 · 模块 1：投影缓存渲染）
//
// 职责：将 turns（已归档轮次的 SessionEvent 序列）+ currentEvents
//       （进行中轮次的实时投影）按时间线交错排列为统一的展示项列表，
//       渲染消息气泡和工具事件卡片。
//       滚动所有权与缩放保护由 useMessageScroll 承担。
//
// 消息气泡/事件卡片全部由投影函数派生（src/projection.ts），
// 不再维护独立的 messages 数组。

import { useMemo } from "react";
import type { SessionEvent } from "../../types";
import type { CompactionMeta } from "../../types";
import { buildArchivedItems, buildLiveItems } from "./messageItems";
import { MessageRow } from "./MessageRow";
import { useMessageScroll } from "./useMessageScroll";
import styles from "./MessageList.module.css";

export { anchorDelta } from "./useMessageScroll";

// ── 组件 ──

interface Props {
  turns: SessionEvent[][];
  currentEvents: SessionEvent[];
  /** 当前没有对话（草稿态 / 首次使用）→ 对话区呈现欢迎字标 */
  showWelcome: boolean;
  /** 中止已发出、轮未收尾（2026-10-08）：轮尾追加一条「正在中止…」 */
  aborting?: boolean;
  /** 压缩记录（模块 D；undefined = 未压缩过）。决定压缩点提示卡位置与摘要 */
  compaction?: CompactionMeta;
  /** 当前对话的插话 pending（待注入气泡；跨轮留存，undefined/空 = 无） */
  pending?: readonly { id: string; text: string }[];
  /** pending 气泡点击取消（cancel_steer 成功移除后消失） */
  onCancelPending?: (entryId: string) => void;
  /** 当前对话 id：变化即视为切换对话（重置滑底标志，保证新对话能自动沉底） */
  conversationId?: string | null;
}

export function MessageList({ turns, currentEvents, showWelcome, compaction, pending, onCancelPending, conversationId, aborting = false }: Props) {
  // ── 展示项派生 ──
  //
  // 归档部分只在 turns / compaction / showWelcome 变化时重算（内部再按轮缓存
  // 投影），流式刷新的每次 currentEvents 变更不再重算整段历史；进行中部分
  // 每次刷新重算，但规模限于当前轮。
  const archivedItems = useMemo(
    () => buildArchivedItems(turns, showWelcome, compaction),
    [turns, showWelcome, compaction]
  );
  const liveItems = useMemo(
    () => buildLiveItems(currentEvents, pending, aborting),
    [currentEvents, pending, aborting]
  );
  // 无进行中内容时直接复用归档数组（引用稳定 → 不触发多余的滚底副作用）
  const items = useMemo(
    () =>
      showWelcome
        ? archivedItems
        : liveItems.length > 0
          ? [...archivedItems, ...liveItems]
          : archivedItems,
    [archivedItems, liveItems, showWelcome]
  );

  const { listRef, moreBelow, scrollToBottomNow, handleScroll } = useMessageScroll({
    items,
    turns,
    currentEvents,
    conversationId,
  });

  return (
    <div className={styles.listArea}>
      <div
        className={styles.list}
        ref={listRef}
        onScroll={handleScroll}
        data-message-list
      >
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
      {/* 回到底部（圆形、水平居中、浮在输入区上方）。
          判据是几何事实 moreBelow（视口下方还有内容）：按钮只在点得出结果时
          出现；欢迎页（showWelcome）无对话，即便被压出溢出（极小窗口）也不出现。 */}
      {!showWelcome && moreBelow && (
        <button
          type="button"
          className={styles.scrollFab}
          aria-label="回到最新"
          title="回到最新"
          onClick={scrollToBottomNow}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <polyline points="6 9 12 15 18 9" />
          </svg>
        </button>
      )}
    </div>
  );
}
