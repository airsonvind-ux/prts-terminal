// 设置页通用组件 · Switch（开关）
//
// 用 role="switch" 的按钮而不是「伪装成滑块的 checkbox」：
//   - 键盘天然可用（Enter/Space 都触发 click），不必手写 onKeyDown；
//   - aria-checked 交给读屏，label 由参数给（视觉上由行内文字承担）；
//   - 与 Button 同一套颜色：亮起用 accent，暗态用 depth-4，不用系统控件蓝。
//
// 不给宽度过渡（规格 §八：不加位移动画）——只过渡颜色与滑块位移 120ms。

import styles from "./settings-ui.module.css";

interface SwitchProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  /** 读屏与悬停提示用；视觉上的文字由调用方的行标题承担 */
  label: string;
  disabled?: boolean;
  /** 覆盖悬停提示（例如「对话处理中」这类临时原因） */
  title?: string;
}

export function Switch({ checked, onChange, label, disabled, title }: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      title={title ?? label}
      disabled={disabled}
      className={[styles.switch, checked ? styles.switchOn : ""]
        .filter(Boolean)
        .join(" ")}
      onClick={() => onChange(!checked)}
    />
  );
}
