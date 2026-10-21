// 设置页导航项与 tab 键。

export const TABS = [
  { key: "app", label: "应用" },
  { key: "model", label: "模型" },
  { key: "safety", label: "安全" },
  { key: "space", label: "空间" },
  { key: "skills", label: "技能" },
  { key: "memory", label: "记忆" },
  { key: "usage", label: "用量" },
  { key: "extension", label: "扩展" },
  { key: "about", label: "关于" },
] as const;

export type SettingsTabKey = (typeof TABS)[number]["key"];
