// src/moodTag.test.ts
// 情绪标签解析器测试（MOOD v2）
//
// 覆盖：
//   1. 头部格式变体（方括号、全角冒号、空格大小写）
//   2. 整行块语义：`】` 之后到行尾是心声，不进正文
//   3. 自由偏向：已知词归一、近义词命中、未知词只定色相
//   4. 只认段首、一行只处理第一个；代码与举例上下文不误伤
//   5. 四层终止边界：换行 / 句末标点 / 超 40 判写错 / 流结束按正文
//   6. 跨 chunk 半标签扣留
//   7. stripTagFromText 一次性剥离（归档路径）
//   8. releasePending / rearmScanner 生命周期

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
  it("中文方括号 + 半角冒号", () => {
    const r = scanTextChunk("s1", "【mood:高兴】好的，博士。\n");
    expect(r.mood).toBe("高兴");
    expect(r.tint).toBe("高兴");
    expect(r.voice).toBe("好的，博士。");
    expect(r.text).toBe("");
  });

  it("ASCII 方括号 + 半角冒号", () => {
    const r = scanTextChunk("s1", "[mood:困惑]我查一下。\n");
    expect(r.mood).toBe("困惑");
    expect(r.voice).toBe("我查一下。");
    expect(r.text).toBe("");
  });

  it("全角冒号、大小写与空格混合", () => {
    const r = scanTextChunk("s1", "[ MOOD ： 沮丧 ] 算了。\n");
    expect(r.mood).toBe("沮丧");
    expect(r.voice).toBe("算了。");
  });

  it("空偏向 → 只剥标签，余文归正文", () => {
    const r = stripTagFromText("【mood:】正文");
    expect(r.tint).toBeNull();
    expect(r.voice).toBeNull();
    expect(r.text).toBe("正文");
  });
});

// ═══════════════════════════════════════════════
// 场景 2：整行块语义
// ═══════════════════════════════════════════════

describe("整行块语义", () => {
  it("心声省略（标签后直接换行）→ 正文另起", () => {
    const r = scanTextChunk("s1", "【mood:高兴】\n正文");
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBeNull();
    expect(r.text).toBe("正文");
  });

  it("心声到行尾为止，换行后的文字是正文", () => {
    const r = scanTextChunk("s1", "【mood:警惕】门外有动静。\n我们走。");
    expect(r.tint).toBe("警惕");
    expect(r.voice).toBe("门外有动静。");
    expect(r.text).toBe("我们走。");
  });

  it("心声里的句号不结束块（换行优先）", () => {
    const r = scanTextChunk("s1", "【mood:高兴】终于对上了。这张图好认。\n后面是正文。");
    expect(r.voice).toBe("终于对上了。这张图好认。");
    expect(r.text).toBe("后面是正文。");
  });
});

// ═══════════════════════════════════════════════
// 场景 3：自由偏向
// ═══════════════════════════════════════════════

describe("自由偏向", () => {
  it("未知偏向 → 只定色相，不驱动动作", () => {
    const r = scanTextChunk("s1", "【mood:不耐烦】三格都不确定。\n");
    expect(r.mood).toBeNull();
    expect(r.tint).toBe("不耐烦");
    expect(r.voice).toBe("三格都不确定。");
  });

  it("近义词归一：开心 → 高兴", () => {
    expect(scanTextChunk("s1", "【mood:开心】好啊。\n").mood).toBe("高兴");
  });

  it("近义词归一：想不通 → 困惑", () => {
    expect(scanTextChunk("s1", "【mood:想不通】再算一遍。\n").mood).toBe("困惑");
  });

  it("近义词归一：失落 → 沮丧", () => {
    expect(scanTextChunk("s1", "【mood:失落】没赶上。\n").mood).toBe("沮丧");
  });

  it("色相保留原词而不是归一后的词", () => {
    const r = scanTextChunk("s1", "【mood:雀跃】好耶。\n");
    expect(r.mood).toBe("高兴");
    expect(r.tint).toBe("雀跃");
  });
});

// ═══════════════════════════════════════════════
// 场景 4：段首与上下文
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
    expect(r.text).toBe("博士，晚上好。");
    expect(r.voice).toBe("我查一下。");
  });

  it("行首允许缩进", () => {
    const r = scanTextChunk("s1", "  【mood:高兴】嗯。\n");
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBe("嗯。");
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
// 场景 5：终止边界
// ═══════════════════════════════════════════════

describe("终止边界", () => {
  it("没有换行时，首个句末标点即块尾，其余归正文（流结束/归档）", () => {
    const r = stripTagFromText("【mood:高兴】太好了。我们开始吧。");
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBe("太好了。");
    expect(r.text).toBe("我们开始吧。");
  });

  it("流式没有换行时先不落位，等换行再定界", () => {
    const r = scanTextChunk("s1", "【mood:高兴】太好了。我们开始吧。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("");
    const done = scanTextChunk("s1", "\n正文");
    expect(done.voice).toBe("太好了。我们开始吧。");
    expect(done.text).toBe("正文");
  });

  it("换行定界且心声超 40 字 → 判写错，只剥标签、文字归正文", () => {
    const long = "嗯".repeat(45) + "。";
    const r = scanTextChunk("s1", "【mood:高兴】" + long + "\n短正文");
    expect(r.voice).toBeNull();
    expect(r.text).toBe(long + "\n短正文");
  });

  it("流式累计超 40 字仍无边界 → 判写错，只剥标签、文字全归正文", () => {
    const long = "嗯".repeat(45);
    const r = scanTextChunk("s1", "【mood:高兴】" + long);
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBeNull();
    expect(r.text).toBe(long);
  });

  it("流结束仍在 pending → 整段按正文放行", () => {
    expect(scanTextChunk("s1", "【mood:高").text).toBe("");
    expect(releasePending("s1")).toBe("【mood:高");
  });

  it("未闭合时遇到换行 → 当正文放行", () => {
    const r = scanTextChunk("s1", "【mood:高兴\n继续");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:高兴\n继续");
  });

  it("头部超 40 字未闭合 → 当正文放行", () => {
    const text = "【mood:" + "a".repeat(45);
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });
});

// ═══════════════════════════════════════════════
// 场景 6：跨 chunk 切断
// ═══════════════════════════════════════════════

describe("跨 chunk 切断（pending 扣留）", () => {
  it("头部被切开：扣留直到闭合", () => {
    expect(scanTextChunk("s1", "【mood:高").text).toBe("");
    const r = scanTextChunk("s1", "兴】收到。\n");
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBe("收到。");
    expect(r.text).toBe("");
  });

  it("心声跨 chunk 累积到行尾", () => {
    expect(scanTextChunk("s1", "【mood:困惑】我").text).toBe("");
    expect(scanTextChunk("s1", "查一下。").text).toBe("");
    const r = scanTextChunk("s1", "\n正文");
    expect(r.voice).toBe("我查一下。");
    expect(r.text).toBe("正文");
  });

  it("半标签跨多个 chunk 也不可见", () => {
    expect(scanTextChunk("s1", "【mo").text).toBe("");
    expect(scanTextChunk("s1", "od:沮").text).toBe("");
    const r = scanTextChunk("s1", "丧】嗯。\n");
    expect(r.mood).toBe("沮丧");
    expect(r.voice).toBe("嗯。");
  });

  it("非 mood 前缀（【moodx）直接放行", () => {
    const r = scanTextChunk("s1", "【moodx 不是标签。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【moodx 不是标签。");
  });

  it("标签后紧跟正文的 chunk 一次性求值", () => {
    const r = scanTextChunk("s1", "【mood:高兴】嗯。\n");
    expect(r.mood).toBe("高兴");
    expect(r.voice).toBe("嗯。");
    expect(r.text).toBe("");
  });
});

// ═══════════════════════════════════════════════
// 场景 7：stripTagFromText（归档一次性剥离）
// ═══════════════════════════════════════════════

describe("stripTagFromText 一次性剥离", () => {
  it("整行成块：心声剥离，正文保留", () => {
    const r = stripTagFromText("【mood:困惑】我查一下。\n正文第一行");
    expect(r.mood).toBe("困惑");
    expect(r.voice).toBe("我查一下。");
    expect(r.text).toBe("正文第一行");
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
    const raw = "【mood:沮丧】没赶上。\n再试一次。";
    expect(stripTagFromText(raw).rawText).toBe(raw);
  });
});

// ═══════════════════════════════════════════════
// 场景 8：强度
// ═══════════════════════════════════════════════

describe("强度档位", () => {
  it("标签带强度", () => {
    const r = scanTextChunk("s1", "【mood:高兴:2】好啊。\n");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(2);
    expect(r.voice).toBe("好啊。");
  });

  it("省略强度 → 0", () => {
    expect(scanTextChunk("s1", "【mood:高兴】嗯。\n").intensity).toBe(0);
  });

  it("非法强度 → 0", () => {
    expect(parseMoodHeader("高兴:0")?.intensity).toBe(0);
    expect(parseMoodHeader("高兴:4")?.intensity).toBe(0);
    expect(parseMoodHeader("高兴:x")?.intensity).toBe(0);
    expect(parseMoodHeader("高兴:2")?.intensity).toBe(2);
  });

  it("全角冒号分隔强度", () => {
    const r = scanTextChunk("s1", "【mood:高兴：3】内容\n");
    expect(r.intensity).toBe(3);
  });

  it("空偏向 → parseMoodHeader 返回 null", () => {
    expect(parseMoodHeader("  ")).toBeNull();
    expect(parseMoodHeader(":2")).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// 场景 9：生命周期
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
