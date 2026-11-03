// 设置页 · "安全"子页（命令执行模式）
//
// 两档模式改成分段卡片：整块可点，选中 = accent 边框 + 12% 底色 + 左侧 2px 条。
// 卡片内部仍是原生 radio（name="execMode"），键盘与读屏行为不变。
//
// 受控组件：mode 来自 Panel settings；变更经上抛
// （Panel 单漏斗：busy 守卫 + 即时保存）。

import type { ExecutionMode } from "../../types";
import { SectionWarning } from "./ui/SettingsSection";
import { ChoiceCard } from "./ui/ChoiceCard";
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
  return (
    <div className={styles.container}>
      {busy && <SectionWarning>对话处理中，执行模式暂不可改</SectionWarning>}
      <div className={styles.cards} role="radiogroup" aria-label="命令执行模式">
        <ChoiceCard
          name="execMode"
          checked={mode === "ask"}
          title="操作前询问"
          description={OPTION_NOTES.ask}
          disabled={busy}
          onSelect={() => onModeChange("ask")}
        />
        <ChoiceCard
          name="execMode"
          checked={mode === "auto"}
          title="自动化"
          description={OPTION_NOTES.auto}
          disabled={busy}
          onSelect={() => onModeChange("auto")}
        />
      </div>
    </div>
  );
}
