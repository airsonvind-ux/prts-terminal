// 设置页通用组件 · ChoiceCard（可选卡片）
//
// 用一个 label 包住原生 radio：语义、键盘（Tab 到组、方向键在组内切换）和
// 测试里的 input[type=radio] 查询都保留，视觉上换成整块卡片——
// 小字号下 radio 的小圆点几乎看不见，卡片把「选中的是哪一块」写在
// 边框、底色和左侧 2px 竖条上。
//
// 选中态由 React 的 checked 驱动（不靠 CSS :has），disabled 时整体降透明、
// 不响应点击；键盘焦点用 :focus-within 画环。

import styles from "./settings-ui.module.css";

interface ChoiceCardProps {
  name: string;
  checked: boolean;
  title: string;
  description: string;
  onSelect: () => void;
  disabled?: boolean;
}

export function ChoiceCard({
  name,
  checked,
  title,
  description,
  onSelect,
  disabled,
}: ChoiceCardProps) {
  return (
    <label
      className={[
        styles.card,
        checked ? styles.cardActive : "",
        disabled ? styles.cardDisabled : "",
      ]
        .filter(Boolean)
        .join(" ")}
    >
      <input
        type="radio"
        name={name}
        className={styles.cardInput}
        checked={checked}
        disabled={disabled}
        onChange={onSelect}
      />
      <span className={styles.cardBody}>
        <span className={styles.cardTitle}>{title}</span>
        <span className={styles.cardDescription}>{description}</span>
      </span>
    </label>
  );
}
