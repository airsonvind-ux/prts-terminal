// src/moodTag.ts
// 情绪标签解析器（MOOD 草案 3）
//
// 标签格式：`【mood:偏向(:强度)(:心声)】`，写在正文最前。
//   【mood:高兴】                        没有强度、没有心声
//   【mood:高兴:2】                      只有强度
//   【mood:高兴:2:终于对上了，这张图好认。】  强度 + 心声
//   【mood:不耐烦:还没完，先别催。】        自由偏向（无强度）+ 心声
//
// 括号内按冒号分字段：偏向 / 可选强度 / 可选心声。
//   第 2 字段是数字 1–3 → 强度，其后（第 3 段起）是心声；
//   不是数字 → 没有强度，第 2 段起就是心声。
// 心声里可以有冒号，只要不含 `】`。
//
// 两条通道拆开：
//   - 偏向：自由词。三个已知词（及近义词）归一后驱动体态动画；其余只决定色相。
//   - 心声：括号内的那句话，可省略（省略时展示层回落到旁白）。
//
// 为什么心声在括号内：括号外永远是正文。否则「有没有写心声」只能靠
// 「标签后到行尾还有没有字」去猜，旧对话里标签后的正文会被当成心声吞掉。
//
// 终止：括号闭合即块结束；未闭合时流式扣留，遇换行或超界整段放行当正文，
// 绝不吞内容。只认段首，代码块与举例上下文不误伤。
//
// 失败一律向「保留内容」倒：任何判定不清的情况，文字都回到正文，不静默吞掉。
// 展示层剥离的是 chunk 的 content，模型原文留在 rawContent 里，下一轮上下文
// 里模型仍看得到自己写过的 MOOD。

import type { Mood, MoodIntensity } from "./types";

export type KnownMood = Exclude<Mood, "平静">;

/** 已知偏向 + 近义词 → 归一后的情绪（命中的才驱动动画） */
export const MOOD_SYNONYMS: Readonly<Record<string, KnownMood>> = {
  高兴: "高兴",
  开心: "高兴",
  愉快: "高兴",
  兴奋: "高兴",
  雀跃: "高兴",
  来劲: "高兴",
  困惑: "困惑",
  疑惑: "困惑",
  迷惑: "困惑",
  不解: "困惑",
  犯难: "困惑",
  想不通: "困惑",
  沮丧: "沮丧",
  失落: "沮丧",
  低落: "沮丧",
  郁闷: "沮丧",
  挫败: "沮丧",
  懊恼: "沮丧",
};

/** 每段只在前多少个字符内寻找标签（覆盖短开场白，避免整段误伤） */
const WINDOW_CHARS = 120;

/** 括号内容的最长长度；超界仍未闭合 → 当正文放行，不无限扣留 */
const MAX_TAG_CHARS = 200;

/** 明显在“讲解/举例”的词语：命中时不把标签当控制语法 */
const META_WORDS =
  /(?:比如|例如|示例|例子|格式|写法|标签|输出|开头|写成|像这样|如下)/;

/** 标签开头（不要求闭合）：方括号/中文方括号 + mood + 冒号 */
const TAG_OPEN_RE = /(?:\[|【)\s*mood\s*[:：]/gi;

const SEPARATOR_RE = /[:：]/;

// ── 类型 ──

/** 解析出的情绪块（不含展示文本） */
export interface MoodScan {
  /** 归一后的已知偏向（驱动动画）；自由偏向未命中 → null */
  mood: KnownMood | null;
  /** 偏向原词（决定色相）；null = 这一块根本没认出来 */
  tint: string | null;
  /** 心声（括号内那句话）；null = 没写（展示层回落旁白） */
  voice: string | null;
  /** 强度档位（0 = 省略；1–3 有效） */
  intensity: MoodIntensity;
}

export interface ScanResult extends MoodScan {
  /** 剥离后的展示文本 */
  text: string;
  /** 本段对应的模型原文（含标签） */
  rawText: string;
}

const NO_MOOD: MoodScan = {
  mood: null,
  tint: null,
  voice: null,
  intensity: 0,
};

/** 单个 session 的扫描器瞬态（状态生命＝本轮，段边界重置段内状态） */
interface TagScannerState {
  /** 已累积的未定界块（从标签开头起，未进缓冲） */
  pending: string | null;
  /** 本段是否已处理过一块（段边界重置） */
  segmentConsumed: boolean;
  /** 本段已见过的原始字符数（用于窗口判定） */
  rawCount: number;
  /** 本段已放行的原始前缀（用于判断行首/代码/举例上下文） */
  prefix: string;
}

const scannerStates = new Map<string, TagScannerState>();

// ── 头部解析 ──

export interface Header {
  mood: KnownMood | null;
  tint: string;
  intensity: MoodIntensity;
  voice: string | null;
}

/**
 * 解析 `】` 内的字段：`偏向`、可选`强度`、可选`心声`。
 * 空偏向 → null（调用方只剥标签、不产块）。
 * 强度非法（0、4+、非整数、非数字）→ 0，此时该字段被当作心声。
 */
export function parseMoodHeader(raw: string): Header | null {
  const firstSep = raw.search(SEPARATOR_RE);
  const bias = (firstSep === -1 ? raw : raw.slice(0, firstSep)).trim();
  if (!bias) return null;

  let intensity: MoodIntensity = 0;
  let voice: string | null = null;

  if (firstSep !== -1) {
    const rest = raw.slice(firstSep + 1);
    const secondSep = rest.search(SEPARATOR_RE);
    const head = (secondSep === -1 ? rest : rest.slice(0, secondSep)).trim();

    if (/^\d+$/.test(head)) {
      // 纯数字才是强度位；越界（0、4+）按无强度处理，且不把它当心声
      const n = Number(head);
      intensity = n >= 1 && n <= 3 ? (n as MoodIntensity) : 0;
      const tail = secondSep === -1 ? "" : rest.slice(secondSep + 1).trim();
      voice = tail || null;
    } else {
      // 第 2 段不是强度 → 从这一段起全是心声（可含冒号）
      voice = rest.trim() || null;
    }
  }

  return { mood: MOOD_SYNONYMS[bias] ?? null, tint: bias, intensity, voice };
}

// ── 块求值 ──

type Outcome =
  /** 还不能定界，继续扣留 */
  | { kind: "hold"; raw: string }
  /** 认不出块：原样当正文放行 */
  | {
      kind: "body";
      text: string;
      raw: string;
      mood: null;
      tint: null;
      voice: null;
      intensity: 0;
    }
  /** 认出了块：标签剥离，body 是括号之后的正文 */
  | {
      kind: "block";
      body: string;
      raw: string;
      header: Header | null;
      voice: string | null;
    };

function hold(raw: string): Outcome {
  return { kind: "hold", raw };
}

function body(text: string, raw: string): Outcome {
  return {
    kind: "body",
    text,
    raw,
    mood: null,
    tint: null,
    voice: null,
    intensity: 0,
  };
}

function block(
  header: Header | null,
  voice: string | null,
  bodyText: string,
  raw: string
): Outcome {
  return { kind: "block", header, voice, body: bodyText, raw };
}

/** 在 pending[from..] 中找闭合括号；跨行或超界 → null */
function findClose(pending: string, from: number): number | null {
  const limit = Math.min(pending.length, from + MAX_TAG_CHARS);
  for (let i = from; i < limit; i++) {
    const ch = pending[i];
    if (ch === "\n") return null;
    if (ch === "]" || ch === "】") return i;
  }
  return null;
}

/**
 * 括号之后的正文：去掉标签与正文之间的排版空白。
 * 标签独占一行时，`】` 后面那个换行属于标签自己的行尾，不该在正文里留空行；
 * 但再往后的空行是作者的排版，保留。
 */
function bodyAfterTag(rest: string): string {
  return rest.replace(/^[ \t]*(?:\r?\n)?/, "");
}

/**
 * 求值一个从标签开头起、尚未定界的文本。
 *
 * eof = true 表示调用方拿到的是完整文本（归档剥离）；
 * eof = false 是流式场景：括号没闭合就继续扣留。
 */
function evaluateLine(pending: string, eof: boolean): Outcome {
  TAG_OPEN_RE.lastIndex = 0;
  const open = TAG_OPEN_RE.exec(pending);
  if (!open || open.index !== 0) {
    // 还没长成完整标签开头（`【`、`【mo`、`【mood`）：流式中先扣留，
    // 否则逐 token 到达时半标签会被当正文逐段放行。
    if (!eof && isPotentialTagPrefix(pending)) return hold(pending);
    return body(pending, pending);
  }

  const headerEnd = open.index + open[0].length;
  const close = findClose(pending, headerEnd);

  // 没有闭合：未写完 / 跨行 / 超界 → 放行当正文
  if (close === null) {
    if (eof || pending.includes("\n") || pending.length - headerEnd > MAX_TAG_CHARS) {
      return body(pending, pending);
    }
    return hold(pending);
  }

  const header = parseMoodHeader(pending.slice(headerEnd, close));
  const rest = bodyAfterTag(pending.slice(close + 1));

  // 空偏向：只剥标签，括号后的正文照旧
  if (!header) return block(null, null, rest, pending);

  return block(header, header.voice, rest, pending);
}

// ── 上下文判定（代码 / 举例 / 段首） ──

/** 是否位于未闭合的代码块或行内代码中 */
function isInsideCode(prefix: string): boolean {
  const fences = (prefix.match(/```/g) ?? []).length;
  if (fences % 2 === 1) return true;
  const ticks = (prefix.match(/`/g) ?? []).length;
  return ticks % 2 === 1;
}

/** 标签前的“说明性引导语”是否像在举例 */
function isMetaLeadIn(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed) return false;
  if (!META_WORDS.test(trimmed)) return false;
  return /[：:]$/.test(trimmed) || /(?:比如|例如|如下|像这样)$/.test(trimmed);
}

/**
 * 候选标签是否像控制标签：
 * - 代码块/行内代码中 → 否；
 * - 只认段首（允许缩进）：前面同一行还有正文 → 否；
 * - 段首但上一行是「格式如下：」这类引导语 → 否（那是在举例）。
 */
function isControlCandidate(prefix: string): boolean {
  if (isInsideCode(prefix)) return false;

  const lineStart = prefix.lastIndexOf("\n") + 1;
  const beforeOnLine = prefix.slice(lineStart);
  if (!/^[ \t]*$/.test(beforeOnLine)) return false;

  const beforeLines = prefix.slice(0, lineStart).trimEnd();
  const prevLine = beforeLines.slice(beforeLines.lastIndexOf("\n") + 1);
  if (prevLine && isMetaLeadIn(prevLine)) return false;
  return true;
}

/** 是否是 mood 标签开头的一个“尚未完整”的前缀（用于跨 chunk 扣留） */
function isPotentialTagPrefix(text: string): boolean {
  if (text.length > MAX_TAG_CHARS + 1) return false;
  if (!text.startsWith("[") && !text.startsWith("【")) return false;
  const rest = text.slice(1);
  const end = rest.search(/[\]】\n]/);
  if (end !== -1) return false; // 已经闭合/跨行 → 不是半标签

  let i = 0;
  while (i < rest.length && (rest[i] === " " || rest[i] === "\t")) i++;

  const word = "mood";
  for (let j = 0; j < word.length; j++) {
    if (i >= rest.length) return true;
    if (rest[i].toLowerCase() !== word[j]) return false;
    i++;
  }

  while (i < rest.length && (rest[i] === " " || rest[i] === "\t")) i++;
  if (i >= rest.length) return true;
  return rest[i] === ":" || rest[i] === "：";
}

/** 找到文本末尾一个仍在累积中的半标签起点（无 → null） */
function findPartialPrefix(text: string): number | null {
  for (let i = text.length - 1; i >= 0; i--) {
    const ch = text[i];
    if (ch !== "[" && ch !== "【") continue;
    if (isPotentialTagPrefix(text.slice(i))) return i;
  }
  return null;
}

// ── 一次性剥离（归档路径） ──

/** 在一次性文本中定位第一个控制标签 */
function locateControlTag(text: string): { start: number } | null {
  const limit = Math.min(text.length, WINDOW_CHARS);
  let from = 0;

  while (from < limit) {
    TAG_OPEN_RE.lastIndex = from;
    const open = TAG_OPEN_RE.exec(text);
    if (!open || open.index >= limit) return null;

    if (isControlCandidate(text.slice(0, open.index))) {
      return { start: open.index };
    }

    from = open.index + 1;
  }

  return null;
}

/**
 * 一次性剥离（assistant/message、归档文本路径）：找到段首控制块并整块剥离。
 * 括号之后全是正文；没有闭合/认不出 → 原样返回。
 */
export function stripTagFromText(text: string): ScanResult {
  const hit = locateControlTag(text);
  if (!hit) return { text, rawText: text, ...NO_MOOD };

  const pending = text.slice(hit.start);
  const outcome = evaluateLine(pending, true);

  if (outcome.kind === "body") {
    return { text, rawText: text, ...NO_MOOD };
  }

  const prefix = text.slice(0, hit.start);
  const header = outcome.kind === "block" ? outcome.header : null;

  return {
    text: prefix + (outcome.kind === "block" ? outcome.body : ""),
    rawText: text,
    mood: header?.mood ?? null,
    tint: header?.tint ?? null,
    voice: outcome.kind === "block" ? outcome.voice : null,
    intensity: header?.intensity ?? 0,
  };
}

// ── 流式扫描 ──

function appendPrefix(state: TagScannerState, text: string): void {
  state.prefix = (state.prefix + text).slice(-WINDOW_CHARS);
}

/** 把一次求值结果转成 ScanResult；hold 时返回空文本 */
function toScanResult(outcome: Outcome, prefix: string, raw: string): ScanResult {
  if (outcome.kind === "hold") {
    return { text: "", rawText: outcome.raw, ...NO_MOOD };
  }
  if (outcome.kind === "body") {
    return { text: prefix + outcome.text, rawText: raw, ...NO_MOOD };
  }
  return {
    text: prefix + outcome.body,
    rawText: raw,
    mood: outcome.header?.mood ?? null,
    tint: outcome.header?.tint ?? null,
    voice: outcome.voice,
    intensity: outcome.header?.intensity ?? 0,
  };
}

/**
 * 流式文本 chunk 扫描（assistant/chunk kind=text 路径，进缓冲前调用）。
 * 返回剥离后的文本 + 情绪块；扣留期间 text 为空（调用方不进缓冲）。
 */
export function scanTextChunk(sessionId: string, content: string): ScanResult {
  let state = scannerStates.get(sessionId);
  if (!state) {
    state = {
      pending: null,
      segmentConsumed: false,
      rawCount: 0,
      prefix: "",
    };
    scannerStates.set(sessionId, state);
  }

  // 本段已处理过块 → 原样放行（每段最多一个）
  if (state.segmentConsumed) {
    return { text: content, rawText: content, ...NO_MOOD };
  }

  // pending 未定界：累积后求值
  if (state.pending !== null) {
    state.pending += content;
    state.rawCount += content.length;
    const outcome = evaluateLine(state.pending, false);
    if (outcome.kind === "hold") {
      return { text: "", rawText: state.pending, ...NO_MOOD };
    }
    state.pending = null;
    state.segmentConsumed = true;
    return toScanResult(outcome, "", content);
  }

  const base = state.rawCount;
  state.rawCount += content.length;

  // 窗口已过：后面不再找标签（保持正文原样，避免误伤示例）
  if (base >= WINDOW_CHARS) {
    appendPrefix(state, content);
    return { text: content, rawText: content, ...NO_MOOD };
  }

  let cursor = 0;
  while (true) {
    TAG_OPEN_RE.lastIndex = cursor;
    const open = TAG_OPEN_RE.exec(content);
    if (!open) break;

    const absoluteStart = base + open.index;
    if (absoluteStart >= WINDOW_CHARS) break;

    const before = state.prefix + content.slice(0, open.index);
    if (!isControlCandidate(before)) {
      cursor = open.index + 1;
      continue;
    }

    const candidate = content.slice(open.index);
    const textBefore = content.slice(0, open.index);
    const outcome = evaluateLine(candidate, false);

    // 明确不可能闭合或在窗口内太长 → 当普通文本，继续找后面的候选
    if (outcome.kind === "body") {
      cursor = open.index + 1;
      continue;
    }

    if (outcome.kind === "hold") {
      // 可能是跨 chunk 的半标签：先扣留标签之前已经放行的文本。
      state.pending = candidate;
      appendPrefix(state, textBefore);
      return {
        text: textBefore,
        rawText: textBefore,
        ...NO_MOOD,
      };
    }

    state.segmentConsumed = true;
    return toScanResult(outcome, textBefore, content);
  }

  // 没有完整候选，但末尾可能是被切断的 `【mo` / `[moo`：先扣留，
  // 避免半标签在下一个 chunk 到达前直接渲染出来。
  const partialStart = findPartialPrefix(content);
  if (partialStart !== null && base + partialStart < WINDOW_CHARS) {
    const before = content.slice(0, partialStart);
    if (isControlCandidate(state.prefix + before)) {
      state.pending = content.slice(partialStart);
      appendPrefix(state, before);
      return {
        text: before,
        rawText: before,
        ...NO_MOOD,
      };
    }
  }

  appendPrefix(state, content);
  return { text: content, rawText: content, ...NO_MOOD };
}

/** 查询当前段是否已处理过情绪块（assistant/message 分支防重） */
export function segmentConsumed(sessionId: string): boolean {
  return scannerStates.get(sessionId)?.segmentConsumed ?? false;
}

// ── 生命周期 ──

/**
 * 段边界 re-arm（tool/call、轮内注入）：释放未定界 pending（该段尾部文本放行），
 * 重置段首与消费位——每段独立，后续段可再匹配（每段最多一个）。
 * 返回待放行的扣留文本（无 → null），调用方以 chunk 形式落位在边界事件之前。
 */
export function rearmScanner(sessionId: string): string | null {
  const state = scannerStates.get(sessionId);
  if (!state) return null;
  const held = state.pending;
  state.pending = null;
  state.segmentConsumed = false;
  state.rawCount = 0;
  state.prefix = "";
  return held;
}

/**
 * 释放未定界 pending 并清空扫描器状态（finishProcessing / failProcessing 路径）。
 * 返回扣留文本（无 → null）。轮末仍在 pending 的整段按正文放行：宁可多显示
 * 一行原文，也不吞掉用户还没看到的内容。
 */
export function releasePending(sessionId: string): string | null {
  const state = scannerStates.get(sessionId);
  if (!state) return null;
  const held = state.pending;
  scannerStates.delete(sessionId);
  return held;
}

/** 测试辅助：清空全部扫描器状态 */
export function resetScanners(): void {
  scannerStates.clear();
}
