// 设置页通用组件 · Row（label + 一行 hint + control）
//
// Hanako SettingsRow 形态（PRTS 极简版，stacked 布局为主——窄面板下 label
// 在上、hint 一行小字、控件在下）。说明文字约定：hint 为一行式（简短），
// 风险类 hintVariant="warn" 变警示色；更长说明用 SectionNote/SectionWarning。

import type { ReactNode } from "react";
import styles from "./settings-ui.module.css";

interface RowProps {
  label: ReactNode;
  /** 一行式说明（简短；缺省不显示） */
  hint?: ReactNode;
  /** warn = 风险/注意类说明文字（--warn 色） */
  hintVariant?: "default" | "warn";
  control: ReactNode;
}

export function SettingsRow({ label, hint, hintVariant = "default", control }: RowProps) {
  return (
    <div className={styles.row}>
      <div className={styles.rowLabel}>{label}</div>
      {hint && (
        <div
          className={
            hintVariant === "warn" ? styles.rowHintWarn : styles.rowHint
          }
        >
          {hint}
        </div>
      )}
      <div className={styles.rowControl}>{control}</div>
    </div>
  );
}
