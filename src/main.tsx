// src/main.tsx
// React 挂载入口
//
// 职责：引入全局样式，创建 React root，挂载 <App />。
// 不做任何应用逻辑——所有业务判断在 App.tsx 及其子组件中。

import "./styles/tokens.css";
import "./styles/reset.css";
import "./styles/scrollbar.css";
import "./styles/highlights.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { installScrollActivity } from "./scrollActivity";

// 全局视觉行为：按需滚动条（滚动/点击滚动条区域时显色）
installScrollActivity();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
