// src/stores/__tests__/deriveUIState.test.ts
// deriveUIState 纯函数单元测试
//
// 覆盖目标：
//   优先级链五个分支（hidden > error > ready > streaming > processing）
//   两个 hiddenMode 子态（shortcut / edge_snap）
//   48 种输入组合中集成测试不会触发的跨优先级边界

import { describe, it, expect } from "vitest";
import { deriveUIState } from "../useUIStore";
import type { HiddenMode } from "../../types";

// ── 辅助：渲染可读的参数标签 ──
// (hiddenMode, activeId, error, agentPhase) → 期望输出

// ═══ 优先级链：hidden > error > ready > streaming > processing ═══

describe("deriveUIState · 优先级链", () => {
  // ── hidden 优先（覆盖所有其他参数） ──

  describe("hiddenMode !== null → 始终返回 'hidden'", () => {
    const hiddenCases: [string, HiddenMode][] = [
      ["快捷键隐藏", "shortcut"],
      ["边缘吸附隐藏", "edge_snap"],
    ];

    it.each(hiddenCases)("%s", (_, hiddenMode) => {
      // 即使其他参数处于各种有意义的状态，hidden 始终胜出
      expect(deriveUIState(hiddenMode, "conv-1", null, "streaming")).toBe("hidden");
      expect(deriveUIState(hiddenMode, "conv-1", "数据库错误", "streaming")).toBe("hidden");
      expect(deriveUIState(hiddenMode, null, null, undefined)).toBe("hidden");
      expect(deriveUIState(hiddenMode, null, "错误", undefined)).toBe("hidden");
    });

    it("hiddenMode 为 null 时不触发 hidden", () => {
      expect(deriveUIState(null, "conv-1", null, "reasoning")).toBe("processing");
    });
  });

  // ── error 优先（hidden 关闭时，error 覆盖 processing/streaming） ──

  describe("hidden === null 且 error !== null → 返回 'error'", () => {
    it("error 覆盖 streaming 状态", () => {
      expect(deriveUIState(null, "conv-1", "数据库连接失败", "streaming")).toBe("error");
    });

    it("error 覆盖 processing 状态", () => {
      expect(deriveUIState(null, "conv-1", "超时", "reasoning")).toBe("error");
    });

    it("即使没有活跃对话，error 仍生效", () => {
      expect(deriveUIState(null, null, "初始化失败", undefined)).toBe("error");
    });
  });

  // ── ready（无隐藏、无错误、无处理中对话） ──

  describe("ready 状态的两条路径", () => {
    it("没有活跃对话 → ready", () => {
      expect(deriveUIState(null, null, null, undefined)).toBe("ready");
    });

    it("有活跃对话但 processing 快照不存在 → ready", () => {
      expect(deriveUIState(null, "conv-1", null, undefined)).toBe("ready");
    });
  });

  // ── streaming ──

  describe("streaming 状态", () => {
    it("agentPhase === 'streaming' 时返回 'streaming'", () => {
      expect(deriveUIState(null, "conv-1", null, "streaming")).toBe("streaming");
    });
  });

  // ── processing（兜底：不是 hidden/error/ready/streaming 就是 processing） ──

  describe("processing 状态（兜底分支）", () => {
    it("agentPhase === 'reasoning' → processing", () => {
      expect(deriveUIState(null, "conv-1", null, "reasoning")).toBe("processing");
    });

    it("agentPhase === 'processing' → processing", () => {
      expect(deriveUIState(null, "conv-1", null, "processing")).toBe("processing");
    });
  });
});

// ═══ 跨优先级组合（集成测试不会触发的边界） ═══

describe("deriveUIState · 跨优先级组合", () => {
  it("同时存在 error 和 processing 快照：error 优先", () => {
    // 场景：Agent 处理中发生不可恢复错误，failProcessing 设置了 errorMsg
    expect(deriveUIState(null, "conv-1", "处理失败", "reasoning")).toBe("error");
  });

  it("error 被清除后回归正常状态", () => {
    // 隐式验证：clearError() → errorMsg 变为 null → deriveUIState 不再走 error 分支
    expect(deriveUIState(null, "conv-1", null, "reasoning")).toBe("processing");
    expect(deriveUIState(null, "conv-1", null, "streaming")).toBe("streaming");
  });

  it("hiddenMode 从 'shortcut' 恢复为 null 后状态回归", () => {
    // 快捷键隐藏 → 再次显示 → 回到之前的状态
    expect(deriveUIState(null, "conv-1", null, "streaming")).toBe("streaming");
  });
});
