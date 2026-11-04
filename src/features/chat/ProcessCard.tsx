// 过程卡（消息流重构 · 面板卡）
//
// 一张卡、一个标题行、展开后按到达顺序逐行。标题行是唯一的收起态内容：
// 箭头 + 「思考与工具」+ 状态 + 右侧数量（思考几段、调用几次、耗时）。
//
// 行文案沿用原来的措辞（PRTS正在… / …完成 / …失败：摘要），状态改由左侧圆点
// 承载：完成绿、进行中金并呼吸、失败红。思考行和工具行同一套骨架，只是点开
// 才显示原文。耗时只在拿得到到达时刻时显示——归档事件没有时间戳，那一侧
// 就留空，不猜一个数出来。
//
// 审批按钮不在这里：确认/拒绝在输入框上方的 Action Dock，卡内只显示等待行。
// 等待审批时这一张卡强制展开（逐卡判断，不是全局布尔）。

import { useMemo, useState } from "react";
import { hasDisplay, renderDisplaySuffix } from "./eventDisplay";
import { buildToolSlots } from "./eventSlots";
import type { ToolSlot } from "./eventSlots";
import type { Block, ProcessRow } from "./blocks";
import { formatElapsed } from "./eventTiming";
import styles from "./ProcessCard.module.css";

type CardBlock = Extract<Block, { kind: "card" }>;
type ToolRow = Extract<ProcessRow, { kind: "tool" }>;

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
  const waiting =
    interactive &&
    block.rows.some(
      (row) =>
        row.kind === "tool" &&
        row.approval === "pending" &&
        row.status === "running"
    );
  const expanded = open || waiting;

  // 只有「流式中的最后一张卡」还在进行；归档轮的卡一律按完成画。
  const running = block.open && interactive;
  const failures = block.summary.failures;
  const elapsed = formatElapsed(block.startedAt, block.endedAt);
  // 收起态的数量信息：思考几段、调用几次、耗时多久。只列非零项——
  // 纯思考的卡不再只剩一句「✓ 完成」，失败项数由左侧状态位承担。
  const usageParts: string[] = [];
  if (block.summary.reasoning > 0) usageParts.push(`思考 ${block.summary.reasoning} 段`);
  if (block.summary.calls > 0) usageParts.push(`${block.summary.calls} 调用`);
  if (elapsed) usageParts.push(elapsed);
  const usage = usageParts.length > 0 ? usageParts.join(" · ") : null;

  // 归档轮里没等到结果的调用 / 没闭合的思考 = 这一轮被打断了。
  // 进行中的轮次不判这个：那时「还没回来」本来就是常态，标题写进行中。
  const danglingCalls = block.summary.danglingCalls;
  const incomplete = !running && (danglingCalls > 0 || block.summary.reasoningDangling);
  const incompleteText =
    danglingCalls > 0
      ? `· 工具调用未完成${danglingCalls > 1 ? `（${danglingCalls} 项）` : ""}`
      : "· 思考未完成";

  return (
    <section
      className={className ? `${styles.card} ${className}` : styles.card}
      data-block-id={block.id}
      data-process-card
    >
      <button
        type="button"
        className={styles.header}
        data-focus-inset
        data-event-toggle
        data-event-kind="card"
        aria-expanded={expanded}
        onClick={() => {
          setOpen((value) => !value);
          onToggle?.();
        }}
      >
        <span
          className={`${styles.arrow} ${expanded ? styles.arrowOpen : ""}`}
          aria-hidden
        >
          ▸
        </span>
        <span className={styles.title}>思考与工具</span>
        {/* 状态先说这一轮怎么了，失败项数放在括号里补充：
            「完成（含 2 项失败）」而不是光秃秃的「✕ 2 项失败」——后者读起来
            像是这轮只调了两次、而且两次都失败了。 */}
        <span className={styles.status}>
          <span
            className={
              running
                ? styles.statusRunning
                : incomplete
                  ? styles.statusIncomplete
                  : styles.statusDone
            }
          >
            {running ? "· 进行中" : incomplete ? incompleteText : "✓ 完成"}
          </span>
          {failures > 0 && (
            <span className={styles.statusFailed} aria-label="有失败项">
              （含 {failures} 项失败）
            </span>
          )}
        </span>
        {usage && <span className={styles.usage}>{usage}</span>}
      </button>

      {expanded && (
        <div className={styles.body}>
          {renderRows(block.rows, slots, interactive, running)}
        </div>
      )}
    </section>
  );
}

/**
 * 按到达顺序出行：连续的思考行合成一个「思考」子块（卡内第二层，默认收起），
 * 工具行逐行摊开。光标只挂在最后一个可见元素上。
 */
function renderRows(
  rows: ProcessRow[],
  slots: Map<string, ToolSlot>,
  interactive: boolean,
  running: boolean
) {
  const nodes: React.ReactNode[] = [];
  let index = 0;
  let key = 0;

  while (index < rows.length) {
    const row = rows[index];
    if (row.kind === "reasoning") {
      let text = "";
      while (index < rows.length && rows[index].kind === "reasoning") {
        text += (rows[index] as Extract<ProcessRow, { kind: "reasoning" }>).text;
        index += 1;
      }
      nodes.push(
        <ReasoningRun
          key={`reasoning-${key++}`}
          text={text}
          streaming={running && index === rows.length}
          cursor={running && index === rows.length}
        />
      );
      continue;
    }

    nodes.push(
      <ToolLine
        key={`tool-${row.callId || key++}`}
        row={row}
        slot={slots.get(row.callId)}
        interactive={interactive}
        cursor={running && index === rows.length - 1}
      />
    );
    index += 1;
  }

  return nodes;
}

// ── 思考子块：卡内第二层，默认收起 ──────────────────────────────────

function ReasoningRun({
  text,
  streaming,
  cursor,
}: {
  text: string;
  /** 这段思考还在生成：文案用「正在」，圆点呼吸 */
  streaming: boolean;
  cursor: boolean;
}) {
  const [open, setOpen] = useState(false);
  // 与工具行同一套骨架：圆点 + 一句 PRTS 的话 + 右侧展开箭头。
  // 之前这里只有一个孤零零的「思考」，跟下面「PRTS查询干员完成」不在
  // 同一个语域里，读的人分不清它是标题还是状态。
  return (
    <div className={styles.reasoning}>
      <button
        type="button"
        className={`${styles.line} ${styles.reasoningToggle}`}
        data-event-kind="reasoning"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <span
          className={`${styles.dot} ${streaming ? styles.dotRunning : styles.dotDone}`}
          aria-hidden
        />
        <span className={styles.lineText}>
          {streaming ? "PRTS正在思考..." : "PRTS思考了一阵"}
        </span>
        {cursor && <span className={styles.cursor} />}
        <span
          className={`${styles.arrow} ${styles.reasoningArrow} ${open ? styles.arrowOpen : ""}`}
          aria-hidden
        >
          ▸
        </span>
      </button>
      {open && <div className={styles.reasoningBody}>{text}</div>}
    </div>
  );
}

// ── 工具行：圆点状态 + 文案 + 右侧耗时 ──────────────────────────────

function ToolLine({
  row,
  slot,
  interactive,
  cursor,
}: {
  row: ToolRow;
  slot: ToolSlot | undefined;
  interactive: boolean;
  cursor: boolean;
}) {
  // 槽位缺失（旧归档里 call 与 result 不成对）→ 用行自身的字段兜底，
  // 不因为拿不到槽位就把这行吞掉。
  const phrase = slot?.phrase ?? row.toolName;
  const summary = row.summary ?? slot?.summary ?? "";
  const display = row.display ?? slot?.display ?? null;
  const elapsed = formatElapsed(row.startedAt, row.endedAt);
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

  if (row.status === "failed" || row.approval === "rejected") {
    const text =
      row.approval === "rejected"
        ? `PRTS${phrase}失败：已拒绝该命令执行`
        : `PRTS${phrase}失败：${summary}`;
    return (
      <div
        className={`${styles.line} ${styles.lineFailed}`}
        data-failed-line
      >
        <span className={`${styles.dot} ${styles.dotFailed}`} aria-hidden />
        <span
          className={styles.lineText}
          title={row.approval === "rejected" ? undefined : summary}
        >
          {text}
          {row.approval === "rejected" ? null : renderDisplaySuffix(display, true)}
        </span>
        {provenanceTag}
        {cursor && <span className={styles.cursor} />}
        {elapsed && <span className={styles.lineTime}>{elapsed}</span>}
      </div>
    );
  }

  if (row.status === "ok") {
    return (
      <div className={styles.line}>
        <span className={`${styles.dot} ${styles.dotDone}`} aria-hidden />
        <span className={styles.lineText}>
          {`PRTS${phrase}完成`}
          {renderDisplaySuffix(display, false)}
        </span>
        {provenanceTag}
        {cursor && <span className={styles.cursor} />}
        {elapsed && <span className={styles.lineTime}>{elapsed}</span>}
      </div>
    );
  }

  if (row.approval === "pending" && interactive) {
    return (
      <>
        <div className={styles.line}>
          <span className={`${styles.dot} ${styles.dotRunning}`} aria-hidden />
          <span className={styles.lineText}>
            PRTS 正在等待你确认{phrase}
          </span>
          {cursor && <span className={styles.cursor} />}
        </div>
        <div className={styles.approvalNote}>请在输入框上方处理</div>
      </>
    );
  }

  return (
    <div className={styles.line}>
      <span
        className={`${styles.dot} ${styles.dotRunning}${interactive ? "" : ` ${styles.dotStill}`}`}
        aria-hidden
      />
      <span className={styles.lineText}>
        {row.approval === "pending" && !interactive
          ? `PRTS正在${phrase}...（等待确认）`
          : `PRTS正在${phrase}`}
        {row.approval === "pending" && !interactive
          ? null
          : hasDisplay(display)
            ? renderDisplaySuffix(display, false)
            : "..."}
        {row.approval === "approved" ? "（已确认）" : ""}
      </span>
      {provenanceTag}
      {cursor && <span className={styles.cursor} />}
      {elapsed && <span className={styles.lineTime}>{elapsed}</span>}
    </div>
  );
}
