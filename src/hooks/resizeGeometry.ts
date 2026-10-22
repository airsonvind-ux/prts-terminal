// src/hooks/resizeGeometry.ts
// 窗口缩放几何（多方向）：纯函数 + 由 CSS 参数体系推导的反算常数。
//
// ── 坐标体系 ──
// 单位 face（--face-size）。窗口 = 宽 RATIO_X·face × 高 RATIO_Y·face。
// .cube 在窗口中的位置（逻辑坐标）：
//   cubeLeft = RATIO_X/2 − 1/2 + 0.0835 = 0.5001667   （0.0835 = left-width/2 = h-shift）
//   cubeTop  = RATIO_Y/2 − SILHOUETTE_H/2 = 0.0254    （造型竖向居中）
// 关键轮廓线（窗口坐标，单位 face）：
//   frontFace 左缘   = cubeLeft          0.5001667
//   frontFace 右缘   = cubeLeft + 1      1.5001667
//   frontFace 上缘   = cubeTop           0.0254
//   leftFace  左缘   = cubeLeft − 0.167  0.3331667
//   底面后缘（下缘） = cubeTop + 1.125   1.1504
//
// ── 反算约定（跟手性：拖动起点零跳变，位移 1:1）──
// 拖动语义 = "热区特征线跟随光标"：face =（光标到锚侧边的距离）/ K。
// 方向 →（测量轴、符号、锚角）：
//   右缘 → x, +, 锚左上     左缘 → x, −, 锚右上
//   下缘 → y, +, 锚左上     上缘 → y, −, 锚左下
//   右下角 → x, +, 锚左上   右上角 → x, +, 锚左下
//   左下角 → x, −, 锚右上   左上角 → x, −, 锚右下
// 四角一律按 x 轴反算。
// 特征线到锚侧边的距离即 K：
//   四缘 + 右列角：K = RATIO_X − LEFT_EDGE_X = 1.5001667（= 旧版 KGX = 11/12 + 0.5835）
//   上缘：        K = RATIO_Y − FRONT_TOP    = 1.5046
//   下缘：        K = BOTTOM_EDGE_Y          = 1.1504
//   左列角：      K = RATIO_X − FRONT_LEFT   = 1.3331667
// （数值变化需同步 DroneShellScene.module.css 的坐标参数体系）

export type ResizeDirection =
  | "right"
  | "left"
  | "top"
  | "bottom"
  | "topLeft"
  | "topRight"
  | "bottomLeft"
  | "bottomRight";

/** 窗口与 face 的固定比例：宽 11/6（翼展所需），高 1.53（造型高 + 上下边距） */
export const RATIO_X = 11 / 6;
export const RATIO_Y = 1.53;

/** face 允许范围（px） */
export const FACE_MIN = 180;
export const FACE_MAX = 520;

// ── 推导常数 ──

/** 造型竖向总高（正面顶边 → 尾巴尖）= 1 + 0.0625 + 0.41667，单位 face */
const SILHOUETTE_H = 1.4792;

const CUBE_LEFT = RATIO_X / 2 - 0.5 + 0.0835; // 0.5001667
const CUBE_TOP = RATIO_Y / 2 - SILHOUETTE_H / 2; // 0.0254
const LEFT_WIDTH = 0.167;
const BOTTOM_HEIGHT = 0.125;

const FRONT_LEFT = CUBE_LEFT;
const FRONT_TOP = CUBE_TOP;
const LEFT_EDGE_X = CUBE_LEFT - LEFT_WIDTH; // 0.3331667
const BOTTOM_EDGE_Y = CUBE_TOP + 1 + BOTTOM_HEIGHT; // 1.1504

const K_EDGE = RATIO_X - LEFT_EDGE_X; // 1.5001667
const K_TOP = RATIO_Y - FRONT_TOP; // 1.5046
const K_BOTTOM = BOTTOM_EDGE_Y; // 1.1504
const K_LEFT_CORNERS = RATIO_X - FRONT_LEFT; // 1.3331667

/** 锚角：拖动期间固定于屏幕的窗口角 */
export type ResizeAnchorCorner =
  | "topLeft"
  | "topRight"
  | "bottomLeft"
  | "bottomRight";

export interface ResizeDirectionSpec {
  /** 反算常数（单位 face） */
  k: number;
  /** 测量轴 */
  axis: "x" | "y";
  /** 距离符号：false = 光标 − 锚；true = 锚 − 光标 */
  neg: boolean;
  /** 锚角 */
  corner: ResizeAnchorCorner;
}

export const RESIZE_SPECS: Record<ResizeDirection, ResizeDirectionSpec> = {
  right: { k: K_EDGE, axis: "x", neg: false, corner: "topLeft" },
  left: { k: K_EDGE, axis: "x", neg: true, corner: "topRight" },
  top: { k: K_TOP, axis: "y", neg: true, corner: "bottomLeft" },
  bottom: { k: K_BOTTOM, axis: "y", neg: false, corner: "topLeft" },
  topLeft: { k: K_LEFT_CORNERS, axis: "x", neg: true, corner: "bottomRight" },
  topRight: { k: K_EDGE, axis: "x", neg: false, corner: "bottomLeft" },
  bottomLeft: { k: K_LEFT_CORNERS, axis: "x", neg: true, corner: "topRight" },
  bottomRight: { k: K_EDGE, axis: "x", neg: false, corner: "topLeft" },
};

/** 锚角屏幕坐标（逻辑域） */
export interface ResizeAnchor {
  x: number;
  y: number;
}

export interface ResizeResult {
  /** 新 face 尺寸（已钳制） */
  face: number;
  /** 窗口新位置（逻辑域，屏幕坐标） */
  x: number;
  y: number;
}

/** 由窗口位置与尺寸求指定锚角的屏幕坐标（宽高分开） */
export function anchorPoint(
  corner: ResizeAnchorCorner,
  winX: number,
  winY: number,
  winW: number,
  winH: number
): ResizeAnchor {
  return {
    x: corner === "topRight" || corner === "bottomRight" ? winX + winW : winX,
    y:
      corner === "bottomLeft" || corner === "bottomRight"
        ? winY + winH
        : winY,
  };
}

/**
 * 从光标屏幕坐标反算 face 与窗口新位置。
 * @param dir 拖动方向
 * @param mx 光标屏幕 x（逻辑域）
 * @param my 光标屏幕 y（逻辑域）
 * @param anchor 锚角屏幕坐标（mousedown 快照；拖动期间不变）
 */
export function computeResize(
  dir: ResizeDirection,
  mx: number,
  my: number,
  anchor: ResizeAnchor
): ResizeResult {
  const spec = RESIZE_SPECS[dir];

  const cursor = spec.axis === "x" ? mx : my;
  const fixed = spec.axis === "x" ? anchor.x : anchor.y;
  const dist = spec.neg ? fixed - cursor : cursor - fixed;

  const raw = dist / spec.k;
  const face = Math.max(FACE_MIN, Math.min(FACE_MAX, raw));
  const w = face * RATIO_X;
  const h = face * RATIO_Y;

  const x =
    spec.corner === "topRight" || spec.corner === "bottomRight"
      ? anchor.x - w
      : anchor.x;
  const y =
    spec.corner === "bottomLeft" || spec.corner === "bottomRight"
      ? anchor.y - h
      : anchor.y;

  return { face, x, y };
}
