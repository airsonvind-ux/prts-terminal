// 扩展页共用的文案与格式化：状态名、能力人话、字节数。
// 只做展示，不含任何判定逻辑。

import type { PluginRuntimeState } from "../../types";

export const STATE_LABELS: Record<PluginRuntimeState, string> = {
  loaded: "正常",
  starting: "启动中",
  failed: "加载失败",
  faulted: "进程退出",
  disabled: "已停用",
};

/** 能力键 → 人话。未登记的能力原样显示（新增能力时不必先改前端）。 */
export const CAPABILITY_LABELS: Record<string, string> = {
  "fs.plugin": "读写自己的数据目录",
  "fs.temp": "读写临时文件",
  "query.call": "查询方舟数据（只读）",
  "maa.bridge": "启动 / 关闭 MAA 桥进程",
};

export function capabilityLabel(key: string): string {
  return CAPABILITY_LABELS[key] ?? key;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 形态：内置还是外部进程。 */
export function kindLabel(kind: string): string {
  return kind === "process" ? "进程" : "内置";
}
