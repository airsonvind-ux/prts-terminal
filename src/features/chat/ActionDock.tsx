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

import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SessionEvent } from "../../types";
import { useConversationStore } from "../../stores/useConversationStore";
import {
  isProcessingBusy,
  useProcessingStore,
} from "../../stores/useProcessingStore";
import { useUpdaterStore } from "../../stores/useUpdaterStore";
import { showToast } from "../../stores/useToastStore";
import { toolAction } from "../../toolDisplay";
import { categoryPhrase } from "./EventBadge";
import { NoticeDock, UpdateDock } from "./UpdateDock";
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

