// 圆角多边形路径 · 外壳左面 / 底面专用
//
// 只处理凸多边形：每个顶点沿两条邻边各裁掉一小段，再用三次贝塞尔
// 连接两个裁点。半径远小于边长时，结果与常见 SVG 圆角库一致。
//
// 这里不追求通用：不处理自交、凹多边形、椭圆角；调用方只有两个四点
// 四边形，半径 0.4。保持实现小、可测试、无外部依赖。

export type PolygonPoint = readonly [number, number];

type Point = { x: number; y: number };

function toPoint(value: PolygonPoint): Point {
  return { x: value[0], y: value[1] };
}

function distance(a: Point, b: Point): number {
  return Math.hypot(b.x - a.x, b.y - a.y);
}

/** 从 from 朝 to 走 distance；distance 会被限制在两点距离内。 */
function toward(from: Point, to: Point, distanceFromFrom: number): Point {
  const length = distance(from, to);
  if (length === 0) return { ...from };
  const ratio = Math.min(1, distanceFromFrom / length);
  return {
    x: from.x + (to.x - from.x) * ratio,
    y: from.y + (to.y - from.y) * ratio,
  };
}

function midpoint(a: Point, b: Point): Point {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

/** 保留 3 位小数，去掉多余的 0；避免 -0。 */
function formatNumber(value: number): string {
  const rounded = Math.round(value * 1000) / 1000;
  if (Object.is(rounded, -0)) return "0";
  return String(rounded);
}

function moveTo(point: Point): string {
  return `M ${formatNumber(point.x)} ${formatNumber(point.y)}`;
}

function lineTo(point: Point): string {
  return `L ${formatNumber(point.x)} ${formatNumber(point.y)}`;
}

function curveTo(controlA: Point, controlB: Point, end: Point): string {
  return `C ${formatNumber(controlA.x)} ${formatNumber(controlA.y)} ${formatNumber(controlB.x)} ${formatNumber(controlB.y)} ${formatNumber(end.x)} ${formatNumber(end.y)}`;
}

/**
 * 生成闭合圆角多边形路径。
 *
 * 半径 <= 0 时退化为普通多边形。点数不足 3 时返回空字符串。
 */
export function roundedPolygonPath(
  points: readonly PolygonPoint[],
  radius: number,
): string {
  if (points.length < 3) return "";

  const vertices = points.map(toPoint);
  const safeRadius = Number.isFinite(radius) ? Math.max(0, radius) : 0;

  if (safeRadius === 0) {
    return [
      moveTo(vertices[0]),
      ...vertices.slice(1).map(lineTo),
      "Z",
    ].join(" ");
  }

  const count = vertices.length;
  const corners = vertices.map((vertex, index) => {
    const previous = vertices[(index - 1 + count) % count];
    const next = vertices[(index + 1) % count];
    // 两边各留一半，防止相邻角的裁切段重叠。
    const trim = Math.min(
      safeRadius,
      distance(vertex, previous) / 2,
      distance(vertex, next) / 2,
    );
    return {
      vertex,
      previous: toward(vertex, previous, trim),
      next: toward(vertex, next, trim),
    };
  });

  const first = corners[0];
  const parts = [moveTo(first.next)];

  // 从第一个角之后开始：直线进入每个角，再用曲线绕过该角。
  for (let index = 1; index < count; index += 1) {
    const corner = corners[index];
    parts.push(lineTo(corner.previous));
    parts.push(
      curveTo(
        midpoint(corner.previous, corner.vertex),
        midpoint(corner.vertex, corner.next),
        corner.next,
      ),
    );
  }

  // 回到起点所在的角。
  parts.push(lineTo(first.previous));
  parts.push(
    curveTo(
      midpoint(first.previous, first.vertex),
      midpoint(first.vertex, first.next),
      first.next,
    ),
  );
  parts.push("Z");

  return parts.join(" ");
}
