import type { MoodEvent, MoodIntensity, ToolDisplay } from "../../types";
import { FileLink } from "../../shared/ui/FileLink";
import styles from "./eventDisplay.module.css";

// ── 情绪描述表（MOOD 草案 3）：没有心声时的旁白兜底 ──

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

/** 未知偏向（没有动作的那一类）的旁白兜底 */
const NEUTRAL_DESCRIPTION = "PRTS 没说什么，安静地待着";

// ── 情绪卡配色（效价/唤醒两轴，不再一个情绪一个色相） ──
//
// 每档给两组颜色：竖线用主色，文字用同色系提亮两档。强度只调竖线明度
// （弱 −6%、中不变、强 +6%），色相恒定——颜色只强化，不承担信息。

type Triple = [number, number, number];

const MOOD_PALETTE: Record<"高兴" | "困惑" | "沮丧", { line: Triple; text: Triple }> = {
  高兴: { line: [20, 46, 53], text: [20, 40, 74] },
  困惑: { line: [160, 26, 46], text: [160, 22, 72] },
  沮丧: { line: [215, 28, 48], text: [215, 28, 70] },
};

/** 自由偏向与旧事件的中性色：不参与运动，也就不参与情绪色相 */
const FREE_PALETTE: { line: Triple; text: Triple } = {
  line: [210, 10, 40],
  text: [210, 8, 58],
};

function cssColor([h, s, l]: Triple): string {
  return `hsl(${h} ${s}% ${l}%)`;
}

/** 强度对竖线明度的偏移：1 弱 / 2 中 / 3 强 */
const LINE_LIGHTNESS_OFFSET: Record<0 | 1 | 2 | 3, number> = {
  0: 0,
  1: -6,
  2: 0,
  3: 6,
};

/** 强度档位文案（只在卡面出现，不进事件） */
export function intensityLabel(intensity: number): string | null {
  if (intensity === 1) return "弱";
  if (intensity === 2) return "中";
  if (intensity === 3) return "强";
  return null;
}

/** 情绪事件 → 竖线 / 文字两色 */
export function moodColors(ev: MoodEvent): { line: string; text: string } {
  const palette = ev.mood ? MOOD_PALETTE[ev.mood] : FREE_PALETTE;
  const [h, s, l] = palette.line;
  const offset = LINE_LIGHTNESS_OFFSET[(ev.intensity ?? 0) as 0 | 1 | 2 | 3] ?? 0;
  return {
    line: cssColor([h, s, Math.min(70, Math.max(28, l + offset))]),
    text: cssColor(palette.text),
  };
}

/** 情绪事件 → 旁白文本（缺心声时兜底；intensity 已钳位在 0~3） */
export function describeMood(ev: MoodEvent): string {
  if (!ev.mood) return NEUTRAL_DESCRIPTION;
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
