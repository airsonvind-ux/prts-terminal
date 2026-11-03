// 设置页通用组件 · Segmented（分段选择）
//
// 2~4 个互斥选项，一排等分按钮：选中 = accent 字 + 12% accent 底。
// 用它替代 radio 组：设置页里 radio 的圆点在小字号下几乎看不见，
// 而分段控件把「当前选的是哪个」直接写在按钮底色上。
//
// 键盘：按钮阵列，Tab 进入后方向键不接管（浏览器默认的按钮行为），
// 每个选项都是独立可聚焦按钮，读屏用 aria-pressed 报状态。

import styles from "./settings-ui.module.css";

export interface SegmentedOption<T extends string> {
  value: T;
  label: string;
  /** 悬停提示（选项含义需要一句话解释时给） */
  title?: string;
}

interface SegmentedProps<T extends string> {
  options: readonly SegmentedOption<T>[];
  value: T;
  onChange: (next: T) => void;
  disabled?: boolean;
  ariaLabel?: string;
}

export function Segmented<T extends string>({
  options,
  value,
  onChange,
  disabled,
  ariaLabel,
}: SegmentedProps<T>) {
  return (
    <div className={styles.segmented} role="group" aria-label={ariaLabel}>
      {options.map((option) => {
        const active = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            title={option.title}
            aria-pressed={active}
            disabled={disabled}
            className={[styles.segment, active ? styles.segmentActive : ""]
              .filter(Boolean)
              .join(" ")}
            onClick={() => onChange(option.value)}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
