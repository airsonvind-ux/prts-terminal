// 设置页 · 扩展页容器（M3 §一）。
//
// 标题区（[管理] + 有设置页的插件）+ 内容区（管理页或某个插件的设置页）。
// 插件的设置页用显式表 `id → 组件` 挂载：不做动态导入、不做命名约定，
// 表里没有的内置插件显示「这个插件没有设置页」，而不是白屏。
//
// 所有写操作都返回最新的 PluginList，直接替换本地快照——不做乐观更新，
// 后端才是唯一事实（安装会落位、卸载会删目录，猜错了界面会撒谎）。

import { useCallback, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PluginList, PluginPreview } from "../../types";
import { MaaSettings, type MaaSettingsProps } from "./MaaSettings";
import { PluginTabBar, type PluginTab } from "./PluginTabBar";
import { SettingsPlugins } from "./SettingsPlugins";
import styles from "./SettingsPlugins.module.css";

export interface PluginSettingsHostProps {
  /** 内置插件设置页要用的设置与回调（Panel 的单漏斗，原样转交） */
  maaProps: MaaSettingsProps;
  /**
   * 有对话正在处理（M3 修补 §十一）。
   *
   * 收权（停用 / 卸载 / 重试）会撕掉别人正在用的东西——停用 MAA 会把正在跑的
   * 任务连桥一起收掉，调用永远等不到结果；给权（启用 / 安装）只是多一个当前轮
   * 用不到的选项，不设闸。闸在后端，这里只负责别让用户白点。
   */
  turnBusy: boolean;
}

/** 收权被闸住时给用户的解释：说清原因与动作。 */
export const TURN_BUSY_HINT =
  "对话处理中，停用会打断正在执行的任务；等这一轮结束再试";

/** 有设置页的插件：id → 组件。加一个插件就在这里加一行。 */
const SETTINGS_PAGES: Partial<
  Record<string, (props: PluginSettingsHostProps) => ReactNode>
> = {
  maa: ({ maaProps }) => <MaaSettings {...maaProps} />,
};

const EMPTY: PluginList = { plugins: [], unregistered: [] };

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function PluginSettingsHost({
  maaProps,
  turnBusy,
}: PluginSettingsHostProps) {
  const [list, setList] = useState<PluginList>(EMPTY);
  const [active, setActive] = useState("manage");
  const [preview, setPreview] = useState<PluginPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    invoke<PluginList>("list_plugins")
      .then((next) => {
        setList(next);
        setError(null);
      })
      .catch((e) => setError(messageOf(e)));
  }, []);

  useEffect(refresh, [refresh]);

  /** 所有写操作的公共壳：忙碌标记、错误落到页面上、成功后用后端快照覆盖。 */
  const run = useCallback(async (action: () => Promise<PluginList>) => {
    setBusy(true);
    setError(null);
    try {
      setList(await action());
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const previewPath = useCallback(async (path: string) => {
    setBusy(true);
    setError(null);
    try {
      setPreview(await invoke<PluginPreview>("preview_plugin_install", { path }));
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const handleInstallFolder = useCallback(async () => {
    try {
      const path = await invoke<string | null>("pick_plugin_folder");
      if (path) await previewPath(path);
    } catch (e) {
      setError(messageOf(e));
    }
  }, [previewPath]);

  const tabs = useMemo<PluginTab[]>(() => {
    const manage: PluginTab = {
      key: "manage",
      label: "管理",
      switchable: false,
    };
    const pages: PluginTab[] = list.plugins
      .filter((plugin) => plugin.hasSettingsPage)
      .map((plugin) => ({
        key: plugin.id,
        label: plugin.name,
        switchable: true,
        enabled: plugin.state !== "disabled",
        // 关掉 = 收权，处理中要闸住；打开 = 给权，随时可以。
        busy: busy || (turnBusy && plugin.state !== "disabled"),
        disabledReason: TURN_BUSY_HINT,
        onToggle: (next: boolean) =>
          void run(() =>
            invoke<PluginList>("set_plugin_enabled", {
              id: plugin.id,
              enabled: next,
            }),
          ),
      }));
    return [manage, ...pages];
  }, [list, busy, run, turnBusy]);

  // 插件被卸载（或没有设置页）时，当前页回退到管理页——不留一个空壳。
  const activePage = SETTINGS_PAGES[active];

  return (
    <div className={styles.container}>
      <PluginTabBar
        tabs={tabs}
        active={activePage ? active : "manage"}
        onSelect={setActive}
      />

      {activePage ? (
        activePage({ maaProps, turnBusy })
      ) : (
        <SettingsPlugins
          list={list}
          busy={busy}
          locked={turnBusy}
          lockedHint={TURN_BUSY_HINT}
          preview={preview}
          installing={busy && preview !== null}
          error={error}
          onInstallFolder={() => void handleInstallFolder()}
          onDropFolder={(path) => void previewPath(path)}
          onConfirmInstall={() => {
            const token = preview?.token;
            if (!token) return;
            void run(async () => {
              const next = await invoke<PluginList>("confirm_plugin_install", {
                token,
              });
              setPreview(null);
              return next;
            });
          }}
          onCancelInstall={() => setPreview(null)}
          onSetEnabled={(id, enabled) =>
            void run(() =>
              invoke<PluginList>("set_plugin_enabled", { id, enabled }),
            )
          }
          onUninstall={(id, deleteData) =>
            void run(() =>
              invoke<PluginList>("uninstall_plugin", { id, deleteData }),
            )
          }
          onRetry={(id) =>
            void run(() => invoke<PluginList>("retry_plugin", { id }))
          }
          onOpenSettings={setActive}
        />
      )}
    </div>
  );
}
