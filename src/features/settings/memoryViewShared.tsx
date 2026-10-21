// 设置页 · 记忆二级视图的共享框架与格式化 helper

import { useEffect, useRef, type ReactNode } from "react";
import styles from "./SettingsMemory.module.css";

/** 当前打开的二级视图（null＝主列表） */
export type MemView =
  | { kind: "growth"; name: string }
  | { kind: "growthNew" }
  | { kind: "compiled" }
  | { kind: "library" }
  | null;

/** 「关于博士」页名（固定键；删除＝复位） */
export const PROFILE_NAME = "关于博士";

/** 「先前」（长期记忆）字数上限：与后端硬闸同源（orchestrator::memory::MAX_LONGTERM_CHARS） */
export const LONGTERM_MAX_CHARS = 600;

/** Unicode 码点数（对齐后端 chars().count()；不用 String.length） */
export function charCount(s: string): number {
  return [...s].length;
}

/** 标签文本 → 数组（、/逗号分隔） */
export function parseTags(text: string): string[] {
  return text
    .split(/[、,，]/)
    .map((t) => t.trim())
    .filter((t) => t.length > 0);
}

/** unix 毫秒 → YYYY-MM-DD（本地时区） */
export function fmtDate(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** 视图框架：返回 + 标题 + 内容 */
export function MemoryViewFrame({
  title,
  onBack,
  children,
}: {
  title: string;
  onBack: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    // 打开视图时把视野滚到视图顶端（内容区是滚动容器；无该 API 的环境跳过）
    ref.current?.scrollIntoView?.({ block: "start" });
  }, []);
  return (
    <div className={styles.view} ref={ref}>
      <div className={styles.viewHeader}>
        <button className={styles.backBtn} type="button" onClick={onBack}>
          ← 返回
        </button>
        <span className={styles.viewTitle}>{title}</span>
      </div>
      <div className={styles.viewBody}>{children}</div>
    </div>
  );
}
