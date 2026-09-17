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
//   pending（request 无配对 result，且 interactive 轮次）→ 确认卡：
//     类别短语行（ruleId→短语）+ 完整命令（等宽）+ 确认/拒绝按钮（invoke resolve_tool_approval）
//   approved → 回归"执行中"（等 tool/result）
//   rejected → 预置失败态（"已拒绝该命令执行"）；tool/result(ok=false) 到达后
//              以正常失败态展示后端文案（summary 覆盖）
//   归档轮（interactive=false）不渲染按钮：turn 结束前 result 必达（preflight
//   await 保证），归档数据中 request 必有配对 result → pending 永不出现于归档
//
// 所有具体内容（推理文本、工具三态文案）默认折叠，用户点 ▸ 才展开。
// 审批 pending 存在时工具阶段强制展开（按钮不可被折叠吞掉）。

import { useState, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  SessionEvent,
  MoodEvent,
  MoodIntensity,
  ToolApprovalResultEvent,
  ToolDisplay,
} from "../../types";
import { toolAction } from "../../toolDisplay";
import { FileLink } from "../ui/FileLink";
import styles from "./EventBadge.module.css";

// ── 情绪描述表（批 2-D 情绪卡片）：情绪 × 强度档位 → 独立文案（定稿） ──

const MOOD_DESCRIPTIONS: Record<
  "高兴" | "困惑" | "沮丧",
  [string, string, string, string]
> = {
  高兴: [
    "PRTS 很开心，轻轻浮动起来",
    "PRTS 心情大好，悬着也轻快起来",
    "PRTS 高兴坏了，浮动得停不下来",
    "PRTS 兴奋极了，像是要飞起来",
  ],
  困惑: [
    "PRTS 有些困惑，微微歪了歪头",
    "PRTS 犯了难，歪着头想了想",
    "PRTS 很困惑，歪着头，想不明白",
    "PRTS 彻底懵了，歪着头，仿佛卡在了思绪里",
  ],
  沮丧: [
    "PRTS 有点低落，轻轻沉了下去",
    "PRTS 有些沮丧，沉了下去",
    "PRTS 很沮丧，沉了下去",
    "PRTS 沮丧极了，低低地悬着",
  ],
};

/** 情绪事件 → 描述文本（intensity 由解析钳位在 0~3，索引安全） */
function describeMood(ev: MoodEvent): string {
  const idx: MoodIntensity = ev.intensity as MoodIntensity;
  return MOOD_DESCRIPTIONS[ev.mood][idx];
}

// ── 展示切片渲染（工具卡片「在做什么、对什么做」） ──

/**
 * 展示文本 / 链接标签的视觉长度上限（纯长度约束，不做「等 N 个」数量逻辑）。
 * 实现对超长文本截断，值可视觉调校。
 */
const DISPLAY_TEXT_MAX = 60;

function truncateDisplay(s: string): string {
  return s.length > DISPLAY_TEXT_MAX ? `${s.slice(0, DISPLAY_TEXT_MAX)}…` : s;
}

/** 展示切片是否有可渲染内容（决定进行中态是否保留「...」尾缀） */
function hasDisplay(display: ToolDisplay | null): boolean {
  return Boolean(display && (display.text || display.link));
}

/**
 * 展示切片后缀：文本（失败态用空格分隔，其余全角冒号）+ 链接（文件/URL）。
 * 链接在三态均保留（挂在 tool/call 侧数据上，失败不回退）。
 */
function renderDisplaySuffix(display: ToolDisplay | null, failed: boolean) {
  if (!display || !hasDisplay(display)) return null;
  const text = display.text ? truncateDisplay(display.text) : "";
  const link = display.link ?? null;
  return (
    <>
      {text && `${failed ? " " : "："}${text}`}
      {link && (
        <>
          {" "}
          <FileLink
            path={link.target}
            linkKind={link.kind}
            className={styles.displayLink}
            title={link.target}
          >
            {truncateDisplay(link.label)}
          </FileLink>
        </>
      )}
    </>
  );
}

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
};

/** ruleId → 类别短语；null/未收录 → undefined（不渲染类别行） */
function categoryPhrase(ruleId: string | null): string | undefined {
  return ruleId ? RULE_ID_PHRASES[ruleId] : undefined;
}

// ── 工具槽位（内部使用，不直接展示）──

interface ToolSlot {
  callId: string;
  toolName: string;
  /** 工具名的显示短语（toolAction 派生，供三态文案拼装） */
  phrase: string;
  /** 执行状态：null=进行中（无 tool/result 配对），true=完成，false=失败 */
  ok: boolean | null;
  /** 结果摘要；仅失败态展示（错误原因） */
  summary: string;
  /** 展示切片（来自 tool/call，短语后紧随 text + link）；null = 无信息 */
  display: ToolDisplay | null;
  /** sessionId（审批 invoke 路由用；与事件同源） */
  sessionId: string;
  /** 审批请求的完整命令（无审批 = null） */
  command: string | null;
  /** 审批事件配对：result 由 approval-result 事件派生（无审批 = null） */
  approval: {
    risk: "low" | "high" | "crit";
    /** 出卡族 reason 码（类别短语映射键；无门信息 = null） */
    ruleId: string | null;
    result: "approved" | "rejected" | null;
    /** 请求身份（M4）：restricted=默认受限 / full=完整权限 */
    privilege: "restricted" | "full";
    /** full 档的一句话理由（restricted 档为 null） */
    justification: string | null;
  } | null;
}

interface Props {
  events: SessionEvent[];
  /**
   * 是否处于进行中轮次（currentEvents）。false = 归档重放（turns）——
   * 归档轮 request 必有配对 result（后端 preflight await 保证），
   * pending 不出现；此开关是双保险（异常数据不渲染交互按钮）。
   */
  interactive?: boolean;
}

export function EventBadge({ events, interactive = false }: Props) {
  // ── 推理内容提取：assistant/chunk(Reasoning) 累积 ──
  const reasoningText = useMemo(() => {
    let text = "";
    for (const e of events) {
      if (e.type === "assistant/chunk" && e.kind === "reasoning") {
        text += e.content;
      }
    }
    return text;
  }, [events]);

  // "正在思考"：推理链未闭合——最后一个事件仍是推理 chunk。
  // 推理结束由后续事件（text chunk / tool/call / turn/end）隐式表达。
  const lastEvent = events[events.length - 1];
  const reasoningActive =
    reasoningText.length > 0 &&
    lastEvent?.type === "assistant/chunk" &&
    lastEvent.kind === "reasoning";

  // ── 工具配对：callId 精确配对（tool/call 开槽，approval/result 事件挂接） ──
  const toolSlots: ToolSlot[] = useMemo(() => {
    const result: ToolSlot[] = [];
    for (const e of events) {
      if (e.type === "tool/call") {
        result.push({
          callId: e.callId,
          toolName: e.toolName,
          phrase: toolAction(e.toolName),
          ok: null,
          summary: "",
          display: e.display ?? null,
          sessionId: e.sessionId,
          command: null,
          approval: null,
        });
      }
      if (e.type === "tool/approval-request") {
        const slot = result.find((s) => s.callId === e.callId);
        if (slot) {
          slot.sessionId = e.sessionId;
          slot.command = e.command;
          slot.approval = {
            risk: e.risk,
            ruleId: e.ruleId,
            result: null,
            privilege: e.privilege,
            justification: e.justification,
          };
        }
        // 无匹配槽（异常时序：request 先于 tool/call 到达）→ 防御性开槽
        if (!slot) {
          result.push({
            callId: e.callId,
            toolName: "run_command",
            phrase: toolAction("run_command"),
            ok: null,
            summary: "",
            display: null,
            sessionId: e.sessionId,
            command: e.command,
            approval: {
              risk: e.risk,
              ruleId: e.ruleId,
              result: null,
              privilege: e.privilege,
              justification: e.justification,
            },
          });
        }
      }
      if (e.type === "tool/approval-result") {
        const ev = e as ToolApprovalResultEvent;
        const slot = result.find((s) => s.callId === e.callId);
        if (slot && slot.approval) {
          // 仅 approved/rejected 会经 IPC 到达（auto-approved/blocked 为
          // log_only，前端不可见）；防御性忽略其余值（不渲染按钮）
          if (ev.decision === "approved" || ev.decision === "rejected") {
            slot.approval.result = ev.decision;
          }
        }
      }
      if (e.type === "tool/result") {
        const pending = result.find((s) => s.callId === e.callId);
        if (pending) {
          pending.ok = e.ok;
          pending.summary = e.summary;
        } else {
          const toolName = e.toolName ?? "?";
          result.push({
            callId: e.callId,
            toolName,
            phrase: toolAction(toolName),
            ok: e.ok,
            summary: e.summary,
            display: null,
            sessionId: e.sessionId,
            command: null,
            approval: null,
          });
        }
      }
    }
    return result;
  }, [events]);

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
  const moodEvents = useMemo(
    () => events.filter((e) => e.type === "mood/event"),
    [events]
  );

  const hasTools = toolSlots.length > 0;
  const toolsActive = toolSlots.some((s) => s.ok === null);
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
    <div className={styles.badge}>
      {/* ── 思考阶段 ── */}
      {reasoningText && (
        <div className={styles.section}>
          <button
            className={styles.toggle}
            onClick={() => setReasoningOpen(!reasoningOpen)}
          >
            <span
              className={`${styles.arrow} ${reasoningOpen ? styles.arrowOpen : ""}`}
            >
              ▸
            </span>
            思考过程
            <span className={reasoningActive ? styles.statusActive : styles.statusDone}>
              {reasoningActive ? "· 正在思考..." : "· ✓ 思考完成"}
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
            onClick={() => setToolsOpen(!toolsOpen)}
          >
            <span
              className={`${styles.arrow} ${toolsOpenEffective ? styles.arrowOpen : ""}`}
            >
              ▸
            </span>
            工具调用
            <span className={toolsActive ? styles.statusActive : styles.statusDone}>
              {toolsActive
                ? "· 正在调用工具..."
                : hasToolFailure
                  ? "· ⚠ 工具调用完成（含失败）"
                  : `· ✓ 工具调用完成${toolSlots.length > 1 ? `（${toolSlots.length}项）` : ""}`}
            </span>
          </button>
          {toolsOpenEffective && (
            <div className={styles.detail}>
              {toolSlots.map((slot, i) => {
                const approval = approvalState(slot);
                // ── 终态优先：tool/result 到达后走既有三态 ──
                if (slot.ok !== null) {
                  return (
                    <div
                      key={slot.callId || i}
                      className={`${styles.toolItem} ${
                        slot.ok ? styles.toolDone : styles.toolFailed
                      }`}
                    >
                      <span>
                        {slot.ok
                          ? `PRTS${slot.phrase}完成`
                          : `PRTS${slot.phrase}失败：${slot.summary}`}
                        {renderDisplaySuffix(slot.display, !slot.ok)}
                      </span>
                    </div>
                  );
                }
                // ── 审批态（进行中且 tool/result 未达） ──
                if (approval === "pending" && interactive) {
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
            onClick={() => setMoodOpen(!moodOpen)}
          >
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
