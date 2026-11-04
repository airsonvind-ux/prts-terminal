// 过程卡（消息流重构 · 第 2 步）
//
// 一张卡、一行摘要、展开后按到达顺序逐行：思考行与工具行混排，不再分
// 「思考过程」「工具调用」两个可折叠区（那是两层折叠）。
//
// 行渲染沿用原来的行样式与文案：工具行的三态（进行中 / 完成 / 失败）、
// 审批等待行、外部资料来源标签都从 ToolSlot 取，不另造一套。
// 审批按钮不在这里——产品界面的确认/拒绝在输入框上方的 Action Dock，
// 卡内只显示「等待确认」这一行。

import { useMemo, useState } from "react";
import { hasDisplay, renderDisplaySuffix } from "./eventDisplay";
import { buildToolSlots } from "./eventSlots";
import type { ToolSlot } from "./eventSlots";
import type { Block, CardSummary } from "./blocks";
import styles from "./EventBadge.module.css";

type CardBlock = Extract<Block, { kind: "card" }>;

/** 收起态摘要：只列非零项；有失败必须露出来（不设阈值）。 */
export function summaryText(summary: CardSummary): string {
  const parts: string[] = [];
  if (summary.reasoning > 0) parts.push(`思考 ${summary.reasoning} 段`);
  if (summary.calls > 0) parts.push(`调用 ${summary.calls} 次`);
  if (summary.failures > 0) parts.push(`${summary.failures} 项失败`);
  return parts.join(" · ");
}

interface Props {
  block: CardBlock;
  /** 进行中轮次（currentEvents）：等待审批时强制展开 */
  interactive: boolean;
  /** 列表行样式（外边距、滚动锚点） */
  className?: string;
  /** 展开/收起：列表据此停止自动跟底 */
  onToggle?: () => void;
}

export function ProcessCard({ block, interactive, className, onToggle }: Props) {
  // 槽位（短语、display、来源、审批态）由本卡自己的事件切片算：
  // 卡与卡之间不共享，呼叫配对也只可能发生在同一张卡里。
  const slots = useMemo(
    () => new Map(buildToolSlots(block.events).map((slot) => [slot.callId, slot])),
    [block.events]
  );
  const [open, setOpen] = useState(false);
  const waiting = interactive && block.rows.some(
    (row) => row.kind === "tool" && row.approval === "pending" && row.status === "running"
  );
  const expanded = open || waiting;
  const summary = summaryText(block.summary);
  const active = block.rows.some(
    (row) => row.kind === "tool" && row.status === "running"
  ) || (block.rows.some((row) => row.kind === "reasoning") && block.rows.length > 0 && interactive);

  return (
    <div
      className={
        className ? `${styles.flowCard} ${className}` : styles.flowCard
      }
      data-block-id={block.id}
      data-process-card
    >
      <button
        type="button"
        className={styles.flowHeader}
        data-focus-inset
        data-event-toggle
        data-event-kind="card"
        aria-expanded={expanded}
        onClick={() => {
          setOpen((v) => !v);
          onToggle?.();
        }}
      >
        <span className={styles.titleGroup}>
          <span className={`${styles.arrow} ${expanded ? styles.arrowOpen : ""}`}>
            ▸
          </span>
          {summary}
          {block.summary.failures > 0 && (
            <span className={styles.flowFailMark} aria-label="有失败项">
              ⚠
            </span>
          )}
          {waiting && <span className={styles.statusActive}>· 等待确认</span>}
          {!waiting && active && interactive && (
            <span className={styles.statusActive}>· 进行中</span>
          )}
        </span>
      </button>

      {expanded && (
        <div className={styles.flowBody}>
          {block.rows.map((row, index) => {
            if (row.kind === "reasoning") {
              return (
                <pre key={`${block.id}-r${index}`} className={styles.flowReasoning}>
                  {row.text}
                </pre>
              );
            }
            const slot = slots.get(row.callId);
            return (
              <ToolLine
                key={`${block.id}-t${row.callId || index}`}
                row={row}
                slot={slot}
                interactive={interactive}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

// ── 工具行：三态 + 审批等待 ──────────────────────────────────────────

function ToolLine({
  row,
  slot,
  interactive,
}: {
  row: Extract<CardBlock["rows"][number], { kind: "tool" }>;
  slot: ToolSlot | undefined;
  interactive: boolean;
}) {
  // 槽位缺失（旧归档里 call 与 result 不成对）→ 用行自身的字段兜底，
  // 不因为拿不到槽位就把这行吞掉。
  const phrase = slot?.phrase ?? row.toolName;
  const summaryText = row.summary ?? slot?.summary ?? "";
  const display = row.display ?? slot?.display ?? null;
  const externalProvenance =
    slot?.provenance?.trust === "external" ? slot.provenance : null;
  const provenanceTag = externalProvenance ? (
    <span
      className={styles.provenanceTag}
      title={`外部资料 · 来源：${externalProvenance.source}${
        externalProvenance.version ? ` · ${externalProvenance.version}` : ""
      }`}
    >
      外部
    </span>
  ) : null;

  if (row.status === "failed") {
    return (
      <div className={`${styles.toolItem} ${styles.toolFailed}`}>
        <span className={styles.toolFailLine} title={summaryText}>
          {`PRTS${phrase}失败：${summaryText}`}
        </span>
        {renderDisplaySuffix(display, true)}
        {provenanceTag}
      </div>
    );
  }

  if (row.status === "ok") {
    return (
      <div className={`${styles.toolItem} ${styles.toolDone}`}>
        <span>
          {`PRTS${phrase}完成`}
          {renderDisplaySuffix(display, false)}
        </span>
        {provenanceTag}
      </div>
    );
  }

  if (row.approval === "pending" && interactive) {
    return (
      <div className={`${styles.toolItem} ${styles.toolApproval}`}>
        <span>PRTS 正在等待你确认{phrase}</span>
        <div className={styles.approvalCategory}>请在输入框上方处理</div>
      </div>
    );
  }

  if (row.approval === "rejected") {
    return (
      <div className={`${styles.toolItem} ${styles.toolFailed}`}>
        <span>{`PRTS${phrase}失败：已拒绝该命令执行`}</span>
      </div>
    );
  }

  if (row.approval === "approved") {
    return (
      <div className={`${styles.toolItem} ${styles.toolRunning}`}>
        <span>
          {`PRTS正在${phrase}`}
          {hasDisplay(display) ? renderDisplaySuffix(display, false) : "..."}
          {"（已确认）"}
        </span>
      </div>
    );
  }

  return (
    <div className={`${styles.toolItem} ${styles.toolRunning}`}>
      <span>
        {`PRTS正在${phrase}`}
        {hasDisplay(display) ? renderDisplaySuffix(display, false) : "..."}
      </span>
    </div>
  );
}
