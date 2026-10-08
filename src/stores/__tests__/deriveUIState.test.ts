// src/stores/__tests__/deriveUIState.test.ts
// deriveUIState 纯函数单元测试
//
// 覆盖目标：
//   优先级链四个分支（error > ready > streaming > processing）
//   16 种输入组合中集成测试不会触发的跨优先级边界
//
// 2026-10-08：hidden 状态与 HiddenMode 随窗口隐藏体系一并删除，
// hiddenMode 参数与两条 hidden 用例同步移除。

import { describe, it, expect } from "vitest";
import { deriveUIState } from "../useUIStore";

// ── 辅助：渲染可读的参数标签 ──
// (activeId, error, agentPhase) → 期望输出

// ═══ 优先级链：error > ready > streaming > processing ═══

describe("deriveUIState · 优先级链", () => {
  // ── error 优先（覆盖 processing / streaming） ──

  describe("error !== null → 返回 'error'", () => {
    it("error 覆盖 streaming 状态", () => {
      expect(deriveUIState("conv-1", "数据库连接失败", "streaming")).toBe("error");
    });

    it("error 覆盖 processing 状态", () => {
      expect(deriveUIState("conv-1", "超时", "reasoning")).toBe("error");
    });

    it("即使没有活跃对话，error 仍生效", () => {
      expect(deriveUIState(null, "初始化失败", undefined)).toBe("error");
    });
  });

  // ── ready（无错误、无处理中对话） ──

  describe("ready 状态的两条路径", () => {
    it("没有活跃对话 → ready", () => {
      expect(deriveUIState(null, null, undefined)).toBe("ready");
    });

    it("有活跃对话但 processing 快照不存在 → ready", () => {
      expect(deriveUIState("conv-1", null, undefined)).toBe("ready");
    });
  });

  // ── streaming ──

  describe("streaming 状态", () => {
    it("agentPhase === 'streaming' 时返回 'streaming'", () => {
      expect(deriveUIState("conv-1", null, "streaming")).toBe("streaming");
    });
  });

  // ── processing（兜底：不是 error / ready / streaming 就是 processing） ──

  describe("processing 状态（兜底分支）", () => {
    it("agentPhase === 'reasoning' → processing", () => {
      expect(deriveUIState("conv-1", null, "reasoning")).toBe("processing");
    });

    it("agentPhase === 'processing' → processing", () => {
      expect(deriveUIState("conv-1", null, "processing")).toBe("processing");
    });
  });
});

// ═══ 跨优先级组合（集成测试不会触发的边界） ═══

describe("deriveUIState · 跨优先级组合", () => {
  it("同时存在 error 和 processing 快照：error 优先", () => {
    // 场景：Agent 处理中发生不可恢复错误，failProcessing 设置了 errorMsg
    expect(deriveUIState("conv-1", "处理失败", "reasoning")).toBe("error");
  });

  it("error 被清除后回归正常状态", () => {
    // 隐式验证：clearError() → errorMsg 变为 null → deriveUIState 不再走 error 分支
    expect(deriveUIState("conv-1", null, "reasoning")).toBe("processing");
    expect(deriveUIState("conv-1", null, "streaming")).toBe("streaming");
  });

  it("快照缺失（轮次收尾后）→ ready，而不是停留在 streaming", () => {
    expect(deriveUIState("conv-1", null, undefined)).toBe("ready");
  });
});
