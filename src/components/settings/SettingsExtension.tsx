// 设置页 · "扩展"子页（MAA 接入）
//
// 三节：账号 → 连接 → 增强。
//
// - 状态与动作同一行：左边状态、右边「测试连接」。用户打开这页只需要看这一行。
// - 连接节的四项（模拟器 / 安装路径 / 连接地址 / ADB 路径）**常驻显示**：
//   自动检测开着时整体置灰（值由我们识别），关掉后原地解锁——任何一行都不出现/消失。
// - 状态行数据来自**只读**命令 maa_status（面板打开取一次、每次操作后刷新）；
//   「测试连接」调 maa_test_connection（那才会真去连一次）。
// - 写设置一律走 Panel 单漏斗（onPatch → set_settings，含 busy 守卫与失败回滚）；
//   两个路径用系统选择器（目录 / 文件），回调同样由 Panel 提供（与工作空间同构）。
// - MAA 任务运行中：整页置灰锁定（后端同样拒绝，双保险）。
//
// 文案口径：只说我们做了什么，不替 MAA 打包票——截图增强写"用模拟器自带截图"，
// 不写"已生效"，因为 MAA 在增强不可用时会静默降级。

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  MaaPatch,
  MaaResolved,
  MaaServer,
  MaaSettings,
  MaaStatus,
} from "../../types";
import { SettingsRow } from "./ui/SettingsRow";
import { Select } from "./ui/Select";
import { SectionHint, SectionWarning, SettingsSection } from "./ui/SettingsSection";
import choiceStyles from "./ui/choice.module.css";
import styles from "./SettingsExtension.module.css";

export interface SettingsExtensionProps {
  maa: MaaSettings;
  /** 对话处理中（禁用全部控件；漏斗另有守卫） */
  busy: boolean;
  /** 「测试连接」进行中（状态由 Panel 持有——子页切走会卸载，留在子页会丢） */
  testing: boolean;
  /** 触发一次连接测试；成功回最新状态（失败已由 Panel 弹提示，回 null） */
  onTest: () => Promise<MaaStatus | null>;
  /** 变更上抛（Panel 单漏斗：busy 守卫 + 即时保存 + 失败回滚） */
  onPatch: (patch: MaaPatch, label?: string) => void;
  /** 「选择」模拟器安装目录（Panel：busy 守卫 → 系统目录选择器 → 选中即存） */
  onChooseEmulatorDir: () => void;
  /** 「选择」adb 可执行文件（同上，文件选择器） */
  onChooseAdbFile: () => void;
}

const SERVER_LABELS: Record<MaaServer, string> = {
  official: "官服",
  bilibili: "B 服",
};

/** 状态行文案：这页的关注中心——连上没有、连的是谁、各功能的生效状态、上次为什么没连上。 */
function statusText(s: MaaStatus): string {
  if (s.connected) {
    let extras: string;
    if (!s.extras) {
      extras = "截图增强未启用";
    } else if (s.screencap?.endsWith("Extras")) {
      extras = `截图增强已生效（${s.screencap}）`;
    } else if (s.screencap) {
      extras = `截图增强已启用，当前走普通通道（${s.screencap}）`;
    } else {
      extras = "截图增强已启用";
    }
    // 地址不进状态行：用户不设它、也不关心；要看在「连接地址」那一行。
    return [s.emulator ? `已连接：${s.emulator}` : "已连接", extras].join(" · ");
  }
  if (s.stale) return "设置已更改，下次连接生效";
  if (!s.maa_installed) return "尚未连接 · 本机没找到 MAA";
  if (s.last_error) return `尚未连接 · ${s.last_error}`;
  return "尚未连接";
}

/** 挑出当前该展示"实际在用"的那个模拟器推导结果。 */
function pickResolved(status: MaaStatus | null, emulator: string): MaaResolved | null {
  if (!status || status.resolved.length === 0) return null;
  const want = emulator || status.emulator || "";
  return status.resolved.find((r) => r.emulator === want) ?? status.resolved[0];
}

export function SettingsExtension({
  maa,
  busy,
  testing,
  onTest,
  onPatch,
  onChooseEmulatorDir,
  onChooseAdbFile,
}: SettingsExtensionProps) {
  const [status, setStatus] = useState<MaaStatus | null>(null);
  // 文本框草稿：失焦时提交（与现有页的文本字段约定一致）
  const [draft, setDraft] = useState({
    adb_path: maa.adb_path,
    address: maa.address,
    emulator_path: maa.emulator_path,
  });

  const refresh = useCallback(() => {
    invoke<MaaStatus>("maa_status")
      .then(setStatus)
      .catch((e) => console.error("[maa] maa_status 失败", e));
  }, []);

  useEffect(refresh, [refresh]);
  // 外部值变了（保存成功 / 失败回滚）→ 草稿跟着回到真实值
  useEffect(() => {
    setDraft({
      adb_path: maa.adb_path,
      address: maa.address,
      emulator_path: maa.emulator_path,
    });
  }, [maa.adb_path, maa.address, maa.emulator_path]);

  const runTest = async () => {
    const view = await onTest();
    if (view) setStatus(view);
    else refresh();
  };

  const running = status?.running ?? false;
  /** 任务运行中 / 对话处理中 / **连接测试进行中**：整页只读。
   *  测试期间改设置会把桥拆掉（设置一变就作废旧连接），连接中断、
   *  写入还会堵在被占住的管理器锁上，界面跟着卡住。 */
  const locked = running || busy || testing;
  /** 自动检测开着：连接节那四项由我们识别，置灰 */
  const auto = maa.auto_detect;
  const selected = pickResolved(status, maa.emulator);
  /** 自动检测中显示判定值（不可改）；关掉后显示用户自己的值 */
  const shown = (autoValue: string | undefined, own: string) =>
    auto ? (autoValue ?? "") : own;

  return (
    <div className={styles.container}>
      {/* ① 账号：一次性的账号事实 */}
      <SettingsSection title="账号">
        <div className={styles.serverRow}>
          {(Object.keys(SERVER_LABELS) as MaaServer[]).map((value) => (
            <label key={value} className={styles.serverOption}>
              <input
                type="radio"
                name="maaServer"
                className={choiceStyles.choice}
                disabled={locked}
                checked={maa.server === value}
                onChange={() => onPatch({ server: value }, "服务器")}
              />
              <span className={styles.serverTitle}>{SERVER_LABELS[value]}</span>
            </label>
          ))}
        </div>
      </SettingsSection>

      {/* ② 连接：状态与动作同一行；下面四项常驻，自动检测开着时置灰 */}
      <SettingsSection title="连接">
        <div className={styles.statusRow}>
          <div className={styles.statusText}>
            {testing
              ? "正在连接…"
              : status
                ? statusText(status)
                : "正在读取状态…"}
          </div>
          <button className={styles.testButton} disabled={testing} onClick={runTest}>
            {testing ? "连接中…" : "测试连接"}
          </button>
        </div>
        {running && <SectionWarning>MAA 正在运行，暂不可改</SectionWarning>}

        <SettingsRow
          controlInline
          label="自动检测"
          hint="自动识别；关掉后可自己指定"
          control={
            <input
              type="checkbox"
              className={choiceStyles.choice}
              disabled={locked}
              checked={auto}
              onChange={(e) => onPatch({ auto_detect: e.target.checked }, "自动检测")}
            />
          }
        />

        <SettingsRow
          label="模拟器"
          hint={auto && selected ? `当前：${selected.emulator}` : undefined}
          control={
            <Select
              ariaLabel="模拟器"
              value={maa.emulator}
              disabled={locked || auto}
              onChange={(v) => onPatch({ emulator: v }, "模拟器")}
              options={[
                { value: "", label: "自动" },
                ...(status?.emulators ?? []).map((label) => ({ value: label, label })),
              ]}
            />
          }
        />

        <SettingsRow
          label="安装路径"
          control={
            <div className={styles.pathRow}>
              <input
                className={styles.input}
                disabled={locked || auto}
                value={shown(selected?.root, draft.emulator_path)}
                placeholder={auto ? "" : "…\\MuMuPlayer-12.0"}
                onChange={(e) => setDraft({ ...draft, emulator_path: e.target.value })}
                onBlur={() => {
                  if (draft.emulator_path !== maa.emulator_path) {
                    onPatch({ emulator_path: draft.emulator_path }, "安装路径");
                  }
                }}
              />
              <button
                className={styles.actionButton}
                disabled={locked || auto}
                onClick={onChooseEmulatorDir}
              >
                选择
              </button>
            </div>
          }
        />

        <SettingsRow
          label="连接地址"
          hint="自动检测失败时填"
          control={
            <input
              className={styles.input}
              disabled={locked || auto}
              value={shown(selected?.address, draft.address)}
              placeholder={auto ? "" : "127.0.0.1:16384"}
              onChange={(e) => setDraft({ ...draft, address: e.target.value })}
              onBlur={() => {
                if (draft.address !== maa.address) {
                  onPatch({ address: draft.address }, "连接地址");
                }
              }}
            />
          }
        />

        <SettingsRow
          label="ADB 路径"
          hint="默认用模拟器自带的"
          control={
            <div className={styles.pathRow}>
              <input
                className={styles.input}
                disabled={locked || auto}
                value={shown(selected?.adb_path, draft.adb_path)}
                placeholder={auto ? "" : "…\\adb.exe"}
                onChange={(e) => setDraft({ ...draft, adb_path: e.target.value })}
                onBlur={() => {
                  if (draft.adb_path !== maa.adb_path) {
                    onPatch({ adb_path: draft.adb_path }, "ADB 路径");
                  }
                }}
              />
              <button
                className={styles.actionButton}
                disabled={locked || auto}
                onClick={onChooseAdbFile}
              >
                选择
              </button>
            </div>
          }
        />
      </SettingsSection>

      {/* ③ 增强：只对支持的模拟器出现 */}
      <SettingsSection title="增强">
        {selected && !selected.extras ? (
          <SectionHint>这台模拟器不支持增强</SectionHint>
        ) : (
          <>
            <SettingsRow
              controlInline
              label="截图增强"
              hint="用模拟器自带截图，速度更快"
              control={
                <input
                  type="checkbox"
                  className={choiceStyles.choice}
                  disabled={locked}
                  checked={maa.screenshot_extras}
                  onChange={(e) =>
                    onPatch({ screenshot_extras: e.target.checked }, "截图增强")
                  }
                />
              }
            />
            <SettingsRow
              controlInline
              label="触控增强"
              hint="支持模拟器开启「应用保活」时使用"
              control={
                <input
                  type="checkbox"
                  className={choiceStyles.choice}
                  disabled={locked}
                  checked={maa.touch_extras}
                  onChange={(e) =>
                    onPatch({ touch_extras: e.target.checked }, "触控增强")
                  }
                />
              }
            />
          </>
        )}
      </SettingsSection>
    </div>
  );
}
