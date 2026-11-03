// 设置页通用组件 · Stat（读数块）
//
// 一个极小的「标签 + 值」两行块，用于把版本、路径、计数这类事实排成仪表盘。
// 值默认等宽（tabular-nums）——数字不会因为位数变化左右跳；accent 只给
// 真正的读数（数据版本、用量数字），不给普通文本，避免青色泛滥。

import type { ReactNode } from "react";
import styles from "./settings-ui.module.css";

interface StatProps {
  label: string;
  value: ReactNode;
  /** 读数强调色（方舟青）：只给数字类事实 */
  accent?: boolean;
  /** 悬停显示完整值（路径这类会被省略号截断的） */
  title?: string;
}

export function Stat({ label, value, accent, title }: StatProps) {
  return (
    <div className={styles.stat} title={title}>
      <span className={styles.statLabel}>{label}</span>
      <span
        className={[styles.statValue, accent ? styles.statValueAccent : ""]
          .filter(Boolean)
          .join(" ")}
      >
        {value}
      </span>
    </div>
  );
}
