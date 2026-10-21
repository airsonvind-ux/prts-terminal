import type { MoodEvent, MoodIntensity, ToolDisplay } from "../../types";
import { FileLink } from "../../shared/ui/FileLink";
import styles from "./EventBadge.module.css";

// ── 情绪描述表（批 2-D 情绪卡片）：情绪 × 强度档位 → 独立文案 ──

const MOOD_DESCRIPTIONS: Record<
  "高兴" | "困惑" | "沮丧",
  [string, string, string, string]
> = {
  高兴: [
    "PRTS 浮在半空，晃晃悠悠，心情不错",
    "PRTS 很开心，飘得轻快",
    "PRTS 高兴坏了，在半空晃个不停",
    "PRTS 高兴得想在半空转个圈",
  ],
  困惑: [
    "PRTS 没太想明白，歪了歪头",
    "PRTS 有点犯难，歪着头琢磨",
    "PRTS 想不明白了，歪着头，又从头捋了一遍",
    "PRTS 怎么也想不通，歪着头，翻来覆去地琢磨",
  ],
  沮丧: [
    "PRTS 提不起精神，安静地沉下去",
    "PRTS 有些沮丧，沉下去就不想动",
    "PRTS 很沮丧，悬在低处，闷着",
    "PRTS 沮丧极了，低低地悬着，一声不响",
  ],
};

/** 情绪事件 → 描述文本（intensity 由解析钳位在 0~3，索引安全） */
export function describeMood(ev: MoodEvent): string {
  const idx: MoodIntensity = ev.intensity as MoodIntensity;
  return MOOD_DESCRIPTIONS[ev.mood][idx];
}

// ── 展示切片渲染（工具卡片「在做什么、对什么做」） ──

/**
 * 展示文本 / 链接标签的视觉长度上限（纯长度约束，不做「等 N 个」数量逻辑）。
 * 实现对超长文本截断，值可视觉调校。
 */
const DISPLAY_TEXT_MAX = 60;

function truncateDisplay(s: string): string {
  return s.length > DISPLAY_TEXT_MAX ? `${s.slice(0, DISPLAY_TEXT_MAX)}…` : s;
}

/** 展示切片是否有可渲染内容（决定进行中态是否保留「...」尾缀） */
export function hasDisplay(display: ToolDisplay | null): boolean {
  return Boolean(display && (display.text || display.link || display.full));
}

/**
 * 展示切片后缀：文本（失败态用空格分隔，其余全角冒号）+ 链接（文件/URL）。
 * 链接在三态均保留（挂在 tool/call 侧数据上，失败不回退）。
 */
export function renderDisplaySuffix(display: ToolDisplay | null, failed: boolean) {
  if (!display || !hasDisplay(display)) return null;
  const text = display.text ? truncateDisplay(display.text) : "";
  const link = display.link ?? null;
  // 截断是视觉约束：悬停给全文，「看全文的渠道」始终在
  const full = display.full ?? null;
  return (
    <>
      {text && (
        <>
          {failed ? " " : "："}
          <span title={full ?? undefined}>{text}</span>
        </>
      )}
      {link && (
        <>
          {" "}
          <FileLink
            path={link.target}
            linkKind={link.kind}
            className={styles.displayLink}
            title={link.target}
          >
            {truncateDisplay(link.label)}
          </FileLink>
        </>
      )}
    </>
  );
}
