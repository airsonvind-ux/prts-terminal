// 情绪卡（情绪系统草案 3）
//
// 消息流里的独立块，位置跟随 mood/event。形态：无边框、无头部、不可
// 折叠——一条 2px 竖线 + 一行正文（可换行，不截断）。
//
// 一行里依次是：偏向词（可选「· 弱/中/强」）+ 心声。心声来自括号内
// 的第三段；没有心声就回落到 describeMood 的旁白，旧归档（只有 mood +
// intensity）也走这条路径。
//
// 颜色按效价/唤醒两轴取值，不再一个情绪一个色相：竖线用主色、文字用
// 提亮两档的同色系；强度只改竖线明度，不改色相。

import type { CSSProperties } from "react";
import type { MoodEvent, SessionEvent } from "../../types";
import { describeMood, moodColors, intensityLabel } from "./eventDisplay";
import styles from "./MoodCard.module.css";

export function MoodCard({ events }: { events: SessionEvent[] }) {
  const moods = events.filter(
    (event): event is MoodEvent => event.type === "mood/event"
  );
  if (moods.length === 0) return null;

  return (
    <div className={styles.mood} data-mood-card>
      {moods.map((event, index) => {
        const { line, text } = moodColors(event);
        // 偏向词：自由偏向取原词，已知偏向取归一词；旧事件只有 mood
        const bias = event.tint ?? event.mood ?? null;
        const strength = intensityLabel(event.intensity);
        return (
          <p
            key={index}
            className={styles.line}
            style={
              {
                "--mood-line": line,
                "--mood-text": text,
              } as CSSProperties
            }
          >
            {bias && (
              <span className={styles.bias} data-mood-bias>
                {bias}
                {strength && <span className={styles.strength}> · {strength}</span>}
              </span>
            )}
            <span className={styles.voice} data-mood-voice>
              {event.text ?? describeMood(event)}
            </span>
          </p>
        );
      })}
    </div>
  );
}
