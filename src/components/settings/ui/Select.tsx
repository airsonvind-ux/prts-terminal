// 设置页通用组件 · Select（自绘下拉）
//
// 为什么不用原生 <select>：收起态能用 CSS 抹平，**展开的选项列表**是系统绘制的，
// 改不动——在 PRTS 的深色界面上就是一块突兀的默认样式。这里照「模型服务」页
// 既有的下拉形态做成通用件：触发按钮 + 绝对定位菜单 + 选项行。
//
// 关闭时机：菜单项用 onMouseDown（先于 click 到达）并阻止冒泡；点别处由
// document 级 mousedown 收摊；禁用时立即收起。

import { useEffect, useRef, useState } from "react";
import styles from "./settings-ui.module.css";

export interface SelectOption {
  value: string;
  label: string;
}

export interface SelectProps {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  /** 末尾附加的单项（前面自动加分隔线），如「自定义…」 */
  footer?: SelectOption;
  /** 无匹配项时显示的文本（如加载中） */
  placeholder?: string;
  /** 无障碍名（不显示） */
  ariaLabel?: string;
}

export function Select({
  value,
  options,
  onChange,
  disabled = false,
  footer,
  placeholder,
  ariaLabel,
}: SelectProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  const current = options.find((o) => o.value === value);

  /** 选中：先收菜单，值变了才回调 */
  const pick = (next: string) => (e: React.MouseEvent) => {
    e.stopPropagation();
    e.nativeEvent.stopImmediatePropagation();
    setOpen(false);
    if (next !== value) onChange(next);
  };

  const item = (o: SelectOption) => (
    <div
      key={o.value}
      className={`${styles.dropdownItem} ${
        o.value === value ? styles.dropdownItemActive : ""
      }`}
      onMouseDown={pick(o.value)}
    >
      {o.label}
    </div>
  );

  return (
    <div className={styles.dropdown} ref={rootRef}>
      <button
        type="button"
        className={`${styles.dropdownTrigger} ${open ? styles.dropdownTriggerOpen : ""}`}
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        <span className={styles.dropdownValue}>
          {current?.label ?? placeholder ?? "—"}
        </span>
        <span className={styles.dropdownArrow}>▾</span>
      </button>
      <div
        className={`${styles.dropdownMenu} ${open ? styles.dropdownMenuOpen : ""}`}
      >
        {options.map(item)}
        {footer && (
          <>
            <div className={styles.dropdownDivider} />
            {item(footer)}
          </>
        )}
      </div>
    </div>
  );
}
