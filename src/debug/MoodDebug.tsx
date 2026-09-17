// src/debug/MoodDebug.tsx
// 情绪动画调试面板
//
// 控制区两组按钮：
// - 散热状态：idle / reasoning / executing / streaming / stopped（驱动翅膀周期渐变）
// - 情绪动作：高兴 / 困惑 / 沮丧 / 平静（一次性动作，可连续点击重复触发）
//
// 参数调整：src/animation/animationMapping.ts 的 MOOD_ACTIONS（情绪）与散热常量，
// 改完刷新页面即可（引擎单例跨刷新不保留，刷新后状态复位）。

import { useEffect, useRef, useState } from "react";
import { DroneShellScene } from "../components/shell/DroneShellScene";
import { Markdown } from "../components/content/Markdown";
import { droneEngine } from "../animation/droneEngine";
import {
  mapMoodAction,
  type ThermalState,
} from "../animation/animationMapping";
import type { MoodIntensity } from "../types";
const THERMAL_STATES: ThermalState[] = [
  "idle",
  "reasoning",
  "executing",
  "streaming",
  "stopped",
];

type MoodKey = "高兴" | "困惑" | "沮丧";

const MOODS: (MoodKey | null)[] = ["高兴", "困惑", "沮丧", null];

const INTENSITIES: MoodIntensity[] = [0, 1, 2, 3];

/**
 * 模拟流式负载（诊断用）：每 33ms 追加一块 markdown 文本（循环），
 * 复刻 ChatView 流式输出期间的主线程压力（marked 解析 + DOM 更新）。
 * 负载强度调节：改 interval 间隔（33ms）或 STREAM_BLOCKS 的块大小。
 */
const STREAM_BLOCKS = [
  "**部署记录**：能天使（费用 12，技能 过载模式，冷却 25s）。\n",
  "目标点：切尔诺伯格 废墟，敌人 3 波。\n",
  "| 干员 | 职业 | 费用 |\n",
  "|:--|:--|:--:|\n",
  "| 能天使 | 狙击 | 12 |\n",
  "| 银灰 | 近卫 | 18 |\n",
  "| 星熊 | 重装 | 28 |\n\n",
  "部署顺序建议：先先锋回费，再狙击输出。\n",
];

const btnStyle = (active: boolean): React.CSSProperties => ({
  padding: "6px 14px",
  fontSize: 13,
  fontFamily: "var(--font-ui)",
  background: active ? "var(--accent)" : "transparent",
  color: active ? "#fff" : "var(--text-2)",
  border: "1px solid var(--depth-1)",
  borderRadius: 4,
  cursor: "pointer",
});

export function MoodDebug(): React.JSX.Element {
  const [thermal, setThermal] = useState<ThermalState>("idle");
  const [intensity, setIntensity] = useState<MoodIntensity>(0);

  // ── 模拟流式负载（诊断）：开启后 33ms 持续追加 markdown 文本 ──
  const [streaming, setStreaming] = useState(false);
  const [streamText, setStreamText] = useState("");
  const streamTick = useRef(0);

  useEffect(() => {
    if (!streaming) return;
    const timer = window.setInterval(() => {
      setStreamText(
        (t) => t + STREAM_BLOCKS[streamTick.current % STREAM_BLOCKS.length]
      );
      streamTick.current += 1;
    }, 33);
    return () => window.clearInterval(timer);
  }, [streaming]);

  const applyThermal = (s: ThermalState): void => {
    setThermal(s);
    droneEngine.setTargets({ thermal: s, moodAction: null });
  };

  const triggerMood = (mood: MoodKey | null): void => {
    droneEngine.setTargets({
      thermal,
      moodAction: mood !== null ? mapMoodAction(mood, intensity) : null,
    });
  };

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        gap: 16,
        padding: 24,
        minHeight: "100vh",
        background: "#141517",
      }}
    >
      {/* 舞台：DroneShellScene 的定位参照（.shell 100% 填充） */}
      <div style={{ width: 560, height: 560, position: "relative" }}>
        <DroneShellScene />
      </div>

      {/* 模拟流式负载 + 帧间隔仪表（诊断） */}
      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", justifyContent: "center" }}>
        <button
          onClick={() => {
            setStreaming((v) => !v);
            setStreamText("");
          }}
          style={btnStyle(streaming)}
        >
          模拟流式负载 {streaming ? "开" : "关"}
        </button>
        <FrameMeter />
      </div>
      {streaming && (
        <div
          style={{
            position: "fixed",
            right: 16,
            bottom: 16,
            width: 360,
            height: 220,
            overflow: "auto",
            background: "var(--depth-2)",
            borderRadius: 4,
            border: "1px solid var(--depth-1)",
            padding: 12,
            fontFamily: "var(--font-ui)",
            fontSize: 13,
            zIndex: 100,
          }}
        >
          <Markdown content={streamText} />
        </div>
      )}

      {/* 散热状态 */}
      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", justifyContent: "center" }}>
        <span style={{ color: "var(--text-3)", fontSize: 12 }}>散热</span>
        {THERMAL_STATES.map((s) => (
          <button
            key={s}
            onClick={() => applyThermal(s)}
            style={btnStyle(thermal === s)}
          >
            {s}
          </button>
        ))}
      </div>

      {/* 情绪动作 */}
      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", justifyContent: "center" }}>
        <span style={{ color: "var(--text-3)", fontSize: 12 }}>情绪</span>
        {MOODS.map((m) => (
          <button
            key={m ?? "平静"}
            onClick={() => triggerMood(m)}
            style={btnStyle(false)}
          >
            {m ?? "平静"}
          </button>
        ))}
      </div>

      {/* 强度档位 */}
      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", justifyContent: "center" }}>
        <span style={{ color: "var(--text-3)", fontSize: 12 }}>强度</span>
        {INTENSITIES.map((n) => (
          <button
            key={n}
            onClick={() => setIntensity(n)}
            style={btnStyle(intensity === n)}
          >
            {n}
          </button>
        ))}
      </div>

      <p style={{ color: "var(--text-3)", fontSize: 12, maxWidth: 560, textAlign: "center" }}>
        参数在 src/animation/animationMapping.ts 的 MOOD_ACTIONS（每情绪 4 档配置）与散热常量中调整，
        改后刷新页面。情绪按钮可连续点击（每次重新触发动作）；强度选中后影响下一次触发。
        诊断：先关负载点沮丧（基线），再开负载等文本积累（帧间隔增大）后点沮丧，对比下沉表现。
      </p>
    </div>
  );
}

/** 帧间隔仪表（诊断）：显示主线程 rAF 实际帧间隔。负载开启后间隔应明显增大（>100ms 即触发引擎 dt 钳制） */
function FrameMeter(): React.JSX.Element {
  const [intervalMs, setIntervalMs] = useState(0);
  useEffect(() => {
    let last = performance.now();
    let id = 0;
    const loop = (now: number): void => {
      setIntervalMs(Math.round(now - last));
      last = now;
      id = requestAnimationFrame(loop);
    };
    id = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(id);
  }, []);
  return (
    <span style={{ color: "var(--text-3)", fontSize: 12 }}>
      帧间隔 {intervalMs} ms
    </span>
  );
}
