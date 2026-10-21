// 设置页通用组件 · Row（label + 一行 hint + control）
//
// 两种形态：
//   stacked（缺省）—— label 在上、hint 一行小字、控件在下（窄面板下的表单默认）
//   controlInline  —— 控件与 label 同行、hint 缩进在下方（开关类，如勾选框）
// 说明文字约定：hint 为一行式（简短），风险类 hintVariant="warn" 变警示色；
// 更长说明用 SectionNote/SectionWarning。

import type { ReactNode } from "react";
import styles from "./settings-ui.module.css";

interface RowProps {
  label: ReactNode;
  /** 一行式说明（简短；缺省不显示） */
  hint?: ReactNode;
  /** warn = 风险/注意类说明文字（--warn 色） */
  hintVariant?: "default" | "warn";
  /** true = 控件与 label 同行（开关类）；缺省 = 纵向堆叠 */
  controlInline?: boolean;
  control: ReactNode;
}

export function SettingsRow({
  label,
  hint,
  hintVariant = "default",
  controlInline = false,
  control,
}: RowProps) {
  if (controlInline) {
    return (
      <div className={styles.rowInline}>
        <label className={styles.rowInlineHead}>
          {control}
          <span>{label}</span>
        </label>
        {hint && (
          <div
            className={
              hintVariant === "warn" ? styles.rowInlineHintWarn : styles.rowInlineHint
            }
          >
            {hint}
          </div>
        )}
      </div>
    );
  }

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
