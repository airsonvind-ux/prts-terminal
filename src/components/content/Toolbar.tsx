// src/components/content/Toolbar.tsx
// 对话标题栏 · 20px 固定高度
//
// 职责：展示当前对话标题 + 功能按钮。
// 不从父组件接 props——标题和按钮操作直接从 store 拿。

import { type MouseEvent } from "react";
import { useConversationStore } from "../../stores/useConversationStore";
import { isProcessingBusy, useProcessingStore } from "../../stores/useProcessingStore";
import { useUIStore } from "../../stores/useUIStore";
import { useTypewriter } from "../../hooks/useTypewriter";
import styles from "./Toolbar.module.css";

interface Props {
  onDragHandle?: (e: MouseEvent) => void;
  /** false = 启动演出期间：标题留白（不剧透已有对话），演出淡出时开始逐字展开 */
  showTitle?: boolean;
}

export function Toolbar({ onDragHandle, showTitle = true }: Props) {
  const activeId = useConversationStore((s) => s.activeId);
  const conversations = useConversationStore((s) => s.conversations);

  const title = activeId
    ? conversations.find((c) => c.id === activeId)?.title
    : undefined;
  const pendingTitle = useConversationStore((s) => s.pendingTitle);

  const displayTitle = title ?? pendingTitle;

  // 标题逐字展开（45ms/字）：启动揭示那一刻开始打，切换对话时从零重打；
  // 旧标题没有退出动画，文字直接不见。
  const typedTitle = useTypewriter(displayTitle, showTitle);

  const newChat = useConversationStore((s) => s.newChat);
  const overlay = useUIStore((s) => s.overlay);
  const setOverlay = useUIStore((s) => s.setOverlay);
  // M3-B：busy（任一对话处理中）→ 设置入口禁用（防处理中打开设置遮住
  // 审批确认卡——overlay 遮罩会盖住 EventBadge 的确认按钮直到 300s 超时）
  const settingsBusy = useProcessingStore(isProcessingBusy);

  /*
   * 覆盖层按钮切换逻辑：当前已打开则关闭，否则打开目标面板。
   * 这样 ◷ 和 ⚙ 既是打开按钮也是关闭按钮，减少 UI 中的关闭入口。
   * busy 时 ⚙ 直接禁用（不进入 toggle——处理中不可开设置）。
   */
  const toggleOverlay = (type: "history" | "settings") => {
    if (type === "settings" && settingsBusy) return;
    setOverlay(overlay === type ? null : type);
  };

  const btn = (type: "history" | "settings") =>
    `${styles.btn}${overlay === type ? ` ${styles.isActive}` : ""}`;

  const isHistoryOpen = overlay === "history";

  return (
    <div className={styles.bar} onMouseDown={onDragHandle} onContextMenu={(e) => e.preventDefault()}>
      <span
        className={`${styles.title} ${isHistoryOpen ? styles.titleActive : ""}`}
        onClick={
          showTitle && displayTitle
            ? () => toggleOverlay("history")
            : undefined
        }
        onMouseDown={(e) => e.stopPropagation()}
      >
        {typedTitle}
      </span>
      <div className={styles.actions}>
        <button
          className={styles.btn}
          style={{ "--btn-expand-ratio": "0.283" } as React.CSSProperties}
          onClick={newChat}
          title="新对话"
        >
          <span className={styles.icon}>+</span>
          <span className={styles.label}>新对话</span>
        </button>
        <button
          className={btn("settings")}
          style={{ "--btn-expand-ratio": "0.217" } as React.CSSProperties}
          onClick={() => toggleOverlay("settings")}
          title={settingsBusy ? "对话处理中，暂不能打开设置" : "设置"}
          disabled={settingsBusy}
        >
          <span className={styles.icon}>⚙</span>
          <span className={styles.label}>设置</span>
        </button>
      </div>
    </div>
  );
}
