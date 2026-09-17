// 设置页 · "安全"子页（草案 v2 定稿形态）
//
// 页名即导航（顶部"安全"），页内单组设置、无节标题。两选项**各自带说明、
// 静态并存**（不随选中切换），说明为选项下缩进灰调小字。
//
// 受控组件：mode 来自 Panel settings.exec.mode；变更经 onModeChange 上抛
// （Panel 单漏斗：busy 守卫 + 即时保存）。

import type { ExecutionMode } from "../../types";
import choiceStyles from "./ui/choice.module.css";
import styles from "./SettingsSafety.module.css";

/** 各选项的自带说明（静态并存，不随选中切换） */
const OPTION_NOTES: Record<ExecutionMode, string> = {
  ask: "每个命令执行前弹确认卡，你确认后才执行",
  auto: "低危命令自动执行；高危与完整权限命令仍会询问",
};

export interface SettingsSafetyProps {
  mode: ExecutionMode;
  /** 变更上抛（Panel 漏斗：busy 守卫 + 即时保存；busy 时无回调 + toast） */
  onModeChange: (mode: ExecutionMode) => void;
}

export function SettingsSafety({ mode, onModeChange }: SettingsSafetyProps) {
  const selectMode = (next: ExecutionMode) => () => onModeChange(next);

  return (
    <div className={styles.container}>
      <label className={styles.option}>
        <span className={styles.optionHead}>
          <input
            type="radio"
            name="execMode"
            className={choiceStyles.choice}
            checked={mode === "ask"}
            onChange={selectMode("ask")}
          />
          <span className={styles.optionTitle}>操作前询问</span>
        </span>
        <span className={styles.optionNote}>{OPTION_NOTES.ask}</span>
      </label>
      <label className={styles.option}>
        <span className={styles.optionHead}>
          <input
            type="radio"
            name="execMode"
            className={choiceStyles.choice}
            checked={mode === "auto"}
            onChange={selectMode("auto")}
          />
          <span className={styles.optionTitle}>自动化</span>
        </span>
        <span className={styles.optionNote}>{OPTION_NOTES.auto}</span>
      </label>
    </div>
  );
}
