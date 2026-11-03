// 扩展页的标题区：[管理] 永远第一，随后是有设置页的插件。
//
// 装配约束（M3 §一）：
// - 点标题只切页；点开关只切开关。两者命中区不重叠，开关不在标题按钮内部。
// - 开关不做宽度动画：状态变化时行内其它内容不跟着抖动。

import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import styles from "./SettingsPlugins.module.css";

export interface PluginTab {
  key: string;
  label: string;
  /** 管理页没有开关 */
  switchable: boolean;
  enabled?: boolean;
  busy?: boolean;
  /** 收权被闸住时（对话处理中）悬停说明 */
  disabledReason?: string;
  onToggle?: (next: boolean) => void;
}

interface PluginTabBarProps {
  tabs: PluginTab[];
  active: string;
  onSelect: (key: string) => void;
}

export function PluginTabBar({ tabs, active, onSelect }: PluginTabBarProps) {
  // 复选框原生只用 Space 切换；额外接受 Enter，和设置页其它开关一致。
  const toggleOnEnter = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      e.preventDefault();
      e.currentTarget.click();
    }
  };

  return (
    <div className={styles.tabBar} role="tablist" aria-label="插件">
      {tabs.map((tab) => {
        const isActive = tab.key === active;
        return (
          <div
            key={tab.key}
            className={`${styles.tab}${isActive ? ` ${styles.tabActive}` : ""}`}
          >
            <button
              type="button"
              role="tab"
              aria-selected={isActive}
              className={styles.tabButton}
              onClick={() => onSelect(tab.key)}
            >
              {tab.label}
            </button>
            {tab.switchable && (
              <span
                className={styles.tabSwitch}
                title={tab.busy ? tab.disabledReason : undefined}
              >
                <input
                  type="checkbox"
                  aria-label={`启用 ${tab.label}`}
                  checked={tab.enabled ?? false}
                  disabled={tab.busy}
                  onChange={(e) => tab.onToggle?.(e.target.checked)}
                  onKeyDown={toggleOnEnter}
                  onClick={(e) => e.stopPropagation()}
                />
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}
