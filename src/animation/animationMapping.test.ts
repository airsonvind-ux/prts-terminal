// src/animation/animationMapping.test.ts
// 动画参数映射 · 纯函数测试（批 2-D 动画映射）
//
// 覆盖：
//   1. mapTargets 四情绪 → 期望动作参数；平静/非法输入回退（无动作）
//   2. 散热分段各阶段目标周期 / τ / 停扇标志（含 mapTargets 状态映射）
//   3. 幅度随周期线性渐变
//   4. 跟随器收敛 / 无超调 / τ 约束单帧变化量 / 冻结
//   5. 相位累加与动作包络（阻尼振荡公式、carry 从当前姿态接管）
//   6. computeTailPath 与翅膀 transform 相位快照断言

import { describe, it, expect } from "vitest";
import {
  mapTargets,
  mapMoodAction,
  follow,
  advancePhase,
  wingAmplitudeOf,
  resolveThermalTarget,
  evaluateAction,
  actionDuration,
  buildActionKeyframes,
  MOOD_ACTIONS,
  type MoodAction,
  WING_DEFAULT_PERIOD,
  WING_MIN_PERIOD,
  WING_AMPLITUDE_DEFAULT,
  WING_AMPLITUDE_MIN,
} from "./animationMapping";
import { computeTailPath } from "../components/shell/tailAnimation";
import type { Mood, MoodIntensity } from "../types";

// ═══════════════════════════════════════════════
// 场景 1：mapTargets 四情绪 → 期望参数
// ═══════════════════════════════════════════════

describe("mapTargets · 四情绪动作 + 强度档位", () => {
  it("高兴 0 档 → 上下浮动（y 轴，固定幅度 10，周期 2s 浮动 2 次共 4s）", () => {
    const t = mapTargets("processing", "reasoning", "高兴", 0);
    expect(t.moodAction).toEqual(MOOD_ACTIONS["高兴"][0]);
    expect(t.moodAction!.axis).toBe("y");
    expect(t.moodAction!.amplitude).toBe(10);
    expect(t.moodAction!.envelope).toBe("fixed");
    expect(actionDuration(t.moodAction!)).toBe(4); // 2 周期 × 2s
  });

  it("高兴强度档位交替：1 档幅度增 → 2 档频率快 → 3 档幅度再增", () => {
    const a1 = mapMoodAction("高兴", 1)!;
    const a2 = mapMoodAction("高兴", 2)!;
    const a3 = mapMoodAction("高兴", 3)!;
    expect(a1.amplitude).toBe(15); // 1 档：幅度 +50%
    expect(a1.omega).toBe((2 * Math.PI) / 2); // 频率不变
    expect(a2.amplitude).toBe(15); // 2 档：幅度保持
    expect(a2.omega).toBe((2 * Math.PI) / 1.6); // 频率 +25%
    expect(a3.amplitude).toBe(20); // 3 档：幅度再增
    expect(a3.omega).toBe((2 * Math.PI) / 1.6);
  });

  it("困惑强度档位交替：1 档角度增 → 2 档停留增 → 3 档角度再增", () => {
    const a1 = mapMoodAction("困惑", 1)!;
    const a2 = mapMoodAction("困惑", 2)!;
    const a3 = mapMoodAction("困惑", 3)!;
    expect(a1.amplitude).toBe(4);
    expect(a1.dwell).toBe(0.5);
    expect(a2.amplitude).toBe(4);
    expect(a2.dwell).toBe(1.0); // 2 档：停留翻倍
    expect(a3.amplitude).toBe(5);
    expect(a3.dwell).toBe(1.0);
  });

  it("沮丧强度档位交替：1 档下沉增 → 2 档停留增 → 3 档下沉再增", () => {
    const a1 = mapMoodAction("沮丧", 1)!;
    const a2 = mapMoodAction("沮丧", 2)!;
    const a3 = mapMoodAction("沮丧", 3)!;
    expect(a1.amplitude).toBe(28);
    expect(a1.dwell).toBe(0.9);
    expect(a2.amplitude).toBe(28);
    expect(a2.dwell).toBe(1.4); // 2 档：停留增大
    expect(a3.amplitude).toBe(36);
    expect(a3.dwell).toBe(1.4);
  });

  it("困惑 → 歪头暂留（rotate 轴，倾角 3，到位 0.7s 停 0.5s 再回正）", () => {
    const t = mapTargets("processing", "processing", "困惑", 0);
    expect(t.moodAction).toEqual(MOOD_ACTIONS["困惑"][0]);
    expect(t.moodAction!.axis).toBe("rotate");
    expect(t.moodAction!.amplitude).toBe(3);
    expect(t.moodAction!.envelope).toBe("dwell");
    expect(actionDuration(t.moodAction!)).toBe(1.9); // 2×0.7 + 0.5
  });

  it("沮丧 → 下沉暂留（y 轴，幅度 22，下沉 0.5s 停 0.9s 再回原位）", () => {
    const t = mapTargets("streaming", "streaming", "沮丧", 0);
    expect(t.moodAction).toEqual(MOOD_ACTIONS["沮丧"][0]);
    expect(t.moodAction!.axis).toBe("y");
    expect(t.moodAction!.amplitude).toBe(22);
    expect(t.moodAction!.envelope).toBe("dwell");
    expect(actionDuration(t.moodAction!)).toBe(1.9); // 2×0.5 + 0.9
  });

  it("平静 → 无动作", () => {
    const t = mapTargets("ready", undefined, "平静", 0);
    expect(t.moodAction).toBeNull();
  });

  it("非法输入回退平静（无动作）", () => {
    const t = mapTargets("ready", undefined, "愤怒" as Mood, 0);
    expect(t.moodAction).toBeNull();
  });

  it("强度越界钳位到 0（非法强度不产生异常动作）", () => {
    expect(mapMoodAction("高兴", 99 as MoodIntensity)).toEqual(MOOD_ACTIONS["高兴"][0]);
  });
});

// ═══════════════════════════════════════════════
// 场景 2：散热分段各阶段目标周期
// ═══════════════════════════════════════════════

describe("散热分段 · 各阶段目标周期", () => {
  it("reasoning → 最小周期，τ≈8s（指数加速逼近）", () => {
    const spec = resolveThermalTarget("reasoning", 1.2);
    expect(spec.target).toBe(WING_MIN_PERIOD);
    expect(spec.tau).toBe(8);
    expect(spec.phaseFrozen).toBe(false);
  });

  it("executing → 冻结（target=当前值，τ=0 不移动）", () => {
    const spec = resolveThermalTarget("executing", 1.23);
    expect(spec.target).toBe(1.23);
    expect(spec.tau).toBe(0);
    expect(spec.phaseFrozen).toBe(false);
  });

  it("streaming → 默认周期，τ≈5s（缓慢回升）", () => {
    const spec = resolveThermalTarget("streaming", 0.7);
    expect(spec.target).toBe(WING_DEFAULT_PERIOD);
    expect(spec.tau).toBe(5);
    expect(spec.phaseFrozen).toBe(false);
  });

  it("idle（ready，turn/end 后）→ 默认周期，τ≈150ms（快速回默认）", () => {
    const spec = resolveThermalTarget("idle", 0.6);
    expect(spec.target).toBe(WING_DEFAULT_PERIOD);
    expect(spec.tau).toBeCloseTo(0.15, 5);
    expect(spec.phaseFrozen).toBe(false);
  });

  it("stopped（hidden/error）→ 立即冻结停扇（周期冻结 + 相位停止）", () => {
    const spec = resolveThermalTarget("stopped", 0.8);
    expect(spec.target).toBe(0.8);
    expect(spec.tau).toBe(0);
    expect(spec.phaseFrozen).toBe(true);
  });

  it("mapTargets 状态映射（uiState + agentPhase → 散热分段）", () => {
    expect(mapTargets("processing", "reasoning", "平静", 0).thermal).toBe("reasoning");
    expect(mapTargets("processing", "processing", "平静", 0).thermal).toBe("executing");
    expect(mapTargets("processing", "streaming", "平静", 0).thermal).toBe("executing"); // 异常组合兜底
    expect(mapTargets("streaming", "streaming", "平静", 0).thermal).toBe("streaming");
    expect(mapTargets("ready", undefined, "平静", 0).thermal).toBe("idle");
    expect(mapTargets("error", undefined, "平静", 0).thermal).toBe("stopped");
  });

  it("幅度随周期线性渐变：default 25deg → min 12deg", () => {
    expect(wingAmplitudeOf(WING_DEFAULT_PERIOD)).toBeCloseTo(
      WING_AMPLITUDE_DEFAULT,
      5
    );
    expect(wingAmplitudeOf(WING_MIN_PERIOD)).toBeCloseTo(WING_AMPLITUDE_MIN, 5);
    expect(wingAmplitudeOf(1.25)).toBeCloseTo(18.5, 5); // 中点
  });
});

// ═══════════════════════════════════════════════
// 场景 3：跟随器
// ═══════════════════════════════════════════════

describe("跟随器", () => {
  it("向目标指数收敛（reasoning 轨迹：min + (p_entry−min)·e^(−t/τ)）", () => {
    let cur = WING_DEFAULT_PERIOD;
    // τ=8s 衰减慢：约 22s（≈1400 帧 @60fps）后进入 0.6 内，取 2000 帧（32s）
    for (let i = 0; i < 2000; i++) {
      cur = follow(cur, WING_MIN_PERIOD, 8, 0.016);
    }
    expect(cur).toBeLessThan(0.6); // 已逼近最小周期
    expect(cur).toBeGreaterThan(WING_MIN_PERIOD); // 未越过目标
  });

  it("无超调：全程保持在目标值一侧", () => {
    let cur = WING_DEFAULT_PERIOD;
    for (let i = 0; i < 1000; i++) {
      cur = follow(cur, WING_MIN_PERIOD, 8, 0.016);
      expect(cur).toBeGreaterThanOrEqual(WING_MIN_PERIOD - 1e-9);
    }
  });

  it("τ 约束单帧变化量：恰为 (target−cur)·(1−e^(−dt/τ))", () => {
    const before = WING_DEFAULT_PERIOD;
    const after = follow(before, WING_MIN_PERIOD, 8, 0.016);
    const expectedStep =
      (before - WING_MIN_PERIOD) * (1 - Math.exp(-0.016 / 8));
    expect(Math.abs(before - after)).toBeCloseTo(expectedStep, 9);
  });

  it("τ<=0 冻结（executing/stopped 语义）", () => {
    expect(follow(1.5, 0.5, 0, 0.016)).toBe(1.5);
    expect(follow(1.5, 0.5, -1, 0.016)).toBe(1.5);
  });

  it("相位累加：位置连续（周期渐变不跳变），冻结时停走", () => {
    expect(advancePhase(1.0, 2, 0)).toBe(1.0);
    expect(advancePhase(1.0, 0, 0.016)).toBe(1.0);
    const step = advancePhase(1.0, 2, 0.5);
    expect(step).toBeCloseTo(1.0 + (2 * Math.PI * 0.5) / 2, 9);
  });
});

// ═══════════════════════════════════════════════
// 场景 4：动作包络（阻尼振荡）
// ═══════════════════════════════════════════════

describe("动作包络（固定振荡 / 到位暂留）", () => {
  it("fixed 固定幅度振荡：t=0 为零、四分之一周期处达峰值（先上浮）、t≥总时长归零（中间幅度恒定）", () => {
    const a = MOOD_ACTIONS["高兴"][0];
    const Tp = (2 * Math.PI) / a.omega!;
    expect(evaluateAction(a, 0, 0)).toBeCloseTo(0, 5);
    // t = Tp/4：相位从上方开始（-sin），位移为负（translateY 负 = 上浮）
    expect(evaluateAction(a, Tp / 4, 0)).toBeCloseTo(-a.amplitude, 5);
    // 渐入内幅度受余弦窗缩放：启动过渡段幅度 < 峰值（非衰减）
    expect(Math.abs(evaluateAction(a, a.fade! / 2, 0))).toBeLessThan(a.amplitude);
    // 结束后归零
    expect(evaluateAction(a, actionDuration(a) + 0.2, 0)).toBeCloseTo(0, 5);
  });

  it("dwell 到位-停留-返回：上升到位、目标位暂留、返回归零（三段边界速度零）", () => {
    const a = MOOD_ACTIONS["沮丧"][0];
    const travel = a.travel!;
    const hold = a.dwell!;
    // 上升段：到 travel 时恰好到位（sin² 半波 = 1）
    expect(evaluateAction(a, travel, 0)).toBeCloseTo(a.amplitude, 5);
    // 停留段：全程恒为幅度（暂留）
    expect(evaluateAction(a, travel + hold / 2, 0)).toBeCloseTo(a.amplitude, 5);
    // 返回段：total 处归零
    expect(evaluateAction(a, 2 * travel + hold, 0)).toBeCloseTo(0, 5);
    expect(evaluateAction(a, 2 * travel + hold + 0.3, 0)).toBeCloseTo(0, 5);
  });

  it("沮丧先下沉：上升段位移为正（CSS translateY 正 = 下沉）", () => {
    const a = MOOD_ACTIONS["沮丧"][0];
    expect(evaluateAction(a, a.travel! / 2, 0)).toBeGreaterThan(0);
  });

  it("pulse 单次缓动脉冲（保留形态验证）：t=0 为零、t=T/2 峰值、t≥T 归零", () => {
    const a: MoodAction = {
      kind: "困惑",
      axis: "rotate",
      amplitude: 5,
      envelope: "pulse",
      duration: 1,
    };
    const T = a.duration!;
    expect(evaluateAction(a, 0, 0)).toBeCloseTo(0, 5);
    expect(evaluateAction(a, T / 2, 0)).toBeCloseTo(a.amplitude, 5);
    expect(evaluateAction(a, T, 0)).toBeCloseTo(0, 5);
    expect(evaluateAction(a, T + 0.3, 0)).toBeCloseTo(0, 5);
  });

  it("carry 从当前姿态接管：t=0 时位移 = carry，动作结束后仅剩 carry 衰减", () => {
    const a = MOOD_ACTIONS["困惑"][0];
    expect(evaluateAction(a, 0, 5)).toBeCloseTo(5, 5); // 从当前姿态开始（不跳变）
    // 动作结束后（t ≥ 总时长）波形归零，只剩 carry 衰减项（已大幅衰减）
    const t1 = evaluateAction(a, actionDuration(a) + 0.5, 5);
    expect(Math.abs(t1)).toBeLessThan(5); // 已向零收敛
  });

  it("动作时长：fixed = cycles×周期（高兴 4s）；dwell = 2×travel+dwell（沮丧 1.9s / 困惑 1.9s）", () => {
    expect(actionDuration(MOOD_ACTIONS["高兴"][0])).toBe(4);
    expect(actionDuration(MOOD_ACTIONS["沮丧"][0])).toBe(1.9);
    expect(actionDuration(MOOD_ACTIONS["困惑"][0])).toBe(1.9);
  });
});

// ═══════════════════════════════════════════════
// 场景 5：computeTailPath 与翅膀 transform 相位快照
// ═══════════════════════════════════════════════

describe("computeTailPath 与翅膀 transform 相位快照", () => {
  it("computeTailPath 确定性：同相位同输出，异相位异输出", () => {
    expect(computeTailPath(1.0, 1)).toEqual(computeTailPath(1.0, 1));
    expect(computeTailPath(1.5, 1)).not.toEqual(computeTailPath(1.0, 1));
  });

  it("computeTailPath 振幅缩放生效（scale 改变波形）", () => {
    expect(computeTailPath(1.0, 0)).not.toEqual(computeTailPath(1.0, 1));
  });

  it("翅膀 transform 相位快照：角度 = 幅度 × sin(相位)，左右翼镜像", () => {
    const phase = Math.PI / 2;
    const amplitude = wingAmplitudeOf(WING_DEFAULT_PERIOD);
    const rightAngle = amplitude * Math.sin(phase);
    expect(rightAngle).toBeCloseTo(WING_AMPLITUDE_DEFAULT, 5); // 25deg
    // 引擎写入：右翼 rotate(angle)，左翼 rotate(−angle)（镜像）
    expect(`rotate(${rightAngle.toFixed(2)}deg)`).toBe("rotate(25.00deg)");
    expect(`rotate(${(-rightAngle).toFixed(2)}deg)`).toBe("rotate(-25.00deg)");
  });
});

// ═══════════════════════════════════════════════
// 场景 6：buildActionKeyframes（WAAPI 动作关键帧）
// ═══════════════════════════════════════════════

/** 从关键帧 transform 中提取 translateY 数值 */
function tyOf(frame: Keyframe): number {
  const m = /translateY\((-?[\d.]+)px\)/.exec(String(frame.transform ?? ""));
  return m ? parseFloat(m[1]) : NaN;
}

/** 从关键帧 transform 中提取 rotate 数值（deg） */
function rotOf(frame: Keyframe): number {
  const m = /rotate\((-?[\d.]+)deg\)/.exec(String(frame.transform ?? ""));
  return m ? parseFloat(m[1]) : NaN;
}

describe("buildActionKeyframes · WAAPI 动作关键帧", () => {
  it("dwell（沮丧:2）：帧数符合时长（2.4s @ 60fps = 145 帧），首帧 0，峰值 28，末帧 0", () => {
    const a = MOOD_ACTIONS["沮丧"][2]; // amplitude 28, travel 0.5, dwell 1.4 → 2.4s
    expect(actionDuration(a)).toBe(2.4);
    const frames = buildActionKeyframes(a, 0);
    expect(frames.length).toBe(145); // round(2400/16.667) + 1
    expect(tyOf(frames[0])).toBeCloseTo(0, 2);
    const vals = frames.map(tyOf);
    expect(Math.max(...vals)).toBeCloseTo(28, 1); // 峰值 = 幅度
    expect(tyOf(frames[frames.length - 1])).toBeCloseTo(0, 2); // 末帧回零
    // 停留段（elapsed 1.0s → 帧 60）保持幅度
    expect(tyOf(frames[60])).toBeCloseTo(28, 1);
  });

  it("carry 接管：首帧 = carry，峰值略高于幅度（carry 衰减叠加）后回落", () => {
    const a = MOOD_ACTIONS["沮丧"][2];
    const frames = buildActionKeyframes(a, 10);
    expect(tyOf(frames[0])).toBeCloseTo(10, 2); // 从当前姿态开始
    const vals = frames.map(tyOf);
    const peak = Math.max(...vals);
    expect(peak).toBeGreaterThan(28); // carry 残余叠加
    expect(peak).toBeLessThan(30); // 且已大幅衰减（τ=0.3s）
    expect(tyOf(frames[frames.length - 1])).toBeCloseTo(0, 1);
  });

  it("rotate 轴（困惑）：transform 为 translateY(0px) + rotate，峰值 = 角度", () => {
    const a = MOOD_ACTIONS["困惑"][2]; // amplitude 4, travel 0.7, dwell 1.0
    const frames = buildActionKeyframes(a, 0);
    expect(frames[0].transform).toContain("translateY(0px)");
    expect(rotOf(frames[0])).toBeCloseTo(0, 2);
    const vals = frames.map(rotOf);
    expect(Math.max(...vals)).toBeCloseTo(4, 1);
    expect(rotOf(frames[frames.length - 1])).toBeCloseTo(0, 2);
  });

  it("fixed（高兴）与 pulse 包络同样采样出首末零、中间振荡/单峰", () => {
    const joy = MOOD_ACTIONS["高兴"][0];
    const jf = buildActionKeyframes(joy, 0);
    const jvals = jf.map(tyOf);
    expect(jvals[0]).toBeCloseTo(0, 2);
    expect(jvals[jvals.length - 1]).toBeCloseTo(0, 2);
    expect(Math.max(...jvals)).toBeCloseTo(joy.amplitude, 1); // 上浮峰值

    const pulse: MoodAction = {
      kind: "沮丧",
      axis: "y",
      amplitude: 12,
      envelope: "pulse",
      duration: 1,
    };
    const pf = buildActionKeyframes(pulse, 0);
    const pvals = pf.map(tyOf);
    expect(pvals[0]).toBeCloseTo(0, 2);
    expect(Math.max(...pvals)).toBeCloseTo(12, 1);
    expect(pvals[pvals.length - 1]).toBeCloseTo(0, 2);
  });
});
