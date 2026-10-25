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
import { DroneShellScene } from "./app/shell/DroneShellScene";
import { ChatView } from "./features/chat/ChatView";
import { ChatErrorBoundary } from "./features/chat/ChatErrorBoundary";
import { GuideHost } from "./features/guide/GuideHost";
import { ContextMenuHost } from "./features/overlays/ContextMenuHost";
import { OverlayContainer } from "./features/overlays/OverlayContainer";
import { ToastHost } from "./features/overlays/ToastHost";
import { useDrag } from "./hooks/useDrag";
import { useResize } from "./hooks/useResize";
import { useAnimationMapping } from "./hooks/useAnimationMapping";
import { useAgentEvents } from "./hooks/useAgentEvents";
import { useUpdaterEvents } from "./hooks/useUpdaterEvents";
import { useConversationHydration } from "./hooks/useConversationHydration";
import { useActiveModelInfo } from "./hooks/useActiveModelInfo";
import { setModelInfo } from "./stores/useProcessingStore";
import { useGuideStore } from "./stores/useGuideStore";
import { showToast } from "./stores/useToastStore";
import { thinkingLabel } from "./thinking";
import type { CredentialsStatus, Settings } from "./types";
import styles from "./App.module.css";

export function App() {
  const { onDragHandle } = useDrag();
  const { onResizeHandle } = useResize();
  useAnimationMapping();
  useAgentEvents();
  useUpdaterEvents();
  // 启动会话恢复（Phase 4）：list → 并行 get → 装载 store + 导航决策。
  // 导航决策收敛于本 hook 单点（欢迎界面/开屏流程改造只改此处）
  useConversationHydration();
  useActiveModelInfo();

  // ── 引导键集（"看过没"）拉一次：说明层由它去重 ──
  useEffect(() => {
    useGuideStore.getState().hydrate();
  }, []);

  // ── 凭据层启动状态：读取失败时提示一次 ──
  // 只陈述用户会看到的事实；保存侧仍有后端硬闸，不靠提示兜底。
  useEffect(() => {
    invoke<CredentialsStatus>("get_credentials_status")
      .then((status) => {
        if (status.writable) return;
        showToast(
          status.note ??
            "已保存的 API Key 这次没有读出来；本次启动期间无法保存新的 Key。",
          {
            kind: "warning",
            ttlMs: 12000,
            dedupeKey: "credentials-status",
          }
        );
      })
      .catch(() => {
        // 状态查询失败不打扰启动
      });
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
                  provider: active,
                  model,
                  thinkingLevel: settings.llm.thinking_level ?? null,
                  // 生效思考档（悬停信息用）：按 thinking.ts 的口径算好后存快照
                  thinking: thinkingLabel(settings.llm.thinking_level, info),
                });
              } else {
                // 自定义模型不在 models.json 中，用原始 ID 作展示名
                setModelInfo({
                  name: model,
                  contextLength: 0,
                  supportsVision: false,
                  provider: active,
                  model,
                  thinkingLevel: settings.llm.thinking_level ?? null,
                  thinking: null,
                });
              }
            })
            .catch(() => {
              setModelInfo({
                name: model,
                contextLength: 0,
                supportsVision: false,
                provider: active,
                model,
                thinkingLevel: settings.llm.thinking_level ?? null,
                thinking: null,
              });
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
        <ChatErrorBoundary>
          <ChatView onDragHandle={onDragHandle} />
        </ChatErrorBoundary>
        <OverlayContainer />
        {/* 引导说明层（挂在正面内，与设置 / 历史覆盖层同一套；z 序在面板之上） */}
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
