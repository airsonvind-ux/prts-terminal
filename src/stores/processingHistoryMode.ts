// 后端会话历史模式（C-lite）的前端快照。
//
// 只在发送消息时需要知道「要不要带完整 history」。模式来自设置 + 环境变量，
// 运行中不会热切换；模块级缓存避免每轮 invoke。测试可通过 reset 清缓存。

import { invoke } from "@tauri-apps/api/core";

export type BackendHistoryMode = "legacy" | "shadow" | "primary";

let cached: Promise<BackendHistoryMode> | null = null;

/** 读取当前模式；后端未就绪/旧版本 → 按 legacy 处理。 */
export function getBackendHistoryMode(): Promise<BackendHistoryMode> {
  if (!cached) {
    cached = invoke<string>("get_backend_history_mode")
      .then((mode) =>
        mode === "primary" || mode === "shadow" ? mode : "legacy"
      )
      .catch(() => "legacy" as const);
  }
  return cached;
}

/** primary 模式下前端不再发送完整 history。 */
export async function backendHistoryEnabled(): Promise<boolean> {
  return (await getBackendHistoryMode()) === "primary";
}

/** 测试/设置变更后清空缓存。 */
export function resetBackendHistoryModeCache(): void {
  cached = null;
}
