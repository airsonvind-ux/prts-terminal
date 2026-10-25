// 设置页通用组件 · Select（自绘下拉）
//
// 为什么不用原生 <select>：收起态能用 CSS 抹平，**展开的选项列表**是系统绘制的，
// 改不动——在 PRTS 的深色界面上就是一块突兀的默认样式。这里照「模型服务」页
// 既有的下拉形态做成通用件：触发按钮 + 绝对定位菜单 + 选项行。
//
// 键盘契约：触发按钮 Enter/Space 打开；ArrowDown/Up、Home/End 在选项间移动；
// Enter/Space 选中；Escape 关闭并回到触发按钮。选项是原生 button + role=option，
// 不用 div 模拟，避免“看得见但键盘选不了”。

import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
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
  const [activeValue, setActiveValue] = useState(value);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const optionRefs = useRef(new Map<string, HTMLButtonElement>());
  const listId = useId();

  const allOptions = footer ? [...options, footer] : options;
  const current = options.find((o) => o.value === value);

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

  // 打开时把焦点放到当前值；没有当前值就落到第一项。
  useEffect(() => {
    if (!open) return;
    const values = allOptions.map((o) => o.value);
    const next = values.includes(value) ? value : values[0] ?? "";
    setActiveValue(next);
    // 只在打开瞬间决定初始项；后续方向键自己维护 activeValue。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (!open) return;
    optionRefs.current.get(activeValue)?.focus();
  }, [activeValue, open]);

  const close = (restoreTrigger = true) => {
    setOpen(false);
    if (restoreTrigger) triggerRef.current?.focus();
  };

  const pick = (next: string) => {
    setOpen(false);
    if (next !== value) onChange(next);
    triggerRef.current?.focus();
  };

  const handleTriggerKeyDown = (
    event: ReactKeyboardEvent<HTMLButtonElement>
  ) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!open) setOpen(true);
    }
  };

  const handleOptionKeyDown = (
    event: ReactKeyboardEvent<HTMLButtonElement>,
    index: number
  ) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      const next = allOptions[(index + 1) % allOptions.length];
      if (next) setActiveValue(next.value);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      const next =
        allOptions[(index - 1 + allOptions.length) % allOptions.length];
      if (next) setActiveValue(next.value);
    } else if (event.key === "Home") {
      event.preventDefault();
      const next = allOptions[0];
      if (next) setActiveValue(next.value);
    } else if (event.key === "End") {
      event.preventDefault();
      const next = allOptions[allOptions.length - 1];
      if (next) setActiveValue(next.value);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      event.stopPropagation();
      const next = allOptions[index];
      if (next) pick(next.value);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (event.key === "Tab") {
      setOpen(false);
    }
  };

  return (
    <div className={styles.dropdown} ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className={`${styles.dropdownTrigger} ${open ? styles.dropdownTriggerOpen : ""}`}
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
        onKeyDown={handleTriggerKeyDown}
      >
        <span className={styles.dropdownValue}>
          {current?.label ?? placeholder ?? "—"}
        </span>
        <span className={styles.dropdownArrow}>▾</span>
      </button>
      {open && (
        <div
          id={listId}
          className={`${styles.dropdownMenu} ${styles.dropdownMenuOpen}`}
          role="listbox"
          aria-label={ariaLabel}
        >
          {allOptions.map((o, index) => (
            <button
              key={o.value}
              ref={(el) => {
                if (el) optionRefs.current.set(o.value, el);
                else optionRefs.current.delete(o.value);
              }}
              type="button"
              role="option"
              aria-selected={o.value === value}
              data-focus-inset
              tabIndex={o.value === activeValue ? 0 : -1}
              className={`${styles.dropdownItem} ${
                o.value === value ? styles.dropdownItemActive : ""
              }`}
              onClick={(event) => {
                event.stopPropagation();
                pick(o.value);
              }}
              onKeyDown={(event) => handleOptionKeyDown(event, index)}
            >
              {o.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
