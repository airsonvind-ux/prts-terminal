// src/projection.ts
// 投影派生纯函数 —— 前端作为后端 session log 投影缓存的派生层
//
// 架构升级 · 模块 1：前端退为 log 的投影缓存。本模块承载从
// SessionEvent 序列派生 LLM 历史（deriveMessages，镜像后端
// session.rs::SessionLog::derive_messages 语义）与展示数据的纯函数。
//
// 全部为纯函数：相同输入永远相同输出，无副作用，便于测试。

import type { Attachment, ChatMessage, CompactionMeta, SessionEvent } from "./types";
import { buildSummaryUserMessage, stripLeadingErrorTurns } from "./projection/compaction";

export {
  SUMMARY_BLOCK_START,
  SUMMARY_BLOCK_END,
  RETAIN_BUDGET_TOKENS,
  NOOP_MIN_FOLD_TURNS,
  NOOP_MIN_FOLD_TOKENS,
  estimateTextTokens,
  estimateTurnTokens,
  computeRetainedFrom,
  buildSummaryUserMessage,
  isErrorOnlyTurn,
  stripLeadingErrorTurns,
  CONTEXT_WARNING_RATIO,
  CONTEXT_HARD_RATIO,
  outputReserveTokens,
  willNextTurnOverflow,
} from "./projection/compaction";

// ── Reminder 块（M1）：build（拼装，sendMessage 用）与 strip（展示剥离）
// 同模块自洽——块格式只在此文件定义：`[prts_reminder]` 独占行起、
// `[/prts_reminder]` 独占行止，条目行 `- {title}：{detail}`。 ──

/** 提醒条目行（维度行或事件行）。 */
export interface ReminderLine {
  title: string;
  detail: string;
}

/**
 * 把提醒条目行组为 `[prts_reminder]` 块（与 stripReminderBlock 互逆）。
 * 空列表 → 空串（调用方不拼）。后端不产块（只给数据）→ 无跨端格式契约。
 *
 * 多行 detail（事件里嵌的条目行 / 长期梗概全文 / 画像全文）续行缩进两格，
 * 保证一条通知的所有行归属同一条、不与其它条目行混淆。
 */
export function buildReminderBlock(entries: ReminderLine[]): string {
  if (entries.length === 0) return "";
  const lines = ["[prts_reminder]"];
  for (const n of entries) {
    const [head, ...rest] = String(n.detail ?? "").split(/\r?\n/);
    lines.push(`- ${n.title}：${head}`);
    for (const line of rest) {
      lines.push(`  ${line}`);
    }
  }
  lines.push("[/prts_reminder]");
  return lines.join("\n");
}

/**
 * 剥离 `[prts_reminder]` 块（与后端约定的块格式兼容；buildReminderBlock 的
 * 互逆）。标记与中间内容整块剥离，不依赖块内格式。
 *
 * 语义（fail closed）：
 * - 无开始标记 → 原样返回
 * - 未闭合块（有开始无结束）→ 从开始标记剥到文本末尾
 * - 闭合块 → 连同块后紧跟的换行（拼装时块尾接 "\n\n"）一并去除，
 *   避免残留空行
 *
 * 真实用途（M1 修复版）：user 消息含块版本是**冻结历史的一部分**（归档/
 * 派生链保留块——deriveMessages 不剥离），仅**展示路径**剥离（MessageList
 * 用户气泡；firstUserContent 等 UI 用途派生）——模型可见、用户不可见。
 */
export function stripReminderBlock(content: string): string {
  const START = "[prts_reminder]";
  const END = "[/prts_reminder]";
  const start = content.indexOf(START);
  if (start === -1) return content;
  const end = content.indexOf(END, start);
  if (end === -1) {
    // 未闭合块：从开始标记剥到文本末尾（fail closed）
    return content.slice(0, start);
  }
  // 整块剥离；块后紧跟的换行一并去除（\r\n 兼容）
  const rest = content.slice(end + END.length).replace(/^[\r\n]+/, "");
  return content.slice(0, start) + rest;
}

// ── 会话默认标题（自动标题功能的哨兵 / 首问素材） ──

/**
 * 默认标题规则：首问 trim + 前 12 字符（沿用既有 deriveTitle 规则）。
 *
 * 自动标题的「默认标题哨兵」与后端 CAS 比较共用此结果，后端不复算
 * ——彻底避免 JS(UTF-16 slice) vs Rust 的截取差异。
 */
export function defaultTitleFromInput(input: string): string {
  const t = input.trim();
  return t.length > 12 ? t.slice(0, 12) : t;
}

/**
 * 从 user 消息内容重算默认标题（写盘侧复检用）。
 *
 * 归档/预置的 user 事件可能含 `[prts_reminder]` 块（仅当发送时有环境通知）
 * → 先 `stripReminderBlock` 再套规则。与创建时
 * `defaultTitleFromInput(原始 input)` 精确相等（块剥离为无损可逆）。
 */
export function defaultTitleForContent(content: string): string {
  return defaultTitleFromInput(stripReminderBlock(content));
}

// ── 轮内多段切分（插话设计定稿）──

/**
 * 按 user/message 边界把一轮事件序列切分为消息段。
 *
 * 轮内插话注入后一轮可含多问答段（u1 → 段A → u2 → 段B → …）；展示层按段
 * 渲染：user 段各成气泡、assistant 段独立闭合（段内最后一个 assistant/message
 * 为该段权威聚合；无则从 chunks 累积——中途段由 dispatch 每段 Completed 出口
 * emit assistant_message，故归档轮各 assistant 段均含权威聚合）。
 *
 * turn/aborted、error 等轮级事件随所在段归属（展示层各自特判）。
 */
export type TurnSegment =
  | { kind: "user"; content: string; attachments?: Attachment[] }
  | { kind: "assistant"; events: SessionEvent[] };

export function splitTurnByUserBoundary(events: SessionEvent[]): TurnSegment[] {
  const segments: TurnSegment[] = [];
  let current: SessionEvent[] = [];
  const flush = () => {
    if (current.length > 0) {
      segments.push({ kind: "assistant", events: current });
      current = [];
    }
  };
  for (const e of events) {
    if (e.type === "user/message") {
      flush();
      const userSeg: TurnSegment = { kind: "user", content: e.content };
      if (e.attachments && e.attachments.length > 0) userSeg.attachments = e.attachments;
      segments.push(userSeg);
    } else {
      current.push(e);
    }
  }
  flush();
  return segments;
}

// ── LLM 历史派生（播种用） ──

/**
 * 从轮次事件序列派生 LLM 可见历史消息（镜像后端 derive_messages）。
 *
 * 派生规则（与后端一致）：
 * - user/message → user 消息
 * - assistant/chunk 按 kind 累积 text / reasoning
 * - tool/call 收集工具调用（同轮多次调用合并进同一条 assistant 消息）
 * - tool/result → 先冲刷挂起的 assistant 消息，再追加 tool 消息
 * - assistant/message 不参与派生（与 chunk 序列同源，避免双计数）
 * - error 事件跳过（前端 UI 元素，不进 LLM 上下文）
 *
 * @param turns 对话的轮次事件序列（投影缓存）
 * @returns LLM 可见历史（error 已过滤；空 assistant 已跳过）
 *
 * user/message 分支**保留 [prts_reminder] 块**（M1 修复版：含块 user 消息是
 * 冻结历史的一部分，派生链必须与归档/后端 log 逐字节一致——缓存前缀递增
 * 命中的保证；剥离仅限展示路径，见 stripReminderBlock）。
 */
export function deriveMessages(turns: SessionEvent[][]): ChatMessage[] {
  const out: ChatMessage[] = [];
  for (const turn of turns) {
    let textBuf = "";
    let reasoningBuf = "";
    const toolCalls: NonNullable<ChatMessage["toolCalls"]> = [];

    const flushAssistant = () => {
      const hasContent = textBuf.length > 0 || reasoningBuf.length > 0;
      if (!hasContent && toolCalls.length === 0) return;
      const msg: ChatMessage = {
        role: "assistant",
        content: textBuf,
        reasoningContent: reasoningBuf.length > 0 ? reasoningBuf : undefined,
      };
      if (toolCalls.length > 0) {
        msg.toolCalls = [...toolCalls];
      }
      out.push(msg);
      textBuf = "";
      reasoningBuf = "";
      toolCalls.length = 0;
    };

    for (const e of turn) {
      switch (e.type) {
        case "user/message": {
          flushAssistant();
          // 保留块：含块 user 消息是冻结历史的一部分（派生链与归档一致）。
          // 附件随消息派生（供请求侧物化，以及后端 derive 时注入 `[附件：名]`）。
          const userMsg: ChatMessage = { role: "user", content: e.content };
          if (e.attachments && e.attachments.length > 0) {
            userMsg.attachments = e.attachments;
          }
          out.push(userMsg);
          break;
        }
        case "assistant/chunk":
          if (e.kind === "text") textBuf += e.rawContent ?? e.content;
          else reasoningBuf += e.content;
          break;
        case "tool/call":
          toolCalls.push({
            id: e.callId,
            callType: "function",
            function: { name: e.toolName, arguments: e.params },
          });
          break;
        case "tool/result":
          flushAssistant();
          out.push({ role: "tool", content: e.summary, toolCallId: e.callId });
          break;
        // assistant/message 与 chunk 同源（聚合快照），不参与派生；
        // error / turn/step 边界 / request/header 不派生
        default:
          break;
      }
    }
    flushAssistant();
  }
  return out;
}

// ── 展示派生 ──

/** assistant 消息的展示聚合（内容 + 推理链） */
export interface AssistantProjection {
  content: string;
  reasoningContent: string;
}

/**
 * 从单轮事件序列派生 assistant 展示消息。
 *
 * 优先取最后一个 assistant/message（归档后的聚合快照）；
 * 流式中（无 assistant/message）从 chunks 累积。
 * 无内容（空回答）返回 null。
 */
export function deriveAssistantFromEvents(
  events: SessionEvent[]
): AssistantProjection | null {
  // 归档轮：assistant/message 是权威聚合
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (e.type === "assistant/message") {
      if (e.content.length === 0) return null;
      return {
        content: e.content,
        reasoningContent: e.reasoningContent ?? "",
      };
    }
  }
  // 流式中：chunks 累积
  let content = "";
  let reasoning = "";
  for (const e of events) {
    if (e.type === "assistant/chunk") {
      if (e.kind === "text") content += e.content;
      else reasoning += e.content;
    }
  }
  if (content.length === 0 && reasoning.length === 0) return null;
  return { content, reasoningContent: reasoning };
}

/** 文本段（分段回溯弱化展示用） */
export interface TextSegment {
  content: string;
  /** true = 被 tool/call 闭合的过渡文本（弱化显示）；false = 最终文本（正常显示） */
  transition: boolean;
}

/**
 * 从单轮事件序列派生 assistant 文本段（分段回溯弱化）。
 *
 * 文本段性质由「闭合方式」决定：
 * - 归档轮（存在 assistant/message）：取最后一个 assistant/message，
 *   内容非空 → 单段 transition=false；空 → null。
 *   归档后过渡文本消失（由 assistant/message 最终文本替换，既定行为）。
 * - 流式中：assistant/chunk(kind=text) 累积到当前段；tool/call 到达时
 *   若当前段非空 → 回溯标记 transition=true、push 进结果并开新段
 *   （新段 transition=false）；遍历结束把最后未闭合段 push（transition=false）。
 * - 总内容为空 → null。
 */
export function deriveTextSegments(
  events: SessionEvent[]
): TextSegment[] | null {
  // 归档轮：assistant/message 是权威聚合 → 单段正常显示
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (e.type === "assistant/message") {
      if (e.content.length === 0) return null;
      return [{ content: e.content, transition: false }];
    }
  }
  // 流式中：chunks 累积；tool/call 回溯闭合当前段（空段不产生）
  const segments: TextSegment[] = [];
  let buf = "";
  for (const e of events) {
    if (e.type === "assistant/chunk" && e.kind === "text") {
      buf += e.content;
    } else if (e.type === "tool/call" && buf.length > 0) {
      segments.push({ content: buf, transition: true });
      buf = "";
    }
  }
  if (buf.length > 0) {
    segments.push({ content: buf, transition: false });
  }
  if (segments.length === 0) return null;
  return segments;
}

/** 事件是否为展示用元事件（推理/工具/审批/情绪/错误，渲染为卡片或气泡） */
export function isMetaEvent(e: SessionEvent): boolean {
  return (
    e.type === "assistant/chunk" ||
    e.type === "tool/call" ||
    e.type === "tool/result" ||
    e.type === "tool/approval-request" ||
    e.type === "tool/approval-result" ||
    e.type === "mood/event"
  );
}

/** 从轮次序列取第一条 user 消息内容（标题派生用；UI 用途 → 剥离块） */
export function firstUserContent(turns: SessionEvent[][]): string | null {
  for (const turn of turns) {
    for (const e of turn) {
      if (e.type === "user/message") return stripReminderBlock(e.content);
    }
  }
  return null;
}

// ═══════════════════════════════════════════════════════
// 上下文压缩（模块 D · 上下文衰减）——派生层截断与摘要消息构造
// ═══════════════════════════════════════════════════════
//
// 压缩 = 前端发起的一次对话级状态转换。turns 全量保留（回看资产，永不回灌
// LLM）；`conv.compaction` 记录折叠边界与摘要。本组纯函数负责：
//   - 估算（轮/文本 token，0.3 ascii + 0.6 cjk 口径，与后端 estimate_tokens
//     同构——仅供 cut 点选择与 noop 预判，权威用量以 provider usage 为准）
//   - 保留段截断（deriveActiveHistory = 摘要消息 + 保留段派生）
//   - 摘要消息定界块构造（模型可见、冻结——格式固定保证压缩后前缀稳定）

/**
 * 派生「发给 LLM 的历史」：压缩后 = 摘要消息（首条）+ 保留段派生；未压缩 =
 * 全量派生（与既有 deriveMessages 输出一致）。sendMessage 的唯一历史来源。
 *
 * 首部错误轮先剥离：错误轮本身不进 LLM 上下文（error 事件已过滤），但其
 * user 消息会残留成悬空历史；首轮 401 后重新配置再对话时，模型不应看到那段
 * 失败的 user 消息。
 */
export function deriveActiveHistory(
  turns: SessionEvent[][],
  compaction?: CompactionMeta
): ChatMessage[] {
  if (!compaction || compaction.retainedFrom <= 0) {
    return deriveMessages(stripLeadingErrorTurns(turns));
  }
  const kept = turns.slice(compaction.retainedFrom);
  return [
    buildSummaryUserMessage(compaction),
    ...deriveMessages(stripLeadingErrorTurns(kept)),
  ];
}

