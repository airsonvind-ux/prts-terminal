// 设置页 · 扩展页容器（UI 规格草案 2 §三）。
//
// 三个视图，全部在面板内切换（不嵌套 modal——设置面板本身已是 aria-modal）：
//   list    扩展列表（整区是拖拽落点）
//   detail  某个插件的详情与设置页
//   install 安装（二级视图，复用「页头 + 返回」）
//
// 所有写操作都返回最新的 PluginList，直接替换本地快照——不做乐观更新，
// 后端才是唯一事实（安装会落位、卸载会删目录，猜错了界面会撒谎）。

import { useCallback, useEffect, useState } from "react";
import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PluginList, PluginPreview } from "../../types";
import { Button } from "./ui/Button";
import { Switch } from "./ui/Switch";
import { MaaSettings } from "./MaaSettings";
import type { MaaSettingsProps } from "./MaaSettings";
import { PluginDetail } from "./PluginDetail";
import { PluginInstall } from "./PluginInstall";
import { SettingsPlugins } from "./SettingsPlugins";
import { showToast } from "../../stores/useToastStore";
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

// 页头图标：12px 线性图标（stroke 1.5，随字色）。不用「＋ / ⟳」字符——
// 字形随字体走，尺寸与基线对不齐，22px 的方框里看着像随手敲的。
function PlusIcon() {
  return (
    <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true" focusable="false">
      <path
        d="M8 3.25v9.5M3.25 8h9.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}

function RefreshIcon() {
  return (
    <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true" focusable="false">
      <path
        d="M13 8a5 5 0 1 1-1.46-3.54"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
      <path
        d="M12.9 2.6v2.9H10"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** 有设置页的插件：id → 组件。加一个插件就在这里加一行。 */
const SETTINGS_PAGES: Partial<
  Record<string, (props: PluginSettingsHostProps) => ReactNode>
> = {
  maa: ({ maaProps }) => <MaaSettings {...maaProps} />,
};

const EMPTY: PluginList = { plugins: [], unregistered: [] };

type View =
  | { kind: "list" }
  | { kind: "detail"; id: string }
  | { kind: "install" };

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function PluginSettingsHost({
  maaProps,
  turnBusy,
}: PluginSettingsHostProps) {
  const [list, setList] = useState<PluginList>(EMPTY);
  const [loading, setLoading] = useState(true);
  const [view, setView] = useState<View>({ kind: "list" });
  const [preview, setPreview] = useState<PluginPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    setLoading(true);
    invoke<PluginList>("list_plugins")
      .then((next) => {
        setList(next);
        setError(null);
      })
      .catch((e) => setError(messageOf(e)))
      .finally(() => setLoading(false));
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
      setView({ kind: "install" });
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const pickFolder = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const path = await invoke<string | null>("pick_plugin_folder");
      if (path) await previewPath(path);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }, [previewPath]);

  const backToList = useCallback(() => {
    setView({ kind: "list" });
    setPreview(null);
  }, []);

  /** 启停：只开关插件，不切页。启用后当前对话看不到新工具，要当面说清。 */
  const togglePlugin = useCallback(
    (id: string, enabled: boolean) => {
      const name = list.plugins.find((p) => p.id === id)?.name ?? id;
      void run(async () => {
        const next = await invoke<PluginList>("set_plugin_enabled", {
          id,
          enabled,
        });
        if (enabled) {
          showToast(`已启用 ${name}。它的工具和技能要在新建对话里才会出现`, {
            kind: "info",
            dedupeKey: "plugin-enabled",
          });
        }
        return next;
      });
    },
    [list.plugins, run]
  );

  // Esc：先回列表，面板还开着。必须 preventDefault——OverlayContainer 只在
  // 事件未被处理时才关闭面板（见它的 Escape 分支）。
  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Escape" || event.nativeEvent.isComposing) return;
    if (view.kind === "list") return;
    event.preventDefault();
    backToList();
  };

  const active =
    view.kind === "detail"
      ? list.plugins.find((plugin) => plugin.id === view.id) ?? null
      : null;

  // 详情页的插件被卸载（或列表刷新后消失）→ 回列表，不留空壳。
  useEffect(() => {
    if (view.kind === "detail" && !active) setView({ kind: "list" });
  }, [view, active]);

  const locked = turnBusy;

  return (
    <div className={styles.host} onKeyDown={onKeyDown}>
      <header className={styles.pageHeader}>
        {view.kind === "list" ? (
          <>
            <span className={styles.pageTitle}>扩展</span>
            <span className={styles.pageSpacer} />
            <Button
              variant="primary"
              aria-label="安装插件"
              title="安装插件"
              disabled={busy}
              onClick={() => {
                setPreview(null);
                setError(null);
                setView({ kind: "install" });
              }}
            >
              <PlusIcon />
              安装
            </Button>
            <Button
              variant="icon"
              aria-label="刷新"
              title="刷新"
              disabled={busy}
              onClick={refresh}
            >
              <RefreshIcon />
            </Button>
          </>
        ) : (
          <>
            <Button
              className={styles.backButton}
              onClick={backToList}
              aria-label="返回列表"
            >
              ‹ 返回
            </Button>
            <span className={styles.pageTitle} title={active?.name}>
              {view.kind === "detail" ? (active?.name ?? "") : "安装插件"}
            </span>
            {view.kind === "detail" && active && (
              <>
                <span className={styles.pageSpacer} />
                <Switch
                  label={`启用 ${active.name}`}
                  checked={active.state !== "disabled"}
                  // 关掉 = 收权，处理中要闸住；打开 = 给权，随时可以。
                  disabled={busy || (locked && active.state !== "disabled")}
                  title={
                    locked && active.state !== "disabled"
                      ? TURN_BUSY_HINT
                      : undefined
                  }
                  onChange={(enabled) => togglePlugin(active.id, enabled)}
                />
              </>
            )}
          </>
        )}
      </header>

      <div className={styles.pageBody}>
        {error && (
          <div className={styles.error} role="alert">
            {error}
          </div>
        )}

        {view.kind === "list" && (
          <SettingsPlugins
            list={list}
            loading={loading}
            busy={busy}
            locked={locked}
            lockedHint={TURN_BUSY_HINT}
            onOpen={(id) => setView({ kind: "detail", id })}
            onRetry={refresh}
            onSetEnabled={togglePlugin}
            onDropFolder={(path) => void previewPath(path)}
          />
        )}

        {view.kind === "detail" && active && (
          <PluginDetail
            plugin={active}
            busy={busy}
            locked={locked}
            lockedHint={TURN_BUSY_HINT}
            settingsPage={SETTINGS_PAGES[active.id]?.({
              maaProps,
              turnBusy,
            })}
            onRetry={() =>
              void run(() => invoke<PluginList>("retry_plugin", { id: active.id }))
            }
            onUninstall={(deleteData) =>
              void run(async () => {
                const next = await invoke<PluginList>("uninstall_plugin", {
                  id: active.id,
                  deleteData,
                });
                setView({ kind: "list" });
                return next;
              })
            }
          />
        )}

        {view.kind === "install" && (
          <PluginInstall
            preview={preview}
            busy={busy}
            onPick={() => void pickFolder()}
            onCancel={() => setPreview(null)}
            onConfirm={() => {
              const token = preview?.token;
              const name = preview?.name;
              if (!token) return;
              void run(async () => {
                const next = await invoke<PluginList>("confirm_plugin_install", {
                  token,
                });
                setPreview(null);
                setView({ kind: "list" });
                // 装完当前对话看不到它的工具：先说清楚，省得用户以为没装上。
                showToast(
                  `已安装 ${name ?? "插件"}。它的工具和技能要在新建对话里才会出现`,
                  { kind: "info", dedupeKey: "plugin-installed" }
                );
                return next;
              });
            }}
          />
        )}
      </div>
    </div>
  );
}
