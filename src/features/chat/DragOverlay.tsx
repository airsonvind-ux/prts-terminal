// src/components/content/DragOverlay.tsx
// 拖入遮罩 —— 拖拽文件到窗口时现形，提示「松开以添加附件」。
//
// 只在拖拽进行中出现（enter/over 显示，leave/drop 隐藏）：平时零占位，
// 不挤输入区；需要时现形，兼作入口的可发现性。不拦截事件（pointer-events:none）。

import styles from "./DragOverlay.module.css";

export function DragOverlay({ active }: { active: boolean }) {
  if (!active) return null;
  return (
    <div className={styles.overlay}>
      <div className={styles.hint}>松开把文件发给 PRTS</div>
    </div>
  );
}
