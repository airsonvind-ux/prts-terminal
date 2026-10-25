import { describe, expect, it } from "vitest";
import { roundedPolygonPath } from "./roundedPolygon";

// 替换前由 svg-smoother 生成的路径，坐标与曲线完全一致；这里只把
// 库自动合并的 H/V 命令展开成 L，便于本地实现保持简单。
const LEFT_EXPECTED =
  "M 0.327 2.769 L 3.923 0.231 C 4.087 0.115 4.248 0.2 4.247 0.4 L 4.053 23.6 C 4.052 23.8 3.889 24.119 3.729 24.238 L 0.321 26.762 C 0.161 26.881 0 26.8 0 26.6 L 0 3.4 C 0 3.2 0.163 2.885 0.327 2.769 Z";

const BOTTOM_EXPECTED =
  "M 4.4 0 L 27.7 0 C 27.9 0 27.936 0.114 27.772 0.229 L 24.128 2.771 C 23.964 2.886 23.6 3 23.4 3 L 0.4 3 C 0.2 3 0.16 2.88 0.32 2.76 L 3.68 0.24 C 3.84 0.12 4.2 0 4.4 0 Z";

const LEFT: [number, number][] = [
  [0, 3],
  [4.25, 0],
  [4.05, 24],
  [0, 27],
];

const BOTTOM: [number, number][] = [
  [4, 0],
  [28.1, 0],
  [23.8, 3],
  [0, 3],
];

describe("roundedPolygonPath", () => {
  it("matches the shell's left-face geometry", () => {
    expect(roundedPolygonPath(LEFT, 0.4)).toBe(LEFT_EXPECTED);
  });

  it("matches the shell's bottom-face geometry", () => {
    expect(roundedPolygonPath(BOTTOM, 0.4)).toBe(BOTTOM_EXPECTED);
  });

  it("falls back to a plain closed polygon at radius 0", () => {
    expect(roundedPolygonPath(LEFT, 0)).toBe(
      "M 0 3 L 4.25 0 L 4.05 24 L 0 27 Z",
    );
  });

  it("treats negative radius as no rounding", () => {
    expect(roundedPolygonPath(LEFT, -1)).toBe(
      "M 0 3 L 4.25 0 L 4.05 24 L 0 27 Z",
    );
  });

  it("returns an empty path for fewer than three points", () => {
    expect(roundedPolygonPath([[0, 0], [1, 1]], 0.4)).toBe("");
  });

  it("never emits NaN for the shell's polygons", () => {
    expect(roundedPolygonPath(LEFT, 0.4)).not.toContain("NaN");
    expect(roundedPolygonPath(BOTTOM, 0.4)).not.toContain("NaN");
  });
});
