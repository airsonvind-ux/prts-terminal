// src/debug/main.tsx
// 情绪动画调试页入口（非应用路径，仅供开发期视觉验收）
//
// 浏览器直接打开（vite dev server）：http://localhost:1420/mood-debug.html
// 不经过 Tauri，不依赖 LLM：按钮直接驱动 droneEngine 触发情绪动作与散热状态。

import "../styles/tokens.css";
import "../styles/reset.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { MoodDebug } from "./MoodDebug";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <MoodDebug />
  </StrictMode>
);
