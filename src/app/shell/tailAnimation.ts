// src/components/shell/tailAnimation.ts
// 尾巴几何 · 纯函数模块
//
// 职责：计算尾巴中线的波浪路径。
//       身体 = 一条 stroked 粗线，箭头 = 一个 filled 三角形（跟随末端切线方向）。
//
// 批 2-D 重构：rAF 循环与 useState 已移除——循环归统一动画引擎
// （droneEngine.ts 每帧以 computeTailPath(phase, scale) 直写 SVG path d，
// 不再每帧 setState 触发整树重渲染）。本模块保持纯函数，node 环境可测。

// ── 几何配置 ──
//
// viewBox = "-15 -10 30 100"，原点 (0,0) 在 CSS 锚点处。
// y 负值区域被底面遮挡，根部藏入立方体后方。

const TAIL_LENGTH = 72;   // 身体长度（viewBox 单位，约等于原 7 段 × 10px 有效高度）
const BODY_POINTS = 9;    // 身体控制点数
const ROOT_Y = -8;        // 根部 y 偏移：负值使根部藏入底面后方，模拟原方案的段间重叠

// 振幅曲线：从根 0.5 递增到末端 7（viewBox 单位），波从根传到尖
const AMPLITUDE_CURVE = [0, 1, 2, 4, 8, 12, 16, 20, 25];

// 每段相位延迟（弧度）。负号使波从根传到尖（根先动，尖跟随）。
// 0.35×8=2.8 rad，尖滞后根约 0.45 个周期。
const PHASE_DELAY = 0.325;

// 箭头几何
const ARROW_BASE = 12;   // 箭头底宽
const ARROW_LEN = 16;    // 箭头长度

// ── 类型 ──

interface Point {
  x: number;
  y: number;
}

export interface TailPaths {
  body: string;   // 身体中线 path d（用于 stroke）
  arrow: string;  // 箭头三角形 path d（用于 fill）
}

// ═══════════════════════════════════════════════
// Catmull-Rom 样条 → Cubic Bezier
// ═══════════════════════════════════════════════

function catmullRomTangent(pPrev: Point, pNext: Point): Point {
  const s = 0.25; // 标准 CR 系数 (1-tension)/2, tension=0.5
  return { x: (pNext.x - pPrev.x) * s, y: (pNext.y - pPrev.y) * s };
}

/** 生成通过 points 的光滑 Cubic Bezier path d 字符串 */
function smoothCurve(points: Point[]): string {
  if (points.length < 2) return "";
  if (points.length === 2) {
    return `M ${pts(points[0])} L ${pts(points[1])}`;
  }
  const n = points.length;
  const parts: string[] = [`M ${pts(points[0])}`];
  for (let i = 0; i < n - 1; i++) {
    const p0 = points[Math.max(0, i - 1)];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[Math.min(n - 1, i + 2)];
    const t1 = catmullRomTangent(p0, p2);
    const t2 = catmullRomTangent(p1, p3);
    parts.push(
      `C ${f(p1.x + t1.x)},${f(p1.y + t1.y)} ` +
        `${f(p2.x - t2.x)},${f(p2.y - t2.y)} ` +
        `${pts(p2)}`
    );
  }
  return parts.join(" ");
}

function pts(p: Point): string {
  return `${f(p.x)},${f(p.y)}`;
}

function f(v: number): string {
  return v.toFixed(2);
}

// ═══════════════════════════════════════════════
// 路径计算
// ═══════════════════════════════════════════════

/**
 * 计算一帧的身体曲线 + 箭头。
 *
 * 身体：中线 Catmull-Rom 曲线，渲染时用 strokeWidth=4。
 * 箭头：三角形，底边与身体末端切线垂直，尖端沿切线方向延伸。
 *
 * @param phase 相位（rad）。由引擎的相位累加器提供，翅膀与尾巴共用。
 * @param scale 振幅缩放（0~1.4）。批 2-D 起引擎固定传 1.0（设计只定义翅膀幅度）。
 */
export function computeTailPath(phase: number, scale: number): TailPaths {
  const n = BODY_POINTS;
  const dy = TAIL_LENGTH / (n - 1);

  // ── 身体中线。y 从 ROOT_Y 开始，根部藏入底面后方 ──
  const spine: Point[] = [];
  for (let i = 0; i < n; i++) {
    const x = Math.sin(phase - i * PHASE_DELAY) * AMPLITUDE_CURVE[i] * scale;
    spine.push({ x, y: ROOT_Y + i * dy });
  }

  const body = smoothCurve(spine);

  // ── 箭头：末端切线方向 ──
  const last = spine[n - 1];
  const prev = spine[n - 2];
  const tdx = last.x - prev.x;
  const tdy = last.y - prev.y;
  const tLen = Math.sqrt(tdx * tdx + tdy * tdy) || 1;
  const tx = tdx / tLen;
  const ty = tdy / tLen;

  // 法向量（切线顺时针旋转 90°）
  const nx = ty;
  const ny = -tx;

  const halfBase = ARROW_BASE / 2;

  const baseLeft: Point = {
    x: last.x + nx * halfBase,
    y: last.y + ny * halfBase,
  };
  const baseRight: Point = {
    x: last.x - nx * halfBase,
    y: last.y - ny * halfBase,
  };
  const tip: Point = {
    x: last.x + tx * ARROW_LEN,
    y: last.y + ty * ARROW_LEN,
  };

  const arrow = `M ${pts(baseLeft)} L ${pts(tip)} L ${pts(baseRight)} Z`;

  return { body, arrow };
}
