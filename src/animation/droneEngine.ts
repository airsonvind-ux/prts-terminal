// src/animation/droneEngine.ts
// 统一 rAF 动画引擎（批 2-D 动画映射）
//
// 分层：纯函数层（animationMapping.ts：跟随器 / 相位 / 动作包络 / 目标映射）+
// 引擎壳（本文件：逐帧直接写 DOM，不经 React setState——顺带修复旧 tailAnimation
// 每帧 setState 触发整树重渲染的问题）。
//
// 引擎持有运行时状态：跟随器当前值（周期）、相位累加器（翅膀与尾巴共用）、
// 情绪动作（WAAPI 动画对象）。DroneShellScene 通过 attach() 注入元素，
// useAnimationMapping 通过 setTargets() 喂目标（散热分段 + 情绪动作信号）。
//
// 生命周期：attach 启动 rAF 循环、detach 取消——StrictMode 双挂载下 effect
// cleanup 成对执行，不会泄漏循环；引擎内部状态跨重挂载保留（视觉连续）。
//
// 逐帧流程：散热跟随器（周期向目标逼近，幅度由周期线性映射）→ 相位积分
// （stopped 冻结停扇）→ 直写 DOM（翅膀 / 尾巴）。情绪动作由 WAAPI 动画播放
// （合成器驱动，时间轴独立于主线程），引擎不参与其逐帧推进。

import {
  advancePhase,
  actionDuration,
  buildActionKeyframes,
  follow,
  resolveThermalTarget,
  wingAmplitudeOf,
  type MoodAction,
  type ThermalState,
} from "./animationMapping";
import { computeTailPath } from "../app/shell/tailAnimation";

/** 引擎操作的 DOM 元素集（DroneShellScene 通过 ref 提供） */
export interface EngineElements {
  /** .cube：情绪动作写 translateY / rotate */
  cube: HTMLElement;
  /** 左翼 SVG（transform-origin 在 CSS 中保留） */
  wingLeft: SVGElement;
  /** 右翼 SVG */
  wingRight: SVGElement;
  /** 尾巴 SVG（两个 <path>：body / arrow，按序直写 d） */
  tailSvg: SVGSVGElement;
}

export interface EngineTargets {
  /** 散热分段状态（引擎据此解析跟随器目标） */
  thermal: ThermalState;
  /** 情绪动作信号；null = 无新动作（不打断进行中的动作，让其自然衰减归零） */
  moodAction: MoodAction | null;
}

/** 尾巴振幅缩放（固定 1.0） */
const TAIL_SCALE = 1;

/** 尾巴摆动周期（s）：与翅膀解耦——尾巴是心情的体现，不随散热加速 */
const TAIL_PERIOD = 2;

/** 翅膀 squash 深度：|sin(相位)| = 1 时 scaleY 压缩到 1 − 0.35 = 0.65（旧 keyframes 口径） */
const WING_SQUASH_MAX = 0.35;

/** 单帧 dt 上限（s）：主线程卡顿恢复后不跳帧，动画表现为暂停而非跳变 */
const MAX_FRAME_DT = 0.1;

class DroneEngine {
  private elements: EngineElements | null = null;
  private rafId: number | null = null;
  private lastTime: number | null = null;

  // ── 散热跟随器状态 ──
  /** 当前周期（s），跟随器 current 值，跨状态保留（reasoning 多段累积） */
  private period = 2;
  /** 相位累加器（rad），翅膀与尾巴共用 */
  private phase = 0;
  /** 尾巴独立相位（rad）：与翅膀解耦，固定周期，不随散热变速 */
  private tailPhase = 0;
  /** 当前散热分段状态（由 useAnimationMapping 推送） */
  private thermal: ThermalState = "idle";

  // ── 情绪动作（WAAPI：合成器动画通道播放，时间轴独立于主线程） ──
  private action: {
    config: MoodAction;
    /** element.animate 返回的动画对象（合成器驱动） */
    anim: Animation;
  } | null = null;

  /** 注入元素并启动循环。重复 attach 只更新元素（StrictMode 重挂载安全） */
  attach(elements: EngineElements): void {
    this.elements = elements;
    if (this.rafId === null) {
      this.lastTime = null;
      this.rafId = requestAnimationFrame(this.tick);
    }
  }

  /** 解除元素并停止循环（组件卸载 / StrictMode cleanup） */
  detach(): void {
    if (this.rafId !== null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
    // 动作动画随元素生命周期：cancel 释放（元素即将被 React 重建）
    if (this.action) {
      this.action.anim.cancel();
      this.action = null;
    }
    this.elements = null;
  }

  /** 喂入目标参数集（useAnimationMapping 每轮订阅变化时调用） */
  setTargets(targets: EngineTargets): void {
    this.thermal = targets.thermal;
    if (targets.moodAction) {
      const el = this.elements?.cube;
      if (!el) return; // 元素未注入（attach 前）：忽略动作（应用流程 attach 先于交互）

      // 1. 读当前生效姿态作为起始值（必须在 cancel 之前：cancel 会立即回落到 inline 值）
      const pose = this.parseCubePose();
      const carry = targets.moodAction.axis === "y" ? pose.ty : pose.rot;

      // 2. cancel 旧动画（替换语义：新动作从当前姿态接管，不累积）
      if (this.action) {
        this.action.anim.cancel();
        this.action = null;
      }

      // 3. 创建合成器动画：keyframes 采样包络曲线（首帧 = carry 平滑接管）
      const anim = el.animate(
        buildActionKeyframes(targets.moodAction, carry),
        {
          duration: actionDuration(targets.moodAction) * 1000,
          easing: "linear",
          fill: "forwards",
        }
      );
      // 4. 结束清理：cancel 释放动画对象（transform 回落空 = 0，与末帧连续）
      anim.onfinish = () => {
        if (this.action?.anim === anim) {
          this.action = null;
        }
        anim.cancel();
      };
      this.action = { config: targets.moodAction, anim };
    }
    // moodAction === null：不打断进行中的动画，让其自然播完归零
  }

  /**
   * 解析 cube 当前生效 transform 的分量（matrix 或 none）。
   * 供新动作覆盖时读取起始姿态（carry）。getComputedStyle 返回动画层生效值。
   */
  private parseCubePose(): { ty: number; rot: number } {
    const el = this.elements?.cube;
    if (!el) return { ty: 0, rot: 0 };
    const cs = getComputedStyle(el).transform;
    if (!cs || cs === "none") return { ty: 0, rot: 0 };
    const m = /matrix\(([^)]+)\)/.exec(cs);
    if (!m) return { ty: 0, rot: 0 };
    const parts = m[1].split(",").map((s) => parseFloat(s.trim()));
    if (parts.length < 6 || parts.some((n) => Number.isNaN(n))) {
      return { ty: 0, rot: 0 };
    }
    return {
      ty: parts[5],
      rot: (Math.atan2(parts[1], parts[0]) * 180) / Math.PI,
    };
  }

  private tick = (now: number): void => {
    if (this.lastTime === null) this.lastTime = now;
    // dt 钳制上限：主线程卡顿恢复后不单帧跳变（动画暂停而非跳帧）
    const dt = Math.min((now - this.lastTime) / 1000, MAX_FRAME_DT);
    this.lastTime = now;

    // 1. 散热跟随器：周期向目标指数逼近（executing/stopped 冻结）
    const { target, tau, phaseFrozen } = resolveThermalTarget(
      this.thermal,
      this.period
    );
    this.period = follow(this.period, target, tau, dt);
    const amplitude = wingAmplitudeOf(this.period);

    // 2. 相位累加（stopped 停扇：相位不积分，翅膀/尾巴定格）
    if (!phaseFrozen) {
      this.phase = advancePhase(this.phase, this.period, dt);
      this.tailPhase = advancePhase(this.tailPhase, TAIL_PERIOD, dt);
    }

    // 3.（情绪动作由 WAAPI 动画播放，合成器驱动，时间轴独立于主线程）

    // 4. 逐帧直写 DOM（翅膀 / 尾巴；cube 的 transform 由动画层管理）
    this.writeFrame(amplitude);

    this.rafId = requestAnimationFrame(this.tick);
  };

  private writeFrame(amplitude: number): void {
    const el = this.elements;
    if (!el) return;

    // 翅膀：绕各自 transform-origin 镜像旋转 + 纵向压缩（squash）。
    // 波形 = 单向抬升 (1−cos(相位))/2 ∈ [0,1]：从水平抬起再落回，永不下探——
    // 忠实还原旧 CSS keyframes（rotate 0→25→0 + scaleY 同步压缩）的扇动形态。
    const lift = (1 - Math.cos(this.phase)) / 2;
    const wingAngle = amplitude * lift;
    const squash = 1 - WING_SQUASH_MAX * lift;
    el.wingRight.style.transform =
      `rotate(${wingAngle.toFixed(2)}deg) scaleY(${squash.toFixed(3)})`;
    el.wingLeft.style.transform =
      `rotate(${(-wingAngle).toFixed(2)}deg) scaleY(${squash.toFixed(3)})`;

    // 立方体：transform 由 WAAPI 动画层管理（合成器驱动）。
    // 引擎不写 cube 的 transform——动画层优先级高于 inline，
    // 写 inline 只会造成动画结束后残值生效的隐患。

    // 尾巴：身体 + 箭头两条 path 直写 d（固定结构 [body, arrow]），独立相位驱动
    const paths = el.tailSvg.querySelectorAll("path");
    if (paths.length === 2) {
      const tail = computeTailPath(this.tailPhase, TAIL_SCALE);
      paths[0].setAttribute("d", tail.body);
      paths[1].setAttribute("d", tail.arrow);
    }
  }
}

/** 全局单例：整个应用只有一台无人机，StrictMode 双挂载由 attach/detach 成对收口 */
export const droneEngine = new DroneEngine();

// HMR 清理：开发期 vite 热更新替换本模块时，旧模块实例的 rAF 循环不会自动停止，
// 多个引擎实例会竞争写入 DOM（新实例写动作位移、旧实例写静止值，互相覆盖
// → 动画抖动/幅度被吞）。dispose 回调在模块被替换前执行，停止旧实例循环。
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    droneEngine.detach();
  });
}
