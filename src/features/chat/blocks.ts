// 轮次 → 有序列块（消息流重构 · 第 1 步）。
//
// 纯函数，不依赖 React、不读写 store：输入一轮事件、输出渲染用的块序列。
// `messageItems` 把块逐个翻成列表条目（一块一条），MessageRow 按条目渲染。
//
// 切点只有两个（外加轮次边界）：
// - 文本：文本 chunk 累积之后出现任何非文本事件 → 文本块闭合；
// - 视图：`tool/result` 携带视图 → 当前卡补上结果行后闭合，视图块紧随其后。
//
// 为什么视图切点在 result 而不是 call：工具槽位按 callId 把 call 与 result
// 配成同一行，完成/失败状态取自 result；在 call 处闭合会把一行劈成两块。
//
// 行分类不复用 `isMetaEvent`：它把 text 与 reasoning 的 chunk 都算 meta，
// 拿来分行会把每个 text chunk 变成一行。

import type {
  MoodEvent,
  SessionEvent,
  ToolCallEvent,
  ToolDisplay,
  ToolResultEvent,
  ToolResultView,
} from "../../types";
import { eventTime } from "./eventTiming";

/** 过程卡内的一行。 */
export type ProcessRow =
  | { kind: "reasoning"; text: string }
  | {
      kind: "tool";
      callId: string;
      toolName: string;
      /** 作者声明的调用短语（插件 action / 内置声明），缺省表示没有 */
      action?: string;
      /** 是否插件工具（决定短语保底走哪一级） */
      pluginTool: boolean;
      display?: ToolDisplay;
      status: "running" | "ok" | "failed";
      /** 结果摘要（status 非 running 时才有） */
      summary?: string;
      /** 审批态：等待确认 / 已确认 / 已拒绝 */
      approval?: "pending" | "approved" | "rejected";
      /** 由哪张视图发起（视图交互的 call 原语）；模型发起的调用没有这个字段 */
      byView?: string;
      /** 事件到达时刻（实时轮才有；工具行右侧耗时用） */
      startedAt?: number;
      endedAt?: number;
    };

/** 收起态摘要的计数。 */
export interface CardSummary {
  /** 思考段数（每段 = 一次连续的 reasoning 流） */
  reasoning: number;
  /** 调用次数 */
  calls: number;
  /** 失败项数（status = failed） */
  failures: number;
  /**
   * 悬空的调用数：发出了 `tool/call` 却没有配对 `tool/result`。
   * 进程被杀、崩溃、会话被清理都会留下这种行——归档回放里它不能看起来
   * 和正常完成的一轮一样。
   */
  danglingCalls: number;
  /** 轮末还有未闭合的思考：模型写到一半被打断 */
  reasoningDangling: boolean;
}

export type Block =
  | {
      kind: "card";
      id: string;
      rows: ProcessRow[];
      summary: CardSummary;
      /**
       * 原始事件切片。卡内的工具槽位（短语、来源、审批态）由卡自己算，
       * 卡与卡之间不共享配对状态。
       */
      events: SessionEvent[];
      /**
       * 收尾时这张卡仍在累积（流式中的最后一张卡）。
       * 标题行据此在「· 进行中」与「✓ 完成」之间切换；归档轮的卡一律为 false。
       */
      open: boolean;
      /** 首末事件的到达时刻（实时轮才有；标题行右侧耗时用） */
      startedAt?: number;
      endedAt?: number;
    }
  | { kind: "text"; id: string; content: string; transition: boolean }
  // 视图块：随 tool/result 到达，独立于过程卡，按到达顺序与文本交错
  | { kind: "view"; id: string; view: ToolResultView }
  // 情绪卡独立成块：它是独立样式、独立展开的东西，塞进过程卡会改变可见性
  | { kind: "mood"; id: string; events: SessionEvent[] };

/**
 * 最后一次 `assistant/reset` 之后的下标。
 *
 * 与 `projection.ts` 的同名逻辑一致：reset 撤回半成品，撤回之前的输出不该再渲染。
 * reset 只会在尚未出现工具调用时发生（见后端 `call_llm_with_retry` 的注释），
 * 所以整段切片是安全的。
 */
function lastResetIndex(events: SessionEvent[]): number {
  for (let i = events.length - 1; i >= 0; i--) {
    if (events[i].type === "assistant/reset") return i + 1;
  }
  return 0;
}

/** 结果事件上的视图载荷；缺省表示这个工具没有视图。 */
function viewOf(result: ToolResultEvent): ToolResultView | undefined {
  return result.view;
}

/**
 * 本轮是否有 chunk 序列可重建。
 *
 * 规格写的是「有 text chunk 就用 chunk 重建」，这里放宽到「有任何 chunk」：
 * 只推理、没有正文的轮（模型直接调工具、或调用中途失败）在旧规则下会掉进
 * 回退分支，思考行与审批状态一起丢掉；而回退分支真正要服务的只是「早于
 * chunk 记录的旧归档」。有 text chunk 必然有 chunk，所以放宽是超集，不违背原意。
 */
function hasChunk(events: SessionEvent[]): boolean {
  return events.some((e) => e.type === "assistant/chunk" && e.content.length > 0);
}

function toolRowOf(call: ToolCallEvent): ProcessRow {
  return {
    kind: "tool",
    callId: call.callId,
    toolName: call.toolName,
    action: call.action,
    pluginTool: call.pluginTool === true,
    display: call.display,
    status: "running",
    startedAt: eventTime(call),
    ...(call.byView ? { byView: call.byView } : {}),
  };
}

/** 卡的首末到达时刻（事件没有时刻时返回空对象，展示层就不显示耗时）。 */
function cardWindow(events: SessionEvent[]): {
  startedAt?: number;
  endedAt?: number;
} {
  let startedAt: number | undefined;
  let endedAt: number | undefined;
  for (const event of events) {
    const at = eventTime(event);
    if (at === undefined) continue;
    if (startedAt === undefined) startedAt = at;
    endedAt = at;
  }
  return { startedAt, endedAt };
}

function summarize(rows: ProcessRow[], reasoningDangling = false): CardSummary {
  let reasoning = 0;
  let calls = 0;
  let failures = 0;
  let danglingCalls = 0;
  for (const row of rows) {
    if (row.kind === "reasoning") {
      reasoning += 1;
      continue;
    }
    calls += 1;
    if (row.status === "failed") failures += 1;
    // 没有 result 的行停在 running：这就是中止留下的悬空项
    if (row.status === "running") danglingCalls += 1;
  }
  return { reasoning, calls, failures, danglingCalls, reasoningDangling };
}

/**
 * 无 chunk 的旧会话整轮回退：等于重构前的外观（过程卡 + 最终文本）。
 *
 * 触发条件是「本轮一条 text chunk 都没有」——早于 chunk 记录的归档、或被外部
 * 工具截断的日志。回退到 `assistant/message` 的聚合文本，不白屏。
 */
function legacyBlocks(events: SessionEvent[], turnIndex: number): Block[] {
  const blocks: Block[] = [];
  const rows: ProcessRow[] = [];
  const cardEvents: SessionEvent[] = [];

  const findRow = (callId: string) =>
    rows.find(
      (candidate): candidate is Extract<ProcessRow, { kind: "tool" }> =>
        candidate.kind === "tool" && candidate.callId === callId
    );

  for (const event of events) {
    if (event.type === "tool/call") {
      rows.push(toolRowOf(event));
      cardEvents.push(event);
    } else if (event.type === "tool/result") {
      cardEvents.push(event);
      const row = findRow(event.callId);
      if (row) {
        row.status = event.ok ? "ok" : "failed";
        row.summary = event.summary;
      }
    } else if (event.type === "tool/approval-request") {
      // 旧归档也要留得住审批历史：没有这两行，被拦下/被拒绝的调用会显示成
      // 一次普通的悬空调用，看不出当时在等人。
      cardEvents.push(event);
      const row = findRow(event.callId);
      if (row) row.approval = "pending";
    } else if (event.type === "tool/approval-result") {
      cardEvents.push(event);
      const row = findRow(event.callId);
      if (row) {
        row.approval = event.decision === "approved" ? "approved" : "rejected";
      }
    }
  }

  if (rows.length > 0) {
    blocks.push({
      kind: "card",
      id: `t${turnIndex}-b${blocks.length}`,
      rows,
      summary: summarize(rows),
      events: cardEvents,
      open: false,
      ...cardWindow(cardEvents),
    });
  }

  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i];
    if (event.type !== "assistant/message") continue;
    if (event.content.length > 0) {
      blocks.push({
        kind: "text",
        id: `t${turnIndex}-b${blocks.length}`,
        content: event.content,
        transition: false,
      });
    }
    break;
  }
  return blocks;
}

/**
 * 一轮事件 → 块序列。
 *
 * @param events 单轮事件（顺序即到达顺序）
 * @param turnIndex 轮序号，只用于生成稳定块 id（`t0-b1`）
 * @param streaming 该段是否还在流式输出。流式段不用旧会话回退：第一句还没
 *   说、只发了工具调用时，回退路径会把正在跑的卡画成「未完成」的归档卡。
 */
export function splitTurn(events: SessionEvent[], turnIndex = 0, streaming = false): Block[] {
  const visible = events.slice(lastResetIndex(events));
  if (!streaming && !hasChunk(visible)) return legacyBlocks(visible, turnIndex);

  const blocks: Block[] = [];
  let rows: ProcessRow[] = [];
  let cardEvents: SessionEvent[] = [];
  let reasoning = "";
  let text = "";

  const nextId = (): string => `t${turnIndex}-b${blocks.length}`;

  const flushReasoning = (): void => {
    // 空白推理（模型只吐了换行）不成段
    if (reasoning.trim().length === 0) {
      reasoning = "";
      return;
    }
    rows.push({ kind: "reasoning", text: reasoning });
    reasoning = "";
  };

  const closeCard = (open = false, reasoningDangling = false): void => {
    // 轮末未闭合的思考也要算进摘要，先记下来再 flush
    const dangling = reasoningDangling && reasoning.trim().length > 0;
    flushReasoning();
    if (rows.length === 0) return;
    blocks.push({
      kind: "card",
      id: nextId(),
      rows,
      summary: summarize(rows, dangling),
      events: cardEvents,
      open,
      ...cardWindow(cardEvents),
    });
    rows = [];
    cardEvents = [];
  };

  const closeText = (transition: boolean): void => {
    if (text.length === 0) return;
    blocks.push({ kind: "text", id: nextId(), content: text, transition });
    text = "";
  };

  const toolRow = (callId: string): Extract<ProcessRow, { kind: "tool" }> | undefined =>
    rows.find(
      (row): row is Extract<ProcessRow, { kind: "tool" }> =>
        row.kind === "tool" && row.callId === callId
    );

  for (const event of visible) {
    switch (event.type) {
      case "assistant/chunk":
        if (event.kind === "text") {
          // 文本开始 = 之前的卡闭合；文本本身成为独立块
          flushReasoning();
          closeCard();
          text += event.content;
        } else {
          // 思考紧跟在文本之后属于新一段：先把文本块收掉
          closeText(true);
          reasoning += event.content;
        }
        break;

      case "tool/call":
        closeText(true);
        flushReasoning();
        cardEvents.push(event);
        rows.push(toolRowOf(event));
        break;

      case "tool/result": {
        closeText(true);
        cardEvents.push(event);
        const row = toolRow(event.callId);
        if (row) {
          row.status = event.ok ? "ok" : "failed";
          row.summary = event.summary;
          row.endedAt = eventTime(event);
        }
        // 视图随结果到达：结果行留在本卡，卡就此闭合，视图紧随其后
        const view = viewOf(event);
        if (view !== undefined) {
          closeCard();
          blocks.push({ kind: "view", id: nextId(), view });
        }
        break;
      }

      case "tool/approval-request": {
        closeText(true);
        flushReasoning();
        cardEvents.push(event);
        const row = toolRow(event.callId);
        // 审批请求只会跟在 tool/call 后面；找不到行说明日志残缺，忽略即可
        if (row) row.approval = "pending";
        break;
      }

      case "tool/approval-result": {
        cardEvents.push(event);
        const row = toolRow(event.callId);
        if (row) {
          row.approval =
            event.decision === "rejected" || event.decision === "blocked"
              ? "rejected"
              : "approved";
        }
        break;
      }

      case "mood/event": {
        closeText(true);
        closeCard();
        blocks.push({
          kind: "mood",
          id: nextId(),
          events: [event as MoodEvent],
        });
        break;
      }

      default:
        break;
    }
  }

  // 轮次边界：收尾。文本块在轮尾闭合 = 最终文本（不弱化）；
  // 还挂着的那张卡是「流式中最后一张」，标 open 让标题行显示进行中。
  // 末尾仍在累积的思考是「没写完」：记进摘要，归档回放里才看得见。
  closeCard(true, true);
  closeText(false);
  return blocks;
}
