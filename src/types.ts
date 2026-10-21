// PRTS 终端 · 全局类型定义
// 所有跨文件共享的类型和接口集中在此，作为应用的"类型词汇表"

export * from "./types/llm";
import type { ApiType, ConversationUsage, TokenUsage, UsageStat } from "./types/llm";

// ── UI 状态机（四状态） ──

/**
 * 前端 UI 状态机：当前画面处于哪个阶段。
 * 无 idle 状态：应用启动时初始为 ready，配合 activeId=null 表现与 idle 完全一致。
 * conversations.length === 0 用于判断首次使用，不单独维护状态。
 * 无 hidden 状态（2026-10-08 删除）：窗口隐藏体系（全局快捷键 / 边缘吸附）已撤销，
 * 该状态与 HiddenMode 一并清掉；将来若真要做隐藏，加回来只是几行。
 */
export type UIState =
  | "ready"
  | "processing"
  | "streaming"
  | "error";

// ── 情绪（批 2-D 动画映射 · 情绪通道） ──

/**
 * 情绪标签枚举。标签格式：【mood:高兴】（全角冒号容错）。
 * 显式枚举：高兴 / 困惑 / 沮丧；省略（无标签）= 平静（默认态）。
 * 显式 "平静" 标签不在枚举中，视为非法值（回退平静）。
 */
export type Mood = "高兴" | "困惑" | "沮丧" | "平静";

/**
 * 情绪强度档位（标签格式：【mood:情绪:强度】）。
 * 0 = 基础（省略强度参数）；1~3 = 加深（各情绪逐档交替改变幅度/频率/停留）。
 * 非法值（0 显式、4+、非数字）回退 0。
 */
export type MoodIntensity = 0 | 1 | 2 | 3;

// ── 覆盖层 ──

/** 覆盖层类型：同一时刻最多一个打开 */
export type OverlayType = null | "history" | "settings" | "more";

// ── Agent 事件（前后端 IPC 合约，架构升级 · 模块 1） ──

/**
 * 后端推送的结构化事件（Discriminated Union）。
 *
 * 事件拆两平面（与 Rust 侧 session.rs SessionEvent + events.rs AgentEvent 对齐）：
 *   - 持久事实（SessionEvent）：turn/start|end、step/start|end、user/message、
 *     assistant/chunk、assistant/message、tool/call、tool/result、request/header、error
 *   - 瞬时 UI 信号（仅 phase）
 *
 * 所有事件均携带 sessionId —— 对应 invoke agent_process 时前端传入的会话标识。
 * 事件路由不依赖全局状态：useAgentEvents 读 event.sessionId 直接写入
 * processing[event.sessionId]，与当前活跃对话完全解耦。
 *
 * 旧词汇（reasoning_delta/reasoning_done/text_delta/generation_done/tool_call/
 * tool_result）已吸收进 assistant/chunk 等事件：推理与文本统一为
 * assistant/chunk（kind 区分），轮次结束由 turn/end（显式 reason）表达，
 * 最终归档由 assistant/message（含 content + reasoning + usage 快照）承载。
 */
export type AgentEvent = SessionEvent | PhaseEvent | ConversationTitleEvent | MemoryUsageEvent;

/** 持久事实（后端 session log 的事件镜像，前端投影缓存消费） */
export type SessionEvent =
  | TurnStartEvent
  | TurnEndEvent
  | StepStartEvent
  | StepEndEvent
  | UserMessageEvent
  | AssistantChunkEvent
  | AssistantMessageEvent
  | ToolCallEvent
  | ToolResultEvent
  | ToolApprovalRequestEvent
  | ToolApprovalResultEvent
  | MoodEvent
  | RequestHeaderEvent
  | SessionErrorEvent
  | CacheStoreEvent
  | CacheLoadEvent
  | CacheRemoveEvent
  // M3-B：轮中止标记（前端私有事件，非后端事件；mood/event 同款先例——
  // derive/展示层不派生内容，仅 MessageList 轮尾展示 + 存储透传）
  | TurnAbortedEvent;

/**
 * 轮中止标记（M3-B §4.1 前端私有事件）。
 *
 * 非后端事件：后端词汇表不对称（mood/event 先例）。由 useProcessingStore
 * 在 finishProcessing(sessionId, "aborted") 归档前合成追加到 currentEvents 末尾，
 * 随轮落库（append_turn events_json 原样写盘 → 重启可见）。
 *
 * 消费方约定：deriveMessages / deriveTextSegments / isMetaEvent 全部忽略它
 * （不进 LLM 历史、不参与展示派生）；MessageList appendTurnItems 渲染正常
 * 内容后，检测到本标记则追加轮尾标记条（“该轮已中止”，灰/斜体小字）。
 */
export interface TurnAbortedEvent {
  type: "turn/aborted";
  sessionId: string;
}

/** 增量片段类型 */
export type ChunkKind = "text" | "reasoning";

/** turn 终止原因（与 Rust TurnEndReason 对齐） */
export type TurnEndReason =
  | "completed"
  | "error"
  | "aborted"
  | "stagnation"
  | "max-tokens";

/** turn 开始：一次 agent_process 处理周期 */
export interface TurnStartEvent {
  type: "turn/start";
  sessionId: string;
}

/** turn 结束：显式终止原因（completed 到达时前端归档本轮） */
export interface TurnEndEvent {
  type: "turn/end";
  sessionId: string;
  reason: TurnEndReason;
  /** 本轮累计用量（统计增量；后端计算，前端随 append_turn 提交） */
  usage?: UsageStat | null;
}

/** step 开始：一次 LLM 请求 + 其调用的工具。estimatedTokens 为请求前估算（双锚兜底） */
export interface StepStartEvent {
  type: "step/start";
  sessionId: string;
  step: number;
  estimatedTokens: number;
}

/** step 结束：本次调用的 token 用量（最后一次即最终上下文状态；前端用于实时圆环） */
export interface StepEndEvent {
  type: "step/end";
  sessionId: string;
  step: number;
  usage: TokenUsage | null;
  elapsedMs: number;
}

/** 用户消息 */
export interface UserMessageEvent {
  type: "user/message";
  sessionId: string;
  content: string;
  /** 附件引用（有附件时携带；无附件/旧数据缺省） */
  attachments?: Attachment[];
}

/** 流式增量片段（token 级回放；推理与文本分道） */
export interface AssistantChunkEvent {
  type: "assistant/chunk";
  sessionId: string;
  kind: ChunkKind;
  content: string;
}

/** 最终 assistant 消息聚合（归档依据：content + reasoning + usage 快照） */
export interface AssistantMessageEvent {
  type: "assistant/message";
  sessionId: string;
  content: string;
  reasoningContent: string | null;
  /** 最后一次 LLM 调用的 token 快照（缺陷 4 口径：最终上下文状态） */
  usage: TokenUsage | null;
}

/** 工具调用请求（callId 用于与 tool/result 配对） */
export interface ToolCallEvent {
  type: "tool/call";
  sessionId: string;
  callId: string;
  toolName: string;
  /** 工具参数（JSON string），匹配 LLM function calling 的 arguments 格式 */
  params: string;
  /**
   * 展示切片（后端在发出本事件前解析）：卡片短语后紧随 text + link。
   * 旧归档/无信息工具无此字段 → 按缺省处理（不附加任何信息）。
   */
  display?: ToolDisplay;
}

/** 展示切片链接类型：file（open_path 打开）/ url（系统浏览器打开） */
export type ToolDisplayLinkKind = "file" | "url";

/** 展示切片链接（后端解析后的绝对路径 / URL；前端不拼路径） */
export interface ToolDisplayLink {
  kind: ToolDisplayLinkKind;
  /** file = 绝对路径；url = 完整 URL */
  target: string;
  /** 显示标签（文件名 / URL） */
  label: string;
}

/** 工具调用展示切片（文本信息 + 可选链接 + 可选完整原文） */
export interface ToolDisplay {
  text?: string;
  link?: ToolDisplayLink;
  /**
   * 完整原文（后端仅在展示被截断时给出）。截断是视觉约束，悬停时用它显示全文，
   * 保证用户始终有看到完整内容的渠道。
   */
  full?: string;
}

/** 情绪事件（批 2-D 情绪卡片）：标签匹配时由前端投影层产出，记录情绪及强度档位 */
export interface MoodEvent {
  type: "mood/event";
  sessionId: string;
  mood: Exclude<Mood, "平静">;
  intensity: MoodIntensity;
}

/** 工具执行结果。toolName 为 null 表示历史播种的旧数据 */
export interface ToolResultEvent {
  type: "tool/result";
  sessionId: string;
  callId: string;
  toolName: string | null;
  /** 结果摘要，前端直接展示 */
  summary: string;
  /** 是否执行成功（失败内容同样回传 LLM，由模型决定下一步） */
  ok: boolean;
}

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
  knowledgeDir: string;
  skillsDir: string;
  /** 《使用说明》文档路径（随包资源；关于页入口用）。 */
  userGuidePath: string;
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

/** 工具审批请求（M3-A §5.1 / M4）：run_command 询问用户确认时后端推送 */
export interface ToolApprovalRequestEvent {
  type: "tool/approval-request";
  sessionId: string;
  callId: string;
  /** 完整命令（≤512 字符，后端已截断） */
  command: string;
  /** 危险度：crit 不产生 request（仅 low/high 可达；类型保留三档对齐后端） */
  risk: "low" | "high" | "crit";
  mode: ExecutionMode;
  /** 出卡族 reason 码（地域性信任门）：确认卡类别短语映射键。
   *  null = 无门信息（前端不渲染类别行）；crit 不产生 request 故不会出现 crit-* */
  ruleId: string | null;
  /** 请求身份（M4）：restricted=默认受限 / full=完整权限（应用自身令牌） */
  privilege: "restricted" | "full";
  /** full 档的一句话理由（确认卡展示；restricted 档为 null） */
  justification: string | null;
}

/** 工具审批结果（M3-A §5.1 / M4）：询问路径 emit（IPC）；auto-approved/blocked 仅 log 不推 IPC */
export interface ToolApprovalResultEvent {
  type: "tool/approval-result";
  sessionId: string;
  callId: string;
  decision: "approved" | "rejected" | "auto-approved" | "blocked";
  risk: "low" | "high" | "crit";
  mode: ExecutionMode;
  ruleId: string | null;
  reason: "user" | "timeout" | "drop" | null;
  /** 请求身份（M4；与 request 配对） */
  privilege: "restricted" | "full";
}

/** 请求头快照（渲染后的 system prompt + 工具 schema；前端不消费） */
export interface RequestHeaderEvent {
  type: "request/header";
  sessionId: string;
  systemPrompt: string;
  toolDefs: unknown[];
}

/** 错误事件。recoverable=true 时 Agent 继续处理，false 时处理中断 */
export interface SessionErrorEvent {
  type: "error";
  sessionId: string;
  message: string;
  recoverable: boolean;
}

/**
 * 缓存桶判别（模块 7：静态桶常驻提醒 / 动态桶会话级）。
 * 缓存区与 log 分层：CacheStore 独立于 log，cache_* 操作入 log 可回放。
 */
export type CacheBucket = "static" | "dynamic";

/** 缓存区写操作（第三轮 cache_store 工具接线后推送） */
export interface CacheStoreEvent {
  type: "cache/store";
  sessionId: string;
  key: string;
  bucket: CacheBucket;
  /** 静态桶的来源知识条目名（None = 动态桶） */
  sourceEntry: string | null;
}

/** 缓存区读取操作（注入动作入 log） */
export interface CacheLoadEvent {
  type: "cache/load";
  sessionId: string;
  key: string;
  bucket: CacheBucket;
  hit: boolean;
}

/** 缓存区移除操作 */
export interface CacheRemoveEvent {
  type: "cache/remove";
  sessionId: string;
  key: string;
  bucket: CacheBucket;
  existed: boolean;
}

/**
 * 模式切换：Agent 处理流程的当前模式转换（瞬时 UI 信号，不入 log）。
 *
 * phase 不承诺线性推进。reasoning 和 executing 可能反复交替
 *（LLM 思考 → 调工具 → 基于结果再思考 → 再调工具），
 * 直到信息足够后进入 generating 输出最终回答。
 *
 * 映射关系：
 *   reasoning  → UI 状态保持 processing，消息区渲染思维链面板
 *   executing  → UI 状态保持 processing，消息区渲染工具卡片
 *   generating → agentPhase 变为 streaming，deriveUIState 返回 "streaming"
 */
export interface PhaseEvent {
  type: "phase";
  sessionId: string;
  /** reasoning = 思考中 | executing = 调用工具中 | generating = 生成最终回答 */
  phase: "reasoning" | "executing" | "generating";
}

/**
 * 会话标题更新事件（后端生成标题后推送；非 session log 事实）。
 *
 * 与 Rust `events.rs::AgentEvent::ConversationTitle`（serde
 * `type="conversation/title"`, camelCase）对齐。携带 conversationId 而非
 * sessionId——turn 快照在轮末删除，sessionId 无法路由；标题可能在轮末之后到达。
 */
export interface ConversationTitleEvent {
  type: "conversation/title";
  conversationId: string;
  title: string;
  /** 本次标题生成用量（未归档时前端携带至下次 append_turn） */
  usage?: UsageStat | null;
}

/**
 * 记忆管线用量事件（后端内部累加后推送；前端仅镜像进投影用于展示，不回写）。
 * 与 conversation/title 同属旁路信号平面。
 */
export interface MemoryUsageEvent {
  type: "memory/usage";
  conversationId: string;
  usage: UsageStat;
}

// ── IPC 请求契约 ──

/**
 * agent_process invoke 的请求体。
 * Rust 侧根据此结构接收参数，适配层负责在 LLM API 调用前组装消息。
 */
export interface AgentRequest {
  /** 用户原始输入（纯文本） */
  input: string;
  /** 完整对话历史（含本次 user 消息），用于播种后端 session log */
  history: ChatMessage[];
  /** 本次调用的唯一会话标识，随所有事件回传以路由事件 */
  sessionId: string;
  /**
   * 对话标识（模块 C：后端缓存区按此分区，跨轮持久）。
   * 可选：与后端 serde default 对齐——旧后端不读此字段，
   * 后端缺失时退化为 sessionId 兜底。
   */
  conversationId?: string;
}

// ── 对话数据（投影缓存） ──

// ── 对话持久化（Phase 4）──

/**
 * 压缩记录（模块 D · 上下文衰减）——会话级元数据扩展。
 *
 * 语义：压缩 = 一次性的对话起点前移。turns 全量保留（回看资产，永不回灌
 * LLM）；`retainedFrom` = 压缩后保留段第一轮在 turns 的下标，派生层从该处
 * 截断并前置摘要消息；`summary` 为 LLM checkpoint 正文（仅存最新一份，
 * 重压缩覆盖）；`tokensBefore` 为折叠区压缩前估算（信息用）。
 */
export interface CompactionMeta {
  /** 保留段第一轮在 turns 的下标 */
  retainedFrom: number;
  /** 结构化 checkpoint 摘要正文（不含定界块——壳由派生层合成） */
  summary: string;
  /** 折叠区压缩前估算 token */
  tokensBefore: number | null;
  /** 压缩发生时间（unix 毫秒） */
  at: number;
}

/** compact_conversation 成功载荷（与后端 CompactionOutcome serde 对齐） */
export interface CompactOutcome {
  summary: string;
  /** true = 输入超窗规则降级产出（无 LLM 调用） */
  degraded: boolean;
  tokensBefore: number | null;
  /** 本次摘要 LLM 调用用量（降级产出时 null） */
  usage: UsageStat | null;
}

/**
 * 会话持久化元数据（后端 index.json 条目 = list_conversations 载荷）。
 * 与后端 ConversationMeta serde（camelCase）对齐；可缺省字段以 null 表达，
 * 恢复（hydrate）时转 undefined 适配 Conversation。
 * updatedAt 供前端恢复 hook 做导航决策（选最近活跃会话为 activeId）。
 */
export interface ConversationMeta {
  id: string;
  title: string;
  /** 创建时间（unix 毫秒） */
  createdAt: number;
  /** 最近一次轮提交/元数据更新时间（unix 毫秒） */
  updatedAt: number;
  modelName: string | null;
  contextLength: number | null;
  contextTokens: TokenUsage | null;
  lastNoticeSeq: number | null;
  /** 压缩记录（模块 D；null = 未压缩过；旧存档缺字段兼容） */
  compaction: CompactionMeta | null;
  /** 用量统计（Phase 5；null / 缺省 = 从未记录） */
  usage?: ConversationUsage | null;
}

/**
 * get_conversation 载荷：meta + 轮行原文数组（每行 = 该轮 SessionEvent 数组
 * JSON，与前端归档序列化逐字节一致）。恢复时逐行 JSON.parse 重建 turns。
 */
export interface ConversationRecord {
  meta: ConversationMeta;
  turns: string[];
}

/**
 * 提醒数据（后端 get_reminder_data 返回）。
 * - `dims`：环境维度覆写式快照（value 空串的维度后端不下发）；stamp = 最近变更序号。
 * - `events`：记忆事件（有界，容量 200）；`seq` = 已分配的最大序号。
 */
export interface ReminderDim {
  key: string;
  title: string;
  /** 类别：`"value"`（值维度，入首轮快照）／`"catalog"`（目录维度，不入）。 */
  kind: "value" | "catalog";
  value: string;
  note: string;
  stamp: number;
}

/** 记忆事件 */
export interface ReminderEvent {
  seq: number;
  title: string;
  detail: string;
  ts: string;
}

/** get_reminder_data 载荷 */
export interface ReminderData {
  seq: number;
  dims: ReminderDim[];
  events: ReminderEvent[];
}

// ── 记忆页（设置页「记忆」子页 · Phase 4 任务二）──

/** 成长页元数据（overview） */
export interface MemoryGrowthMeta {
  name: string;
  brief: string;
  tags: string[];
  updatedAt: number;
  isProfile: boolean;
}

/** 「近期」条目（与提示词渲染同源） */
export interface MemoryRecentItem {
  date: string;
  title: string;
  summary: string;
}

/** memory_overview 载荷 */
export interface MemoryOverview {
  growth: MemoryGrowthMeta[];
  recent: MemoryRecentItem[];
  previous: string;
  experienceCount: number;
}

/** 成长页详情（memory_get growth） */
export interface MemoryGrowthPage {
  name: string;
  brief: string;
  body: string;
  tags: string[];
  source: string;
  createdAt: number;
  updatedAt: number;
  isProfile: boolean;
}

/** 经历条目（list / get）。无正文层：状态句＋标签＋来源。 */
export interface MemoryExperienceItem {
  id: string;
  date: string;
  title: string;
  summary: string;
  tags: string[];
  absorbed: boolean;
  /** 未收纳且重写达封存线（列表小标用） */
  sealed: boolean;
  /** 来源：对话 id ＋ 轮次范围 */
  conversationId: string;
  turnFrom: number;
  turnTo: number;
}

/** memory_get 返回（kind 判别） */
export interface MemoryDetail {
  kind: "growth" | "experience";
  page?: MemoryGrowthPage | null;
  entry?: MemoryExperienceItem | null;
}

/** memory_set_longterm 返回 */
export interface MemorySetLongtermResult {
  version: number;
  chars: number;
  /** 是否有实际改动（false＝原文逐字相同：未写盘、未推进版本、未推提醒） */
  changed: boolean;
}

/** memory_tidy 返回 */
export interface MemoryTidyResult {
  status: "created" | "rewritten" | "compressed" | "no_delta" | "cooldown" | "failed";
  message: string;
  created: number;
  rewritten: number;
  compressed: boolean;
}

/** 会话附件引用（稳定路径 + 展示名；不含字节）。与后端 attachments::Attachment 对齐。 */
export interface Attachment {
  /** 会话内稳定路径（缩略图经 asset 协议读它） */
  path: string;
  /** 展示名（用户原始文件名） */
  name: string;
  /** MIME 类型 */
  mimeType: string;
  /** 字节数 */
  size: number;
}

/** 历史消息（播种用投影。role: user/assistant/tool 传给 LLM，error 仅 UI 渲染） */
export interface ChatMessage {
  /** "user" = 用户输入 | "assistant" = Agent 回答 | "error" = 错误提示（不传给 LLM）| "tool" = 工具执行结果 */
  role: "user" | "assistant" | "error" | "tool";
  content: string;
  /** 推理模型的思维链（DeepSeek thinking mode） */
  reasoningContent?: string;
  /** assistant 消息携带的工具调用请求 */
  toolCalls?: Array<{
    id: string;
    callType: "function";
    function: { name: string; arguments: string };
  }>;
  /** tool 消息携带的 tool_call_id（工具执行结果匹配） */
  toolCallId?: string;
  /** user 消息附带的附件（有附件时携带） */
  attachments?: Attachment[];
}

/**
 * 一个完整的对话记录（前端 = 后端 session log 的投影缓存）。
 *
 * `turns` 每轮一组 SessionEvent 序列（user/message 由前端合成置于轮首），
 * 消息气泡与事件卡片均由投影函数（deriveMessages / buildDisplayItems）从
 * turns 派生，不再单独维护 messages 数组。
 */
export interface Conversation {
  id: string;
  title: string;
  /**
   * 真标题（后端生成 / 用户改名）是否已落地。
   *
   * `createConversation` 时 title 是"首条消息截断"出来的占位标题 → false；
   * 收到 conversation/title 事件或用户改名后 → true。历史数据（水合）没有这个
   * 字段 = undefined，视为已落地（它们的标题本来就来自盘上）。
   *
   * 用途：引导的"对话标题"那条要等真标题落地才出——否则会先框住占位标题，
   * 等真标题到了再框一次，用户看到闪烁。
   */
  titleGenerated?: boolean;
  /** 每轮的 SessionEvent 序列（投影缓存——后端 log 的事件镜像） */
  turns: SessionEvent[][];
  createdAt: number;
  /** 创建对话时使用的模型名（ContextIndicator 展示） */
  modelName?: string;
  /** 创建对话时模型的上下文窗口大小（Token 数） */
  contextLength?: number;
  /** 最近一轮 assistant/message 的上下文 Token 用量快照（覆盖式更新） */
  contextTokens?: TokenUsage;
  /**
   * 当前轮实时上下文用量（仅内存，不落盘）。
   * step/end 收到 provider usage 时写入；turn/end、失败、压缩时清空，
   * 圆环回退到 contextTokens。
   */
  liveContextTokens?: TokenUsage;
  /**
   * 压缩记录（模块 D；undefined = 未压缩过）。压缩后 contextTokens 被置空
   * （进度环归零）——下一次真实轮 usage 自然覆盖。
   */
  compaction?: CompactionMeta;
  /**
   * 环境通知已告知游标（M1 Reminder）：上次拼装进本对话历史的通知最大 seq。
   * sendMessage 时经 advanceNoticeSeq action 推进（拼装即防重复；invoke
   * 失败回滚）。游标为会话元数据的一部分——对话持久化落地时随会话落盘，
   * 接口不变（读写收敛于 ConversationStore action）。缺省 = 0：下轮全量告知。
   * 压缩成功时经 applyCompaction 复位（批 4：下一条消息重发三值快照）。
   */
  lastNoticeSeq?: number;
  /** 用量统计（Phase 5）：按四类累计，随轮 / 压缩 / 记忆 / 标题更新。 */
  usage?: ConversationUsage;
}
