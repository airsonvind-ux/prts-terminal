// src/moodTag.test.ts
// 情绪标签解析器测试（批 2-D 动画映射 · 情绪通道）
//
// 覆盖：
//   1. 方括号/中文方括号、全角冒号、大小写、空格变体
//   2. 标签不在第一个字符（短开场白后、换行后）
//   3. 识别区外不处理
//   4. 非法值只剥离、不更新情绪
//   5. 代码/举例上下文不误伤
//   6. 跨 chunk 切断（pending 扣留，用户不可见半标签）
//   7. 段边界 re-arm 与每段最多一个标签
//   8. stripTagFromText 一次性剥离
//   9. releasePending 生命周期

import { describe, it, expect, beforeEach } from "vitest";
import {
  scanTextChunk,
  stripTagFromText,
  rearmScanner,
  releasePending,
  resetScanners,
  segmentConsumed,
} from "./moodTag";

beforeEach(() => resetScanners());

// ═══════════════════════════════════════════════
// 场景 1：格式变体
// ═══════════════════════════════════════════════

describe("格式变体", () => {
  it("中文方括号 + 半角冒号", () => {
    const r = scanTextChunk("s1", "【mood:高兴】好的，博士。");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("好的，博士。");
  });

  it("ASCII 方括号 + 半角冒号", () => {
    const r = scanTextChunk("s1", "[mood:高兴]好的，博士。");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("好的，博士。");
  });

  it("ASCII 方括号 + 全角冒号", () => {
    const r = scanTextChunk("s1", "[mood：沮丧]明白了。");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("明白了。");
  });

  it("大小写、空格与全角冒号混合", () => {
    const r = scanTextChunk("s1", "[ MOOD : 困惑 ] 收到。");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe(" 收到。");
  });
});

// ═══════════════════════════════════════════════
// 场景 2：标签不在段首
// ═══════════════════════════════════════════════

describe("标签不在段首", () => {
  it("短开场白后同一行出现标签 → 仍识别", () => {
    const r = scanTextChunk("s1", "好的，博士。[mood:高兴]我们开始吧。");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("好的，博士。我们开始吧。");
  });

  it("第一行问候、第二行标签 → 仍识别", () => {
    const r = scanTextChunk("s1", "博士，晚上好。\n【mood:困惑】我查一下。");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe("博士，晚上好。\n我查一下。");
  });

  it("识别区外（第 120 字之后）不处理", () => {
    const r = scanTextChunk("s1", "a".repeat(130) + "[mood:高兴]尾部");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("a".repeat(130) + "[mood:高兴]尾部");
  });

  it("识别区外的半标签也不扣留", () => {
    const text = "a".repeat(121) + "[mo";
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });
});

// ═══════════════════════════════════════════════
// 场景 3：非法值
// ═══════════════════════════════════════════════

describe("非法值只剥离", () => {
  it("非法情绪值 → 剥离标签，mood 不更新", () => {
    const r = scanTextChunk("s1", "【mood:愤怒】内容");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("内容");
  });

  it("显式 平静 不在枚举中 → 剥离但不更新", () => {
    const r = scanTextChunk("s1", "【mood:平静】");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("");
  });

  it("跨 chunk 的非法值同样剥离", () => {
    scanTextChunk("s1", "【mood:愤");
    const r = scanTextChunk("s1", "怒】内容");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("内容");
  });
});

// ═══════════════════════════════════════════════
// 场景 4：代码与举例上下文
// ═══════════════════════════════════════════════

describe("代码与举例上下文不误伤", () => {
  it("行内代码中的格式示例 → 原样保留", () => {
    const r = scanTextChunk("s1", "格式是 `[mood:高兴]`，写在开头。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("格式是 `[mood:高兴]`，写在开头。");
  });

  it("代码块中的格式示例 → 原样保留", () => {
    const text = "示例：\n```text\n[mood:高兴]\n```";
    const r = scanTextChunk("s1", text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });

  it("“比如”后的标签 → 视为举例，不剥离", () => {
    const r = scanTextChunk("s1", "比如【mood:高兴】这样写。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("比如【mood:高兴】这样写。");
  });

  it("“格式如下：”下一行的标签 → 视为举例，不剥离", () => {
    const r = scanTextChunk("s1", "格式如下：\n【mood:高兴】");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("格式如下：\n【mood:高兴】");
  });
});

// ═══════════════════════════════════════════════
// 场景 5：跨 chunk 切断（pending 扣留）
// ═══════════════════════════════════════════════

describe("跨 chunk 切断（pending 扣留）", () => {
  it("前缀被切开：扣留直到闭合后才剥离", () => {
    const r1 = scanTextChunk("s1", "【mood:高");
    expect(r1.text).toBe("");
    expect(r1.mood).toBeNull();

    const r2 = scanTextChunk("s1", "兴】收到。");
    expect(r2.mood).toBe("高兴");
    expect(r2.text).toBe("收到。");
  });

  it("标签前已有文本时，文本先放行，标签继续扣留", () => {
    const r1 = scanTextChunk("s1", "好的，[mood:高");
    expect(r1.text).toBe("好的，");
    expect(r1.mood).toBeNull();

    const r2 = scanTextChunk("s1", "兴】继续。");
    expect(r2.mood).toBe("高兴");
    expect(r2.text).toBe("继续。");
  });

  it("非 mood 前缀（【moodx）直接放行", () => {
    const r = scanTextChunk("s1", "【moodx 不是标签。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【moodx 不是标签。");
  });

  it("真实流式逐字切开中文括号标签也能闭合", () => {
    const parts = ["【", "m", "ood", ":", "困惑", "】\n\n"];
    let closed = null;
    for (const part of parts) closed = scanTextChunk("s1", part);
    expect(closed?.mood).toBe("困惑");
    expect(closed?.text).toBe("\n\n");
    expect(segmentConsumed("s1")).toBe(true);
  });

  it("半标签跨多个 chunk 也不可见", () => {
    expect(scanTextChunk("s1", "【mo").text).toBe("");
    expect(scanTextChunk("s1", "od:沮").text).toBe("");
    const r = scanTextChunk("s1", "丧】嗯。");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("嗯。");
  });
});

// ═══════════════════════════════════════════════
// 场景 6：pending 超上界 / 换行
// ═══════════════════════════════════════════════

describe("pending 超上界或跨行放行", () => {
  it("超过 40 字符未闭合 → 放行", () => {
    let r = scanTextChunk("s1", "【mood:" + "a".repeat(34));
    expect(r.text).toBe("");
    expect(r.mood).toBeNull();

    r = scanTextChunk("s1", "b"); // 41 字符：超界 → 放行
    expect(r.text).toBe("【mood:" + "a".repeat(34) + "b");
    expect(r.mood).toBeNull();
  });

  it("未闭合时遇到换行 → 放行", () => {
    const r = scanTextChunk("s1", "【mood:高兴\n继续");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:高兴\n继续");
  });
});

// ═══════════════════════════════════════════════
// 场景 7：段边界与每段最多一个
// ═══════════════════════════════════════════════

describe("段边界与每段最多一个", () => {
  it("tool/call 段边界 re-arm 后可再匹配", () => {
    scanTextChunk("s1", "我先查一下");
    expect(rearmScanner("s1")).toBeNull();

    const r = scanTextChunk("s1", "【mood:困惑】查到了。");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe("查到了。");
  });

  it("re-arm 释放未闭合 pending", () => {
    scanTextChunk("s1", "【mood:高");
    const held = rearmScanner("s1");
    expect(held).toBe("【mood:高");

    const r = scanTextChunk("s1", "【mood:高兴】新段");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("新段");
  });

  it("段内第二个标签不处理", () => {
    scanTextChunk("s1", "【mood:高兴】第一段");
    const r = scanTextChunk("s1", "【mood:沮丧】还在同一段");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:沮丧】还在同一段");
  });
});

// ═══════════════════════════════════════════════
// 场景 8：stripTagFromText 一次性剥离
// ═══════════════════════════════════════════════

describe("stripTagFromText 一次性剥离", () => {
  it("全角冒号标签被剥离（容错）", () => {
    const r = stripTagFromText("【mood：高兴】内容");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("内容");
  });

  it("ASCII 方括号标签被剥离", () => {
    const r = stripTagFromText("[mood:沮丧]");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("");
  });

  it("无标签 → 原样返回", () => {
    const r = stripTagFromText("没有标签的文本");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("没有标签的文本");
  });

  it("流式闭合后的归档文本仍可一次性剥离", () => {
    const r = stripTagFromText("【mood:困惑】\n\n正文");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe("\n\n正文");
  });

  it("非法值只剥离", () => {
    const r = stripTagFromText("【mood:愤怒】提示");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("提示");
  });

  it("识别区外不剥离", () => {
    const text = "a".repeat(130) + "【mood:高兴】";
    const r = stripTagFromText(text);
    expect(r.mood).toBeNull();
    expect(r.text).toBe(text);
  });
});

// ═══════════════════════════════════════════════
// 场景 9：releasePending 生命周期
// ═══════════════════════════════════════════════

describe("releasePending 生命周期", () => {
  it("未闭合 pending 释放并清空扫描器状态", () => {
    scanTextChunk("s1", "【mood:高");
    const held = releasePending("s1");
    expect(held).toBe("【mood:高");

    const r = scanTextChunk("s1", "【mood:高兴】新轮");
    expect(r.mood).toBe("高兴");
  });

  it("无 pending / 无状态 → 返回 null", () => {
    expect(releasePending("s1")).toBeNull();
  });

  it("已消费标签后 releasePending → null", () => {
    scanTextChunk("s1", "【mood:高兴】");
    expect(releasePending("s1")).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// 场景 10：强度档位解析
// ═══════════════════════════════════════════════

describe("强度档位解析", () => {
  it("标签带强度", () => {
    const r = scanTextChunk("s1", "【mood:高兴:2】内容");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(2);
    expect(r.text).toBe("内容");
  });

  it("省略强度 → 0", () => {
    const r = scanTextChunk("s1", "【mood:高兴】内容");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(0);
  });

  it("非法强度 → 回退 0", () => {
    expect(scanTextChunk("s1", "【mood:高兴:0】a").intensity).toBe(0);
    expect(scanTextChunk("s2", "【mood:高兴:4】a").intensity).toBe(0);
    expect(scanTextChunk("s3", "【mood:高兴:x】a").intensity).toBe(0);
    expect(scanTextChunk("s4", "【mood:高兴:2】a").intensity).toBe(2);
  });

  it("全角冒号分隔强度", () => {
    const r = scanTextChunk("s1", "【mood:高兴：3】内容");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(3);
    expect(r.text).toBe("内容");
  });

  it("stripTagFromText 同样解析强度", () => {
    const r = stripTagFromText("【mood:沮丧:1】提示");
    expect(r.mood).toBe("沮丧");
    expect(r.intensity).toBe(1);
    expect(r.text).toBe("提示");
  });

  it("segmentConsumed：匹配后 true；段边界重置", () => {
    expect(segmentConsumed("s1")).toBe(false);
    scanTextChunk("s1", "【mood:高兴】内容");
    expect(segmentConsumed("s1")).toBe(true);
    rearmScanner("s1");
    expect(segmentConsumed("s1")).toBe(false);
    expect(segmentConsumed("s2")).toBe(false);
  });
});
