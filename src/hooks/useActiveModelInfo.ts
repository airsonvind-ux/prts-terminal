// 活跃对话的模型信息同步。
//
// 模型/思考档是会话级状态：切换对话、设置页改模型后，全局 modelInfo 缓存
// （发送前上下文检查 / 图片能力判断）必须跟着当前对话走。ContextIndicator
// 自己读会话配置；本 hook 只维护兼容缓存。

import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Settings } from "../types";
import { useConversationStore } from "../stores/useConversationStore";
import { setModelInfo } from "../stores/useProcessingStore";
import { thinkingLabel } from "../thinking";

interface LocalModelInfo {
  displayName: string;
  contextLength: number;
  supportsVision: boolean;
  supportsReasoning?: boolean;
  thinkingLevels?: string[] | null;
  defaultThinkingLevel?: string | null;
}

export function useActiveModelInfo(): void {
  const activeId = useConversationStore((s) => s.activeId);
  const activeConv = useConversationStore((s) =>
    s.activeId
      ? s.conversations.find((c) => c.id === s.activeId)
      : undefined
  );

  useEffect(() => {
    if (!activeConv) return;
    // 供应商已选但模型为空：不显示任何旧模型（发送侧会拦截）。
    if (
      Boolean(activeConv.provider || activeConv.model) &&
      (!activeConv.provider || !activeConv.model)
    ) {
      setModelInfo(null);
      return;
    }
    // 旧对话没有会话级身份 → 用全局种子兜底，避免继续沿用上一个对话的模型。
    if (!activeConv.provider && !activeConv.model) {
      let cancelled = false;
      invoke<Settings>("get_settings")
        .then((settings) => {
          if (cancelled) return;
          const provider = settings.llm.active_provider;
          const pc = settings.llm.providers[provider];
          const modelId = pc?.model ?? "";
          if (!provider || !modelId) {
            setModelInfo(null);
            return;
          }
          invoke<LocalModelInfo | null>("get_local_model_info", {
            provider,
            modelId,
          })
            .then((info) => {
              if (cancelled) return;
              setModelInfo({
                name: info?.displayName ?? modelId,
                contextLength: info?.contextLength ?? 0,
                supportsVision: info?.supportsVision ?? false,
                provider,
                model: modelId,
                thinkingLevel: settings.llm.thinking_level ?? null,
                thinking: info
                  ? thinkingLabel(settings.llm.thinking_level, info)
                  : null,
              });
            })
            .catch(() => {
              if (cancelled) return;
              setModelInfo(null);
            });
        })
        .catch(() => {
          if (!cancelled) setModelInfo(null);
        });
      return () => {
        cancelled = true;
      };
    }
    let cancelled = false;
    const provider = activeConv.provider!;
    const modelId = activeConv.model!;
    const thinkingLevel = activeConv.thinkingLevel ?? null;
    invoke<LocalModelInfo | null>("get_local_model_info", { provider, modelId })
      .then((info) => {
        if (cancelled) return;
        setModelInfo({
          name: info?.displayName ?? activeConv.modelName ?? modelId,
          contextLength: info?.contextLength ?? activeConv.contextLength ?? 0,
          supportsVision: info?.supportsVision ?? false,
          provider,
          model: modelId,
          thinkingLevel,
          thinking: info
            ? thinkingLabel(thinkingLevel, info)
            : null,
        });
      })
      .catch(() => {
        if (cancelled) return;
        setModelInfo({
          name: activeConv.modelName ?? modelId,
          contextLength: activeConv.contextLength ?? 0,
          supportsVision: false,
          provider,
          model: modelId,
          thinkingLevel,
          thinking: null,
        });
      });
    return () => {
      cancelled = true;
    };
  }, [
    activeId,
    activeConv?.provider,
    activeConv?.model,
    activeConv?.thinkingLevel,
    activeConv?.modelName,
    activeConv?.contextLength,
  ]);
}
