// src/toolDisplay.ts
// 工具显示短语派生 —— 纯函数，无 React 依赖
//
// 职责：把工具名映射为面向用户的"核心短语"，供消息流拼装
// 三态文案（进行中/完成/失败）；以及审批出卡族的类别短语。
// 粗粒度分类映射，不暴露工具内部细节。
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
  list_dir: "浏览目录",
  // 游戏画面截图（与 MAA 独立：模拟器走 adb，PC 端走窗口捕获）
  capture_game_screen: "截取画面",
  // 命令执行（M3-A）
  run_command: "执行命令",
  // 行动推演
  sim_path: "模拟",
  // Skill 激活
  activate_skill: "激活skill",
  // 时间参照（Phase4 1.3）
  time: "查看时间",
  // MAA 接入：把游戏操作交给本机 MAA
  run_maa_task: "执行MAA任务",
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

// ── 出卡族类别短语（M3-A 地域性信任门 · 设计 §9 定稿） ──
// reason 码（ruleId 通道）↔ 短语一一对应；确认卡类别行直接回答"为什么问"。
// crit-* 不产生 request（恒拦直接拒绝，无确认卡）故无条目；ruleId 为 null
// 或未收录（region-in/system-query 等区内放行族在询问模式下弹卡、未来新增
// 码）→ 不渲染类别行，也不显示任何"高危"字样（高危徽章已废除）。

const RULE_ID_PHRASES: Record<string, string> = {
  "region-external": "修改工作目录外的文件或目录",
  "run-external-program": "运行工作目录外的程序或脚本",
  "delete-always-ask": "删除文件或目录（任何位置都需要你确认）",
  "code-exec": "执行代码片段",
  "network-op": "访问网络或下载内容",
  "install-op": "安装或卸载软件",
  "process-control": "终止进程",
  "config-op": "修改系统网络配置",
  "git-undo": "修改仓库历史（可能丢弃更改）",
  "unanalyzable-syntax": "组合命令：无法判断具体内容",
  "skill-package-script": "Skill 包内的脚本",
  "skill-untrusted-active": "不可信 Skill 相关命令",
};

/** ruleId → 类别短语；null/未收录 → undefined（不渲染类别行） */
export function categoryPhrase(ruleId: string | null): string | undefined {
  return ruleId ? RULE_ID_PHRASES[ruleId] : undefined;
}
