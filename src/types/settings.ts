// PRTS 终端 · 设置、MAA、更新与启动自检类型

import type { ApiType } from "./llm";

/** 命令执行模式（M3-A：全局声明层设置，默认操作前询问） */
export type ExecutionMode = "ask" | "auto";

/** 命令执行设置（settings.exec 段，与后端 serde kebab-case 对齐） */
export interface ExecutionSettings {
  mode: ExecutionMode;
}

// ── 设置全量（settings.json 镜像；与后端 settings.rs Settings 对齐，snake_case）──
// get_settings 返回 / set_settings patch 载荷。llm 段 = LlmConfig 镜像
// （api_key 经 get_settings 掩码下发，见 mask 契约：patch 缺席不更新 /
// "********" 保留现值 / 空串清除 / 明文写入）。

export interface Settings {
  schema_version: number;
  llm: LlmSection;
  exec: ExecutionSettings;
  maa: MaaSettings;
  workspace: WorkspaceSettings;
  window: WindowSettings;
}

// ── 凭据层状态（get_credentials_status）──

/** 凭据层是否可写；writable=false 时 note 是给用户看的原因。 */
export interface CredentialsStatus {
  writable: boolean;
  note: string | null;
}

// ── MAA 段（settings.json 的 maa 段；snake_case 对齐后端 MaaSettings）──

/** 账号服务器（kebab-case 序列化） */
export type MaaServer = "official" | "bilibili";

/** MAA 设置。字符串字段空串 = 交由自动判定。 */
export interface MaaSettings {
  server: MaaServer;
  /** 自动检测连接：开着时忽略下面的覆盖项 */
  auto_detect: boolean;
  /** 截图增强（MuMu / 雷电）；不可用时 MAA 自行降级 */
  screenshot_extras: boolean;
  /** 触控增强（MuMu / 雷电） */
  touch_extras: boolean;
  /** 指定模拟器（machine.json 的标签；空串 = 自动） */
  emulator: string;
  /** 模拟器路径（空 = 用探测到的安装目录） */
  emulator_path: string;
  adb_path: string;
  address: string;
}

/** MAA 只读状态（maa_status / maa_test_connection 返回值） */
export interface MaaStatus {
  maa_installed: boolean;
  bridge_available: boolean;
  connected: boolean;
  running: boolean;
  stale: boolean;
  last_error: string | null;
  emulator: string | null;
  address: string | null;
  extras: boolean;
  /** 实际生效的截图方式（如 MumuExtras；MAA 静默降级时也如实写到） */
  screencap: string | null;
  emulators: string[];
  resolved: MaaResolved[];
}

/** 一个已装模拟器推导出的连接参数（页面用来显示“自动时实际在用”的值） */
export interface MaaResolved {
  emulator: string;
  config: string;
  adb_path: string;
  address: string;
  extras: boolean;
  root: string;
}

/** 应用内更新状态（updater_status / update-status 事件） */
export type UpdatePhase =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "downloading"
  | "downloaded"
  | "installing"
  | "error"
  | "dev";

/** 更新失败发生阶段；启动检查失败不应显示成“安装没有完成” */
export type UpdateErrorStage = "check" | "download" | "disk" | "install";

export interface UpdateSourceInfo {
  id: string;
  label: string;
  endpoint: string;
}

export interface AvailableUpdate {
  version: string;
  currentVersion: string;
  notes: string | null;
  pubDate: string | null;
  source: string;
  downloadUrl: string;
}

export interface UpdateStatus {
  phase: UpdatePhase;
  currentVersion: string;
  devMode: boolean;
  sources: UpdateSourceInfo[];
  available: AvailableUpdate | null;
  source: string | null;
  downloadedBytes: number | null;
  totalBytes: number | null;
  lastCheckedAt: string | null;
  warnings: string[];
  manualDownloadUrl: string | null;
  error: string | null;
  /** 旧版后端可能不带此字段 */
  errorStage?: UpdateErrorStage | null;
}

/** 安装后重启展示一次的更新日志 */
export interface PostInstallNotice {
  version: string;
  previousVersion: string;
  notes: string | null;
  pubDate: string | null;
}

/** 安装前实时阻塞原因 */
export interface UpdaterInstallBlockers {
  activeTurn: boolean;
  maaRunning: boolean;
}

/** llm 段（v2 供应商分区；snake 对齐后端 LlmSection） */
export interface LlmSection {
  /** 当前生效供应商（providers 的键） */
  active_provider: string;
  /** 各供应商独立配置（api_key 经 get_settings 逐家掩码） */
  providers: Record<string, ProviderConfig>;
  /** 全局思考偏好（跨供应商；null = 跟随模型默认档） */
  thinking_level?: string | null;
}

export interface ProviderConfig {
  api_type: ApiType;
  model: string;
  api_key: string;
  base_url: string | null;
  headers: string | null;
  max_tokens: number;
}

export interface WorkspaceSettings {
  /** 空串 = 用默认工作区（<主目录>/PRTS-WorkSpace）；修改即时生效（后端热切换） */
  workspace_root: string;
}

export interface WindowSettings {
  always_on_top: boolean;
}

/** 思考强度 canonical 档（与 models.json 声明的 thinking_levels 白名单联动；
 *  具体 UI 可见档由模型声明驱动）。未设置 = 不干预（不传参跟服务端默认）。 */
export type ThinkingLevel =
  | "off"
  | "low"
  | "medium"
  | "high"
  | "max";

/** set_settings 局部 patch：字段缺席 = 不更新（per-field）；
 *  llm 段 = v2 分区 patch：active_provider 切供应商指针；providers.{家} 深层
 *  字段合并（新家懒建）；thinking_level 全局（空串 = 清除回跟随模型默认）。
 *  api_key 三态：缺席不更新 / 掩码保留该家现值 / 空串清除 / 明文写入。 */
export interface SettingsPatch {
  llm?: LlmPatch;
  exec?: ExecPatch;
  maa?: MaaPatch;
  workspace?: WorkspacePatch;
  window?: WindowPatch;
}

/** MAA 段 patch（per-field：缺席 = 不更新） */
export interface MaaPatch {
  server?: MaaServer;
  auto_detect?: boolean;
  screenshot_extras?: boolean;
  touch_extras?: boolean;
  emulator?: string;
  emulator_path?: string;
  adb_path?: string;
  address?: string;
}

export interface LlmPatch {
  active_provider?: string;
  providers?: Record<string, ProviderPatch>;
  thinking_level?: string;
}

export interface ProviderPatch {
  api_type?: ApiType;
  model?: string;
  api_key?: string;
  base_url?: string;
  headers?: string;
  max_tokens?: number;
}

export interface ExecPatch {
  mode?: ExecutionMode;
}

export interface WorkspacePatch {
  workspace_root?: string;
}

export interface WindowPatch {
  always_on_top?: boolean;
}

/** get_boot_status 载荷：启动自检单件（只报事实，不参与业务判定） */
export interface BootComponent {
  /** 是否就位 */
  ok: boolean;
  /** ok 时是事实（如「已接入 · 128 条」）；失败时是原因原文 */
  note: string;
}

/** get_boot_status 载荷：三件内置资源的启动自检报告（启动演出消费） */
export interface BootStatus {
  database: BootComponent;
  knowledge: BootComponent;
  memory: BootComponent;
}

/** get_app_meta 载荷（关于页 + 空间页共用只读信息）。
 *
 *  命名与 Settings 侧不同：后端 AppMeta 标了 `rename_all = camelCase`（命令 DTO
 *  惯例），而 Settings / WorkspaceSettings 等持久化结构是 snake_case（文件格式）。
 *  两者不要互相照抄——照抄会静默取到 undefined。
 *
 *  内容都是**静态事实**（面板挂载时拉一次的快照）。当前生效工作区是派生值，
 *  不在这里：它由设置值决定，前端自算（设置非空则用它，否则用 defaultWorkspaceRoot）。 */
export interface AppMeta {
  version: string;
  dataDir: string;
  /** 默认工作区路径（主目录下的 PRTS-WorkSpace）：恒定，与设置无关。 */
  defaultWorkspaceRoot: string;
  dbPath: string;
  /** 当前运行时数据库的 meta；旧库或读取失败时为 null。 */
  dbMeta?: {
    dataVersion: string;
    generatedAt: string;
    sourceCommit: string;
    schemaVersion: number;
  } | null;
  knowledgeDir: string;
  skillsDir: string;
  /** 《使用说明》文档路径（随包资源；关于页入口用）。 */
  userGuidePath: string;
  /** 数据与内容声明路径（随包资源）。 */
  dataNoticePath?: string;
}

/** list_skills / open_path 载荷（技能页技能清单；来源 skills md frontmatter）
 *  字段与 Rust SkillEntry `#[serde(rename_all = "camelCase")]` 对齐：
 *  后端序列化为 filePath/name/description。filePath 由后端供给（源 md 绝对
 *  路径，前端不拼接硬编码）；open_path 只要求绝对路径且目标存在，不设白名单。 */
export interface SkillEntry {
  name: string;
  description: string;
  /** 源 md 绝对路径（camelCase 对齐后端；null/缺省 = 无文件可打开；
   *  文件夹包为包内 SKILL.md 路径） */
  filePath?: string | null;
  /** 来源分层（后端 frontmatter source 原样透出；"builtin" = 内置，其余视为用户） */
  source?: string | null;
  /** R-29 信任级别；旧后端无此字段时按 trusted 展示 */
  trust?: "trusted" | "untrusted";
  /** 包指纹（FNV-1a 64；单文件为文件内容指纹） */
  packageHash?: string | null;
  /** 技能包根目录（单 `.md` = null） */
  resourceDir?: string | null;
}

/** 安装预览：脚本文件清单（R-29） */
export interface SkillScriptFile {
  path: string;
  sizeBytes: number;
}

/** 安装预览结果（preview_skill_source） */
export interface SkillPreview {
  name: string;
  description: string;
  source?: string | null;
  trust: "trusted" | "untrusted";
  packageHash: string;
  scripts: SkillScriptFile[];
}

/** 空间页 · 工作区树节点（list_workspace_dir 载荷；字段对齐 Rust camelCase） */
export interface WorkspaceEntry {
  name: string;
  isDir: boolean;
  /** 绝对路径（普通形式，非 canonical） */
  path: string;
}

/** 空间页 · 搜索命中（search_workspace_files 载荷）
 *  path / ancestorDirs 由后端供给：前端不拼路径（与 FileLink 同口径），
 *  点中后直接拿展开链做状态合并。 */
export interface WorkspaceSearchHit {
  name: string;
  /** 绝对路径（点击打开 / 树选中定位） */
  path: string;
  /** 相对工作区根，'/' 分隔（展示） */
  relativePath: string;
  /** 相对根的父目录（根级命中 = 空串） */
  parentSubdir: string;
  /** 从根下一级到命中项所在目录的绝对路径链（展开用） */
  ancestorDirs: string[];
  isDir: boolean;
}
