// src/components/content/ContextIndicator.tsx
// 上下文用量指示器 · 圆环 + 悬停浮层 + 压缩入口（模块 D · 上下文衰减）
//
// 圆环进度 = 已用上下文 input token / 模型上下文窗口。
// 水位口径（模块 D 修正）：used = contextTokens.inputTokens——usage 是上轮请求
// 的真实输入占用，即当时的上下文大小；output 是成本信息非水位，仅展示于
// tooltip 辅行（input/output 分开展示，避免再被误当水位）。
//
// 交互（产品决策 + 验收修正 2026-09-06）：
//   hover    → 纯信息浮层（模型名 + 占用 + 预警行）
//   点击圆环 → 压缩菜单：单一动作项（无标题/说明/二次确认），随状态切换——
//              空闲 =「压缩」点击即执行；压缩中 =「取消压缩」点击即走
//              abort_turn（ctxdecay-{conversationId} 中止通道）
//   压缩中   → 圆环进度暂隐，弧点绕圆周旋转加载（CSS keyframes）；输入区由
//              ChatView 按 compacting 态禁用
//   高水位   → used ≥ 70% 窗口时 tooltip 追加「接近上限，点击圆环压缩」；
//              发送前硬拦由 useProcessingStore.sendMessage 承担（0.9 阈值）

import { useState, useCallback, useRef, useEffect } from "react";
import { createPortal } from "react-dom";
import { useConversationStore } from "../../stores/useConversationStore";
import { useCompactionStore } from "../../stores/useCompactionStore";
import { useUIStore } from "../../stores/useUIStore";
import { getModelInfo } from "../../stores/useProcessingStore";
import { CONTEXT_WARNING_RATIO } from "../../projection";
import styles from "./ContextIndicator.module.css";

const SIZE = 20;
const STROKE = 2;
const RADIUS = (SIZE - STROKE) / 2;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

function calcOffset(used: number, window: number): number {
  if (window <= 0) return CIRCUMFERENCE;
  const pct = Math.min(used / window, 1);
  return CIRCUMFERENCE * (1 - pct);
}

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  const k = n / 1000;
  return k >= 10 ? `${Math.round(k)}K` : `${k.toFixed(1)}K`;
}

/**
 * 圆环颜色 · 三段锚点 + 非线性映射
 *
 * 0~25%   ：锚定方舟蓝 hsl(205, 65%, 50%)，用量充裕无需关注
 * 25~75%  ：蓝→方舟橙 hsl(40, 85%, 55%)，主要过渡区间
 * 75~100% ：橙→红 hsl(0, 70%, 50%)，加速变化，临界警告
 *
 * 色相变化通过三次缓动曲线映射，让中段承载大部分颜色差异，
 * 两端分别"拉住"——低用量区颜色几乎不动，高用量区剧烈转红。
 */
function ringColor(pct: number): string {
  const t = pct / 100;
  // easeInOutCubic：两端缓、中间陡，让 25~75% 承载主要颜色变化
  const eased = t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
  let hue: number;
  if (eased <= 0.25) {
    hue = 205;
  } else if (eased <= 0.75) {
    const seg = (eased - 0.25) / 0.5;
    hue = 205 + seg * (40 - 205);
  } else {
    const seg = (eased - 0.75) / 0.25;
    hue = 40 + seg * (0 - 40);
  }
  return `hsl(${Math.round(hue)}, 65%, 50%)`;
}

/** 浮层相对圆环的定位（tooltip 与压缩菜单共用；向上弹出） */
function anchorAbove(ringEl: HTMLElement): React.CSSProperties {
  const rect = ringEl.getBoundingClientRect();
  return {
    position: "fixed",
    left: rect.left + rect.width / 2,
    top: rect.top - 8,
    transform: "translate(-50%, -100%)",
  };
}

export function ContextIndicator({ reveal = true }: { reveal?: boolean } = {}) {
  const [hovered, setHovered] = useState(false);
  const [ringEl, setRingEl] = useState<HTMLDivElement | null>(null);
  const leaveTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  // ── 模块 D：压缩菜单 + compacting 态 ──
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const compactingConversationId = useCompactionStore(
    (s) => s.compactingConversationId
  );
  const runCompaction = useCompactionStore((s) => s.runCompaction);
  const cancelCompaction = useCompactionStore((s) => s.cancelCompaction);

  const activeId = useConversationStore((s) => s.activeId);
  const conversations = useConversationStore((s) => s.conversations);

  const activeConv = activeId
    ? conversations.find((c) => c.id === activeId)
    : undefined;
  const visible = activeConv !== undefined;
  // 揭示前（启动演出期间）按“无对话”的初始态呈现：宽 0、不占位——
  // 输入框因此保持初始位置，揭示时再靠 width 过渡自然地向右缩。
  const shown = visible && reveal;
  // 压缩中 = 活跃对话正在进行压缩（输入区禁用 + 圆环旋转）
  const compacting = activeId !== null && compactingConversationId === activeId;

  const modelInfo = getModelInfo();
  const modelName = modelInfo?.name;
  const contextWindow = modelInfo?.contextLength ?? 0;

  // 水位口径（模块 D 修正）：input 侧。output 是成本信息非水位。
  const used = activeConv?.contextTokens
    ? activeConv.contextTokens.inputTokens
    : 0;
  const pct = contextWindow > 0 ? Math.round((used / contextWindow) * 100) : 0;
  const offset = calcOffset(used, contextWindow);
  // 揭示前显示空环：圆环随覆盖层淡出从空扫到当前水位（.fill 已有
  // stroke-dashoffset 过渡，无需额外动画）。reveal 缺省 true = 无揭示行为。
  const shownOffset = reveal ? offset : CIRCUMFERENCE;
  const warning =
    !compacting && contextWindow > 0 && used / contextWindow >= CONTEXT_WARNING_RATIO;

  // 点击模型名 → 跳转设置页"模型"标签（覆盖记忆一次）
  const handleModelClick = useCallback(() => {
    useUIStore.getState().setSettingsInitialTab("model");
    useUIStore.getState().setOverlay("settings");
    setHovered(false);
  }, []);

  const handleRingLeave = () => {
    leaveTimer.current = setTimeout(() => setHovered(false), 150);
  };
  const handleRingEnter = () => {
    clearTimeout(leaveTimer.current);
    setHovered(true);
  };
  const handleTipEnter = () => {
    clearTimeout(leaveTimer.current);
  };
  const handleTipLeave = () => {
    setHovered(false);
  };

  // ── 压缩入口（点击圆环：空闲开压缩菜单 / 压缩中开取消确认） ──
  const handleRingClick = useCallback(() => {
    setHovered(false); // 菜单与 hover 浮层互斥
    setMenuOpen((v) => !v);
  }, []);

  // 点击菜单外部关闭
  useEffect(() => {
    if (!menuOpen) return;
    const close = (e: MouseEvent) => {
      const target = e.target as Node;
      if (ringEl && ringEl.contains(target)) return;
      if (menuRef.current && menuRef.current.contains(target)) return;
      setMenuOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [menuOpen, ringEl]);

  const handleStartCompaction = useCallback(() => {
    setMenuOpen(false);
    if (activeId) void runCompaction(activeId);
  }, [activeId, runCompaction]);

  const handleCancelCompaction = useCallback(() => {
    setMenuOpen(false);
    void cancelCompaction();
  }, [cancelCompaction]);

  const tooltipStyle = ringEl ? anchorAbove(ringEl) : undefined;
  const menuStyle = ringEl ? anchorAbove(ringEl) : undefined;

  return (
    <>
      <div
        ref={setRingEl}
        className={`${styles.ringWrapper} ${shown ? styles.visible : ""}`}
        onMouseEnter={handleRingEnter}
        onMouseLeave={handleRingLeave}
        onClick={handleRingClick}
        role="button"
        tabIndex={shown ? 0 : -1}
        aria-label={compacting ? "压缩进行中，点击取消" : "压缩对话"}
        aria-expanded={menuOpen}
        onKeyDown={(e) => {
          if (shown && (e.key === "Enter" || e.key === " ")) {
            e.preventDefault();
            handleRingClick();
          }
        }}
      >
        <svg
          className={styles.ring}
          viewBox={`0 0 ${SIZE} ${SIZE}`}
          role="img"
        >
          <circle
            className={styles.track}
            cx={SIZE / 2}
            cy={SIZE / 2}
            r={RADIUS}
            fill="none"
            strokeWidth={STROKE}
          />
          {compacting ? (
            // 压缩中：弧点绕圆环圆周旋转（CSS keyframes；进度暂隐）
            <circle
              className={styles.arcDot}
              cx={SIZE / 2}
              cy={SIZE / 2}
              r={RADIUS}
              fill="none"
              strokeWidth={STROKE}
              strokeLinecap="round"
              strokeDasharray={`${CIRCUMFERENCE * 0.12} ${CIRCUMFERENCE}`}
            />
          ) : (
            <circle
              className={styles.fill}
              cx={SIZE / 2}
              cy={SIZE / 2}
              r={RADIUS}
              fill="none"
              strokeWidth={STROKE}
              strokeDasharray={CIRCUMFERENCE}
              strokeDashoffset={shownOffset}
              strokeLinecap="round"
              transform={`rotate(-90 ${SIZE / 2} ${SIZE / 2})`}
              style={{ stroke: ringColor(pct) }}
            />
          )}
        </svg>
      </div>

      {/* hover 纯信息浮层（压缩菜单打开时抑制） */}
      {hovered &&
        !menuOpen &&
        tooltipStyle &&
        createPortal(
          <div
            className={styles.tooltip}
            style={tooltipStyle}
            onMouseEnter={handleTipEnter}
            onMouseLeave={handleTipLeave}
          >
            <div className={styles.tooltipArrow} />
            {modelName && (
              <button className={styles.modelName} onClick={handleModelClick}>
                {modelName}
              </button>
            )}
            {compacting ? (
              <span className={styles.usage}>正在压缩对话…</span>
            ) : contextWindow > 0 ? (
              <>
                <span className={styles.usage}>
                  上下文 {formatTokens(used)} / {formatTokens(contextWindow)} ({pct}%)
                </span>
                {warning && (
                  <span className={styles.warning}>接近上限，点击圆环压缩</span>
                )}
              </>
            ) : used > 0 ? (
              <span className={styles.usage}>
                已用 {formatTokens(used)} token
              </span>
            ) : null}
          </div>,
          document.body
        )}

      {/* 压缩菜单（点击圆环：单一动作项，随状态切换，点击即执行——无确认卡） */}
      {menuOpen &&
        shown &&
        menuStyle &&
        createPortal(
          <div
            ref={menuRef}
            className={styles.menu}
            style={menuStyle}
            role="menu"
          >
            <div className={styles.menuArrow} />
            {compacting ? (
              <button
                type="button"
                className={styles.menuAction}
                onClick={handleCancelCompaction}
              >
                取消压缩
              </button>
            ) : (
              <button
                type="button"
                className={styles.menuAction}
                onClick={handleStartCompaction}
              >
                压缩
              </button>
            )}
          </div>,
          document.body
        )}
    </>
  );
}
