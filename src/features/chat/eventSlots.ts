// 工具槽位派生：tool/call 开槽，approval/result 事件按 callId 挂接。
// 纯函数，不依赖 React 或组件状态。

import type {
  SessionEvent,
  ToolApprovalResultEvent,
  ToolDisplay,
  ToolProvenance,
} from "../../types";
import { toolAction } from "../../toolDisplay";

export interface ToolSlot {
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
  /** 结果来源（来自 tool/result；旧归档/未声明工具为 null） */
  provenance: ToolProvenance | null;
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

export function buildToolSlots(events: SessionEvent[]): ToolSlot[] {
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
        provenance: null,
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
          provenance: null,
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
        pending.provenance = e.provenance ?? null;
      } else {
        const toolName = e.toolName ?? "?";
        result.push({
          callId: e.callId,
          toolName,
          phrase: toolAction(toolName),
          ok: e.ok,
          summary: e.summary,
          display: null,
          provenance: e.provenance ?? null,
          sessionId: e.sessionId,
          command: null,
          approval: null,
        });
      }
    }
  }
  return result;
}
