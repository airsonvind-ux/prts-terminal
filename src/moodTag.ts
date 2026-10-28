// src/moodTag.ts
// 情绪标签解析器（批 2-D 动画映射 · 情绪通道）
//
// 标签格式：`[mood:高兴]` / `【mood:高兴】`（方括号与全角括号、全角冒号、
// `mood` 大小写与冒号两侧空格均容错）。显式枚举：高兴 / 困惑 / 沮丧；省略 = 平静。
// 解析位置：useProcessingStore.appendCurrentEvent 内、33ms 节流缓冲之前——
// assistant/chunk(kind=text) 进缓冲前过扫描器；reasoning chunk 不进扫描器；
// assistant/message 用 stripTagFromText 一次性剥离后再入列（防止归档瞬间标签复活）。
//
// 扫描器语义（跨 chunk 前缀累积，模块级 Map 瞬态，与 chunkBuffers 并列，不进 Zustand）：
// - 只在每段开头 WINDOW_CHARS 个字符内寻找标签；不要求标签在第一个字符。
// - 识别方括号/中文方括号、大小写、全半角冒号与空格变体。
// - 标签在段中也可以出现；但代码块、行内代码和明显的“举例/格式说明”不作为控制标签。
// - 合法值更新情绪；非法值只剥离、不更新（绝不把控制标签漏进正文）。
// - pending 上界 40 字符，超界、换行未闭合 → 全量放行。
// - 段边界 re-arm（tool/call、轮内注入）：重置段首与消费位，后续段可再尝试匹配。
// - 每段最多消费一个标签（段边界重置；段内首次命中后关闭本段匹配机会）。
//
// 展示/历史双字段：assistant/chunk 的 `content` 是展示文本（情绪标签已剥离），
// `rawContent` 是模型原始输出（含标签）。展示层只读 content；历史派生优先
// rawContent，保证模型能看到自己上一轮写过的标签，且轮内/跨轮前缀一致。
// 旧事件没有 rawContent 时回退 content。

import type { Mood, MoodIntensity } from "./types";

// ── 常量 ──

/** 标签值枚举（显式 3 值；平静仅作省略默认，显式写视为非法） */
const MOOD_VALUES = ["高兴", "困惑", "沮丧"] as const;

/** 每段只在前多少个字符内寻找标签（覆盖短开场白，避免整段误伤） */
const WINDOW_CHARS = 120;

/** 标签最大长度（字符）；超界未闭合 → 放行 */
const MAX_TAG_CHARS = 40;

/** 非行首标签允许的最大前置文本长度（字符）；更长的前置文本更可能是正文 */
const MAX_INLINE_PREFIX = 24;

/** 明显在“讲解/举例”的词语：命中时不把标签当控制语法 */
const META_WORDS = /(?:比如|例如|示例|例子|格式|写法|标签|输出|开头|写成|像这样|如下)/;

/** 标签开头（不要求闭合）：方括号/中文方括号 + mood + 冒号 */
const TAG_OPEN_RE = /(?:\[|【)\s*mood\s*[:：]/gi;

/** 完整标签：括号内只允许一行，值合法性由 parseMoodTag 判定 */
const TAG_RE = /^(?:\[|【)\s*mood\s*[:：]\s*([^\]】\n]*?)\s*(?:\]|】)$/i;

// ── 类型 ──

export interface ScanResult {
  /** 剥离后的展示文本（未命中 → 原样；pending 扣留中 → 空串，不进缓冲） */
  text: string;
  /** 本段对应的模型原文（含情绪标签；pending 闭合时包含此前扣留的前缀） */
  rawText: string;
  /** 命中的情绪（null = 未命中；调用方不更新 mood） */
  mood: Exclude<Mood, "平静"> | null;
  /** 命中的强度档位（0 = 基础/省略；未命中 → 0） */
  intensity: MoodIntensity;
}

/** 解析结果（parseMoodTag） */
interface ParsedTag {
  mood: Exclude<Mood, "平静"> | null;
  intensity: MoodIntensity;
}

/** 单个 session 的扫描器瞬态（每轮一个 sessionId；状态生命＝本轮，段边界重置段内状态） */
interface TagScannerState {
  /** 已累积的未闭合候选标签（扣留中，未进缓冲） */
  pending: string | null;
  /** 本段是否已消费一个标签（段边界重置；消费后本段关闭匹配机会） */
  segmentConsumed: boolean;
  /** 本段已见过的原始字符数（用于窗口判定） */
  rawCount: number;
  /** 本段已放行的原始前缀（用于判断行首/代码/举例上下文） */
  prefix: string;
}

/** sessionId → 扫描器状态（模块级瞬态，与 chunkBuffers 并列，不进 Zustand） */
const scannerStates = new Map<string, TagScannerState>();

// ── 解析纯函数 ──

/**
 * 解析完整标签文本中的情绪值与强度档位。
 * 值格式：情绪 或 情绪:数字（内部冒号半角/全角均容错）。
 * 强度 1~3 合法；省略/非法（0、4+、非整数）→ 0（基础档）。
 * 非法情绪值（含显式 "平静"）→ mood=null（回退平静：调用方不更新 mood）。
 */
export function parseMoodTag(tag: string): ParsedTag {
  const m = TAG_RE.exec(tag);
  if (!m) return { mood: null, intensity: 0 };
  const parts = m[1].split(/[:：]/);
  const value = parts[0].trim();
  if (!(MOOD_VALUES as readonly string[]).includes(value)) {
    return { mood: null, intensity: 0 };
  }
  let intensity: MoodIntensity = 0;
  if (parts.length > 1) {
    const n = Number(parts[1].trim());
    if (Number.isInteger(n) && n >= 1 && n <= 3) {
      intensity = n as MoodIntensity;
    }
  }
  return { mood: value as Exclude<Mood, "平静">, intensity };
}

/** 查找下一个标签开头（不要求闭合） */
function findTagOpen(
  text: string,
  from: number,
): { start: number; end: number } | null {
  TAG_OPEN_RE.lastIndex = from;
  const m = TAG_OPEN_RE.exec(text);
  if (!m) return null;
  return { start: m.index, end: m.index + m[0].length };
}

/**
 * 从指定位置读出一个完整标签。
 * 不完整、跨行或超过 MAX_TAG_CHARS 仍未闭合 → null。
 */
function readTagAt(
  text: string,
  start: number,
): { end: number; parsed: ParsedTag } | null {
  const open = findTagOpen(text, start);
  if (!open || open.start !== start) return null;

  const limit = Math.min(text.length, start + MAX_TAG_CHARS);
  for (let i = open.end; i < limit; i++) {
    const ch = text[i];
    if (ch === "\n") return null;
    if (ch === "]" || ch === "】") {
      const parsed = parseMoodTag(text.slice(start, i + 1));
      return { end: i + 1, parsed };
    }
  }
  return null;
}

/** 是否是 mood 标签开头的一个“尚未完整”的前缀（用于跨 chunk 扣留） */
function isPotentialTagPrefix(text: string): boolean {
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
 * 判断一个候选标签是否像控制标签，而不是正文里的示例。
 *
 * 规则保持宽松：
 * - 标签在行首（允许缩进）→ 接受；上一行若是“格式如下：/比如：”这类引导语 → 拒绝。
 * - 标签不在行首 → 只接受前置文本很短的场景（如“好的，”），且不含讲解词。
 * - 代码块/行内代码中的标签 → 拒绝。
 */
function isControlCandidate(prefix: string): boolean {
  if (isInsideCode(prefix)) return false;

  const lineStart = prefix.lastIndexOf("\n") + 1;
  const beforeOnLine = prefix.slice(lineStart);

  if (/^[ \t]*$/.test(beforeOnLine)) {
    const beforeLines = prefix.slice(0, lineStart).trimEnd();
    const prevLine = beforeLines.slice(beforeLines.lastIndexOf("\n") + 1);
    if (prevLine && isMetaLeadIn(prevLine)) return false;
    return true;
  }

  const trimmed = prefix.trim();
  if (trimmed.length > MAX_INLINE_PREFIX) return false;
  if (META_WORDS.test(trimmed)) return false;
  return true;
}

/** 在一次性文本中定位第一个控制标签 */
function locateControlTag(
  text: string,
): { start: number; end: number; parsed: ParsedTag } | null {
  const limit = Math.min(text.length, WINDOW_CHARS);
  let from = 0;

  while (from < limit) {
    const open = findTagOpen(text, from);
    if (!open || open.start >= limit) return null;

    if (isControlCandidate(text.slice(0, open.start))) {
      const tag = readTagAt(text, open.start);
      if (tag) return { start: open.start, end: tag.end, parsed: tag.parsed };
    }

    from = open.start + 1;
  }

  return null;
}

/**
 * 一次性剥离（assistant/message 路径）：在段首识别区内找到第一个控制标签并剥离。
 * 合法值返回情绪；非法值只剥离，不更新情绪。
 * 无标签 → 原样返回。
 */
export function stripTagFromText(text: string): ScanResult {
  const hit = locateControlTag(text);
  if (!hit) return { text, rawText: text, mood: null, intensity: 0 };
  return {
    text: text.slice(0, hit.start) + text.slice(hit.end),
    rawText: text,
    mood: hit.parsed.mood,
    intensity: hit.parsed.intensity,
  };
}

/** 追加前缀并保留窗口所需长度 */
function appendPrefix(state: TagScannerState, text: string): void {
  state.prefix = (state.prefix + text).slice(-WINDOW_CHARS);
}

/** 求值累积中的 pending：闭合 → 剥离；超界/换行 → 放行；否则继续扣留 */
function evaluatePending(state: TagScannerState): ScanResult {
  const pending = state.pending!;
  const tag = readTagAt(pending, 0);

  if (tag) {
    state.pending = null;
    state.segmentConsumed = true;
    return {
      text: pending.slice(tag.end),
      rawText: pending,
      mood: tag.parsed.mood,
      intensity: tag.parsed.intensity,
    };
  }

  // 超界、跨行或已经不可能长成标签 → 放行；本段不再尝试。
  if (
    pending.length > MAX_TAG_CHARS ||
    pending.includes("\n") ||
    !isPotentialTagPrefix(pending)
  ) {
    state.pending = null;
    state.segmentConsumed = true;
    return { text: pending, rawText: pending, mood: null, intensity: 0 };
  }

  return { text: "", rawText: pending, mood: null, intensity: 0 };
}

// ── 扫描入口 ──

/**
 * 流式文本 chunk 扫描（assistant/chunk kind=text 路径，进缓冲前调用）。
 * 返回剥离后的文本 + 命中的情绪；pending 扣留时返回空串（调用方不进缓冲）。
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

  // 本段已消费标签 → 原样放行（每段最多一个）
  if (state.segmentConsumed)
    return { text: content, rawText: content, mood: null, intensity: 0 };

  // pending 未闭合：累积后求值
  if (state.pending !== null) {
    state.pending += content;
    state.rawCount += content.length;
    return evaluatePending(state);
  }

  const base = state.rawCount;
  state.rawCount += content.length;

  // 窗口已过：后面不再找标签（保持正文原样，避免误伤示例）
  if (base >= WINDOW_CHARS) {
    appendPrefix(state, content);
    return { text: content, rawText: content, mood: null, intensity: 0 };
  }

  let cursor = 0;
  while (true) {
    const open = findTagOpen(content, cursor);
    if (!open) break;

    const absoluteStart = base + open.start;
    if (absoluteStart >= WINDOW_CHARS) break;

    const before = state.prefix + content.slice(0, open.start);
    if (isControlCandidate(before)) {
      const tag = readTagAt(content, open.start);

      if (tag) {
        state.segmentConsumed = true;
        return {
          text: content.slice(0, open.start) + content.slice(tag.end),
          rawText: content,
          mood: tag.parsed.mood,
          intensity: tag.parsed.intensity,
        };
      }

      const candidateTail = content.slice(open.start);
      // 明确不可能闭合 → 当普通文本；继续找后面的候选。
      if (candidateTail.includes("\n") || candidateTail.length > MAX_TAG_CHARS) {
        cursor = open.start + 1;
        continue;
      }

      // 可能是跨 chunk 的半标签：先扣留标签之前已经放行的文本。
      const textBefore = content.slice(0, open.start);
      state.pending = candidateTail;
      appendPrefix(state, textBefore);
      return {
        text: textBefore,
        rawText: textBefore,
        mood: null,
        intensity: 0,
      };
    }

    cursor = open.start + 1;
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
        mood: null,
        intensity: 0,
      };
    }
  }

  appendPrefix(state, content);
  return { text: content, rawText: content, mood: null, intensity: 0 };
}

/** 查询当前段是否已消费情绪标签（assistant/message 分支防重：本段已消费则只剥离不产事件） */
export function segmentConsumed(sessionId: string): boolean {
  return scannerStates.get(sessionId)?.segmentConsumed ?? false;
}

// ── 生命周期 ──

/**
 * 段边界 re-arm（tool/call、轮内注入）：释放未闭合 pending（该段尾部文本放行），
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
 * 释放未闭合 pending 并清空扫描器状态（finishProcessing / failProcessing 路径）。
 * 返回扣留文本（无 → null）。fail 路径下该文本是刻意隐藏的片段，直接丢弃；
 * 已进入节流缓冲的可见 chunk 由 failProcessing 先 flush 保留。
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
