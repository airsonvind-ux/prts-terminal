// 设置页 · 扩展列表（UI 规格草案 2 §三）。
//
// 列表本身就是拖拽落点：拖文件夹到列表区任意位置都能安装，不再要求对准一个
// 虚线方块。命中判定仍走 pluginDropArbiter 的不变量——enter/over 写命中，
// leave 写 false，drop 后微任务复位（ChatView 的守卫读同一个值决定让不让路）。

import { useEffect, useRef } from "react";
import type { PluginList, PluginRuntimeState } from "../../types";
import { ListRow } from "./ui/ListRow";
import type { ListRowDot } from "./ui/ListRow";
import { Button } from "./ui/Button";
import { finishPluginDrop, notePluginDragOver } from "./pluginDropArbiter";
import styles from "./SettingsPlugins.module.css";

const STATE_LABELS: Record<PluginRuntimeState, string> = {
  loaded: "正常",
  starting: "启动中",
  failed: "加载失败",
  faulted: "进程退出",
  disabled: "已停用",
};

const STATE_DOT: Record<PluginRuntimeState, ListRowDot> = {
  loaded: "ok",
  starting: "ok",
  failed: "bad",
  faulted: "bad",
  disabled: "idle",
};

interface SettingsPluginsProps {
  list: PluginList;
  loading: boolean;
  busy: boolean;
  /** 有对话在处理：收权（停用）置灰 */
  locked: boolean;
  lockedHint: string;
  onOpen: (id: string) => void;
  onRetry: () => void;
  onSetEnabled: (id: string, enabled: boolean) => void;
  onDropFolder: (path: string) => void;
}

export function SettingsPlugins({
  list,
  loading,
  busy,
  locked,
  lockedHint,
  onOpen,
  onRetry,
  onSetEnabled,
  onDropFolder,
}: SettingsPluginsProps) {
  const zoneRef = useRef<HTMLDivElement>(null);

  // 拖放：Tauri 的 webview 级事件没有 DOM 命中测试，只能按坐标判断。
  // 整个列表区是落点；每次 enter/over 都把命中结果记给仲裁器，drop 时以它为准。
  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;

    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const webview = getCurrentWebview();
        const unlisten = await webview.onDragDropEvent((event) => {
          if (event.payload.type === "leave") {
            notePluginDragOver(false);
            return;
          }
          if (event.payload.type === "drop") {
            const inside = hitsList(zoneRef.current, event.payload.position);
            // 与 ChatView 的监听器共用一次投放：谁先跑都读到同一个值，
            // 复位放到微任务里。
            queueMicrotask(finishPluginDrop);
            if (inside) {
              notePluginDragOver(true);
              const path = event.payload.paths[0];
              if (path) onDropFolder(path);
            }
            return;
          }
          notePluginDragOver(
            hitsList(zoneRef.current, event.payload.position),
          );
        });
        if (cancelled) unlisten();
        else off = unlisten;
      } catch {
        /* 浏览器环境（测试/预览）没有 webview 拖放，忽略 */
      }
    })();

    return () => {
      cancelled = true;
      off?.();
    };
  }, [onDropFolder]);

  const rows = list.plugins;

  return (
    <div ref={zoneRef} className={styles.listZone}>
      {loading && rows.length === 0 ? (
        <div className={styles.skeleton} aria-hidden="true">
          <span />
          <span />
          <span />
        </div>
      ) : rows.length === 0 && list.unregistered.length === 0 ? (
        <div className={styles.empty}>
          <p className={styles.emptyText}>没有加载到任何插件</p>
          <Button onClick={onRetry} disabled={busy}>
            重试
          </Button>
        </div>
      ) : (
        <div className={styles.rowList} role="list">
          {rows.map((plugin) => {
            const abnormal =
              plugin.state === "failed" || plugin.state === "faulted";
            return (
              <ListRow
                key={plugin.id}
                title={plugin.name}
                titleAttr={plugin.name}
                dot={STATE_DOT[plugin.state]}
                subtitle={
                  abnormal && plugin.reason
                    ? plugin.reason
                    : `${plugin.version} · ${
                        plugin.kind === "process" ? "进程" : "内置"
                      } · ${STATE_LABELS[plugin.state]}`
                }
                tone={abnormal && plugin.reason ? "danger" : "default"}
                disabled={plugin.state === "starting"}
                onActivate={() => onOpen(plugin.id)}
                trailing={
                  <input
                    type="checkbox"
                    aria-label={`启用 ${plugin.name}`}
                    checked={plugin.state !== "disabled"}
                    // 关掉 = 收权，处理中要闸住；打开 = 给权，随时可以。
                    disabled={
                      busy || plugin.state === "starting" ||
                      (locked && plugin.state !== "disabled")
                    }
                    title={
                      locked && plugin.state !== "disabled"
                        ? lockedHint
                        : undefined
                    }
                    onChange={(e) =>
                      onSetEnabled(plugin.id, e.target.checked)
                    }
                  />
                }
              />
            );
          })}
        </div>
      )}

      {list.unregistered.length > 0 && (
        <div className={styles.rowList}>
          <p className={styles.sectionHint}>读不出 plugin.json 的目录</p>
          {list.unregistered.map((dir) => (
            <ListRow
              key={dir.dirName}
              title={dir.dirName}
              subtitle={dir.reason}
              titleAttr={dir.reason}
              muted
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** 指针是否落在列表区内（Tauri 给的是物理像素，需要除以 DPR）。 */
function hitsList(
  element: HTMLElement | null,
  position: { x: number; y: number } | undefined,
): boolean {
  if (!element || !position) return false;
  const rect = element.getBoundingClientRect();
  const ratio = window.devicePixelRatio || 1;
  const x = position.x / ratio;
  const y = position.y / ratio;
  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}
