// src/components/shell/wingGeometry.ts
// 翅膀几何定义 · 命名点体系
//
// 职责：以命名坐标点为基础，组装膜翼轮廓 path 与结构接缝线 path。
// 每个坐标点有语义标签，调整翼形 = 修改对应点的 x/y，无需在 path 字符串中翻找。
// 纯函数模块，不接触 React / DOM / Store。
//
// 阶段 C · Step 4（翅膀）。所有坐标在 viewBox 归一化比例空间中。

// ── 基础类型 ──

/** viewBox 内的二维坐标 */
export interface Point {
  x: number;
  y: number;
}

/** 膜翼轮廓的一段：从上一段终点到此 to 点，经二次贝塞尔控制点 cp */
export interface MembraneSegment {
  /** 到达的命名点 key */
  to: string;
  /** 二次贝塞尔控制点 */
  cp: Point;
}

/** 一条结构接缝线 */
export interface SeamLine {
  /** 起点 key */
  from: string;
  /** 终点 key */
  to: string;
  /** 二次贝塞尔控制点（undefined = 直线） */
  cp?: Point;
  /** 线宽（viewBox 比例） */
  strokeWidth: number;
}

/** 翅膀蓝图：命名点 + 轮廓段 + 接缝线 */
export interface WingBlueprint {
  /** SVG viewBox */
  viewBox: { minX: number; minY: number; width: number; height: number };
  /** 所有命名坐标点。key → coordinates */
  points: Record<string, Point>;
  /** 膜翼轮廓的初始起点 key */
  membraneStart: string;
  /** 膜翼轮廓段序列。第一段的起点是 membraneStart */
  membraneSegments: MembraneSegment[];
  /** 结构接缝线序列 */
  seamLines: SeamLine[];
}

/** 翅膀的渲染输出 */
export interface WingPaths {
  /** 膜翼闭合路径 d 字符串 */
  membrane: string;
  /** 结构接缝线 path d 字符串数组，按 seamLines 顺序对应 */
  seams: string[];
  /** 各接缝线宽度，按 seamLines 顺序对应 */
  seamWidths: number[];
}

// ── 组装函数 ──

/**
 * 从蓝图生成翅膀的 SVG path 字符串。
 * 纯函数：相同蓝图永远返回相同 path，无副作用。
 */
export function buildWing(blueprint: WingBlueprint): WingPaths {
  const { points, membraneStart, membraneSegments, seamLines } = blueprint;

  // ── 膜翼路径 ──
  const start = points[membraneStart];
  if (!start) {
    throw new Error(`membraneStart "${membraneStart}" not found in points`);
  }

  const membraneParts: string[] = [`M ${start.x},${start.y}`];

  for (const seg of membraneSegments) {
    const to = points[seg.to];
    if (!to) {
      throw new Error(`membrane segment to "${seg.to}" not found in points`);
    }
    membraneParts.push(`Q ${seg.cp.x},${seg.cp.y} ${to.x},${to.y}`);
  }
  membraneParts.push("Z");

  const membrane = membraneParts.join(" ");

  // ── 结构接缝线 ──
  const seams: string[] = [];
  const seamWidths: number[] = [];

  for (const line of seamLines) {
    const from = points[line.from];
    const to = points[line.to];
    if (!from) {
      throw new Error(`seam line from "${line.from}" not found in points`);
    }
    if (!to) {
      throw new Error(`seam line to "${line.to}" not found in points`);
    }

    if (line.cp) {
      seams.push(`M ${from.x},${from.y} Q ${line.cp.x},${line.cp.y} ${to.x},${to.y}`);
    } else {
      seams.push(`M ${from.x},${from.y} L ${to.x},${to.y}`);
    }
    seamWidths.push(line.strokeWidth);
  }

  return { membrane, seams, seamWidths };
}

/**
 * 水平镜像蓝图。所有 x 坐标绕 viewBox 水平中心翻转。
 * 输入右翼蓝图，输出左翼蓝图。坐标语义保持不变（根部在左、翼尖在右），形状面朝左。
 */
export function mirrorBlueprint(blueprint: WingBlueprint): WingBlueprint {
  const cx = blueprint.viewBox.width / 2;
  const flipX = (x: number) => +(cx * 2 - x).toFixed(3);

  const points: Record<string, Point> = {};
  for (const [key, pt] of Object.entries(blueprint.points)) {
    points[key] = { x: flipX(pt.x), y: pt.y };
  }

  return {
    ...blueprint,
    points,
    membraneSegments: blueprint.membraneSegments.map((seg) => ({
      ...seg,
      cp: { x: flipX(seg.cp.x), y: seg.cp.y },
    })),
    seamLines: blueprint.seamLines.map((line) => ({
      ...line,
      cp: line.cp ? { x: flipX(line.cp.x), y: line.cp.y } : undefined,
    })),
  };
}

// ── 默认蓝图 ──
//
// DEFAULT_WING        右翼基准，坐标来自 Phase 2 验证过的翅膀形状
// DEFAULT_WING_LEFT   左翼（从右翼镜像生成，修改右翼后自动同步）

export const DEFAULT_WING: WingBlueprint = {
  // x=0 为翅膀最左缘（sparBase），CSS left:100% 将其贴在立方体右边缘。
  viewBox: { minX: 0, minY: 0, width: 80, height: 200 },

  points: {
    // ── 膜翼轮廓点（沿路径顺序）──
    //   x=0(sparBase) = 翅膀最左缘，贴立方体右边缘
    rootMid:      { x: 24, y: 86 },
    upperTip1:    { x: 12, y: 58 },
    upperNotch1:  { x: 28, y: 65 },
    upperTip2:    { x: 12, y: 38 },
    upperNotch2:  { x: 34, y: 58 },
    wingTip:      { x: 66, y: 110 },
    lowerCurve1:  { x: 37, y: 78 },
    lowerCurve2:  { x: 50, y: 110 },
    rootBottom:   { x: 32, y: 110 },

    // ── 结构接缝线端点 ──
    sparBase:     { x: 0, y: 100 },
    sparEnd:      { x: 16, y: 100 },
    panelDivTop:  { x: 12, y: 58 },
    panelDivMid:  { x: 12, y: 38 },
    seamLowEnd:   { x: 32, y: 110 },
  },

  membraneStart: "rootMid",

  membraneSegments: [
    { to: "upperTip1",   cp: { x: 19, y: 68 } },
    { to: "upperNotch1", cp: { x: 18, y: 59 } },
    { to: "upperTip2",   cp: { x: 18, y: 48 } },
    { to: "upperNotch2", cp: { x: 20, y: 42 } },
    { to: "wingTip",     cp: { x: 54, y: 78 } },
    { to: "lowerCurve1", cp: { x: 50, y: 88 } },
    { to: "lowerCurve2", cp: { x: 46, y: 95 } },
    { to: "rootBottom",  cp: { x: 30, y: 80 } },
  ],

  seamLines: [
    // 从粗到细排列，线宽递减（3→3→1→1→1）
    { from: "sparEnd",    to: "rootMid",      cp: { x: 20, y: 90 }, strokeWidth: 3 },
    { from: "sparBase",   to: "sparEnd",      cp: { x: 8, y: 100 }, strokeWidth: 3 },
    { from: "rootMid",    to: "seamLowEnd",   cp: { x: 30, y: 95 }, strokeWidth: 1 },
    { from: "panelDivTop", to: "lowerCurve2", cp: { x: 32, y: 75 }, strokeWidth: 1 },
    { from: "panelDivMid", to: "wingTip",     cp: { x: 40, y: 65 }, strokeWidth: 1 },
  ],
};

/** 左翼蓝图：从右翼镜像生成。修改 DEFAULT_WING 后自动同步 */
export const DEFAULT_WING_LEFT = mirrorBlueprint(DEFAULT_WING);
