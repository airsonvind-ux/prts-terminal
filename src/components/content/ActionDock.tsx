// Action Dock · 输入框上方的待处理动作区
//
// 两种业务共用外壳，不共用状态机：
//   - 工具审批：阻塞请求，必须确认/拒绝；消息流只保留只读审计记录。
//   - 更新：可选任务，可收起；下载与重启安装分开确认。
//
// 优先级（同一时刻只显示一张主卡）：
//   工具审批 > 更新已下载 > 更新下载中 > 发现新版本 > 更新失败 > 更新日志
//
// 边界：不抢输入焦点；正文区有高度上限并内部滚动；按钮行永远在滚动区外。

import { useEffect, useMemo, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  PostInstallNotice,
  SessionEvent,
  UpdateStatus,
} from "../../types";
import { useConversationStore } from "../../stores/useConversationStore";
import {
  isProcessingBusy,
  useProcessingStore,
} from "../../stores/useProcessingStore";
import { useUpdaterStore } from "../../stores/useUpdaterStore";
import { showToast } from "../../stores/useToastStore";
import { toolAction } from "../../toolDisplay";
import { categoryPhrase } from "./EventBadge";
import styles from "./ActionDock.module.css";

type Decision = "approved" | "rejected";

interface PendingApprovalView {
  key: string;
  sessionId: string;
  callId: string;
  conversationId: string;
  conversationTitle: string;
  command: string;
  phrase: string;
  ruleId: string | null;
  privilege: "restricted" | "full";
  justification: string | null;
}

function collectPendingApprovals(
  processing: ReturnType<typeof useProcessingStore.getState>["processing"]
): PendingApprovalView[] {
  const views: PendingApprovalView[] = [];
  const seen = new Set<string>();

  for (const snapshot of Object.values(processing)) {
    const decided = new Set(
      snapshot.currentEvents
        .filter((event) => event.type === "tool/approval-result")
        .map((event) => event.callId)
    );
    for (const event of snapshot.currentEvents) {
      if (event.type !== "tool/approval-request") continue;
      if (decided.has(event.callId)) continue;
      const key = `${event.sessionId}:${event.callId}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const call = snapshot.currentEvents.find(
        (candidate): candidate is Extract<SessionEvent, { type: "tool/call" }> =>
          candidate.type === "tool/call" && candidate.callId === event.callId
      );
      views.push({
        key,
        sessionId: event.sessionId,
        callId: event.callId,
        conversationId: snapshot.conversationId,
        conversationTitle: "",
        command: event.command,
        phrase: toolAction(call?.toolName ?? "run_command"),
        ruleId: event.ruleId,
        privilege: event.privilege,
        justification: event.justification,
      });
    }
  }
  return views;
}

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
  if (stage === "install") return "安装没有完成";
  return "更新失败";
}

export function ActionDock() {
  const processing = useProcessingStore((s) => s.processing);
  const conversations = useConversationStore((s) => s.conversations);
  const activeId = useConversationStore((s) => s.activeId);
  const globallyBusy = useProcessingStore(isProcessingBusy);

  const status = useUpdaterStore((s) => s.status);
  const notice = useUpdaterStore((s) => s.notice);
  const blockers = useUpdaterStore((s) => s.blockers);
  const action = useUpdaterStore((s) => s.action);
  const actionError = useUpdaterStore((s) => s.actionError);
  const collapsed = useUpdaterStore((s) => s.collapsed);
  const dismissedVersion = useUpdaterStore((s) => s.dismissedVersion);
  const dismissedError = useUpdaterStore((s) => s.dismissedError);

  const approvals = useMemo(
    () => collectPendingApprovals(processing),
    [processing]
  );
  const titledApprovals = useMemo(() => {
    const byId = new Map(conversations.map((c) => [c.id, c.title] as const));
    return approvals.map((item) => ({
      ...item,
      conversationTitle: byId.get(item.conversationId) ?? "",
    }));
  }, [approvals, conversations]);

  const [resolving, setResolving] = useState<Record<string, boolean>>({});
  const [optimistic, setOptimistic] = useState<Record<string, Decision>>({});

  // 请求消失（后端已裁决/会话结束）→ 清掉本地乐观态，避免影响下一张卡。
  useEffect(() => {
    const live = new Set(titledApprovals.map((item) => item.key));
    setOptimistic((prev) => {
      const next: Record<string, Decision> = {};
      for (const [key, value] of Object.entries(prev)) {
        if (live.has(key)) next[key] = value;
      }
      return next;
    });
  }, [titledApprovals]);

  // 新版本出现时重新展开；阶段变化（下载完成/失败）不重置用户的收起选择。
  // 下载动作本身会主动展开，见 useUpdaterStore.download。
  useEffect(() => {
    useUpdaterStore.getState().setCollapsed(false);
  }, [status?.available?.version, notice?.version]);

  // 已下载阶段实时查阻塞原因；MAA 可能在这期间启动。
  useEffect(() => {
    if (status?.phase !== "downloaded") return;
    void useUpdaterStore.getState().refreshBlockers();
    const timer = window.setInterval(() => {
      void useUpdaterStore.getState().refreshBlockers();
    }, 5000);
    return () => window.clearInterval(timer);
  }, [status?.phase]);

  const resolve = async (
    request: PendingApprovalView,
    decision: Decision
  ) => {
    if (resolving[request.callId]) return;
    setResolving((prev) => ({ ...prev, [request.callId]: true }));
    try {
      const delivered = await invoke<boolean>("resolve_tool_approval", {
        request: {
          sessionId: request.sessionId,
          callId: request.callId,
          decision,
        },
      });
      if (delivered) {
        setOptimistic((prev) => ({ ...prev, [request.callId]: decision }));
      } else {
        showToast("这条审批请求已过期", {
          kind: "warning",
          dedupeKey: `approval-expired-${request.callId}`,
        });
      }
    } catch (error) {
      showToast(`提交审批失败：${String(error)}`, {
        kind: "warning",
        dedupeKey: `approval-failed-${request.callId}`,
      });
    } finally {
      setResolving((prev) => {
        const next = { ...prev };
        delete next[request.callId];
        return next;
      });
    }
  };

  const primary = titledApprovals[0];
  if (primary) {
    const decided = optimistic[primary.callId];
    const submitted = resolving[primary.callId] === true;
    const source =
      primary.conversationId === activeId
        ? "当前对话"
        : primary.conversationTitle
          ? `对话「${primary.conversationTitle}」`
          : "另一个对话";
    const category = categoryPhrase(primary.ruleId);

    return (
      <div className={styles.wrap} data-action-dock="approval">
        <div className={`${styles.card} ${styles.approvalCard}`}>
          <div className={styles.strip}>
            <span className={styles.dot} />
            <span>{decided ? "已确认，等待执行" : "等待你确认"}</span>
            <span className={styles.source}>{source}</span>
            {titledApprovals.length > 1 && (
              <span className={styles.queue}>
                还有 {titledApprovals.length - 1} 个请求
              </span>
            )}
          </div>
          <div
            className={styles.body}
            tabIndex={0}
            role="group"
            aria-label="待确认命令"
          >
            <div className={styles.headline}>PRTS 请求运行一条命令</div>
            {category !== undefined && (
              <div className={styles.category}>将执行：{category}</div>
            )}
            {primary.privilege === "full" && (
              <div className={styles.category}>
                {primary.justification
                  ? `将以完整权限运行：${primary.justification}`
                  : "将以完整权限运行"}
              </div>
            )}
            <pre className={styles.command}>{primary.command}</pre>
          </div>
          <div className={styles.actions}>
            {decided ? (
              <span className={styles.submitted}>
                {decided === "approved" ? "已提交确认" : "已提交拒绝"}
              </span>
            ) : (
              <>
                <button
                  className={`${styles.btn} ${styles.reject}`}
                  disabled={submitted}
                  onClick={() => void resolve(primary, "rejected")}
                >
                  拒绝
                </button>
                <button
                  className={`${styles.btn} ${styles.primary}`}
                  disabled={submitted}
                  onClick={() => void resolve(primary, "approved")}
                >
                  {submitted ? "提交中…" : "确认执行"}
                </button>
              </>
            )}
          </div>
        </div>
      </div>
    );
  }

  if (
    status &&
    (status.phase === "error"
      ? dismissedError
      : (status.phase === "available" || status.phase === "downloaded") &&
        status.available?.version === dismissedVersion)
  ) {
    return null;
  }

  if (status?.phase === "downloaded" || status?.phase === "installing") {
    return (
      <UpdateDock
        status={status}
        collapsed={collapsed}
        blocker={
          blockers?.activeTurn || globallyBusy
            ? "有对话正在处理，完成或中止后再安装"
            : blockers?.maaRunning
              ? "MAA 任务正在运行，完成或中止后再安装"
              : null
        }
        action={action}
        actionError={actionError}
        onExpand={() => useUpdaterStore.getState().setCollapsed(false)}
        onCollapse={() => useUpdaterStore.getState().setCollapsed(true)}
        onDownload={() => void useUpdaterStore.getState().download()}
        onInstall={() => void useUpdaterStore.getState().install()}
        onRetry={() => void useUpdaterStore.getState().check()}
      />
    );
  }

  if (status && status.phase === "downloading") {
    return (
      <UpdateDock
        status={status}
        collapsed={collapsed}
        blocker={null}
        action={action}
        actionError={actionError}
        onExpand={() => useUpdaterStore.getState().setCollapsed(false)}
        onCollapse={() => useUpdaterStore.getState().setCollapsed(true)}
        onDownload={() => void useUpdaterStore.getState().download()}
        onInstall={() => void useUpdaterStore.getState().install()}
        onRetry={() => void useUpdaterStore.getState().check()}
      />
    );
  }

  if (status && status.phase === "available") {
    return (
      <UpdateDock
        status={status}
        collapsed={collapsed}
        blocker={null}
        action={action}
        actionError={actionError}
        onExpand={() => useUpdaterStore.getState().setCollapsed(false)}
        onCollapse={() => useUpdaterStore.getState().setCollapsed(true)}
        onDownload={() => void useUpdaterStore.getState().download()}
        onInstall={() => void useUpdaterStore.getState().install()}
        onRetry={() => void useUpdaterStore.getState().check()}
      />
    );
  }

  if (status && status.phase === "error") {
    return (
      <UpdateDock
        status={status}
        collapsed={collapsed}
        blocker={null}
        action={action}
        actionError={actionError}
        onExpand={() => useUpdaterStore.getState().setCollapsed(false)}
        onCollapse={() => useUpdaterStore.getState().setCollapsed(true)}
        onDownload={() => void useUpdaterStore.getState().download()}
        onInstall={() => void useUpdaterStore.getState().install()}
        onRetry={() => void useUpdaterStore.getState().check()}
      />
    );
  }

  if (notice) {
    return (
      <NoticeDock
        notice={notice}
        onDismiss={() => useUpdaterStore.getState().dismissNotice()}
      />
    );
  }

  return null;
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

function UpdateDock({
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
              {manualUrl && (
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

function NoticeDock({
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
