// src/animation/animationMapping.ts
// 动画参数映射 · 纯函数层（批 2-D 动画映射）
//
// 设计：双通道正交——散热通道（状态驱动，驱动翅膀周期/幅度，走跟随器渐进）
// 与情绪通道（标签驱动，一次性阻尼振荡动作，驱动立方体姿态）。
// 本模块只含纯函数（node 环境 vitest 可测，无 DOM 依赖），引擎壳
// （droneEngine.ts）持有跟随器当前值 / 相位累加器 / 动作脚本并逐帧消费。
//
// 散热分段（uiState + agentPhase 驱动）：
//   reasoning（处理中子阶段）→ 周期向最小周期指数逼近（τ≈8s，跟随器天然满足
//     period(t) = min + (p_entry − min)·e^(−t/τ)）；多段 reasoning 从冻结值继续加速
//   executing（处理中子阶段）→ 周期冻结（跟随器 target=当前值，不移动）
//   streaming → 向默认周期缓慢回升（τ≈5s）
//   idle（ready，turn/end 后）→ 快速回默认（τ≈150ms，约 0.4s 收敛）
//   stopped（hidden/error）→ 立即冻结停扇（周期冻结 + 相位停止积分）
// 幅度是周期的连续函数：default 25deg → min 12deg（线性映射）。
//
// 情绪动作：位移(t) = 幅度 × sin(ωt) × e^(−t/τ)，执行完自动归零（回到悬浮原位）。
// 新动作到达时从当前姿态覆盖旧动作，不累积——carry 衰减项使旧姿态平滑收敛。

import type { Mood, MoodIntensity, UIState } from "../types";

// ── 散热通道常量 ──

/** 默认周期（s）：ready / streaming 回升目标 */
export const WING_DEFAULT_PERIOD = 2;
/** 最小周期（s）：reasoning 加速目标 */
export const WING_MIN_PERIOD = 0.5;
/** 默认幅度（deg）：周期 = 默认时的翅膀振幅 */
export const WING_AMPLITUDE_DEFAULT = 25;
/** 最小幅度（deg）：周期 = 最小时的翅膀振幅 */
export const WING_AMPLITUDE_MIN = 12;
/** reasoning 加速时间常数（s） */
export const TAU_REASONING = 8;
/** streaming 回升时间常数（s） */
export const TAU_STREAMING = 5;
/** idle 快速回默认时间常数（s）——turn/end 后约 0.4s 收敛 */
export const TAU_IDLE = 0.15;
/** 情绪动作覆盖旧姿态时 carry 的衰减时间常数（s） */
export const TAU_CARRY = 0.3;

// ── 散热分段状态 ──

export type ThermalState =
  | "idle"      // ready：无处理，回默认周期
  | "reasoning" // 处理中：LLM 思考 → 周期向最小加速
  | "executing" // 处理中：调工具/等工具 → 周期冻结
  | "streaming" // 生成最终回答 → 周期缓慢回升
  | "stopped";  // hidden/error：立即冻结停扇

/** uiState + agentPhase → 散热分段状态（纯映射） */
export function thermalStateOf(
  uiState: UIState,
  agentPhase: "reasoning" | "processing" | "streaming" | undefined
): ThermalState {
  switch (uiState) {
    case "ready":
      return "idle";
    case "hidden":
    case "error":
      return "stopped";
    case "processing":
      // 子阶段区分：reasoning 与 executing（工具调用）在 uiState 上不可分
      return agentPhase === "reasoning" ? "reasoning" : "executing";
    case "streaming":
      return "streaming";
  }
}

/** 跟随器：当前值每帧向目标值指数逼近。tau<=0 → 冻结（不移动） */
export function follow(
  current: number,
  target: number,
  tau: number,
  dt: number
): number {
  if (tau <= 0 || dt <= 0) return current;
  const k = 1 - Math.exp(-dt / tau);
  return current + (target - current) * k;
}

/** 相位累加：翅膀与尾巴共用（周期渐变时位置连续）。period<=0 / dt<=0 → 停走 */
export function advancePhase(phase: number, period: number, dt: number): number {
  if (period <= 0 || dt <= 0) return phase;
  return phase + (2 * Math.PI * dt) / period;
}

/** 幅度随周期线性渐变：default 25deg → min 12deg（线性映射） */
export function wingAmplitudeOf(period: number): number {
  const t =
    (period - WING_MIN_PERIOD) / (WING_DEFAULT_PERIOD - WING_MIN_PERIOD);
  return WING_AMPLITUDE_MIN + t * (WING_AMPLITUDE_DEFAULT - WING_AMPLITUDE_MIN);
}

export interface ThermalTargetSpec {
  /** 跟随器目标周期（s） */
  target: number;
  /** 跟随器时间常数（s）；0 = 冻结 */
  tau: number;
  /** 是否冻结相位（停扇：相位不积分，翅膀/尾巴定格） */
  phaseFrozen: boolean;
}

/**
 * 散热分段 → 跟随器目标（freeze 状态 target=当前值，tau=0 不移动）。
 * reasoning 段 target=min、τ=8s：跟随器轨迹即 period(t) = min + (p_entry−min)·e^(−t/τ)，
 * 且 executing 冻结后续 reasoning 从冻结值继续加速（跨段累积由跟随器当前值天然承接）。
 */
export function resolveThermalTarget(
  state: ThermalState,
  currentPeriod: number
): ThermalTargetSpec {
  switch (state) {
    case "reasoning":
      return { target: WING_MIN_PERIOD, tau: TAU_REASONING, phaseFrozen: false };
    case "executing":
      return { target: currentPeriod, tau: 0, phaseFrozen: false };
    case "streaming":
      return { target: WING_DEFAULT_PERIOD, tau: TAU_STREAMING, phaseFrozen: false };
    case "idle":
      return { target: WING_DEFAULT_PERIOD, tau: TAU_IDLE, phaseFrozen: false };
    case "stopped":
      return { target: currentPeriod, tau: 0, phaseFrozen: true };
  }
}

// ── 情绪通道（标签驱动 · 一次性动作） ──

export type MoodAxis = "y" | "rotate";

/** 动作包络形态：osc = 阻尼振荡（不再用于情绪动作）；pulse = 单次缓动脉冲；fixed = 固定幅度振荡；dwell = 到位-停留-返回（暂留） */
export type MoodEnvelope = "osc" | "pulse" | "fixed" | "dwell";

export interface MoodAction {
  /** 触发动作的情绪 */
  kind: Mood;
  /** 位移轴：y = 立方体上下平移（px）；rotate = 立方体倾斜（deg） */
  axis: MoodAxis;
  /** 幅度（px 或 deg） */
  amplitude: number;
  /** 包络形态 */
  envelope: MoodEnvelope;
  /** osc / fixed 专用：角频率（rad/s） */
  omega?: number;
  /** osc 专用：阻尼时间常数（s），动作时长 = 3τ */
  tau?: number;
  /** pulse 专用：动作总时长（s） */
  duration?: number;
  /** fixed 专用：振荡周期数（0.5 = 单次下沉回升） */
  cycles?: number;
  /** fixed 专用：首尾渐入渐出时长（s，余弦窗，防突变） */
  fade?: number;
  /** dwell 专用：出发/返回各时长（s） */
  travel?: number;
  /** dwell 专用：目标位置停留时长（s，情绪暂留） */
  dwell?: number;
}

/**
 * 情绪动作配置 · 按强度档位（0 = 基础，1~3 = 加深，各档交替改变维度）。
 * 档位变化设计：
 * - 高兴（fixed 浮动）：1 档幅度增大 → 2 档频率增快 → 3 档幅度再增
 * - 困惑（dwell 歪头）：1 档角度增大 → 2 档停留时间增大 → 3 档角度再增
 * - 沮丧（dwell 下沉）：1 档下沉量增大 → 2 档停留时间增大 → 3 档下沉量再增
 */
export const MOOD_ACTIONS: Record<
  "高兴" | "困惑" | "沮丧",
  MoodAction[]
> = {
  高兴: [
    {
      kind: "高兴",
      axis: "y",
      amplitude: 10,
      envelope: "fixed",
      omega: (2 * Math.PI) / 2,
      cycles: 2,
      fade: 0.25,
    },
    {
      kind: "高兴",
      axis: "y",
      amplitude: 15,
      envelope: "fixed",
      omega: (2 * Math.PI) / 2,
      cycles: 2,
      fade: 0.25,
    },
    {
      kind: "高兴",
      axis: "y",
      amplitude: 15,
      envelope: "fixed",
      omega: (2 * Math.PI) / 1.6,
      cycles: 2,
      fade: 0.25,
    },
    {
      kind: "高兴",
      axis: "y",
      amplitude: 20,
      envelope: "fixed",
      omega: (2 * Math.PI) / 1.6,
      cycles: 2,
      fade: 0.25,
    },
  ],
  困惑: [
    {
      kind: "困惑",
      axis: "rotate",
      amplitude: 3,
      envelope: "dwell",
      travel: 0.7,
      dwell: 0.5,
    },
    {
      kind: "困惑",
      axis: "rotate",
      amplitude: 4,
      envelope: "dwell",
      travel: 0.7,
      dwell: 0.5,
    },
    {
      kind: "困惑",
      axis: "rotate",
      amplitude: 4,
      envelope: "dwell",
      travel: 0.7,
      dwell: 1.0,
    },
    {
      kind: "困惑",
      axis: "rotate",
      amplitude: 5,
      envelope: "dwell",
      travel: 0.7,
      dwell: 1.0,
    },
  ],
  沮丧: [
    {
      kind: "沮丧",
      axis: "y",
      amplitude: 22,
      envelope: "dwell",
      travel: 0.5,
      dwell: 0.9,
    },
    {
      kind: "沮丧",
      axis: "y",
      amplitude: 28,
      envelope: "dwell",
      travel: 0.5,
      dwell: 0.9,
    },
    {
      kind: "沮丧",
      axis: "y",
      amplitude: 28,
      envelope: "dwell",
      travel: 0.5,
      dwell: 1.4,
    },
    {
      kind: "沮丧",
      axis: "y",
      amplitude: 36,
      envelope: "dwell",
      travel: 0.5,
      dwell: 1.4,
    },
  ],
};

/**
 * 情绪 + 强度档位 → 动作配置。平静 / 非法情绪 → null（无动作，回退平静）。
 * 合法情绪取对应档位的完整配置（MOOD_ACTIONS[mood][intensity]，intensity 越界钳位到 0）。
 */
export function mapMoodAction(
  mood: Mood,
  intensity: MoodIntensity
): MoodAction | null {
  if (mood === "高兴" || mood === "困惑" || mood === "沮丧") {
    const levels = MOOD_ACTIONS[mood];
    const idx = intensity >= 0 && intensity <= 3 ? intensity : 0;
    return levels[idx];
  }
  return null;
}

/**
 * 动作包络求值。
 * osc：阻尼振荡 位移(t) = 幅度 × sin(ωt) × e^(−t/τ)；
 * pulse：单次缓动脉冲 位移(t) = 幅度 × sin²(πt/T)（t≥T 归零，起止斜率零）；
 * fixed：固定幅度振荡 位移(t) = 幅度 × sin(ωt) × 余弦窗(t)——窗口首尾 fade 秒余弦渐入渐出
 *        （两端导数零，无速度跳变），中间幅度恒定，t ≥ cycles × 周期 归零；
 * dwell：到位-停留-返回——上升段 sin² 半波（0→幅度）、停留段恒为幅度、下降段 cos² 半波
 *        （幅度→0），三段边界速度均为零，t ≥ 2×travel + dwell 归零。
 * carry 为覆盖旧动作时的起始姿态偏移，以 τ_carry 快速衰减——各形态下都从当前姿态平滑接管，不累积。
 */
export function evaluateAction(
  action: MoodAction,
  localTime: number,
  carry: number
): number {
  let wave: number;
  if (action.envelope === "pulse") {
    const T = action.duration!;
    wave =
      localTime >= T
        ? 0
        : action.amplitude *
          Math.sin((Math.PI * localTime) / T) ** 2;
  } else if (action.envelope === "fixed") {
    const Tp = (2 * Math.PI) / action.omega!;
    const total = action.cycles! * Tp;
    if (localTime >= total) {
      wave = 0;
    } else {
      const fade = Math.min(action.fade!, total / 2);
      const w =
        localTime < fade
          ? (1 - Math.cos((Math.PI * localTime) / fade)) / 2
          : localTime > total - fade
            ? (1 - Math.cos((Math.PI * (total - localTime)) / fade)) / 2
            : 1;
      // 负 sin：相位从上方开始（先上浮后下沉）——高兴语义是雀跃上浮，
      // 而非先下沉。translateY 负 = 上浮。
      wave = -action.amplitude * Math.sin(action.omega! * localTime) * w;
    }
  } else if (action.envelope === "dwell") {
    const travel = action.travel!;
    const hold = action.dwell!;
    const total = 2 * travel + hold;
    if (localTime >= total) {
      wave = 0;
    } else if (localTime < travel) {
      wave =
        action.amplitude *
        Math.sin((Math.PI * localTime) / (2 * travel)) ** 2;
    } else if (localTime < travel + hold) {
      wave = action.amplitude;
    } else {
      const t = localTime - travel - hold;
      wave =
        action.amplitude *
        Math.cos((Math.PI * t) / (2 * travel)) ** 2;
    }
  } else {
    wave =
      action.amplitude * Math.sin(action.omega! * localTime) *
      Math.exp(-localTime / action.tau!);
  }
  return wave + carry * Math.exp(-localTime / TAU_CARRY);
}

/** 动作时长（s）：osc = 3τ；pulse = T；fixed = cycles × 周期；dwell = 2×travel + dwell */
export function actionDuration(action: MoodAction): number {
  if (action.envelope === "pulse") return action.duration!;
  if (action.envelope === "fixed") {
    return Math.round(action.cycles! * ((2 * Math.PI) / action.omega!) * 10) / 10;
  }
  if (action.envelope === "dwell") {
    return 2 * action.travel! + action.dwell!;
  }
  return Math.round(action.tau! * 3 * 10) / 10;
}

// ── WAAPI 动作关键帧（批 2-D 修复：动作改由合成器动画通道播放） ──

/** WAAPI 关键帧采样粒度（ms）：约 60fps 一帧，线性插值足够平滑还原包络曲线 */
const ACTION_SAMPLE_MS = 1000 / 60;

/**
 * 生成动作的 WAAPI 关键帧数组（纯函数，可单测）。
 * 直接采样 evaluateAction（包络 + carry 衰减一体），linear 插值还原曲线：
 * 首帧 = carry（旧动作当前姿态，平滑接管不累积），末帧 ≈ 0（回到悬浮原位）。
 * 动作时间轴由合成器驱动（element.animate），主线程卡顿不再影响动作播放——
 * 这正是流式中 CSS 动画正常、逐帧写 transform 异常的本质区别。
 */
export function buildActionKeyframes(
  action: MoodAction,
  carry: number,
  sampleMs: number = ACTION_SAMPLE_MS
): Keyframe[] {
  const dur = actionDuration(action);
  const n = Math.max(1, Math.round((dur * 1000) / sampleMs));
  const frames: Keyframe[] = [];
  for (let i = 0; i <= n; i++) {
    const t = (dur * i) / n;
    const v = evaluateAction(action, t, carry);
    const transform =
      action.axis === "y"
        ? `translateY(${v.toFixed(2)}px) rotate(0deg)`
        : `translateY(0px) rotate(${v.toFixed(2)}deg)`;
    frames.push({ transform });
  }
  return frames;
}

// ── 统一目标集（useAnimationMapping 消费） ──

export interface EngineTargets {
  /** 散热分段状态（引擎据此解析跟随器目标） */
  thermal: ThermalState;
  /** 情绪动作信号（null = 无新动作；引擎不打断进行中的动作） */
  moodAction: MoodAction | null;
}

/**
 * uiState + agentPhase + mood + intensity → 引擎目标参数集（纯函数）。
 * 散热目标 = thermalStateOf(uiState, agentPhase)；
 * 情绪触发信号 = mapMoodAction(mood, intensity)（hook 侧仅在 mood/intensity 变化时下发，一次性触发）。
 */
export function mapTargets(
  uiState: UIState,
  agentPhase: "reasoning" | "processing" | "streaming" | undefined,
  mood: Mood,
  intensity: MoodIntensity
): EngineTargets {
  return {
    thermal: thermalStateOf(uiState, agentPhase),
    moodAction: mapMoodAction(mood, intensity),
  };
}
