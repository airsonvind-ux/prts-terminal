// 设置页 · 记忆子页的二级视图出口
//
// 三套视图：
//  - GrowthView   成长页阅读 / 编辑（新建走同一表单的空白态）
//  - CompiledView 「当下记忆」：近期（只读）＋ 先前（唯一可编辑处）
//  - LibraryView  「记忆库」：全部经历条目，按日期编排（新在前）
//
// 视图框架与格式化 helper 在 memoryViewShared；本文件只保留对外导出。

export type { MemView } from "./memoryViewShared";
export { GrowthView } from "./SettingsMemoryGrowth";
export { CompiledView } from "./SettingsMemoryCompiled";
export { LibraryView } from "./SettingsMemoryLibrary";
