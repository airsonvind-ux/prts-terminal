// 设置页 · "应用"子页（草案 v2 定稿形态）
//
// 窗口节：置顶显示开关（读 settings.window.always_on_top；切换经
// onSetAlwaysOnTop 上抛 → Panel commit → set_always_on_top 专用命令：
// 先应用窗口、成功落盘、失败回滚）。窗口其余行为未来在本节扩展。
//
// 退出节：关闭终端（quit_app，危险按钮样式，原通用页迁入）。
// 节间大间距分隔由 SettingsSection 规范样式承载。

import { invoke } from "@tauri-apps/api/core";
import { SettingsSection } from "./ui/SettingsSection";
import choiceStyles from "./ui/choice.module.css";
import styles from "./SettingsApp.module.css";

export interface SettingsAppProps {
  /** 当前置顶态（settings.window.always_on_top） */
  alwaysOnTop: boolean;
  /** 切换上抛（Panel commit：busy 守卫 + set_always_on_top + 回滚） */
  onSetAlwaysOnTop: (enabled: boolean) => void;
}

export function SettingsApp({ alwaysOnTop, onSetAlwaysOnTop }: SettingsAppProps) {
  const handleQuit = () => {
    invoke("quit_app");
  };

  return (
    <div className={styles.container}>
      <SettingsSection title="窗口">
        <label className={styles.topRow}>
          <input
            type="checkbox"
            className={choiceStyles.choice}
            checked={alwaysOnTop}
            onChange={(e) => onSetAlwaysOnTop(e.target.checked)}
          />
          <span className={styles.topTitle}>置顶显示</span>
        </label>
        <p className={styles.topHint}>开启后窗口始终浮于其他窗口之上</p>
      </SettingsSection>

      <SettingsSection title="退出">
        <div className={styles.quitWrap}>
          <button className={styles.quitButton} onClick={handleQuit}>
            关闭终端
          </button>
        </div>
      </SettingsSection>
    </div>
  );
}
