// src/debug/wingMain.tsx
// 翅膀线条调试页入口（非应用路径，仅供开发期调参）
//
// 浏览器直接打开（vite dev server）：http://localhost:1420/wing-debug.html
// 不经过 Tauri：参数面板实时调整 wingGeometry 蓝图，右侧实时重绘。
// 调参满意后点击「导出蓝图 JSON」，将参数用于 src/components/shell/wingGeometry.ts 的 DEFAULT_WING。

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { WingDebug } from "./WingDebug";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <WingDebug />
  </StrictMode>
);
