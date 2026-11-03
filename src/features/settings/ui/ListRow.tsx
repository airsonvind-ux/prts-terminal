// 设置页通用组件 · ListRow（列表项：主行 + 副行 + 右侧槽位）
//
// 与 SettingsRow 的分工（UI 规格草案 2 §二）：
//   SettingsRow —— 表单字段（label / hint / control 纵排），不改
//   ListRow     —— 列表项（一行标题、一行副文、右侧放开关或按钮）
//
// 交互：给了 onActivate 才是可进入的行（role=button、Tab 聚焦、Enter/Space）。
// 行内的开关/按钮放在 trailing 槽位，槽位自己 stopPropagation——点开关不会
// 顺带进详情，两个命中区不重叠。

import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import styles from "./settings-ui.module.css";

export type ListRowDot = "ok" | "bad" | "idle";

interface ListRowProps {
  title: ReactNode;
  /** 第二行：版本 · 形态 · 状态，异常时换成原因 */
  subtitle?: ReactNode;
  /** 右侧槽位（开关、按钮）；点击不触发行进入 */
  trailing?: ReactNode;
  /** 给了就可进入（详情/展开）；缺省是只读行 */
  onActivate?: () => void;
  /**
   * 行内自带可点区域（例如整段标题是链接）时的行级悬停反馈。
   * 与 onActivate 的区别：不挂 role/tabIndex，键盘焦点仍落在行内真正的链接上。
   */
  hoverable?: boolean;
  /** danger = 副行变红并截断（异常原因） */
  tone?: "default" | "danger";
  /** 左侧状态点；缺省不画 */
  dot?: ListRowDot;
  /** 置灰不可点（未登记目录这类只读行） */
  muted?: boolean;
  /** 暂时不可操作（例如插件启动中） */
  disabled?: boolean;
  /** 长标题/副文的完整内容（截断时悬停可见） */
  titleAttr?: string;
}

const DOT_CLASS: Record<ListRowDot, string> = {
  ok: styles.listRowDotOk,
  bad: styles.listRowDotBad,
  idle: styles.listRowDotIdle,
};

export function ListRow({
  title,
  subtitle,
  trailing,
  onActivate,
  hoverable = false,
  tone = "default",
  dot,
  muted = false,
  disabled = false,
  titleAttr,
}: ListRowProps) {
  const interactive = Boolean(onActivate) && !disabled;

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!interactive) return;
    if (event.key !== "Enter" && event.key !== " ") return;
    if (event.nativeEvent.isComposing) return;
    event.preventDefault();
    onActivate?.();
  };

  const className = [
    styles.listRow,
    interactive ? styles.listRowInteractive : "",
    hoverable && !interactive ? styles.listRowHoverable : "",
    muted ? styles.listRowMuted : "",
    disabled ? styles.listRowDisabled : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      className={className}
      role={interactive ? "button" : undefined}
      tabIndex={interactive ? 0 : undefined}
      aria-disabled={disabled || undefined}
      title={titleAttr}
      onClick={interactive ? () => onActivate?.() : undefined}
      onKeyDown={interactive ? onKeyDown : undefined}
    >
      {dot && (
        <span
          className={`${styles.listRowDot} ${DOT_CLASS[dot]}`}
          aria-hidden="true"
        />
      )}
      <span className={styles.listRowText}>
        <span className={styles.listRowTitle}>{title}</span>
        {subtitle !== undefined && subtitle !== null && subtitle !== "" && (
          <span
            className={
              tone === "danger" ? styles.listRowSubDanger : styles.listRowSub
            }
          >
            {subtitle}
          </span>
        )}
      </span>
      {trailing !== undefined && (
        <span
          className={styles.listRowTrailing}
          onClick={(event) => event.stopPropagation()}
        >
          {trailing}
        </span>
      )}
    </div>
  );
}
