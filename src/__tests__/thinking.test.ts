// src/__tests__/thinking.test.ts
// 思考强度派生（src/thinking.ts）· 与后端 thinking.rs::resolve_requested 同口径

import { describe, expect, it } from "vitest";
import { resolveThinking, thinkingLabel } from "../thinking";

describe("thinking 派生", () => {
  it("模型不支持推理 → 没有档位（整段不显示）", () => {
    expect(resolveThinking("high", { supportsReasoning: false })).toBeNull();
    expect(resolveThinking("high", null)).toBeNull();
    expect(thinkingLabel("high", { supportsReasoning: false })).toBeNull();
  });

  it("用户档位在白名单内 → 用它，不标默认", () => {
    const m = {
      supportsReasoning: true,
      thinkingLevels: ["low", "high"],
      defaultThinkingLevel: "low",
    };
    expect(resolveThinking("high", m)?.effective).toBe("high");
    expect(thinkingLabel("high", m)).toBe("高");
  });

  it("越界 / 未设 → 回落到模型默认档，并标「（默认）」", () => {
    const m = {
      supportsReasoning: true,
      thinkingLevels: ["low", "high"],
      defaultThinkingLevel: "high",
    };
    expect(resolveThinking("max", m)?.effective).toBe("high");
    expect(thinkingLabel("max", m)).toBe("高（默认）");
    expect(thinkingLabel(null, m)).toBe("高（默认）");
    // 用户选的正好等于默认档 → 不算"没选过"
    expect(thinkingLabel("high", m)).toBe("高");
  });

  it("模型未声明白名单 / 默认 → 推断集 [off,medium,high] + medium", () => {
    const m = { supportsReasoning: true };
    expect(resolveThinking("", m)?.levels).toEqual(["off", "medium", "high"]);
    expect(thinkingLabel("", m)).toBe("中（默认）");
    expect(thinkingLabel("high", m)).toBe("高");
  });
});
