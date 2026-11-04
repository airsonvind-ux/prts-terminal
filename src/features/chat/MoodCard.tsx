// 情绪卡（MOOD v2）
//
// 消息流里的独立块，位置跟随 mood/event。形态按规格：无边框、无头部、
// 不可折叠——一条短竖线 + 正文，颜色跟随偏向色相。
//
// 有心声就显示心声（正体、第一人称，模型自己写的那句）；没有心声回落到
// describeMood 的旁白，旧归档（只有 mood + intensity）也走这条路径。
// 强度不做额外视觉表达：文案本身已经表达程度。

import type { MoodEvent, SessionEvent } from "../../types";
import { describeMood, moodHue } from "./eventDisplay";
import styles from "./MoodCard.module.css";

export function MoodCard({ events }: { events: SessionEvent[] }) {
  const moods = events.filter(
    (event): event is MoodEvent => event.type === "mood/event"
  );
  if (moods.length === 0) return null;

  return (
    <div className={styles.mood} data-mood-card>
      {moods.map((event, index) => (
        <p
          key={index}
          className={styles.line}
          style={{ "--mood-hue": moodHue(event) } as React.CSSProperties}
        >
          {event.text ?? describeMood(event)}
        </p>
      ))}
    </div>
  );
}
