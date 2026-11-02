// 设置页 · 插件管理页（M3 §二）。
//
// - 安装：按钮选目录，或把目录拖进虚线区（zip 请先解压——M3 只支持文件夹）。
// - 预览卡片：确认前不给落位；`builtin` 的包在预览阶段就被后端拒掉。
// - 已安装行：状态点 + 名称（有设置页才是链接）+ 版本·形态·工具数 + 开关 + 卸载。
//   停用或异常时不显示工具数——那时的数字只会误导。
// - 卸载两步确认；确认条 3 秒不动就自己收回去。
// - 读不出 manifest 的目录单列一节：装错位置不该静默消失。

import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import type { PluginList, PluginPreview, PluginView } from "../../types";
import { finishPluginDrop, notePluginDragOver } from "./pluginDropArbiter";
import styles from "./SettingsPlugins.module.css";

/** 能力名的「人话」；没登记的原样显示——宁可难看，也不替插件美化。 */
const CAPABILITY_LABELS: Record<string, string> = {
  "fs.plugin": "读写自己的数据目录",
  "fs.temp": "读写临时文件",
  "query.call": "查询方舟数据（只读）",
  "maa.bridge": "启动 / 关闭 MAA 桥进程",
};

const STATE_LABELS: Record<PluginView["state"], string> = {
  loaded: "正常",
  starting: "启动中",
  failed: "加载失败",
  faulted: "进程退出",
  disabled: "已停用",
};

function capabilityText(key: string): string {
  return CAPABILITY_LABELS[key] ?? key;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

interface SettingsPluginsProps {
  list: PluginList;
  busy: boolean;
  preview: PluginPreview | null;
  installing: boolean;
  error: string | null;
  onInstallFolder: () => void;
  onDropFolder: (path: string) => void;
  onConfirmInstall: () => void;
  onCancelInstall: () => void;
  onSetEnabled: (id: string, enabled: boolean) => void;
  onUninstall: (id: string, deleteData: boolean) => void;
  onRetry: (id: string) => void;
  onOpenSettings: (id: string) => void;
}

export function SettingsPlugins({
  list,
  busy,
  preview,
  installing,
  error,
  onInstallFolder,
  onDropFolder,
  onConfirmInstall,
  onCancelInstall,
  onSetEnabled,
  onUninstall,
  onRetry,
  onOpenSettings,
}: SettingsPluginsProps) {
  const [over, setOver] = useState(false);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [deleteData, setDeleteData] = useState(false);
  const zoneRef = useRef<HTMLDivElement>(null);

  // 拖放：Tauri 的 webview 级事件没有 DOM 命中测试，只能自己按坐标判断。
  // 每次 enter / over 都把命中结果记给仲裁器（见 pluginDropArbiter）：drop 时
  // 对话区以「最后一次 dragover 是否落在虚线区」为准决定让不让路。
  useEffect(() => {
    let off: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const unlisten = await getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload;
          if (payload.type === "leave") {
            setOver(false);
            notePluginDragOver(false);
            return;
          }
          const rect = zoneRef.current?.getBoundingClientRect();
          const position = payload.position;
          const scale = window.devicePixelRatio || 1;
          const inside =
            rect !== undefined &&
            position.x / scale >= rect.left &&
            position.x / scale <= rect.right &&
            position.y / scale >= rect.top &&
            position.y / scale <= rect.bottom;
          if (payload.type === "enter" || payload.type === "over") {
            setOver(inside);
            notePluginDragOver(inside);
            return;
          }
          // drop：命中判定已经在最后的 over 里记过了，这里直接沿用。
          setOver(false);
          queueMicrotask(finishPluginDrop);
          if (inside && payload.paths.length > 0) {
            onDropFolder(payload.paths[0]);
          }
        });
        if (disposed) unlisten();
        else off = unlisten;
      } catch {
        // 非 Tauri 环境（测试 / 浏览器）：没有拖放事件，按钮仍然可用
      }
    })();
    return () => {
      disposed = true;
      off?.();
      finishPluginDrop();
    };
  }, [onDropFolder]);

  // 卸载确认条：3 秒不动就收回去，避免一直挂在那里。
  useEffect(() => {
    if (confirming === null) return;
    const timer = window.setTimeout(() => {
      setConfirming(null);
      setDeleteData(false);
    }, 3000);
    return () => window.clearTimeout(timer);
  }, [confirming]);

  const toggleOnEnter = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      e.preventDefault();
      e.currentTarget.click();
    }
  };

  return (
    <div className={styles.container}>
      <div className={styles.installRow}>
        <button
          type="button"
          className={styles.actionButton}
          disabled={busy || installing}
          onClick={onInstallFolder}
        >
          安装文件夹
        </button>
        <p className={styles.hint}>选插件的文件夹（zip 请先解压）</p>
      </div>

      <div
        ref={zoneRef}
        className={`${styles.dropZone}${over ? ` ${styles.dropZoneOver}` : ""}`}
      >
        {over ? "松手即可预览安装" : "把插件文件夹拖到这里"}
      </div>

      {error && <p className={styles.error}>{error}</p>}

      {preview && (
        <div className={styles.previewCard}>
          <div className={styles.previewTitle}>确认安装「{preview.name}」</div>
          <div className={styles.previewGrid}>
            <span className={styles.previewKey}>id</span>
            <span>{preview.id}</span>
            <span className={styles.previewKey}>版本</span>
            <span>{preview.version}</span>
            <span className={styles.previewKey}>作者</span>
            <span>{preview.author ?? "未署名"}</span>
            <span className={styles.previewKey}>形态</span>
            <span>{preview.kind === "process" ? "独立进程" : "内置"}</span>
            <span className={styles.previewKey}>能力</span>
            <span>
              {preview.capabilities.length === 0
                ? "不需要任何能力"
                : preview.capabilities.map(capabilityText).join("、")}
            </span>
            <span className={styles.previewKey}>内容</span>
            <span>
              {preview.fileCount} 个文件 · {formatBytes(preview.totalBytes)}
            </span>
          </div>
          <div className={styles.previewActions}>
            <button
              type="button"
              className={styles.actionButton}
              disabled={installing}
              onClick={onCancelInstall}
            >
              取消
            </button>
            <button
              type="button"
              className={styles.primaryButton}
              disabled={installing}
              onClick={onConfirmInstall}
            >
              {installing ? "安装中…" : "安装"}
            </button>
          </div>
        </div>
      )}

      <div className={styles.rowList}>
        {list.plugins.length === 0 && (
          <p className={styles.empty}>还没有插件</p>
        )}
        {list.plugins.map((plugin) => {
          if (confirming === plugin.id) {
            return (
              <div key={plugin.id} className={styles.confirmRow}>
                <span>卸载「{plugin.name}」？</span>
                <label className={styles.confirmCheck}>
                  <input
                    type="checkbox"
                    checked={deleteData}
                    onChange={(e) => setDeleteData(e.target.checked)}
                  />
                  同时删除它的数据
                </label>
                <span className={styles.rowSpacer} />
                <button
                  type="button"
                  className={styles.actionButton}
                  onClick={() => {
                    setConfirming(null);
                    setDeleteData(false);
                  }}
                >
                  取消
                </button>
                <button
                  type="button"
                  className={styles.primaryButton}
                  disabled={busy}
                  onClick={() => {
                    setConfirming(null);
                    onUninstall(plugin.id, deleteData);
                    setDeleteData(false);
                  }}
                >
                  卸载
                </button>
              </div>
            );
          }

          const ok = plugin.state === "loaded" || plugin.state === "starting";
          const bad = plugin.state === "failed" || plugin.state === "faulted";
          const nameInteractive = plugin.hasSettingsPage;
          return (
            <div key={plugin.id} className={styles.row}>
              <span
                className={`${styles.dot}${ok ? ` ${styles.dotOk}` : ""}${
                  bad ? ` ${styles.dotBad}` : ""
                }`}
                aria-hidden="true"
              />
              {nameInteractive ? (
                <button
                  type="button"
                  className={styles.name}
                  onClick={() => onOpenSettings(plugin.id)}
                >
                  {plugin.name}
                </button>
              ) : (
                <span className={styles.name}>{plugin.name}</span>
              )}
              <span className={styles.meta}>
                {STATE_LABELS[plugin.state]} · {plugin.version} ·{" "}
                {plugin.kind === "process" ? "进程" : "内置"}
                {plugin.toolCount !== null ? ` · ${plugin.toolCount} 个工具` : ""}
              </span>
              {bad && plugin.reason && (
                <span className={styles.reason} title={plugin.reason}>
                  {plugin.reason}
                </span>
              )}
              <span className={styles.rowSpacer} />
              {plugin.state === "faulted" && (
                <button
                  type="button"
                  className={styles.actionButton}
                  disabled={busy}
                  onClick={() => onRetry(plugin.id)}
                >
                  重试
                </button>
              )}
              {plugin.kind === "process" && (
                <button
                  type="button"
                  className={styles.actionButton}
                  disabled={busy}
                  onClick={() => {
                    setConfirming(plugin.id);
                    setDeleteData(false);
                  }}
                >
                  卸载
                </button>
              )}
              <input
                type="checkbox"
                aria-label={`启用 ${plugin.name}`}
                checked={plugin.state !== "disabled"}
                disabled={busy}
                onChange={(e) => onSetEnabled(plugin.id, e.target.checked)}
                onKeyDown={toggleOnEnter}
              />
            </div>
          );
        })}
      </div>

      {list.unregistered.length > 0 && (
        <div className={styles.rowList}>
          <p className={styles.hint}>读不出 plugin.json 的目录</p>
          {list.unregistered.map((dir) => (
            <div key={dir.dirName} className={styles.row}>
              <span className={styles.name}>{dir.dirName}</span>
              <span className={styles.reason} title={dir.reason}>
                {dir.reason}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
