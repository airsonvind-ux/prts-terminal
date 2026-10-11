// src/components/guide/guideContent.ts
// 引导文案 · 单一来源
//
// 逐字对齐台账 `_phase5-staging.md`「首启引导 · 文案（第三版）」。改文案只改这里。
//
// 硬约束（第一版被否的教训）：只讲功能；不加劝导、不承诺未来行为、
// 不引用此刻不存在的元素、不设无人称的「我」。必要例外单列成 note。

export type GuideForm =
  /** 聚焦式：挖洞高亮锚点 + 贴锚点气泡 */
  | "focus"
  /** 通知式：压暗面板 + 居中说明卡 */
  | "notice";

export interface GuideItem {
  form: GuideForm;
  /** 聚焦式：锚点的 data-guide 值（量不到 → 整步跳过）。通知式固定挂设置面板 */
  anchor?: string;
  /** 标题（通知式留空：背后压暗的就是那一页，标题重复） */
  title?: string;
  body: string;
  /** 必要例外（如"当前没有对话，点了不会有变化"），单独一行小字 */
  note?: string;
}

/** 通知式的压暗目标（设置面板根）。 */
export const NOTICE_ANCHOR = "settings-panel";

/**
 * 局部坐标的参照系：外壳正面（`.frontFace`）。
 *
 * 说明层与设置 / 历史覆盖层一样挂在它内部（`position:absolute`），所以
 * ① 压暗被它的 `overflow:hidden` 天然裁在应用表面内，不会铺到窗口的透明区
 * （2026-10-11：按窗口铺过一版，整窗口一层黑）；
 * ② 锚点 / 卡片坐标取它内部的局部值，随立方体情绪动作一起动，不脱节。
 */
export const FACE_ANCHOR = "face";

export const GUIDE_ITEMS: Record<string, GuideItem> = {
  // ── 欢迎页聚焦（3 步，无收尾） ──
  "welcome.new-chat": {
    form: "focus",
    anchor: "new-chat",
    title: "新建对话",
    body: "创建一个新的对话。",
  },
  "welcome.settings": {
    form: "focus",
    anchor: "settings",
    title: "设置",
    body: "对PRTS系统设置进行修改。",
  },
  "welcome.input": {
    form: "focus",
    anchor: "input",
    title: "输入区",
    body: "输入内容并发送以创建第一个对话。",
    note: "对话生成中可以点击中止按钮进行中止，也可以再发送一段内容进行插话",
  },

  // ── 设置页通知（九页，每页一段；进入该页触发，每页一次） ──
  "settings.app": { form: "notice", body: "提供窗口置顶切换、关闭PRTS功能" },
  "settings.model": {
    form: "notice",
    body: "选择你的供应商及其模型，并配置API Key",
  },
  "settings.safety": {
    form: "notice",
    body: "命令执行前要不要问你，在这里定。",
  },
  "settings.space": {
    form: "notice",
    body: "PRTS当前的工作目录，修改后下一轮对话即生效",
  },
  "settings.skills": {
    form: "notice",
    body: "SKILL管理页，查看并管理PRTS的现有技能",
  },
  "settings.memory": {
    form: "notice",
    body: "PRTS记下的信息，成长型记忆由PRTS自己管理，经历型记忆由系统自动生成",
  },
  "settings.usage": {
    form: "notice",
    body: "查看你的token消耗量，可切换到不同的对话用量",
  },
  "settings.extension": {
    form: "notice",
    body: "MAA 的连接设置在这里。没装 MAA 可以忽略这一页。",
  },
  "settings.about": {
    form: "notice",
    body: "PRTS应用的基本信息与更新管理",
  },

  // ── 首次出现型（元素真的出现，且本轮生成已结束、界面空闲） ──
  "first.conv-title": {
    form: "focus",
    anchor: "conv-title",
    title: "对话标题",
    body: "对话标题由后台模型自动生成，点击标题后进入对话历史，可切换到不同对话、修改标题、删除对话",
  },
  "first.context-ring": {
    form: "focus",
    anchor: "context-ring",
    title: "上下文进度环",
    body: "显示当前对话的上下文占用情况，悬停时显示模型信息，点它可以进行上下文压缩",
  },
};

/** 欢迎页导览（顺序即队列顺序）。 */
export const WELCOME_TOUR: string[] = [
  "welcome.new-chat",
  "welcome.settings",
  "welcome.input",
];

/** 设置页 tab key → 引导项 key。 */
export const SETTINGS_TAB_GUIDE: Record<string, string> = {
  app: "settings.app",
  model: "settings.model",
  safety: "settings.safety",
  space: "settings.space",
  skills: "settings.skills",
  memory: "settings.memory",
  usage: "settings.usage",
  extension: "settings.extension",
  about: "settings.about",
};
