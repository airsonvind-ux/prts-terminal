// 设置页通用组件 · Section（沿用既有 SettingsSection 形态，PRTS 极简版）
//
// 结构：可选 title 标题行 + body 主体 + Note/Warning 说明块（页内任意位置使用，
// 非强制 slot——按内容顺序自然摆放）。说明文字约定：常态一行式 hint 用
// SettingsRow.hint，节级的一行提示用 SectionHint（无底无框），
// 长文只进 Note（中性说明）与 Warning（警示）块。

import type { ReactNode } from "react";
import styles from "./settings-ui.module.css";

interface SectionProps {
  title?: ReactNode;
  /** 无可见标题时的节语义（例如关于页的身份区） */
  ariaLabel?: string;
  children: ReactNode;
  className?: string;
}

export function SettingsSection({
  title,
  ariaLabel,
  children,
  className,
}: SectionProps) {
  return (
    <section
      aria-label={ariaLabel}
      className={[styles.section, className].filter(Boolean).join(" ")}
    >
      {title && <h2 className={styles.sectionTitle}>{title}</h2>}
      <div className={styles.sectionBody}>{children}</div>
    </section>
  );
}

/** 节级一行提示（无底无框的常态小字；节内普通说明用它，长文才用 SectionNote） */
export function SectionHint({ children }: { children: ReactNode }) {
  return <div className={styles.sectionHint}>{children}</div>;
}

/** 中性说明块（长文/补充说明用；一行式 hint 请用 SettingsRow.hint） */
export function SectionNote({ children }: { children: ReactNode }) {
  return <div className={styles.sectionNote}>{children}</div>;
}

/** 警示块（风险说明用；文字可多行，属"长文只进警示块"的落点） */
export function SectionWarning({ children }: { children: ReactNode }) {
  return <div className={styles.sectionWarning}>{children}</div>;
}
