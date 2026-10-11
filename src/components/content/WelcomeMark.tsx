// src/components/content/WelcomeMark.tsx
// PRTS 欢迎字标（单一来源）：logo + 细线 + 标语。
//
// 使用处（两处的外观必须完全一致，样式收口在本组件）：
//   - MessageList 欢迎页（无对话时）：静态呈现
//   - BootSequence 幕二（过渡页）：entering 态由父级控制——置位时保持
//     入场前状态，撤下时触发字标收拢 / 细线展开的入场过渡
//
// 动效口径：入场 ease-out（cubic-bezier(0,0,.2,1)）；时长与组件常量同步。

import styles from "./WelcomeMark.module.css";

interface Props {
  /** true = 保持入场前状态（过渡幕未切换时）；false/缺省 = 落定呈现 */
  entering?: boolean;
}

export function WelcomeMark({ entering = false }: Props) {
  return (
    <div
      className={`${styles.mark} ${entering ? styles.entering : ""}`}
      data-guide="welcome-mark"
    >
      <div className={styles.logo}>PRTS</div>
      <div className={styles.underline} />
      <div className={styles.tagline}>明日方舟AI辅助终端</div>
    </div>
  );
}
