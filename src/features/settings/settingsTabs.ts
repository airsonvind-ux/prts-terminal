// 设置页导航项与 tab 键。
//
// 顺序即显示顺序。导航是一列平铺的入口，不做分组：56px 宽的窄栏里，
// 没有标签的空隙只会被读成一截算错的边距（看起来像 bug）。

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
