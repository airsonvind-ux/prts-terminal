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
import { GuideHost } from "./components/guide/GuideHost";
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
import { setModelInfo } from "./stores/useProcessingStore";
import { useGuideStore } from "./stores/useGuideStore";
import { thinkingLabel } from "./thinking";
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

  // ── 引导键集（"看过没"）拉一次：说明层由它去重 ──
  useEffect(() => {
    useGuideStore.getState().hydrate();
  }, []);

  // ── 启动时初始化模型信息缓存 ──
  // get_settings 全量读；取 active 供应商视角（providers[active_provider]）。
  useEffect(() => {
    invoke<Settings>("get_settings")
      .then((settings) => {
        const active = settings.llm.active_provider;
        const pc = settings.llm.providers[active];
        const model = pc?.model ?? "";
        if (active && model) {
          invoke<{
            displayName: string;
            contextLength: number;
            supportsVision: boolean;
            supportsReasoning?: boolean;
            thinkingLevels?: string[] | null;
            defaultThinkingLevel?: string | null;
          } | null>(
            "get_local_model_info",
            { provider: active, modelId: model }
          )
            .then((info) => {
              if (info) {
                setModelInfo({
                  name: info.displayName,
                  contextLength: info.contextLength,
                  supportsVision: info.supportsVision,
                  // 生效思考档（悬停信息用）：按 thinking.ts 的口径算好后存快照
                  thinking: thinkingLabel(settings.llm.thinking_level, info),
                });
              } else {
                // 自定义模型不在 models.json 中，用原始 ID 作展示名
                setModelInfo({ name: model, contextLength: 0, thinking: null });
              }
            })
            .catch(() => {
              setModelInfo({ name: model, contextLength: 0, thinking: null });
            });
        }
      })
      .catch(() => {
        // get_settings 失败（后端未就绪等），静默忽略
      });
  }, []);

  return (
    <div className={styles.app}>
      <DroneShellScene
        onDragHandle={onDragHandle}
        onResizeHandle={onResizeHandle}
      >
        <ChatView onDragHandle={onDragHandle} />
        <OverlayContainer />
        {/* 引导说明层（portal 到 body；z 序在面板之上、Toast 之下） */}
        <GuideHost />
        {/* 右键菜单统一宿主（document 级兜底：全应用不再出现浏览器菜单） */}
        <ContextMenuHost />
        {/* M3-B：Toast 浮层（OverlayContainer 同级——overlay=null 也常驻；
            内部 portal 到 body，见 ToastHost） */}
        <ToastHost />
      </DroneShellScene>
    </div>
  );
}
