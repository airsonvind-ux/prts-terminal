// 设置页通用组件 · Button（三个变体，全站按钮唯一来源）
//
// UI 规格草案 2 §二：
//   plain   —— 无描边，悬停才出现底色（行尾动作、次要操作）
//   primary —— accent 描边，只用于「安装」「保存」这类主动作
//   danger  —— 红字，只用于卸载确认
//   icon    —— 22×22 方形，只放图标（复制、刷新这类行内小动作）
//
// 尺寸与字号跟设置页正文一致（11px / 高 22px），不随页面变。

import type { ButtonHTMLAttributes, ReactNode } from "react";
import styles from "./settings-ui.module.css";

export type ButtonVariant = "plain" | "primary" | "danger" | "icon";

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  children: ReactNode;
}

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  plain: styles.buttonPlain,
  primary: styles.buttonPrimary,
  danger: styles.buttonDanger,
  icon: styles.buttonIcon,
};

export function Button({
  variant = "plain",
  type = "button",
  className,
  children,
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={[styles.button, VARIANT_CLASS[variant], className]
        .filter(Boolean)
        .join(" ")}
      {...rest}
    >
      {children}
    </button>
  );
}
