import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PostInstallNotice, UpdateStatus } from "../../types";
import { useUpdaterStore } from "../../stores/useUpdaterStore";
import { showToast } from "../../stores/useToastStore";
import styles from "./ActionDock.module.css";

function formatBytes(bytes: number | null): string {
  if (bytes === null || !Number.isFinite(bytes) || bytes <= 0) return "";
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function progressPercent(status: UpdateStatus): number | null {
  if (
    status.downloadedBytes === null ||
    status.totalBytes === null ||
    status.totalBytes <= 0
  ) {
    return null;
  }
  return Math.max(
    0,
    Math.min(100, Math.round((status.downloadedBytes / status.totalBytes) * 100))
  );
}

function errorTitleFor(stage: UpdateStatus["errorStage"]): string {
  if (stage === "check") return "检查更新失败";
  if (stage === "download") return "下载更新失败";
  if (stage === "disk") return "磁盘空间不足";
  if (stage === "install") return "安装没有完成";
  return "更新失败";
}

interface UpdateDockProps {
  status: UpdateStatus;
  collapsed: boolean;
  blocker: string | null;
  action: "check" | "download" | "install" | null;
  actionError: string | null;
  onExpand: () => void;
  onCollapse: () => void;
  onDownload: () => void;
  onInstall: () => void;
  onRetry: () => void;
}

function miniBar(
  text: string,
  extra: string,
  onExpand: () => void
): ReactNode {
  return (
    <div className={styles.wrap} data-action-dock="update">
      <button className={styles.mini} onClick={onExpand} type="button">
        <span className={styles.dot} />
        <span className={styles.miniText}>{text}</span>
        <span className={styles.miniExtra}>{extra}</span>
      </button>
    </div>
  );
}

export function UpdateDock({
  status,
  collapsed,
  blocker,
  action,
  actionError,
  onExpand,
  onCollapse,
  onDownload,
  onInstall,
  onRetry,
}: UpdateDockProps) {
  const version = status.available?.version ?? "新版本";
  const displayVersion = status.available?.version ?? null;
  const manualUrl =
    status.manualDownloadUrl ?? status.available?.downloadUrl ?? null;
  const pct = progressPercent(status);
  const downloaded = formatBytes(status.downloadedBytes);
  const total = formatBytes(status.totalBytes);
  const busy = action !== null;
  const blocked = blocker !== null;
  const dismiss = () => useUpdaterStore.getState().dismiss();

  if (status.phase === "downloading" && collapsed) {
    return miniBar(
      "正在后台下载更新",
      pct === null ? "查看" : `${pct}% · 查看`,
      onExpand
    );
  }
  if (status.phase === "downloaded" && collapsed) {
    return miniBar("更新已就绪", "查看", onExpand);
  }
  if (status.phase === "available" && collapsed) {
    return miniBar(`更新 ${version} 待处理`, "查看", onExpand);
  }
  if (status.phase === "error" && collapsed) {
    return miniBar(errorTitleFor(status.errorStage), "查看", onExpand);
  }

  if (status.phase === "installing") {
    return (
      <div className={styles.wrap} data-action-dock="update">
        <div className={`${styles.card} ${styles.updateCard}`}>
          <div className={styles.strip}>
            <span className={styles.dot} />
            <span>正在启动安装器</span>
            <span className={styles.source}>{version}</span>
          </div>
          <div
            className={styles.body}
            tabIndex={0}
            role="group"
            aria-label="更新详情"
          >
            <div className={styles.headline}>安装器启动后应用会退出</div>
            <div className={styles.meta}>更新完成后再打开 PRTS。</div>
          </div>
        </div>
      </div>
    );
  }

  const error = status.error ?? actionError;
  const showError = status.phase === "error";
  const errorStage = status.errorStage ?? null;
  const errorTitle = errorTitleFor(status.errorStage);
  const errorHeadline =
    errorStage === "check"
      ? "没能连接到更新源"
      : errorStage === "download"
        ? "更新没有下载完成"
        : errorStage === "disk"
          ? "磁盘剩余空间不足"
          : "更新没有完成";
  const retryLabel = errorStage === "download" ? "重新下载" : "重新检查";

  return (
    <div className={styles.wrap} data-action-dock="update">
      <div
        className={`${styles.card} ${
          showError ? styles.errorCard : styles.updateCard
        }`}
      >
        <div className={styles.strip}>
          <span className={styles.dot} />
          <span>
            {status.phase === "available"
              ? "发现新版本"
              : status.phase === "downloading"
                ? "正在下载更新"
                : status.phase === "downloaded"
                  ? "更新已下载"
                  : errorTitle}
          </span>
          {displayVersion && (
            <span className={styles.source}>{displayVersion}</span>
          )}
        </div>
        <div
          className={styles.body}
          tabIndex={0}
          role="group"
          aria-label="更新详情"
        >
          {status.phase === "available" && (
            <>
              <div className={styles.headline}>PRTS 有新版本可用</div>
              <div className={styles.notes}>
                {status.available?.notes?.trim() || "更新内容已取回。"}
              </div>
            </>
          )}
          {status.phase === "downloading" && (
            <>
              <div className={styles.headline}>
                下载可以继续，输入框仍然可用
              </div>
              <div className={styles.progressRow}>
                <span>{pct === null ? "下载中" : `${pct}%`}</span>
                <span>
                  {downloaded && total
                    ? `${downloaded} / ${total}`
                    : "正在获取大小"}
                </span>
              </div>
              <div
                className={styles.progress}
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={pct ?? undefined}
              >
                <span style={{ width: `${pct ?? 0}%` }} />
              </div>
            </>
          )}
          {status.phase === "downloaded" && (
            <>
              <div className={styles.headline}>重启后生效</div>
              <div className={styles.meta}>
                安装会中断当前对话。{blocker ?? "当前没有活跃任务。"}
              </div>
              {actionError && (
                <div className={styles.errorText} style={{ marginTop: 4 }}>
                  {actionError}
                </div>
              )}
            </>
          )}
          {showError && (
            <>
              <div className={styles.headline}>{errorHeadline}</div>
              <div className={styles.errorText}>{error ?? "更新失败，请重试。"}</div>
              {errorStage === "check" && (
                <div className={styles.meta}>
                  稍后会自动重试，也可以在关于页手动检查。
                </div>
              )}
              {errorStage === "disk" && (
                <div className={styles.meta}>
                  释放安装目录所在磁盘空间后重新检查。
                </div>
              )}
            </>
          )}
        </div>
        <div className={styles.actions}>
          {status.phase === "available" && (
            <>
              <button className={styles.btn} onClick={dismiss} type="button">
                关闭
              </button>
              <button
                className={`${styles.btn} ${styles.primary}`}
                disabled={busy}
                onClick={onDownload}
                type="button"
              >
                {action === "download" ? "开始下载…" : "下载更新"}
              </button>
            </>
          )}
          {status.phase === "downloading" && (
            <button className={styles.btn} onClick={onCollapse} type="button">
              后台继续
            </button>
          )}
          {status.phase === "downloaded" && (
            <>
              <button className={styles.btn} onClick={dismiss} type="button">
                关闭
              </button>
              <button
                className={`${styles.btn} ${styles.primary}`}
                disabled={busy || blocked}
                onClick={onInstall}
                title={blocker ?? undefined}
                type="button"
              >
                {action === "install" ? "正在启动…" : "重启并安装"}
              </button>
            </>
          )}
          {showError && (
            <>
              <button className={styles.btn} onClick={dismiss} type="button">
                关闭
              </button>
              {manualUrl && errorStage !== "disk" && (
                <button
                  className={styles.btn}
                  onClick={() => {
                    void invoke("open_url", { url: manualUrl }).catch(
                      (err: unknown) =>
                        showToast(`打开下载页失败：${String(err)}`, {
                          kind: "warning",
                        })
                    );
                  }}
                  type="button"
                >
                  手动下载
                </button>
              )}
              <button
                className={`${styles.btn} ${styles.primary}`}
                disabled={busy}
                onClick={errorStage === "download" ? onDownload : onRetry}
                type="button"
              >
                {action === "check"
                  ? "检查中…"
                  : action === "download"
                    ? "下载中…"
                    : retryLabel}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

export function NoticeDock({
  notice,
  onDismiss,
}: {
  notice: PostInstallNotice;
  onDismiss: () => void;
}) {
  return (
    <div className={styles.wrap} data-action-dock="notice">
      <div className={`${styles.card} ${styles.updateCard}`}>
        <div className={styles.strip}>
          <span className={styles.dot} />
          <span>更新完成</span>
          <span className={styles.source}>{notice.version}</span>
        </div>
        <div
          className={styles.body}
          tabIndex={0}
          role="group"
          aria-label="更新日志"
        >
          <div className={styles.headline}>
            PRTS 已更新到 {notice.version}
          </div>
          <div className={styles.notes}>
            {notice.notes?.trim() || "本次更新没有附带说明。"}
          </div>
        </div>
        <div className={styles.actions}>
          <button className={styles.btn} onClick={onDismiss} type="button">
            知道了
          </button>
        </div>
      </div>
    </div>
  );
}
