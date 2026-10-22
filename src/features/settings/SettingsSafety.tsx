// 设置页 · "安全"子页（命令执行模式）
//
// 页名即导航（顶部"安全"）。两选项各自带说明、静态并存；自动化模式下
// 补充一条用户可理解的提示，不新增确认。
//
// 受控组件：mode 来自 Panel settings；变更经上抛
// （Panel 单漏斗：busy 守卫 + 即时保存）。

import type { ExecutionMode } from "../../types";
import { SectionWarning } from "./ui/SettingsSection";
import { SettingsRow } from "./ui/SettingsRow";
import choiceStyles from "./ui/choice.module.css";
import styles from "./SettingsSafety.module.css";

/** 各选项的自带说明（静态并存，不随选中切换） */
const OPTION_NOTES: Record<ExecutionMode, string> = {
  ask: "每个命令执行前弹确认卡，你确认后才执行",
  auto: "低风险命令自动执行，可能运行程序或连接网络；高风险与完整权限命令仍会询问",
};

export interface SettingsSafetyProps {
  mode: ExecutionMode;
  /** 对话处理中：执行模式暂不可改 */
  busy?: boolean;
  /** 变更上抛（Panel 漏斗：busy 守卫 + 即时保存；busy 时无回调 + toast） */
  onModeChange: (mode: ExecutionMode) => void;
}

export function SettingsSafety({
  mode,
  busy = false,
  onModeChange,
}: SettingsSafetyProps) {
  const option = (value: ExecutionMode, title: string) => (
    <SettingsRow
      controlInline
      label={title}
      hint={OPTION_NOTES[value]}
      control={
        <input
          type="radio"
          name="execMode"
          className={choiceStyles.choice}
          disabled={busy}
          checked={mode === value}
          onChange={() => onModeChange(value)}
        />
      }
    />
  );

  return (
    <div className={styles.container}>
      {busy && <SectionWarning>对话处理中，执行模式暂不可改</SectionWarning>}
      {option("ask", "操作前询问")}
      {option("auto", "自动化")}
    </div>
  );
}
