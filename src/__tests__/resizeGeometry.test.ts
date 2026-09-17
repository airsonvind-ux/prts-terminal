// resizeGeometry · 纯几何
// 覆盖：K 常量表、锚角分配、各方向起点零跳变（face 与窗口位置复原）、
//       位移 1:1 跟手与锚角固定、face 钳制、anchorPoint。

import { describe, expect, it } from "vitest";
import {
  anchorPoint,
  computeResize,
  FACE_MAX,
  FACE_MIN,
  RATIO,
  RESIZE_SPECS,
  type ResizeDirection,
} from "../hooks/resizeGeometry";

const F = 300;
const WX = 1000;
const WY = 480;
const SIZE = F * RATIO;

const ALL: ResizeDirection[] = [
  "right",
  "left",
  "top",
  "bottom",
  "topLeft",
  "topRight",
  "bottomLeft",
  "bottomRight",
];

/** 各方向"热区特征线"相对窗口左上角的偏移（单位 face）——光标起点落在线上 */
const HOTSPOT: Record<ResizeDirection, { ox: number; oy: number }> = {
  right: { ox: 1.5001667, oy: 0.5 },
  left: { ox: 0.3331667, oy: 0.8 },
  top: { ox: 1.0, oy: 0.3496667 },
  bottom: { ox: 1.0, oy: 1.4746667 },
  topLeft: { ox: 0.5001667, oy: 0.3496667 },
  topRight: { ox: 1.5001667, oy: 0.3496667 },
  bottomLeft: { ox: 0.5001667, oy: 1.3496667 },
  bottomRight: { ox: 1.5001667, oy: 1.3496667 },
};

describe("resizeGeometry · 反算常数", () => {
  it("K 常量与 CSS 参数体系推导一致", () => {
    expect(RESIZE_SPECS.right.k).toBeCloseTo(1.5001667, 6);
    expect(RESIZE_SPECS.left.k).toBeCloseTo(1.5001667, 6);
    expect(RESIZE_SPECS.top.k).toBeCloseTo(1.4836667, 6);
    expect(RESIZE_SPECS.bottom.k).toBeCloseTo(1.4746667, 6);
    expect(RESIZE_SPECS.topLeft.k).toBeCloseTo(1.3331667, 6);
    expect(RESIZE_SPECS.bottomLeft.k).toBeCloseTo(1.3331667, 6);
    expect(RESIZE_SPECS.topRight.k).toBeCloseTo(1.5001667, 6);
    expect(RESIZE_SPECS.bottomRight.k).toBeCloseTo(1.5001667, 6);
  });

  it("锚角分配：右/下系锚左上，左/上系锚对应侧", () => {
    expect(RESIZE_SPECS.right.corner).toBe("topLeft");
    expect(RESIZE_SPECS.bottom.corner).toBe("topLeft");
    expect(RESIZE_SPECS.bottomRight.corner).toBe("topLeft");
    expect(RESIZE_SPECS.left.corner).toBe("topRight");
    expect(RESIZE_SPECS.bottomLeft.corner).toBe("topRight");
    expect(RESIZE_SPECS.top.corner).toBe("bottomLeft");
    expect(RESIZE_SPECS.topRight.corner).toBe("bottomLeft");
    expect(RESIZE_SPECS.topLeft.corner).toBe("bottomRight");
  });
});

describe("resizeGeometry · 拖动行为", () => {
  it("各方向在特征线上按下时零跳变：face 不变、窗口位置复原", () => {
    for (const dir of ALL) {
      const anchor = anchorPoint(RESIZE_SPECS[dir].corner, WX, WY, SIZE);
      const spot = HOTSPOT[dir];
      const mx = WX + spot.ox * F;
      const my = WY + spot.oy * F;

      const r = computeResize(dir, mx, my, anchor);
      expect(r.face, `${dir} face`).toBeCloseTo(F, 1);
      expect(r.x, `${dir} x`).toBeCloseTo(WX, 1);
      expect(r.y, `${dir} y`).toBeCloseTo(WY, 1);
    }
  });

  it("向外拖动 60px：face 按 K 反算，锚角固定不动", () => {
    const anchorTL = anchorPoint("topLeft", WX, WY, SIZE);
    const anchorTR = anchorPoint("topRight", WX, WY, SIZE);
    const anchorBL = anchorPoint("bottomLeft", WX, WY, SIZE);
    const anchorBR = anchorPoint("bottomRight", WX, WY, SIZE);

    // 右缘：光标右移 60；锚左上 → 位置不动
    const r1 = computeResize("right", WX + 1.5001667 * F + 60, WY + 150, anchorTL);
    expect(r1.face).toBeCloseTo(F + 60 / 1.5001667, 2);
    expect(r1.x).toBeCloseTo(WX, 1);
    expect(r1.y).toBeCloseTo(WY, 1);

    // 左缘：光标左移 60；锚右上 → 右缘固定
    const r2 = computeResize("left", WX + 0.3331667 * F - 60, WY + 150, anchorTR);
    const e2 = F + 60 / 1.5001667;
    expect(r2.face).toBeCloseTo(e2, 2);
    expect(r2.x).toBeCloseTo(WX + SIZE - e2 * RATIO, 1);
    expect(r2.y).toBeCloseTo(WY, 1);

    // 上缘：光标上移 60；锚左下 → 下缘固定
    const r3 = computeResize("top", WX + 100, WY + 0.3496667 * F - 60, anchorBL);
    const e3 = F + 60 / 1.4836667;
    expect(r3.face).toBeCloseTo(e3, 2);
    expect(r3.x).toBeCloseTo(WX, 1);
    expect(r3.y).toBeCloseTo(WY + SIZE - e3 * RATIO, 1);

    // 下缘：光标下移 60；锚左上 → 位置不动
    const r4 = computeResize("bottom", WX + 100, WY + 1.4746667 * F + 60, anchorTL);
    expect(r4.face).toBeCloseTo(F + 60 / 1.4746667, 2);
    expect(r4.x).toBeCloseTo(WX, 1);
    expect(r4.y).toBeCloseTo(WY, 1);

    // 左上角：光标左移 60；锚右下 → 右下角固定
    const r5 = computeResize("topLeft", WX + 0.5001667 * F - 60, WY + 100, anchorBR);
    const e5 = F + 60 / 1.3331667;
    expect(r5.face).toBeCloseTo(e5, 2);
    expect(r5.x).toBeCloseTo(WX + SIZE - e5 * RATIO, 1);
    expect(r5.y).toBeCloseTo(WY + SIZE - e5 * RATIO, 1);

    // 右上角：光标右移 60；锚左下 → 左下角固定
    const r6 = computeResize("topRight", WX + 1.5001667 * F + 60, WY + 100, anchorBL);
    const e6 = F + 60 / 1.5001667;
    expect(r6.face).toBeCloseTo(e6, 2);
    expect(r6.x).toBeCloseTo(WX, 1);
    expect(r6.y).toBeCloseTo(WY + SIZE - e6 * RATIO, 1);

    // 左下角：光标左移 60；锚右上 → 右缘与上缘固定
    const r7 = computeResize("bottomLeft", WX + 0.5001667 * F - 60, WY + 100, anchorTR);
    const e7 = F + 60 / 1.3331667;
    expect(r7.face).toBeCloseTo(e7, 2);
    expect(r7.x).toBeCloseTo(WX + SIZE - e7 * RATIO, 1);
    expect(r7.y).toBeCloseTo(WY, 1);

    // 右下角：光标右移 60；锚左上 → 位置不动
    const r8 = computeResize("bottomRight", WX + 1.5001667 * F + 60, WY + 100, anchorTL);
    expect(r8.face).toBeCloseTo(F + 60 / 1.5001667, 2);
    expect(r8.x).toBeCloseTo(WX, 1);
    expect(r8.y).toBeCloseTo(WY, 1);
  });

  it("face 超限时钳制；钳制后锚角仍固定", () => {
    const anchorTL = anchorPoint("topLeft", WX, WY, SIZE);
    const anchorTR = anchorPoint("topRight", WX, WY, SIZE);

    // 拖到远超上限：右缘
    const grow = computeResize("right", WX + 999999, WY, anchorTL);
    expect(grow.face).toBe(FACE_MAX);
    expect(grow.x).toBeCloseTo(WX, 1);
    expect(grow.y).toBeCloseTo(WY, 1);

    // 拖到下限之下：左缘（距离反向 → 钳制到最小值）
    const shrink = computeResize("left", WX + 999999, WY, anchorTR);
    expect(shrink.face).toBe(FACE_MIN);
    expect(shrink.x).toBeCloseTo(anchorTR.x - FACE_MIN * RATIO, 1);
    expect(shrink.y).toBeCloseTo(WY, 1);
  });
});

describe("resizeGeometry · anchorPoint", () => {
  it("返回四个角的屏幕坐标", () => {
    expect(anchorPoint("topLeft", 10, 20, 100)).toEqual({ x: 10, y: 20 });
    expect(anchorPoint("topRight", 10, 20, 100)).toEqual({ x: 110, y: 20 });
    expect(anchorPoint("bottomLeft", 10, 20, 100)).toEqual({ x: 10, y: 120 });
    expect(anchorPoint("bottomRight", 10, 20, 100)).toEqual({ x: 110, y: 120 });
  });
});
