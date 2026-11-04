// src/moodTag.test.ts
// 情绪标签解析器测试（MOOD 草案 3）
//
// 覆盖：
//   1. 头部格式变体（方括号、全角冒号、空格大小写）
//   2. 括号内分字段：偏向 / 可选强度 / 可选心声；心声可含冒号
//   3. 括号外永远是正文：标签后接正文不再被吞
//   4. 自由偏向：已知词归一、近义词命中、未知词只定色相
//   5. 只认段首、一行只处理第一个；代码与举例上下文不误伤
//   6. 未闭合的边界：跨行 / 超界 / 流结束，一律放行当正文
//   7. 跨 chunk 半标签扣留
//   8. stripTagFromText 一次性剥离（归档路径）
//   9. releasePending / rearmScanner 生命周期

import { describe, it, expect, beforeEach } from "vitest";
import {
  scanTextChunk,
  stripTagFromText,
  rearmScanner,
  releasePending,
  resetScanners,
  segmentConsumed,
  parseMoodHeader,
} from "./moodTag";

beforeEach(() => resetScanners());

// ═══════════════════════════════════════════════
// 场景 1：头部格式变体
// ═══════════════════════════════════════════════

describe("头部格式变体", () => {
  it("中文方括号 + 半角冒号，括号外是正文", () => {
    const r = scanTextChunk("s1", "【mood:高兴】好的，博士。\n");
    expect(r.mood).toBe("高兴");
    expect(r.tint).toBe("高兴");
    expect(r.voice).toBeNull();
    expect(r.text).toBe("好的，博士。\n");
  });

  it("ASCII 方括号 + 半角冒号", () => {
    const r = scanTextChunk("s1", "[mood:困惑]我查一下。\n");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe("我查一下。\n");
  });

  it("全角冒号、大小写与空格混合", () => {
    const r = scanTextChunk("s1", "[ MOOD ： 沮丧 ] 算了。\n");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("算了。\n");
  });

  it("空偏向 → 只剥标签，余文归正文", () => {
    const r = stripTagFromText("【mood:】正文");
    expect(r.tint).toBeNull();
    expect(r.voice).toBeNull();
    expect(r.text).toBe("正文");
  });
});

// ═══════════════════════════════════════════════
// 场景 2：括号内分字段
// ═══════════════════════════════════════════════

describe("括号内分字段", () => {
  it("两段：偏向 + 心声", () => {
    const r = scanTextChunk("s1", "【mood:不耐烦:还没完，先别催。】\n");
    expect(r.mood).toBeNull();
    expect(r.tint).toBe("不耐烦");
    expect(r.intensity).toBe(0);
    expect(r.voice).toBe("还没完，先别催。");
    expect(r.text).toBe("");
  });

  it("三段：偏向 + 强度 + 心声", () => {
    const r = scanTextChunk("s1", "【mood:高兴:3:终于对上了，这张图好认。】");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(3);
    expect(r.voice).toBe("终于对上了，这张图好认。");
    expect(r.text).toBe("");
  });

  it("心声里可以带冒号", () => {
    const r = scanTextChunk("s1", "【mood:好奇:这层的构造：有点眼熟】正文");
    expect(r.tint).toBe("好奇");
    expect(r.voice).toBe("这层的构造：有点眼熟");
    expect(r.text).toBe("正文");
  });

  it("心声可以省略", () => {
    expect(scanTextChunk("s1", "【mood:高兴:2】\n").voice).toBeNull();
    expect(scanTextChunk("s1", "【mood:高兴】\n").voice).toBeNull();
  });

  it("空心声（只有冒号）算没写", () => {
    expect(scanTextChunk("s1", "【mood:高兴:】\n").voice).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// 场景 3：括号外永远是正文
// ═══════════════════════════════════════════════

describe("括号外永远是正文", () => {
  it("标签后紧跟正文 → 正文保留，不被当心声", () => {
    const r = scanTextChunk("s1", "【mood:高兴】好的，博士。");
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBeNull();
    expect(r.text).toBe("好的，博士。");
  });

  it("标签独占一行 → 那个换行归标签，不留空行", () => {
    const r = scanTextChunk("s1", "【mood:高兴】\n正文");
    expect(r.text).toBe("正文");
  });

  it("标签行之后还有空行 → 空行是作者排版，保留", () => {
    const r = stripTagFromText("【mood：高兴】\n\n正文");
    expect(r.text).toBe("\n正文");
  });

  it("标签后面接多行正文 → 全部保留", () => {
    const r = scanTextChunk("s1", "【mood:警惕】门外有动静。\n我们走。");
    expect(r.voice).toBeNull();
    expect(r.text).toBe("门外有动静。\n我们走。");
  });
});

// ═══════════════════════════════════════════════
// 场景 4：自由偏向
// ═══════════════════════════════════════════════

describe("自由偏向", () => {
  it("未知偏向 → 只定色相，不驱动动作", () => {
    const r = scanTextChunk("s1", "【mood:不耐烦】三格都不确定。\n");
    expect(r.mood).toBeNull();
    expect(r.tint).toBe("不耐烦");
    expect(r.text).toBe("三格都不确定。\n");
  });

  it("近义词归一：开心 → 高兴", () => {
    expect(scanTextChunk("s1", "【mood:开心】好啊。\n").mood).toBe("高兴");
  });

  it("近义词归一：兴奋 → 高兴", () => {
    expect(scanTextChunk("s1", "【mood:兴奋】好耶。\n").mood).toBe("高兴");
  });

  it("近义词归一：想不通 → 困惑", () => {
    expect(scanTextChunk("s1", "【mood:想不通】再算一遍。\n").mood).toBe("困惑");
  });

  it("近义词归一：郁闷 → 沮丧", () => {
    expect(scanTextChunk("s1", "【mood:郁闷】没赶上。\n").mood).toBe("沮丧");
  });

  it("色相保留原词而不是归一后的词", () => {
    const r = scanTextChunk("s1", "【mood:雀跃】好耶。\n");
    expect(r.mood).toBe("高兴");
    expect(r.tint).toBe("雀跃");
  });
});

// ═══════════════════════════════════════════════
// 场景 5：段首与上下文
// ═══════════════════════════════════════════════

describe("只认段首", () => {
  it("行中标签 → 当普通文本，不识别", () => {
    const r = scanTextChunk("s1", "好的，博士。【mood:高兴】我们开始吧。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("好的，博士。【mood:高兴】我们开始吧。");
  });

  it("上一行有正文、标签在新行行首 → 识别", () => {
    const r = scanTextChunk("s1", "博士，晚上好。\n【mood:困惑】我查一下。\n");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe("博士，晚上好。\n我查一下。\n");
  });

  it("行首允许缩进", () => {
    const r = scanTextChunk("s1", "  【mood:高兴】嗯。\n");
    expect(r.mood).toBe("高兴");
    expect(r.text.trim()).toBe("嗯。");
  });

  it("识别区（前 120 字）之外不处理", () => {
    const text = "a".repeat(130) + "\n【mood:高兴】尾部";
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });

  it("行内代码中的格式示例 → 原样保留", () => {
    const text = "格式是 `[mood:高兴]`，写在开头。";
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });

  it("代码块中的格式示例 → 原样保留", () => {
    const text = "示例：\n```text\n【mood:高兴】\n```";
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });

  it("“比如”后的标签 → 视为举例", () => {
    const text = "比如【mood:高兴】这样写。";
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });

  it("“格式如下：”下一行段首的标签 → 视为举例", () => {
    const text = "格式如下：\n【mood:高兴】";
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });
});

// ═══════════════════════════════════════════════
// 场景 6：未闭合的边界
// ═══════════════════════════════════════════════

describe("未闭合的边界", () => {
  it("流式未闭合 → 先扣留，闭合后立即落位（不等换行）", () => {
    expect(scanTextChunk("s1", "【mood:高").text).toBe("");
    const r = scanTextChunk("s1", "兴】太好了。\n");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("太好了。\n");
  });

  it("未闭合时遇到换行 → 当正文放行", () => {
    const r = scanTextChunk("s1", "【mood:高兴\n继续");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:高兴\n继续");
  });

  it("括号超界仍未闭合 → 当正文放行", () => {
    const text = "【mood:" + "a".repeat(210);
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });

  it("流结束仍在 pending → 整段按正文放行", () => {
    expect(scanTextChunk("s1", "【mood:高").text).toBe("");
    expect(releasePending("s1")).toBe("【mood:高");
  });
});

// ═══════════════════════════════════════════════
// 场景 7：跨 chunk 切断
// ═══════════════════════════════════════════════

describe("跨 chunk 切断（pending 扣留）", () => {
  it("头部被切开：扣留直到闭合", () => {
    expect(scanTextChunk("s1", "【mood:高").text).toBe("");
    const r = scanTextChunk("s1", "兴】收到。\n");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("收到。\n");
  });

  it("心声在括号内被切开", () => {
    expect(scanTextChunk("s1", "【mood:困惑:我").text).toBe("");
    const r = scanTextChunk("s1", "查一下。】正文");
    expect(r.voice).toBe("我查一下。");
    expect(r.text).toBe("正文");
  });

  it("半标签跨多个 chunk 也不可见", () => {
    expect(scanTextChunk("s1", "【mo").text).toBe("");
    expect(scanTextChunk("s1", "od:沮").text).toBe("");
    const r = scanTextChunk("s1", "丧】嗯。\n");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("嗯。\n");
  });

  it("非 mood 前缀（【moodx）直接放行", () => {
    const r = scanTextChunk("s1", "【moodx 不是标签。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【moodx 不是标签。");
  });

  it("标签与正文同 chunk 一次性求值", () => {
    const r = scanTextChunk("s1", "【mood:高兴:2:嗯。】正文\n");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(2);
    expect(r.voice).toBe("嗯。");
    expect(r.text).toBe("正文\n");
  });
});

// ═══════════════════════════════════════════════
// 场景 8：stripTagFromText（归档一次性剥离）
// ═══════════════════════════════════════════════

describe("stripTagFromText 一次性剥离", () => {
  it("只剥括号，括号后的正文原样保留", () => {
    const r = stripTagFromText("【mood:困惑】我查一下。\n正文第一行");
    expect(r.mood).toBe("困惑");
    expect(r.voice).toBeNull();
    expect(r.text).toBe("我查一下。\n正文第一行");
  });

  it("没有心声只剥标签", () => {
    const r = stripTagFromText("【mood：高兴】\n\n正文");
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBeNull();
    expect(r.text).toBe("\n正文");
  });

  it("无标签 → 原样返回", () => {
    const r = stripTagFromText("没有标签的文本");
    expect(r.tint).toBeNull();
    expect(r.text).toBe("没有标签的文本");
  });

  it("行中标签不剥离", () => {
    const text = "好的，博士。【mood:高兴】我们开始吧。";
    const r = stripTagFromText(text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });

  it("代码块中的示例不剥离", () => {
    const text = "```text\n【mood:高兴】\n```";
    const r = stripTagFromText(text);
    expect(r.tint).toBeNull();
    expect(r.text).toBe(text);
  });

  it("识别区外不剥离", () => {
    const text = "a".repeat(130) + "【mood:高兴】";
    const r = stripTagFromText(text);
    expect(r.tint).toBeNull();
    expect(r.text).toBe(text);
  });

  it("空偏向只剥标签", () => {
    const r = stripTagFromText("【mood:】提示");
    expect(r.tint).toBeNull();
    expect(r.text).toBe("提示");
  });

  it("rawText 保留原文", () => {
    const raw = "【mood:沮丧:没赶上。】\n再试一次。";
    expect(stripTagFromText(raw).rawText).toBe(raw);
  });

  it("旧归档（心声在括号外）读回时正文完整", () => {
    const r = stripTagFromText("【mood:沮丧】没赶上。\n再试一次。");
    expect(r.mood).toBe("沮丧");
    expect(r.voice).toBeNull();
    expect(r.text).toBe("没赶上。\n再试一次。");
  });
});

// ═══════════════════════════════════════════════
// 场景 9：强度
// ═══════════════════════════════════════════════

describe("强度档位", () => {
  it("标签带强度", () => {
    const r = scanTextChunk("s1", "【mood:高兴:2】\n");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(2);
  });

  it("省略强度 → 0", () => {
    expect(scanTextChunk("s1", "【mood:高兴】\n").intensity).toBe(0);
  });

  it("非法强度 → 0，且不作为心声", () => {
    expect(parseMoodHeader("高兴:0")?.intensity).toBe(0);
    expect(parseMoodHeader("高兴:0")?.voice).toBeNull();
    expect(parseMoodHeader("高兴:4")?.intensity).toBe(0);
    expect(parseMoodHeader("高兴:2")?.intensity).toBe(2);
  });

  it("非数字第 2 段是心声，不是强度", () => {
    const h = parseMoodHeader("高兴:终于对上了");
    expect(h?.intensity).toBe(0);
    expect(h?.voice).toBe("终于对上了");
  });

  it("全角冒号分隔强度", () => {
    const r = scanTextChunk("s1", "【mood:高兴：3:内容】\n");
    expect(r.intensity).toBe(3);
    expect(r.voice).toBe("内容");
  });

  it("空偏向 → parseMoodHeader 返回 null", () => {
    expect(parseMoodHeader("  ")).toBeNull();
    expect(parseMoodHeader(":2")).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// 场景 10：生命周期
// ═══════════════════════════════════════════════

describe("生命周期", () => {
  it("段边界 re-arm 后可再匹配，每段最多一个", () => {
    const first = scanTextChunk("s1", "【mood:高兴】第一段。\n");
    expect(first.mood).toBe("高兴");
    expect(segmentConsumed("s1")).toBe(true);

    // 同一段再出现标签 → 不处理
    const second = scanTextChunk("s1", "\n【mood:沮丧】还在同一段");
    expect(second.mood).toBeNull();

    rearmScanner("s1");
    expect(segmentConsumed("s1")).toBe(false);
    const third = scanTextChunk("s1", "【mood:困惑】新段。\n");
    expect(third.mood).toBe("困惑");
  });

  it("re-arm 释放未闭合 pending", () => {
    scanTextChunk("s1", "【mood:高");
    expect(rearmScanner("s1")).toBe("【mood:高");
  });

  it("releasePending：无状态 → null；有 pending → 返回原文并清空", () => {
    expect(releasePending("nope")).toBeNull();
    scanTextChunk("s1", "【mood:高");
    expect(releasePending("s1")).toBe("【mood:高");
    expect(releasePending("s1")).toBeNull();
  });

  it("已消费标签后 releasePending → null", () => {
    scanTextChunk("s1", "【mood:高兴】嗯。\n");
    expect(releasePending("s1")).toBeNull();
  });

  it("resetScanners 清空全部状态", () => {
    scanTextChunk("s1", "【mood:高兴】嗯。\n");
    resetScanners();
    expect(segmentConsumed("s1")).toBe(false);
  });
});
