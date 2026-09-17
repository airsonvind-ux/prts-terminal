// src/toolDisplay.ts
// 工具显示短语派生 —— 纯函数，无 React 依赖
//
// 职责：把工具名映射为面向用户的"核心短语"，供 EventBadge 拼装
// 三态文案（进行中/完成/失败）。粗粒度分类映射，不暴露工具内部细节。
//
// 映射策略：精确表（在册工具）优先 → 前缀兜底（未来新增工具自动归类）→ 最终兜底。
// 在册工具清单见 src-tauri/src/tools/mod.rs register_builtin。

// ── 精确映射表（在册工具） ──

const EXACT_MAP: Record<string, string> = {
  // query_* + search_entities：数据库查询
  query_enemy: "查询数据库",
  query_operator: "查询数据库",
  query_level: "查询数据库",
  query_stage: "查询数据库",
  query_trap: "查询数据库",
  query_token: "查询数据库",
  query_rogue: "查询数据库",
  query_table: "查询数据库",
  search_entities: "查询数据库",
  // 知识库导航 / 阅读
  list_knowledge: "浏览知识库目录",
  read_knowledge: "阅读",
  read_file: "阅读",
  // 文件域写 / 检索（M2；edit/grep 为通用短词显式特例，write_/list_ 另有前缀兜底）
  write_file: "写入",
  edit: "编辑",
  grep: "搜索",
  list_dir: "浏览",
  // 命令执行（M3-A）
  run_command: "执行命令",
  // 行动推演
  sim_path: "模拟",
  // Skill 激活
  activate_skill: "激活skill",
  // 时间参照（Phase4 1.3）
  time: "查看时间",
};

// ── 前缀兜底表（粗粒度分类，顺序即优先级） ──

const PREFIX_MAP: ReadonlyArray<readonly [string, string]> = [
  ["query_", "查询数据库"],
  ["read_", "阅读"],
  ["write_", "写入"],
  ["list_", "浏览"],
  ["search_", "搜索"],
  ["sim_", "模拟"],
  ["run_", "执行"],
  // 网络域（M5）：web_search / web_fetch
  ["web_", "浏览网页"],
];

/** 最终兜底：未知工具 */
const FALLBACK_PHRASE = "处理";

/** 工具名 → 核心短语 */
export function toolAction(toolName: string): string {
  const exact = EXACT_MAP[toolName];
  if (exact) return exact;
  for (const [prefix, phrase] of PREFIX_MAP) {
    if (toolName.startsWith(prefix)) return phrase;
  }
  return FALLBACK_PHRASE;
}
