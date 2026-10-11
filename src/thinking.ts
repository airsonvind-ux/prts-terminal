// src/thinking.ts
// 思考强度派生 · 单一来源
//
// 口径必须与后端 `llm/thinking.rs::resolve_requested` 一致：
//   - 模型不支持推理（或不在注册表）→ 无档位（不显示、不注入）
//   - 支持推理 → 恒有一档：存储偏好命中白名单则用它，越界/未设 → 模型默认档
//     （声明默认；模型未声明默认 → "medium" 推断默认）
//   - 白名单 = 模型声明的 thinkingLevels；未声明 → 推断集 [off, medium, high]
//
// 这条规则原先只内联在模型页（SettingsModel）。2026-10-11 抽出来给上下文
// 进度环的悬停信息共用——两处必须永远一致，所以只留这一份。
//
// 注意：显示的是**生效档**（实际会用的那一档），不是设置里存的偏好值。

/** 模型未声明档位集时的推断集（与后端 INFERRED_* 同值）。 */
export const INFERRED_LEVELS: string[] = ["off", "medium", "high"];
/** 模型未声明默认档时的推断默认（与后端同值）。 */
export const INFERRED_DEFAULT = "medium";

/** canonical 档 → 界面标签（与模型页选项同源）。 */
export const THINKING_LABELS: Record<string, string> = {
  off: "关闭",
  low: "低",
  medium: "中",
  high: "高",
  max: "极致",
};

/** 派生所需的模型声明子集（ModelInfo / get_local_model_info 的交集）。 */
export interface ThinkingModelFacts {
  /** 该模型是否支持推理（false/未收录 → 无档位） */
  supportsReasoning?: boolean;
  /** 声明的档位白名单（空 = 未声明 → 推断集） */
  thinkingLevels?: string[] | null;
  /** 声明的默认档（空 = medium） */
  defaultThinkingLevel?: string | null;
}

export interface ThinkingView {
  /** 可见档位集（声明集，或未声明时的推断集） */
  levels: string[];
  /** 模型默认档 */
  defaultLevel: string;
  /** 生效档：存储偏好经 clamp 后的实际档 */
  effective: string;
}

/**
 * 派生某模型的档位视图。模型不支持推理 → null（调用方整段不显示）。
 *
 * @param stored 设置里的全局思考偏好（`llm.thinking_level`，空 = 跟随模型默认）
 */
export function resolveThinking(
  stored: string | null | undefined,
  model: ThinkingModelFacts | null | undefined
): ThinkingView | null {
  if (!model?.supportsReasoning) return null;
  const declared = model.thinkingLevels;
  const levels = declared && declared.length > 0 ? declared : INFERRED_LEVELS;
  const defaultLevel = model.defaultThinkingLevel ?? INFERRED_DEFAULT;
  const v = stored ?? "";
  return {
    levels,
    defaultLevel,
    effective: levels.includes(v) ? v : defaultLevel,
  };
}

/** 生效档的展示文案（`中` / `高（默认）` 这类）。不支持推理 → null。 */
export function thinkingLabel(
  stored: string | null | undefined,
  model: ThinkingModelFacts | null | undefined
): string | null {
  const view = resolveThinking(stored, model);
  if (!view) return null;
  const base = THINKING_LABELS[view.effective] ?? view.effective;
  // 用户没选过 / 选的越界 → 生效档是回落到模型默认的结果，标「（默认）」
  const v = stored ?? "";
  return view.levels.includes(v) ? base : `${base}（默认）`;
}
