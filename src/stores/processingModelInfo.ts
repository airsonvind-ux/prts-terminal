// 全局模型信息缓存
//
// 模块级 ref：SettingsProvider 在用户选模型时写入，sendMessage 创建对话时读取。
// 不在任何 store 中——模型信息本质是全局配置的快照，不需要响应式订阅。
// 组件通过 getModelInfo() / setModelInfo() 读写，避免直接操作 ref 内部结构。

export interface ModelInfoCache {
  name: string;
  contextLength: number;
  /** 当前模型是否支持图像输入（未知/未收录 → false；必填防止调用点漏写） */
  supportsVision: boolean;
  /**
   * 生效思考档的展示文案（如 `中（默认）`；模型不支持推理 → null/undefined）。
   * **算好的值，不是存储偏好**——由写入点（App 启动 / 模型页选中 / 改档位）
   * 用 `thinking.ts` 派生后写入（悬停信息读的是快照，不能在渲染时现算）。
   */
  thinking?: string | null;
}

const modelInfoRef: { current: ModelInfoCache | null } = { current: null };

/** SettingsProvider 在用户选定模型时调用，写入缓存 */
export function setModelInfo(info: ModelInfoCache | null): void {
  modelInfoRef.current = info;
}

/** 读取缓存的模型信息 */
export function getModelInfo(): ModelInfoCache | null {
  return modelInfoRef.current;
}

/**
 * 只更新缓存的「生效思考档」文案（模型没变、档位变了时用；名字/窗口不动）。
 * 缓存里还没模型信息（未配置）→ 无操作。
 */
export function setModelThinking(label: string | null): void {
  if (modelInfoRef.current) modelInfoRef.current.thinking = label;
}
