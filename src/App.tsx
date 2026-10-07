// src/App.tsx
// 应用入口 · 薄编排
//
// 职责：调用四个横切 Hook，组装三层视图（外壳 + 内容 + 覆盖）。
// 不做任何业务计算，不维护本地状态。
//
// 初始化：挂载时从后端配置读取当前模型信息，写入全局 modelInfoRef，
// 确保 ContextIndicator 在设置页未打开时也能获取模型名和上下文窗口大小。

import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { DroneShellScene } from "./components/shell/DroneShellScene";
import { ChatView } from "./components/content/ChatView";
import { ContextMenuHost } from "./components/overlay/ContextMenuHost";
import { OverlayContainer } from "./components/overlay/OverlayContainer";
import { ToastHost } from "./components/overlay/ToastHost";
import { useDrag } from "./hooks/useDrag";
import { useResize } from "./hooks/useResize";
import { useAnimationMapping } from "./hooks/useAnimationMapping";
import { useAgentEvents } from "./hooks/useAgentEvents";
import { useConversationHydration } from "./hooks/useConversationHydration";
// useMockAgent 仅在后端 Agent 未就绪时用于前端 UI 调试。
// 后端就绪后必须注释掉，否则 Mock 事件与真实后端事件会在同一 sessionId 上冲突。
// import { useMockAgent } from "./hooks/useMockAgent";
import { useUIStore } from "./stores/useUIStore";
import { setModelInfo } from "./stores/useProcessingStore";
import type { Settings } from "./types";
import styles from "./App.module.css";

export function App() {
  const { onDragHandle } = useDrag();
  const { onResizeHandle } = useResize();
  useAnimationMapping();
  useAgentEvents();
  // 启动会话恢复（Phase 4）：list → 并行 get → 装载 store + 导航决策。
  // 导航决策收敛于本 hook 单点（欢迎界面/开屏流程改造只改此处）
  useConversationHydration();
  // useMockAgent(); // 后端就绪后已关闭，避免事件冲突

  // ── 启动时初始化模型信息缓存 ──
  // get_settings 全量读；取 active 供应商视角（providers[active_provider]）。
  useEffect(() => {
    invoke<Settings>("get_settings")
      .then((settings) => {
        const active = settings.llm.active_provider;
        const pc = settings.llm.providers[active];
        const model = pc?.model ?? "";
        if (active && model) {
          invoke<{ displayName: string; contextLength: number; supportsVision: boolean } | null>(
            "get_local_model_info",
            { provider: active, modelId: model }
          )
            .then((info) => {
              if (info) {
                setModelInfo({ name: info.displayName, contextLength: info.contextLength, supportsVision: info.supportsVision });
              } else {
                // 自定义模型不在 models.json 中，用原始 ID 作展示名
                setModelInfo({ name: model, contextLength: 0 });
              }
            })
            .catch(() => {
              setModelInfo({ name: model, contextLength: 0 });
            });
        }
      })
      .catch(() => {
        // get_settings 失败（后端未就绪等），静默忽略
      });
  }, []);

  /*
   * 快捷键隐藏：窗口完全不可见时不渲染任何内容。
   *
   * 不渲染（return null）而非 display:none 的原因：
   * display:none 仍保留 DOM 树和 React 组件树，rAF 驱动的尾巴动画、
   * CSS @keyframes 翅膀动画继续消耗 CPU/GPU 资源。
   * return null 让 React 卸载整棵组件树，动画自然停止。
   *
   * edge_snap 模式不同——窗口收缩为标签，仍需渲染（留给 useEdgeSnap 实现时处理）。
   */
  const hiddenMode = useUIStore((s) => s.hiddenMode);
  if (hiddenMode === "shortcut") return null;

  return (
    <div className={styles.app}>
      <DroneShellScene
        onDragHandle={onDragHandle}
        onResizeHandle={onResizeHandle}
      >
        <ChatView onDragHandle={onDragHandle} />
        <OverlayContainer />
        {/* 右键菜单统一宿主（document 级兜底：全应用不再出现浏览器菜单） */}
        <ContextMenuHost />
        {/* M3-B：Toast 浮层（OverlayContainer 同级——overlay=null 也常驻；
            内部 portal 到 body，见 ToastHost） */}
        <ToastHost />
      </DroneShellScene>
    </div>
  );
}
