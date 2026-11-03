// 设置页导航项与 tab 键。
//
// 顺序即显示顺序；`breakBefore` 只加一道 4px 空隙做分组（不改结构）：
//   应用 | 模型 / 安全 / 空间 / 技能 / 记忆 | 用量 / 扩展 / 关于

export const TABS = [
  { key: "app", label: "应用" },
  { key: "model", label: "模型", breakBefore: true },
  { key: "safety", label: "安全" },
  { key: "space", label: "空间" },
  { key: "skills", label: "技能" },
  { key: "memory", label: "记忆" },
  { key: "usage", label: "用量", breakBefore: true },
  { key: "extension", label: "扩展" },
  { key: "about", label: "关于" },
] as const;

export type SettingsTabKey = (typeof TABS)[number]["key"];
