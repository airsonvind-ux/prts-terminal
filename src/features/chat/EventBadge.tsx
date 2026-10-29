// src/components/content/EventBadge.tsx
// Agent 处理过程展示 · 阶段状态卡片（架构升级 · 模块 1）
//
// 职责：从 SessionEvent 序列中提取处理阶段，展示工作进度。
// 不暴露工具名、参数等内部细节——工具项以分类短语 + 三态文案呈现。
//
// 显示逻辑：
//   思考阶段：推理 chunk 到达 → "正在思考..."，推理链闭合（后续事件）→ "✓ 思考完成"
//   工具阶段：tool/call 到达 → "正在调用工具..."，全部 tool/result 配对 →
//            "✓ 工具调用完成（N项）"；存在失败项（ok=false）→ "⚠ 工具调用完成（含失败）"
//
// 工具项三态文案（短语由 toolAction 派生，summary 仅失败态显示错误原因）：
//   进行中（ok=null） → "PRTS正在{短语}..."
//   完成（ok=true）   → "PRTS{短语}完成"
//   失败（ok=false）  → "PRTS{短语}失败：{summary}"
//
// 审批第四态（M3-A §5.3）：run_command 询问路径在 tool/call 与 tool/result
// 之间插入 tool/approval-request / tool/approval-result 两事件：
//   pending（request 无配对 result，且 interactive 轮次）→ 消息流只读提示；
//     产品界面的确认/拒绝按钮由输入框上方的 Action Dock 承担
//     （showApprovalControls=false）。独立用法/旧测试可开 true 渲染内置按钮。
//   approved → 回归"执行中"（等 tool/result）
//   rejected → 预置失败态（"已拒绝该命令执行"）；tool/result(ok=false) 到达后
//              以正常失败态展示后端文案（summary 覆盖）
//   归档轮（interactive=false）不渲染按钮：turn 结束前 result 必达（preflight
//   await 保证），归档数据中 request 必有配对 result → pending 永不出现于归档
//
// 所有具体内容（推理文本、工具三态文案）默认折叠，用户点 ▸ 才展开。
// 审批 pending 存在时工具阶段强制展开（等待提示不可被折叠吞掉）。

import { useState, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  SessionEvent,
  MoodEvent,
} from "../../types";
import { describeMood, hasDisplay, renderDisplaySuffix } from "./eventDisplay";
import { buildToolSlots, type ToolSlot } from "./eventSlots";
import styles from "./EventBadge.module.css";

// ── 审批类型（M3-A） ──

/** 槽位内审批状态：由事件派生（result）与乐观态（按钮点击）合并 */
type ApprovalState = "pending" | "approved" | "rejected";

// ── 出卡族类别短语表（M3-A 地域性信任门 · 设计 §9 定稿） ──
// reason 码（ruleId 通道）↔ 短语一一对应；确认卡类别行直接回答"为什么问"。
// crit-* 不产生 request（恒拦直接拒绝，无确认卡）故无条目；ruleId 为 null
// 或未收录（region-in/system-query 等区内放行族在询问模式下弹卡、未来新增
// 码）→ 不渲染类别行，也不显示任何"高危"字样（高危徽章已废除）。

/** ruleId（出卡族 reason 码）→ 确认卡类别短语（设计 §9 定稿） */
const RULE_ID_PHRASES: Record<string, string> = {
  "region-external": "修改工作目录外的文件或目录",
  "run-external-program": "运行工作目录外的程序或脚本",
  "delete-always-ask": "删除文件或目录（任何位置都需要你确认）",
  "code-exec": "执行代码片段",
  "network-op": "访问网络或下载内容",
  "install-op": "安装或卸载软件",
  "process-control": "终止进程",
  "config-op": "修改系统网络配置",
  "git-undo": "修改仓库历史（可能丢弃更改）",
  "unanalyzable-syntax": "组合命令：无法判断具体内容",
  "skill-package-script": "Skill 包内的脚本",
  "skill-untrusted-active": "不可信 Skill 相关命令",
};

/** ruleId → 类别短语；null/未收录 → undefined（不渲染类别行） */
export function categoryPhrase(ruleId: string | null): string | undefined {
  return ruleId ? RULE_ID_PHRASES[ruleId] : undefined;
}

// ── 工具槽位（内部使用，不直接展示）──

interface Props {
  events: SessionEvent[];
  /**
   * 是否处于进行中轮次（currentEvents）。false = 归档重放（turns）——
   * 归档轮 request 必有配对 result（后端 preflight await 保证），
   * pending 不出现；此开关是双保险（异常数据不渲染交互按钮）。
   */
  interactive?: boolean;
  /** 外层附加类（消息列表传入的行级样式：屏幕外跳过布局等） */
  className?: string;
  /** 展开/收起任一阶段卡；消息列表用来停止自动跟底 */
  onToggle?: () => void;
  /**
   * 是否在事件卡内渲染审批按钮（默认 true，兼容既有测试/独立用法）。
   * 产品界面传 false：交互按钮只在 Action Dock，消息流保留只读记录。
   */
  showApprovalControls?: boolean;
}

export function EventBadge({
  events,
  interactive = false,
  className,
  showApprovalControls = true,
  onToggle,
}: Props) {
  // ── 推理内容提取：assistant/chunk(Reasoning) 累积 ──
  const reasoningText = useMemo(() => {
    let text = "";
    for (const e of events) {
      if (e.type === "assistant/reset") {
        text = "";
      } else if (e.type === "assistant/chunk" && e.kind === "reasoning") {
        text += e.content;
      }
    }
    return text;
  }, [events]);

  // "正在思考"：推理链未闭合——最后一个事件仍是推理 chunk。
  // 推理结束由后续事件（text chunk / tool/call / turn/end）隐式表达。
  const lastEvent = events[events.length - 1];
  const reasoningChainOpen =
    reasoningText.length > 0 &&
    lastEvent?.type === "assistant/chunk" &&
    lastEvent.kind === "reasoning";
  // 归档轮没有“正在执行”语义：中止时停在推理 chunk 的轮，重启后
  // 只能显示未完成，不能永远显示“正在思考...”。
  const reasoningActive = interactive && reasoningChainOpen;
  const reasoningIncomplete = !interactive && reasoningChainOpen;

  // ── 工具配对：callId 精确配对（tool/call 开槽，approval/result 事件挂接） ──
  const toolSlots: ToolSlot[] = useMemo(() => buildToolSlots(events), [events]);

  // ── 审批乐观态（M3-A）：确认/拒绝按钮点击成功 → 本地置态。
  //    与事件派生的 result 合并（事件先到则事件为准；乐观态只填事件未达的窗口）。 ──
  const [optimistic, setOptimistic] = useState<
    Record<string, "approved" | "rejected">
  >({});
  /** 正在提交的 callId（防双击） */
  const [resolving, setResolving] = useState<Record<string, boolean>>({});

  const handleResolve = async (
    sessionId: string,
    callId: string,
    decision: "approved" | "rejected"
  ): Promise<void> => {
    if (resolving[callId]) return;
    setResolving((prev) => ({ ...prev, [callId]: true }));
    try {
      const delivered = await invoke<boolean>("resolve_tool_approval", {
        request: { sessionId, callId, decision },
      });
      // delivered=false = 无此 pending（已超时/已决/会话结束）→ 不置乐观态，
      // 等待后端 approval-result / tool/result 事件兜底终态（幂等）
      if (delivered) {
        setOptimistic((prev) => ({ ...prev, [callId]: decision }));
      }
    } catch {
      // invoke 失败（后端不可达等）：保持 pending，用户可重试
    } finally {
      setResolving((prev) => {
        const next = { ...prev };
        delete next[callId];
        return next;
      });
    }
  };

  /**
   * 槽位审批展示态：事件派生 result 优先，其次乐观态，最后 pending。
   * approval=null（非执行类工具/无审批）→ null。
   */
  const approvalState = (slot: ToolSlot): ApprovalState | null => {
    if (!slot.approval) return null;
    const fromEvents = slot.approval.result;
    if (fromEvents) return fromEvents;
    const fromOptimistic = optimistic[slot.callId];
    if (fromOptimistic) return fromOptimistic;
    return "pending";
  };

  // ── 情绪事件（批 2-D）：本轮 mood/event 聚合（时序由事件序列承载） ──
  const moodEvents = useMemo(() => {
    const result: MoodEvent[] = [];
    for (const e of events) {
      if (e.type === "assistant/reset") result.length = 0;
      else if (e.type === "mood/event") result.push(e);
    }
    return result;
  }, [events]);

  const hasTools = toolSlots.length > 0;
  // 归档轮没有“正在执行”语义：无 result 只可能是中止/异常留下的未完成项，
  // 不能在重启后继续显示为活跃状态。
  const hasPendingTools = toolSlots.some((s) => s.ok === null);
  const toolsActive = interactive && hasPendingTools;
  const toolsIncomplete = !interactive && hasPendingTools;
  const hasToolFailure = toolSlots.some((s) => s.ok === false);
  /**
   * 存在审批态且未到 tool/result 终态的槽位（pending / 乐观或事件已决）→
   * 工具阶段保持展开：审批按钮与"已确认/已拒绝"反馈不能被折叠吞掉。
   */
  const hasApprovalActive =
    interactive && toolSlots.some((s) => s.approval !== null && s.ok === null);

  // ── 折叠状态 ──
  const [reasoningOpen, setReasoningOpen] = useState(false);
  const [toolsOpen, setToolsOpen] = useState(false);
  const [moodOpen, setMoodOpen] = useState(false);
  const toolsOpenEffective = toolsOpen || hasApprovalActive;

  // ── 空守卫 ──
  if (!reasoningText && !hasTools && moodEvents.length === 0) return null;

  return (
    <div className={className ? `${styles.badge} ${className}` : styles.badge}>
      {/* ── 思考阶段 ── */}
      {reasoningText && (
        <div className={styles.section}>
          <button
            className={styles.toggle}
            data-focus-inset
            onClick={() => {
              setReasoningOpen(!reasoningOpen);
              onToggle?.();
            }}
          >
            <span className={styles.titleGroup}>
              <span
                className={`${styles.arrow} ${reasoningOpen ? styles.arrowOpen : ""}`}
              >
                ▸
              </span>
              思考过程
              <span className={reasoningActive ? styles.statusActive : styles.statusDone}>
                {reasoningActive
                  ? "· 正在思考..."
                  : reasoningIncomplete
                    ? "· 思考未完成"
                    : "· ✓ 思考完成"}
              </span>
            </span>
          </button>
          {reasoningOpen && (
            <pre className={styles.detail}>{reasoningText}</pre>
          )}
        </div>
      )}

      {/* ── 工具阶段 ── */}
      {hasTools && (
        <div className={styles.section}>
          <button
            className={styles.toggle}
            data-focus-inset
            onClick={() => {
              setToolsOpen(!toolsOpen);
              onToggle?.();
            }}
          >
            <span className={styles.titleGroup}>
              <span
                className={`${styles.arrow} ${toolsOpenEffective ? styles.arrowOpen : ""}`}
              >
                ▸
              </span>
              工具调用
              <span className={toolsActive ? styles.statusActive : styles.statusDone}>
                {toolsActive
                  ? "· 正在调用工具..."
                  : toolsIncomplete
                    ? `· 工具调用未完成${toolSlots.length > 1 ? `（${toolSlots.length}项）` : ""}`
                    : hasToolFailure
                      ? "· ⚠ 工具调用完成（含失败）"
                      : `· ✓ 工具调用完成${toolSlots.length > 1 ? `（${toolSlots.length}项）` : ""}`}
              </span>
            </span>
          </button>
          {toolsOpenEffective && (
            <div className={styles.detail}>
              {toolSlots.map((slot, i) => {
                const approval = approvalState(slot);
                const externalProvenance =
                  slot.provenance?.trust === "external" ? slot.provenance : null;
                const provenanceTag = externalProvenance ? (
                  <span
                    className={styles.provenanceTag}
                    title={`外部资料 · 来源：${externalProvenance.source}${
                      externalProvenance.version
                        ? ` · ${externalProvenance.version}`
                        : ""
                    }`}
                  >
                    外部
                  </span>
                ) : null;
                // ── 终态优先：tool/result 到达后走既有三态 ──
                if (slot.ok !== null) {
                  // 失败行单行省略（悬停 title 看全文）：展示面不铺长错误串；
                  // 与模型上下文共用的 summary 本身不变。
                  if (!slot.ok) {
                    return (
                      <div
                        key={slot.callId || i}
                        className={`${styles.toolItem} ${styles.toolFailed}`}
                      >
                        <span
                          className={styles.toolFailLine}
                          title={slot.summary}
                        >
                          {`PRTS${slot.phrase}失败：${slot.summary}`}
                        </span>
                        {renderDisplaySuffix(slot.display, true)}
                        {provenanceTag}
                      </div>
                    );
                  }
                  return (
                    <div
                      key={slot.callId || i}
                      className={`${styles.toolItem} ${styles.toolDone}`}
                    >
                      <span>
                        {`PRTS${slot.phrase}完成`}
                        {renderDisplaySuffix(slot.display, false)}
                      </span>
                      {provenanceTag}
                    </div>
                  );
                }
                // ── 审批态（进行中且 tool/result 未达） ──
                if (approval === "pending" && interactive) {
                  if (!showApprovalControls) {
                    return (
                      <div
                        key={slot.callId || i}
                        className={`${styles.toolItem} ${styles.toolApproval}`}
                      >
                        <span>PRTS 正在等待你确认{slot.phrase}</span>
                        <div className={styles.approvalCategory}>
                          请在输入框上方处理
                        </div>
                      </div>
                    );
                  }
                  const category = categoryPhrase(
                    slot.approval?.ruleId ?? null
                  );
                  return (
                    <div
                      key={slot.callId || i}
                      className={`${styles.toolItem} ${styles.toolApproval}`}
                    >
                      <span>PRTS需要你确认{slot.phrase}</span>
                      {slot.approval?.privilege === "full" && (
                        <div className={styles.approvalCategory}>
                          {`将以完整权限运行${
                            slot.approval.justification
                              ? `：${slot.approval.justification}`
                              : ""
                          }`}
                        </div>
                      )}
                      {category !== undefined && (
                        <div className={styles.approvalCategory}>
                          将执行：{category}
                        </div>
                      )}
                      {slot.command !== null && (
                        <pre className={styles.approvalCommand}>
                          {slot.command}
                        </pre>
                      )}
                      <div className={styles.approvalActions}>
                        <button
                          className={styles.approveBtn}
                          disabled={!!resolving[slot.callId]}
                          onClick={() =>
                            handleResolve(slot.sessionId, slot.callId, "approved")
                          }
                        >
                          确认执行
                        </button>
                        <button
                          className={styles.rejectBtn}
                          disabled={!!resolving[slot.callId]}
                          onClick={() =>
                            handleResolve(slot.sessionId, slot.callId, "rejected")
                          }
                        >
                          拒绝
                        </button>
                      </div>
                    </div>
                  );
                }
                if (approval === "approved") {
                  return (
                    <div
                      key={slot.callId || i}
                      className={`${styles.toolItem} ${styles.toolRunning}`}
                    >
                      <span>
                        {`PRTS正在${slot.phrase}`}
                        {hasDisplay(slot.display)
                          ? renderDisplaySuffix(slot.display, false)
                          : "..."}
                        {"（已确认）"}
                      </span>
                    </div>
                  );
                }
                if (approval === "rejected") {
                  // 预置失败态：result 已拒绝但 tool/result 未达（后端终态必达，
                  // 到达后 summary 覆盖）
                  return (
                    <div
                      key={slot.callId || i}
                      className={`${styles.toolItem} ${styles.toolFailed}`}
                    >
                      <span>{`PRTS${slot.phrase}失败：已拒绝该命令执行`}</span>
                    </div>
                  );
                }
                // ── 无审批的普通进行中 / 防御（归档轮 pending 理论不可达） ──
                return (
                  <div
                    key={slot.callId || i}
                    className={`${styles.toolItem} ${styles.toolRunning}`}
                  >
                    <span>
                      {approval === "pending" ? (
                        `PRTS正在${slot.phrase}...（等待确认）`
                      ) : (
                        <>
                          {`PRTS正在${slot.phrase}`}
                          {hasDisplay(slot.display)
                            ? renderDisplaySuffix(slot.display, false)
                            : "..."}
                        </>
                      )}
                    </span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
      {/* ── 情绪阶段（批 2-D）：折叠摘要 + 展开时间线 ── */}
      {moodEvents.length > 0 && (
        <div className={styles.section}>
          <button
            className={styles.toggle}
            onClick={() => {
              setMoodOpen(!moodOpen);
              onToggle?.();
            }}
          >
            <span className={styles.titleGroup}>
              <span
                className={`${styles.arrow} ${moodOpen ? styles.arrowOpen : ""}`}
              >
                ▸
              </span>
              情绪
              <span className={styles.statusDone}>
                {moodEvents.length === 1
                  ? `· ${describeMood(moodEvents[0] as MoodEvent)}`
                  : `· 心情变化了 ${moodEvents.length} 次`}
              </span>
            </span>
          </button>
          {moodOpen && (
            <div className={styles.detail}>
              {moodEvents.map((ev, i) => (
                <div key={i} className={styles.toolItem}>
                  <span>{describeMood(ev as MoodEvent)}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
