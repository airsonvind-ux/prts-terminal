// 设置页 · 插件详情（UI 规格草案 2 §三）。
//
// 两列键值：左边是标签、右边是值。能力超过两条时折叠成「能力（N）」，
// 点一下展开——能力是声明，铺满一屏反而看不清状态。
//
// 卸载是两步的：先点「卸载」变成确认条，勾不勾「同时删除它的数据」都在确认条里，
// 3 秒不动自动收回（误触的代价是删一个目录，值得多问一次）。

import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import type { PluginView } from "../../types";
import { Button } from "./ui/Button";
import {
  capabilityLabel,
  kindLabel,
  STATE_LABELS,
} from "./pluginText";
import styles from "./SettingsPlugins.module.css";

interface PluginDetailProps {
  plugin: PluginView;
  busy: boolean;
  locked: boolean;
  lockedHint: string;
  /** 有设置页时由 host 传进来的组件实例；没有就显示一句说明 */
  settingsPage?: ReactNode;
  onRetry: () => void;
  onUninstall: (deleteData: boolean) => void;
}

const CAPABILITY_PREVIEW = 2;

export function PluginDetail({
  plugin,
  busy,
  locked,
  lockedHint,
  settingsPage,
  onRetry,
  onUninstall,
}: PluginDetailProps) {
  const [expanded, setExpanded] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [deleteData, setDeleteData] = useState(false);

  // 3 秒不动就收回确认条：这个动作会删目录，不能一直挂着等误触。
  useEffect(() => {
    if (!confirming) return;
    const timer = window.setTimeout(() => {
      setConfirming(false);
      setDeleteData(false);
    }, 3000);
    return () => window.clearTimeout(timer);
  }, [confirming]);

  const abnormal = plugin.state === "failed" || plugin.state === "faulted";
  const canUninstall = plugin.kind !== "builtin";

  return (
    <div className={styles.detail}>
      <dl className={styles.facts}>
        <dt>状态</dt>
        <dd className={abnormal ? styles.factBad : undefined}>
          {STATE_LABELS[plugin.state]}
          {abnormal && plugin.reason ? ` — ${plugin.reason}` : ""}
        </dd>

        <dt>版本</dt>
        <dd>
          {plugin.version} · {kindLabel(plugin.kind)}
        </dd>

        <dt>能力</dt>
        <dd>
          {plugin.capabilities.length === 0 ? (
            "—"
          ) : (
            <>
              {(expanded
                ? plugin.capabilities
                : plugin.capabilities.slice(0, CAPABILITY_PREVIEW)
              ).map((key) => (
                <span key={key} className={styles.capability}>
                  {capabilityLabel(key)}
                </span>
              ))}
              {plugin.capabilities.length > CAPABILITY_PREVIEW && (
                <button
                  type="button"
                  className={styles.moreButton}
                  onClick={() => setExpanded((v) => !v)}
                >
                  {expanded ? "收起" : `能力（${plugin.capabilities.length}）`}
                </button>
              )}
            </>
          )}
        </dd>

        <dt>工具</dt>
        {/* 停用/异常时后端给 null：显示破折号，不写 0（0 会被读成"这个插件没能力"） */}
        <dd>{plugin.toolCount === null ? "—" : plugin.toolCount}</dd>
      </dl>

      {settingsPage ? (
        settingsPage
      ) : (
        <p className={styles.sectionHint}>这个插件没有设置页</p>
      )}

      <div className={styles.detailActions}>
        {plugin.state === "faulted" && (
          <Button disabled={busy || locked} onClick={onRetry}>
            重试
          </Button>
        )}
        {canUninstall && !confirming && (
          <Button
            variant="danger"
            disabled={busy}
            onClick={() => setConfirming(true)}
          >
            卸载
          </Button>
        )}
      </div>

      {canUninstall && confirming && (
        <div className={styles.confirmRow}>
          <label className={styles.confirmCheck}>
            <input
              type="checkbox"
              checked={deleteData}
              disabled={busy || locked}
              onChange={(e) => setDeleteData(e.target.checked)}
            />
            同时删除它的数据
          </label>
          <Button
            variant="danger"
            disabled={busy || locked}
            title={locked ? lockedHint : undefined}
            onClick={() => {
              setConfirming(false);
              onUninstall(deleteData);
            }}
          >
            确认卸载
          </Button>
          <Button
            disabled={busy}
            onClick={() => {
              setConfirming(false);
              setDeleteData(false);
            }}
          >
            取消
          </Button>
        </div>
      )}
    </div>
  );
}
