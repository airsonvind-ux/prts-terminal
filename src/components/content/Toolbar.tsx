// src/components/content/Toolbar.tsx
// 对话标题栏 · 20px 固定高度
//
// 职责：展示当前对话标题 + 功能按钮。
// 不从父组件接 props——标题和按钮操作直接从 store 拿。

import { type MouseEvent } from "react";
import { useConversationStore } from "../../stores/useConversationStore";
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

  /*
   * 覆盖层按钮切换逻辑：当前已打开则关闭，否则打开目标面板。
   * 这样 ◷ 和 ⚙ 既是打开按钮也是关闭按钮，减少 UI 中的关闭入口。
   *
   * 对话处理中不再挡在门口（2026-10-08）：面板内按项锁，用户进得去、也看得见
   * 哪里可改、哪里暂不可改。原先整页禁用，用户点到的是一颗原生 disabled 按钮，
   * 连 title 提示都弹不出来——看起来就像坏了。
   * 真正需要挡的只有“有待审批”（确认卡会被遮罩盖住），那由 SettingsPanel 处理。
   */
  const toggleOverlay = (type: "history" | "settings") => {
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
          title="设置"
        >
          <span className={styles.icon}>⚙</span>
          <span className={styles.label}>设置</span>
        </button>
      </div>
    </div>
  );
}
