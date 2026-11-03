// 设置页 · "应用"子页（UI 优化第二版：系统读数块 + 两个行式设置）
//
// 版式规则（全站统一）：一行里「标签 + 说明在左，控件在右」。
// 开关、按钮都靠右边缘对齐，标签永远从左边起——不再出现
// 「节标题在左、孤立按钮居中/靠右」的悬空布局（用户 m05402 的异议）。
//
// 系统节：版本 / 数据版本是两个读数块，数据目录单独一行等宽路径
// （过长省略，悬停看全路径）。数据来自 AppMeta 启动快照，不额外查库。
//
// 窗口节：置顶显示开关（读 settings.window.always_on_top；切换经
// onSetAlwaysOnTop 上抛 → Panel commit → set_always_on_top 专用命令：
// 先应用窗口、成功落盘、失败回滚）。
//
// 退出节：一行式危险动作（quit_app；后端统一 abort_all 并等本轮收尾）。

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppMeta } from "../../types";
import { SettingsSection } from "./ui/SettingsSection";
import { Button } from "./ui/Button";
import { Stat } from "./ui/Stat";
import { Switch } from "./ui/Switch";
import styles from "./SettingsApp.module.css";

export interface SettingsAppProps {
  /** 启动快照（版本、数据版本、数据目录）；未就绪时为 null */
  meta: AppMeta | null;
  /** 当前置顶态（settings.window.always_on_top） */
  alwaysOnTop: boolean;
  /** 切换上抛（Panel commit：busy 守卫 + set_always_on_top + 回滚） */
  onSetAlwaysOnTop: (enabled: boolean) => void;
}

export function SettingsApp({
  meta,
  alwaysOnTop,
  onSetAlwaysOnTop,
}: SettingsAppProps) {
  const [quitting, setQuitting] = useState(false);

  const handleQuit = async () => {
    if (quitting) return;
    setQuitting(true);
    // 由后端统一 abort_all（原因 app-shutdown）并等待本轮收尾；
    // 正常关闭/窗口关闭都走同一条路径。
    await invoke("quit_app").catch(() => setQuitting(false));
  };

  const dataDir = meta?.dataDir ?? "";
  const dataVersion = meta?.dbMeta?.dataVersion;

  return (
    <div className={styles.container}>
      <SettingsSection title="系统">
        <div className={styles.statRow}>
          <Stat label="版本" value={meta?.version ?? "—"} />
          <Stat
            label="数据版本"
            value={dataVersion || "—"}
            accent
            title={dataVersion ? `数据版本 ${dataVersion}` : "尚未读取到数据库版本"}
          />
        </div>
        <div className={styles.pathRow} title={dataDir || undefined}>
          <span className={styles.pathLabel}>数据目录</span>
          <span className={styles.pathValue}>{dataDir || "—"}</span>
        </div>
      </SettingsSection>

      <SettingsSection title="窗口">
        <div className={styles.row}>
          <div className={styles.rowText}>
            <span className={styles.rowLabel}>置顶显示</span>
            <span className={styles.rowHint}>开启后窗口始终浮于其他窗口之上</span>
          </div>
          <Switch
            checked={alwaysOnTop}
            onChange={onSetAlwaysOnTop}
            label="置顶显示"
          />
        </div>
      </SettingsSection>

      <SettingsSection title="退出">
        <div className={styles.row}>
          {/* 这里不放行标题：「退出」已经是节标题，再写一遍就成了同词重复。
              动作名由右侧按钮承担，左侧只留一句后果说明。 */}
          <span className={styles.rowHint}>中止进行中的对话并退出应用</span>
          <Button
            variant="danger"
            onClick={() => void handleQuit()}
            disabled={quitting}
          >
            {quitting ? "正在退出…" : "关闭终端"}
          </Button>
        </div>
      </SettingsSection>
    </div>
  );
}
