// src/components/shell/DroneShellScene.tsx
// 无人机视觉外壳 · 单一组件
//
// 职责：渲染立方体六面 + 翅膀 + 尾巴，管理所有视觉元素的坐标体系。
// 内容层（对话界面）通过 children 注入正面内部。
// 拖拽手柄通过 onDragHandle 交给调用方，缩放通过 onResizeHandle 绑定 8 方向热区。
//
// 阶段 C · 全部 7 步完成；批 2-D · 动画归统一 rAF 引擎（droneEngine）。
//
// 几何常量层：所有 SVG 顶点在 viewBox 比例坐标系中的定义。
// 比例系 24:4:3（正面:左面:底面），viewBox 整数化表达。

import { type ReactNode, type MouseEvent, useRef, useEffect } from "react";
import type { ResizeDirection } from "../../hooks/resizeGeometry";
import { buildWing, DEFAULT_WING, DEFAULT_WING_LEFT } from "./wingGeometry";
import { computeTailPath } from "./tailAnimation";
import { roundedPolygonPath } from "./roundedPolygon";
import { droneEngine } from "../../animation/droneEngine";
import styles from "./DroneShellScene.module.css";

// 翅膀 path 静态预计算
const wingPaths = buildWing(DEFAULT_WING);
const wingPathsLeft = buildWing(DEFAULT_WING_LEFT);

// 尾巴初始中性姿态（引擎挂载前 / React 重渲染瞬间兜底，随后逐帧被引擎覆盖）
const INITIAL_TAIL = computeTailPath(0, 1);

// ── 几何常量 ──
// 所有坐标在各自 viewBox 的比例空间中，非 CSS 像素。
// 顶点按顺时针排列。坐标中带有手动微调溢出补偿（防接缝、贴 border），
// 详见各面的语义注释。

/** 左面：viewBox 4×27，比例 = left-width : (face-size + bottom-height) */
const LEFT_FACE = {
  viewBox: "0 0 4 27",
  vertices: [
    [0, 3],     // 后上：x=0 锚定正面左边界，y 微降 3 与正面 border-radius 后上弧段对齐
    [4.25, 0],  // 前上：x 溢 +0.25 防接缝，y=0 贴正面顶边
    [4.05, 24], // 前下：x 溢 +0.05 防接缝，y=24 贴正面底边
    [0, 27],     // 后下：x=0 锚定正面左边界，y=27 为退远向量 dy 收束点（3 + 24 = 27）
  ] as [number, number][],
};

/** 底面：viewBox 28×3，比例 = (face-size + left-width) : bottom-height */
const BOTTOM_FACE = {
  viewBox: "0 0 28 3",
  vertices: [
    [4, 0],      // 前左：x=4 为正面左下角 viewBox 投影
    [28.1, 0],   // 前右：x 溢 +0.1 防接缝
    [23.8, 3],     // 后右：退远向量 dx 收束，y=3 = bottom-height
    [0, 3],      // 后左：x=0 与左面后下角的退远分量为 0 一致
  ] as [number, number][],
};

/** 接缝线：单 SVG 包围盒 viewBox 28×27，比例 = (face+left) : (face+bottom) */
const JUNCTION = {
  viewBox: "0 0 28 27",
  lines: [
    { x1: 4.05, y1: 0.19, x2: 4.05, y2: 24 },     // 正面左缘，x 与左面前下溢位对齐；y1 微降 0.19
    { x1: 4, y1: 24, x2: 27.85, y2: 24 },      // 正面底缘，x2 溢 -0.15 防接缝
    { x1: 4, y1: 24, x2: 0.2, y2: 26.8 },   // 左面底缘（斜线），端点溢出对齐两面交汇处
  ],
};

interface DroneShellSceneProps {
  /** 正面内容区插槽（Step 6 由 ChatView 填充） */
  children?: ReactNode;
  /** 拖拽手柄回调（Step 7 对接 useDrag Hook） */
  onDragHandle?: (e: MouseEvent) => void;
  /** 缩放拖拽回调（对接 useResize Hook）。8 方向热区共用，方向随按下热区传入 */
  onResizeHandle?: (e: MouseEvent, dir: ResizeDirection) => void;
}

export function DroneShellScene({
  children,
  onDragHandle,
  onResizeHandle,
}: DroneShellSceneProps) {
  // 动画元素 ref（批 2-D）：引擎逐帧直写 transform / path d，不经 React state
  const cubeRef = useRef<HTMLDivElement>(null);
  const wingLeftRef = useRef<SVGSVGElement>(null);
  const wingRightRef = useRef<SVGSVGElement>(null);
  const tailRef = useRef<SVGSVGElement>(null);

  useEffect(() => {
    droneEngine.attach({
      cube: cubeRef.current!,
      wingLeft: wingLeftRef.current!,
      wingRight: wingRightRef.current!,
      tailSvg: tailRef.current!,
    });
    // StrictMode 双挂载：cleanup 成对取消循环，引擎状态跨重挂载保留
    return () => droneEngine.detach();
  }, []);

  return (
    <div className={styles.shell}>
      <div className={styles.cube} ref={cubeRef}>
        {/* 左翼 */}
        <svg ref={wingLeftRef} className={styles.wingLeft} viewBox={`${DEFAULT_WING.viewBox.minX} ${DEFAULT_WING.viewBox.minY} ${DEFAULT_WING.viewBox.width} ${DEFAULT_WING.viewBox.height}`} xmlns="http://www.w3.org/2000/svg">
          <path d={wingPathsLeft.membrane} fill="#000" />
          {wingPathsLeft.seams.map((d, i) => (
            <path key={i} d={d} stroke="#000" strokeWidth={wingPathsLeft.seamWidths[i]} fill="none" strokeLinecap="round" />
          ))}
        </svg>
        {/* 右翼 */}
        <svg ref={wingRightRef} className={styles.wingRight} viewBox={`${DEFAULT_WING.viewBox.minX} ${DEFAULT_WING.viewBox.minY} ${DEFAULT_WING.viewBox.width} ${DEFAULT_WING.viewBox.height}`} xmlns="http://www.w3.org/2000/svg">
          <path d={wingPaths.membrane} fill="#000" />
          {wingPaths.seams.map((d, i) => (
            <path key={i} d={d} stroke="#000" strokeWidth={wingPaths.seamWidths[i]} fill="none" strokeLinecap="round" />
          ))}
        </svg>
        {/* 尾巴：身体 = stroked 粗线，箭头 = filled 三角形。路径由引擎逐帧直写 */}
        <svg
          ref={tailRef}
          className={styles.tailSvg}
          viewBox="-15 -10 30 100"
          xmlns="http://www.w3.org/2000/svg"
        >
          <path d={INITIAL_TAIL.body} stroke="#000" strokeWidth="2" strokeLinecap="round" fill="none" />
          <path d={INITIAL_TAIL.arrow} fill="#000" />
        </svg>
        {/* 左面 */}
        <svg className={styles.leftFace} viewBox={LEFT_FACE.viewBox} xmlns="http://www.w3.org/2000/svg" onMouseDown={onDragHandle}>
          <path
            d={roundedPolygonPath(LEFT_FACE.vertices, 0.4)}
            fill="#1a1a1a"
          />
        </svg>
        {/* 底面 */}
        <svg className={styles.bottomFace} viewBox={BOTTOM_FACE.viewBox} xmlns="http://www.w3.org/2000/svg" onMouseDown={onDragHandle}>
          <path
            d={roundedPolygonPath(BOTTOM_FACE.vertices, 0.4)}
            fill="#2a2a2a"
          />
        </svg>
        {/* 接缝线 */}
        <svg className={styles.junctionLines} viewBox={JUNCTION.viewBox} xmlns="http://www.w3.org/2000/svg">
          {JUNCTION.lines.map((line, i) => (
            <line
              key={i}
              x1={line.x1} y1={line.y1} x2={line.x2} y2={line.y2}
              stroke="#2d3033" strokeWidth="0.2" strokeLinecap="round"
            />
          ))}
        </svg>
        {/* 正面：立方体主面，无边框 */}
        <div className={styles.frontFace} data-guide="face">
          {/* ── 缩放热区（无视觉标记，仅光标暗示；方向语义见 resizeGeometry.ts）──
              边缘 6px / 角 12×12；缘在前、角在后（角覆盖边缘交叉区） */}
          <div className={styles.resizeRight} onMouseDown={(e) => onResizeHandle?.(e, "right")} />
          <div className={styles.resizeTop} onMouseDown={(e) => onResizeHandle?.(e, "top")} />
          <div className={styles.resizeTopLeft} onMouseDown={(e) => onResizeHandle?.(e, "topLeft")} />
          <div className={styles.resizeTopRight} onMouseDown={(e) => onResizeHandle?.(e, "topRight")} />
          <div className={styles.resizeBottomLeft} onMouseDown={(e) => onResizeHandle?.(e, "bottomLeft")} />
          <div className={styles.resizeBottomRight} onMouseDown={(e) => onResizeHandle?.(e, "bottomRight")} />
          {children}
        </div>
        {/* 左缘 / 下缘缩放热区（贴左面与底面轮廓，位于正面之外） */}
        <div className={styles.resizeLeft} onMouseDown={(e) => onResizeHandle?.(e, "left")} />
        <div className={styles.resizeBottom} onMouseDown={(e) => onResizeHandle?.(e, "bottom")} />
      </div>
    </div>
  );
}
