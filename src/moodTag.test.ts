// src/moodTag.test.ts
// 情绪标签解析器测试（批 2-D 动画映射 · 情绪通道）
//
// 覆盖：
//   1. 单 chunk 完整标签剥离
//   2. 跨 chunk 切断（pending 扣留，用户不可见半标签）
//   3. 非标签开头放行（含【开头但不是 mood 标签）
//   4. 非法值回退平静（标签全量放行、mood 不更新）
//   5. pending 超上界未闭合 → 全量放行
//   6. 段中标签不剥离（原样渲染）；tool/call 段边界 re-arm 后可再匹配
//   7. 每轮最多消费一个标签
//   8. stripTagFromText 一次性剥离（全角冒号容错）
//   9. releasePending 生命周期（finish/fail 路径放行）

import { describe, it, expect, beforeEach } from "vitest";
import {
  scanTextChunk,
  stripTagFromText,
  rearmScanner,
  releasePending,
  resetScanners,
  consumedIn,
} from "./moodTag";

beforeEach(() => resetScanners());

// ═══════════════════════════════════════════════
// 场景 1：单 chunk 完整标签剥离
// ═══════════════════════════════════════════════

describe("单 chunk 完整标签剥离", () => {
  it("段首完整标签 → 剥离 + 返回 mood", () => {
    const r = scanTextChunk("s1", "【mood:高兴】好的，博士。");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("好的，博士。");
  });

  it("整段只有标签 → 剥离后为空（调用方不进缓冲）", () => {
    const r = scanTextChunk("s1", "【mood:困惑】");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe("");
  });

  it("全角冒号同样匹配", () => {
    const r = scanTextChunk("s1", "【mood：沮丧】明白了。");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("明白了。");
  });
});

// ═══════════════════════════════════════════════
// 场景 2：跨 chunk 切断（pending 扣留）
// ═══════════════════════════════════════════════

describe("跨 chunk 切断（pending 扣留）", () => {
  it("前缀被切开：扣留直到闭合后才剥离", () => {
    const r1 = scanTextChunk("s1", "【mood:高");
    expect(r1.text).toBe(""); // 扣留：不进缓冲
    expect(r1.mood).toBeNull();

    const r2 = scanTextChunk("s1", "兴】收到。");
    expect(r2.mood).toBe("高兴");
    expect(r2.text).toBe("收到。");
  });

  it("扣留期间半标签对用户不可见（后续 chunk 也不进缓冲）", () => {
    expect(scanTextChunk("s1", "【mo").text).toBe("");
    expect(scanTextChunk("s1", "od:沮").text).toBe("");
    const r = scanTextChunk("s1", "丧】嗯。");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("嗯。");
  });
});

// ═══════════════════════════════════════════════
// 场景 3：非标签开头放行
// ═══════════════════════════════════════════════

describe("非标签开头放行", () => {
  it("普通文本立即放行", () => {
    const r = scanTextChunk("s1", "博士，下午好。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("博士，下午好。");
  });

  it("【开头但不是 mood 标签 → 立即放行", () => {
    const r = scanTextChunk("s1", "【注】这不是情绪标签。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【注】这不是情绪标签。");
  });

  it("空 chunk 不消耗段首匹配机会（后续真标签仍可匹配）", () => {
    expect(scanTextChunk("s1", "").text).toBe("");
    const r = scanTextChunk("s1", "【mood:高兴】内容");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("内容");
  });

  it("前缀中途分叉（【moodx）→ 放行，不进入 pending", () => {
    const r = scanTextChunk("s1", "【moodx 不是标签。");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【moodx 不是标签。");
  });
});

// ═══════════════════════════════════════════════
// 场景 4：非法值回退平静
// ═══════════════════════════════════════════════

describe("非法值回退平静", () => {
  it("非法情绪值 → 全量放行（标签原样渲染），mood 不更新", () => {
    const r = scanTextChunk("s1", "【mood:愤怒】内容");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:愤怒】内容");
  });

  it("显式 平静 不在枚举中 → 视为非法（省略才是平静）", () => {
    const r = scanTextChunk("s1", "【mood:平静】");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:平静】");
  });

  it("跨 chunk 的非法值同样全量放行", () => {
    scanTextChunk("s1", "【mood:愤");
    const r = scanTextChunk("s1", "怒】内容");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:愤怒】内容");
  });
});

// ═══════════════════════════════════════════════
// 场景 5：pending 超上界放行
// ═══════════════════════════════════════════════

describe("pending 超上界放行", () => {
  it("24 字符未闭合仍扣留，第 25 字符触发放行", () => {
    let r = scanTextChunk("s1", "【mood:"); // 6 字符
    expect(r.text).toBe("");
    r = scanTextChunk("s1", "a".repeat(17)); // 23 字符
    expect(r.text).toBe("");
    r = scanTextChunk("s1", "b"); // 24 字符：仍在上界内
    expect(r.text).toBe("");
    r = scanTextChunk("s1", "c"); // 25 字符：超界 → 全量放行
    expect(r.text).toBe("【mood:" + "a".repeat(17) + "bc");
    expect(r.mood).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// 场景 6：段中标签不剥离 + 段边界 re-arm
// ═══════════════════════════════════════════════

describe("段中标签与段边界", () => {
  it("段内已放行文本后再出现标签 → 原样渲染（段中标签不剥离）", () => {
    scanTextChunk("s1", "好的，");
    const r = scanTextChunk("s1", "【mood:高兴】继续");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:高兴】继续");
  });

  it("tool/call 段边界 re-arm 后，段首标签可再匹配", () => {
    scanTextChunk("s1", "我先查一下"); // 放行
    expect(rearmScanner("s1")).toBeNull(); // 无 pending

    const r = scanTextChunk("s1", "【mood:困惑】查到了。");
    expect(r.mood).toBe("困惑");
    expect(r.text).toBe("查到了。");
  });

  it("re-arm 释放未闭合 pending（扣留文本放行）", () => {
    scanTextChunk("s1", "【mood:高"); // 扣留
    const held = rearmScanner("s1");
    expect(held).toBe("【mood:高");

    // re-arm 后新段可再匹配
    const r = scanTextChunk("s1", "【mood:高兴】新段");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("新段");
  });
});

// ═══════════════════════════════════════════════
// 场景 7：每轮最多消费一个标签
// ═══════════════════════════════════════════════

describe("每轮最多消费一个标签", () => {
  it("首次成功匹配后关闭本轮匹配机会（re-arm 也不恢复）", () => {
    scanTextChunk("s1", "【mood:高兴】第一段");
    rearmScanner("s1");
    const r = scanTextChunk("s1", "【mood:沮丧】第二段");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:沮丧】第二段");
  });
});

// ═══════════════════════════════════════════════
// 场景 8：stripTagFromText 一次性剥离（assistant/message 路径）
// ═══════════════════════════════════════════════

describe("stripTagFromText 一次性剥离", () => {
  it("全角冒号标签被剥离（容错）", () => {
    const r = stripTagFromText("【mood：高兴】内容");
    expect(r.mood).toBe("高兴");
    expect(r.text).toBe("内容");
  });

  it("半角冒号标签被剥离", () => {
    const r = stripTagFromText("【mood:沮丧】");
    expect(r.mood).toBe("沮丧");
    expect(r.text).toBe("");
  });

  it("无标签开头 → 原样返回", () => {
    const r = stripTagFromText("没有标签的文本");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("没有标签的文本");
  });

  it("非法值不剥离（与扫描器全量放行一致，归档与流式视图保持一致）", () => {
    const r = stripTagFromText("【mood:愤怒】提示");
    expect(r.mood).toBeNull();
    expect(r.text).toBe("【mood:愤怒】提示");
  });
});

// ═══════════════════════════════════════════════
// 场景 9：releasePending 生命周期（finish/fail 路径）
// ═══════════════════════════════════════════════

describe("releasePending 生命周期", () => {
  it("未闭合 pending 释放并清空扫描器状态", () => {
    scanTextChunk("s1", "【mood:高");
    const held = releasePending("s1");
    expect(held).toBe("【mood:高");

    // 状态已清空：新轮（新 sessionId 或复用）从干净状态开始
    const r = scanTextChunk("s1", "【mood:高兴】新轮");
    expect(r.mood).toBe("高兴");
  });

  it("无 pending / 无状态 → 返回 null", () => {
    expect(releasePending("s1")).toBeNull();
  });

  it("已消费标签后 releasePending → null（无 pending 可释放）", () => {
    scanTextChunk("s1", "【mood:高兴】");
    expect(releasePending("s1")).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// 场景 10：强度档位解析（【mood:情绪:N】，N=1~3 加深，省略=0）
// ═══════════════════════════════════════════════

describe("强度档位解析", () => {
  it("标签带强度：【mood:高兴:2】→ mood=高兴 + intensity=2", () => {
    const r = scanTextChunk("s1", "【mood:高兴:2】内容");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(2);
    expect(r.text).toBe("内容");
  });

  it("省略强度 → intensity=0（基础档）", () => {
    const r = scanTextChunk("s1", "【mood:高兴】内容");
    expect(r.mood).toBe("高兴");
    expect(r.intensity).toBe(0);
  });

  it("非法强度（0、4、非数字）→ 回退 0", () => {
    expect(scanTextChunk("s1", "【mood:高兴:0】a").intensity).toBe(0);
    expect(scanTextChunk("s2", "【mood:高兴:4】a").intensity).toBe(0);
    expect(scanTextChunk("s3", "【mood:高兴:x】a").intensity).toBe(0);
    expect(scanTextChunk("s4", "【mood:高兴:2】a").intensity).toBe(2);
  });

  it("全角冒号分隔强度：【mood:高兴：3】", () => {
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

  it("consumedIn：匹配成功返回 true，未匹配返回 false", () => {
    expect(consumedIn("s1")).toBe(false);
    scanTextChunk("s1", "【mood:高兴】内容");
    expect(consumedIn("s1")).toBe(true);
    expect(consumedIn("s2")).toBe(false);
  });
});
