// src/debug/WingDebug.tsx
// 翅膀线条调试面板（wing-debug.html）
//
// 左栏参数面板 + 右侧实时渲染（右翼大图 + 左翼镜像小图）。
// 参数结构 = wingGeometry 的 WingBlueprint（命名点 + 膜翼贝塞尔控制点 + 接缝线）。
// 调整实时生效；「导出蓝图 JSON」复制当前参数，用于更新 wingGeometry.ts 的 DEFAULT_WING。

import { useMemo, useState } from "react";
import {
  buildWing,
  mirrorBlueprint,
  DEFAULT_WING,
  type WingBlueprint,
} from "../components/shell/wingGeometry";

// ── 蓝图深拷贝（结构化克隆兜底，避免直接改共享常量） ──

function cloneBlueprint(b: WingBlueprint): WingBlueprint {
  return {
    ...b,
    viewBox: { ...b.viewBox },
    points: Object.fromEntries(
      Object.entries(b.points).map(([k, v]) => [k, { ...v }])
    ),
    membraneSegments: b.membraneSegments.map((s) => ({ ...s, cp: { ...s.cp } })),
    seamLines: b.seamLines.map((l) => ({ ...l, cp: l.cp ? { ...l.cp } : undefined })),
  };
}

// ── 布局常量 ──

const PANEL_W = 300;
const RENDER_SCALE = 5; // viewBox 80×200 → 400×1000 显示

const fieldStyle: React.CSSProperties = {
  width: 58,
  fontSize: 12,
  padding: "1px 4px",
  border: "1px solid #c9ccd1",
  borderRadius: 3,
  background: "#fff",
  color: "#333",
};

const rowStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: 4,
  marginBottom: 3,
};

const labelStyle: React.CSSProperties = {
  fontSize: 11,
  color: "#555",
  width: 110,
  flexShrink: 0,
  overflow: "hidden",
  textOverflow: "ellipsis",
  whiteSpace: "nowrap",
};

const sectionTitleStyle: React.CSSProperties = {
  fontSize: 12,
  fontWeight: 600,
  color: "#222",
  margin: "10px 0 6px",
  borderBottom: "1px solid #d8dade",
  paddingBottom: 2,
};

/** 数值输入（非法输入忽略，保持原值） */
function NumField({
  value,
  onChange,
}: {
  value: number;
  onChange: (v: number) => void;
}): React.JSX.Element {
  return (
    <input
      type="number"
      step={0.5}
      value={value}
      style={fieldStyle}
      onChange={(e) => {
        const n = parseFloat(e.target.value);
        if (!Number.isNaN(n)) onChange(n);
      }}
    />
  );
}

/** 坐标行：名称 + x + y */
function CoordRow({
  label,
  pt,
  onChange,
}: {
  label: string;
  pt: { x: number; y: number };
  onChange: (axis: "x" | "y", v: number) => void;
}): React.JSX.Element {
  return (
    <div style={rowStyle}>
      <span style={labelStyle}>{label}</span>
      <NumField value={pt.x} onChange={(v) => onChange("x", v)} />
      <NumField value={pt.y} onChange={(v) => onChange("y", v)} />
    </div>
  );
}

export function WingDebug(): React.JSX.Element {
  const [bp, setBp] = useState<WingBlueprint>(() => cloneBlueprint(DEFAULT_WING));
  const [exportText, setExportText] = useState<string | null>(null);

  // ── 更新函数（不可变更新） ──
  const setPoint = (key: string, axis: "x" | "y", v: number): void =>
    setBp((b) => {
      const pt = b.points[key];
      return {
        ...b,
        points: {
          ...b.points,
          [key]: axis === "x" ? { x: v, y: pt.y } : { x: pt.x, y: v },
        },
      };
    });
  const setMembraneCp = (i: number, axis: "x" | "y", v: number): void =>
    setBp((b) => ({
      ...b,
      membraneSegments: b.membraneSegments.map((s, j) =>
        j === i
          ? {
              ...s,
              cp: axis === "x" ? { x: v, y: s.cp.y } : { x: s.cp.x, y: v },
            }
          : s
      ),
    }));
  const setSeamCp = (i: number, axis: "x" | "y", v: number): void =>
    setBp((b) => ({
      ...b,
      seamLines: b.seamLines.map((l, j) =>
        j === i && l.cp
          ? {
              ...l,
              cp: axis === "x" ? { x: v, y: l.cp.y } : { x: l.cp.x, y: v },
            }
          : l
      ),
    }));
  const setSeamWidth = (i: number, v: number): void =>
    setBp((b) => ({
      ...b,
      seamLines: b.seamLines.map((l, j) => (j === i ? { ...l, strokeWidth: v } : l)),
    }));
  const setViewBox = (axis: "width" | "height", v: number): void =>
    setBp((b) => ({ ...b, viewBox: { ...b.viewBox, [axis]: v } }));

  // ── 渲染输出（纯函数，随参数实时重算） ──
  const right = useMemo(() => buildWing(bp), [bp]);
  const left = useMemo(() => buildWing(mirrorBlueprint(bp)), [bp]);

  const exportJson = (): void => {
    const json = JSON.stringify(bp, null, 2);
    setExportText(json);
    navigator.clipboard?.writeText(json).catch(() => {
      // 剪贴板不可用时 textarea 手动复制
    });
  };

  const reset = (): void => {
    setBp(cloneBlueprint(DEFAULT_WING));
    setExportText(null);
  };

  return (
    <div
      style={{
        display: "flex",
        minHeight: "100vh",
        background: "#f2f3f5",
        fontFamily: '"Microsoft YaHei", sans-serif',
      }}
    >
      {/* ── 左栏：参数面板 ── */}
      <div
        style={{
          width: PANEL_W,
          flexShrink: 0,
          padding: 12,
          background: "#e8eaee",
          borderRight: "1px solid #d0d3d7",
          overflowY: "auto",
          height: "100vh",
          position: "sticky",
          top: 0,
          boxSizing: "border-box",
        }}
      >
        <div style={{ fontSize: 13, fontWeight: 700, color: "#222" }}>
          翅膀蓝图参数
        </div>
        <div style={{ fontSize: 11, color: "#666", marginTop: 2 }}>
          调整实时生效 · 满意后「导出蓝图 JSON」用于 wingGeometry.ts
        </div>

        <div style={sectionTitleStyle}>viewBox</div>
        <div style={rowStyle}>
          <span style={labelStyle}>宽 / 高</span>
          <NumField value={bp.viewBox.width} onChange={(v) => setViewBox("width", v)} />
          <NumField value={bp.viewBox.height} onChange={(v) => setViewBox("height", v)} />
        </div>

        <div style={sectionTitleStyle}>命名点 points</div>
        {Object.entries(bp.points).map(([key, pt]) => (
          <CoordRow
            key={key}
            label={key}
            pt={pt}
            onChange={(axis, v) => setPoint(key, axis, v)}
          />
        ))}

        <div style={sectionTitleStyle}>膜翼控制点 membrane cp</div>
        {bp.membraneSegments.map((seg, i) => (
          <CoordRow
            key={i}
            label={`${i}→${seg.to}`}
            pt={seg.cp}
            onChange={(axis, v) => setMembraneCp(i, axis, v)}
          />
        ))}

        <div style={sectionTitleStyle}>接缝线 seam（cp + 线宽）</div>
        {bp.seamLines.map((line, i) => (
          <div key={i}>
            <CoordRow
              label={`${i} ${line.from}→${line.to}`}
              pt={line.cp ?? { x: 0, y: 0 }}
              onChange={(axis, v) => setSeamCp(i, axis, v)}
            />
            <div style={{ ...rowStyle, marginLeft: 14 }}>
              <span style={{ fontSize: 11, color: "#666", width: 60 }}>线宽</span>
              <NumField value={line.strokeWidth} onChange={(v) => setSeamWidth(i, v)} />
            </div>
          </div>
        ))}

        <div style={{ display: "flex", gap: 8, marginTop: 14 }}>
          <button
            onClick={exportJson}
            style={{
              padding: "4px 12px",
              fontSize: 12,
              background: "#4AABEA",
              color: "#fff",
              border: "none",
              borderRadius: 3,
              cursor: "pointer",
            }}
          >
            导出蓝图 JSON
          </button>
          <button
            onClick={reset}
            style={{
              padding: "4px 12px",
              fontSize: 12,
              background: "transparent",
              color: "#444",
              border: "1px solid #aaa",
              borderRadius: 3,
              cursor: "pointer",
            }}
          >
            恢复默认
          </button>
        </div>

        {exportText && (
          <textarea
            readOnly
            value={exportText}
            onFocus={(e) => e.target.select()}
            style={{
              width: "100%",
              height: 220,
              marginTop: 10,
              fontSize: 10,
              fontFamily: "monospace",
              boxSizing: "border-box",
              border: "1px solid #c0c3c8",
              borderRadius: 3,
            }}
          />
        )}
      </div>

      {/* ── 右栏：渲染区 ── */}
      <div style={{ flex: 1, padding: 24, display: "flex", flexDirection: "column", gap: 24, alignItems: "flex-start" }}>
        <div>
          <div style={{ fontSize: 12, color: "#666", marginBottom: 6 }}>
            右翼（基准蓝图）
          </div>
          <WingSvg
            membrane={right.membrane}
            seams={right.seams}
            seamWidths={right.seamWidths}
            vb={bp.viewBox}
            width={bp.viewBox.width * RENDER_SCALE}
          />
        </div>
        <div>
          <div style={{ fontSize: 12, color: "#666", marginBottom: 6 }}>
            左翼（自动镜像）
          </div>
          <WingSvg
            membrane={left.membrane}
            seams={left.seams}
            seamWidths={left.seamWidths}
            vb={bp.viewBox}
            width={bp.viewBox.width * RENDER_SCALE * 0.5}
          />
        </div>
      </div>
    </div>
  );
}

/** 单翼渲染：浅色底 + 网格 + 黑色膜翼/接缝（还原 PRTS 线条观感） */
function WingSvg({
  membrane,
  seams,
  seamWidths,
  vb,
  width,
}: {
  membrane: string;
  seams: string[];
  seamWidths: number[];
  vb: { minX: number; minY: number; width: number; height: number };
  width: number;
}): React.JSX.Element {
  const height = (width * vb.height) / vb.width;
  return (
    <svg
      viewBox={`${vb.minX} ${vb.minY} ${vb.width} ${vb.height}`}
      width={width}
      height={height}
      style={{ background: "#fff", border: "1px solid #d0d3d7", borderRadius: 4 }}
    >
      <defs>
        <pattern id={`grid-${width}`} width="10" height="10" patternUnits="userSpaceOnUse">
          <path d="M 10 0 L 0 0 0 10" fill="none" stroke="#e2e4e8" strokeWidth="0.4" />
        </pattern>
      </defs>
      <rect width={vb.width} height={vb.height} fill={`url(#grid-${width})`} />
      <path d={membrane} fill="#000" />
      {seams.map((d, i) => (
        <path
          key={i}
          d={d}
          stroke="#000"
          strokeWidth={seamWidths[i]}
          fill="none"
          strokeLinecap="round"
        />
      ))}
    </svg>
  );
}
