// src/moodTag.ts
// 情绪标签解析器（批 2-D 动画映射 · 情绪通道）
//
// 标签格式：【mood:高兴】（全角冒号容错）。显式枚举：高兴 / 困惑 / 沮丧；省略 = 平静。
// 解析位置：useProcessingStore.appendCurrentEvent 内、33ms 节流缓冲之前——
// assistant/chunk(kind=text) 进缓冲前过扫描器；reasoning chunk 不进扫描器；
// assistant/message 用 stripTagFromText 一次性剥离后再入列（防止归档瞬间标签复活）。
//
// 扫描器语义（跨 chunk 前缀累积，模块级 Map 瞬态，与 chunkBuffers 并列，不进 Zustand）：
// - 匹配 【mood: 前缀后：pending 扣留（整段不进缓冲，用户不可见半标签），
//   直到 】 闭合 → matched（剥离 + 更新 mood）或非法值 → 全量放行
// - 非标签开头 → 立即放行；段中标签（非段首）不剥离，原样渲染（既定行为）
// - pending 上界 24 字符，超界未闭合 → 放行
// - 段边界 re-arm（tool/call、轮内注入）：重置段首与消费位，后续段可再尝试匹配
// - 每段最多消费一个标签（段边界重置；段内首次成功匹配后关闭本段的匹配机会）
//
// 展示/历史双字段：assistant/chunk 的 `content` 是展示文本（情绪标签已剥离），
// `rawContent` 是模型原始输出（含标签）。展示层只读 content；历史派生优先
// rawContent，保证模型能看到自己上一轮写过的标签，且轮内/跨轮前缀一致。
// 旧事件没有 rawContent 时回退 content。

import type { Mood, MoodIntensity } from "./types";

// ── 常量 ──

/** 标签值枚举（显式 3 值；平静仅作省略默认，显式写视为非法） */
const MOOD_VALUES = ["高兴", "困惑", "沮丧"] as const;

/** 半角冒号前缀 */
const TAG_PREFIX_HALF = "【mood:";
/** 全角冒号前缀（容错） */
const TAG_PREFIX_FULL = "【mood：";

/** pending 上界（字符）。超界未闭合 → 全量放行 */
const MAX_PENDING = 24;

/** 完整标签正则：以段首标签开头（含闭合），值不限（合法性由 parseMoodTag 判定） */
const TAG_RE = /^【mood[:：]([^】]*)】/;

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
  /** 已累积的未闭合标签前缀（扣留中，未进缓冲） */
  pending: string | null;
  /** 本段是否已消费一个标签（段边界重置；消费后本段关闭匹配机会） */
  segmentConsumed: boolean;
  /** 当前段是否已放行过文本（段中标签不剥离） */
  segmentStarted: boolean;
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

/**
 * 一次性剥离（assistant/message 路径）：剥离开头的完整有效标签。
 * 非法值不剥离（与扫描器「非法值全量放行」一致——归档与流式视图保持一致）。
 * 无标签 → 原样返回。
 */
export function stripTagFromText(text: string): ScanResult {
  const m = TAG_RE.exec(text);
  if (!m) return { text, rawText: text, mood: null, intensity: 0 };
  const parsed = parseMoodTag(m[0]);
  if (parsed.mood === null)
    return { text, rawText: text, mood: null, intensity: 0 };
  return {
    text: text.slice(m[0].length),
    rawText: text,
    mood: parsed.mood,
    intensity: parsed.intensity,
  };
}

/** content 是否为 mood 标签前缀的（部分）匹配（含前缀自身） */
function matchTagPrefix(content: string): boolean {
  if (content.length === 0 || content[0] !== "【") return false;
  const maxLen = Math.min(content.length, TAG_PREFIX_HALF.length);
  for (let i = 1; i <= maxLen; i++) {
    const slice = content.slice(0, i);
    if (!TAG_PREFIX_HALF.startsWith(slice) && !TAG_PREFIX_FULL.startsWith(slice)) {
      return false;
    }
  }
  return true;
}

/** 求值累积中的 pending：闭合 → matched / 非法值全量放行；超界 → 全量放行；否则继续扣留 */
function evaluatePending(state: TagScannerState): ScanResult {
  const pending = state.pending!;

  // 超上界未闭合 → 全量放行
  if (pending.length > MAX_PENDING) {
    state.pending = null;
    state.segmentStarted = true;
    return { text: pending, rawText: pending, mood: null, intensity: 0 };
  }

  const tagEnd = pending.indexOf("】");
  if (tagEnd !== -1) {
    state.pending = null;
    state.segmentStarted = true;
    const tag = pending.slice(0, tagEnd + 1);
    const parsed = parseMoodTag(tag);
    if (parsed.mood !== null) {
      state.segmentConsumed = true;
      return {
        text: pending.slice(tagEnd + 1),
        rawText: pending,
        mood: parsed.mood,
        intensity: parsed.intensity,
      };
    }
    // 非法值 → 全量放行（标签原样渲染）
    return { text: pending, rawText: pending, mood: null, intensity: 0 };
  }

  // 未闭合 → 继续扣留（用户不可见半标签）
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
    state = { pending: null, segmentConsumed: false, segmentStarted: false };
    scannerStates.set(sessionId, state);
  }

  // 本段已消费标签 → 原样放行（每段最多一个）
  if (state.segmentConsumed)
    return { text: content, rawText: content, mood: null, intensity: 0 };

  // pending 未闭合：累积后求值
  if (state.pending !== null) {
    state.pending += content;
    return evaluatePending(state);
  }

  // 段中已放行文本 → 原样放行（段中标签不剥离）
  if (state.segmentStarted)
    return { text: content, rawText: content, mood: null, intensity: 0 };

  // 段首：mood 标签前缀？
  if (matchTagPrefix(content)) {
    const tagEnd = content.indexOf("】");
    if (tagEnd !== -1) {
      // 单 chunk 完整标签
      state.segmentStarted = true;
      const tag = content.slice(0, tagEnd + 1);
      const parsed = parseMoodTag(tag);
      if (parsed.mood !== null) {
        state.segmentConsumed = true;
        return {
          text: content.slice(tagEnd + 1),
          rawText: content,
          mood: parsed.mood,
          intensity: parsed.intensity,
        };
      }
      // 非法值 → 全量放行
      return { text: content, rawText: content, mood: null, intensity: 0 };
    }
    // 未闭合前缀 → pending 扣留
    state.pending = content;
    return { text: "", rawText: content, mood: null, intensity: 0 };
  }

  // 非标签开头 → 立即放行（空 chunk 不标记段已开始，段首匹配机会保留）
  if (content.length > 0) state.segmentStarted = true;
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
  state.segmentStarted = false;
  state.segmentConsumed = false;
  return held;
}

/**
 * 释放未闭合 pending 并清空扫描器状态（finishProcessing / failProcessing 路径）。
 * 返回扣留文本（无 → null）。fail 路径下返回文本随后被既有 chunk 丢弃语义吞掉。
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
