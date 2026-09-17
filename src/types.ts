// PRTS 终端 · 全局类型定义
// 所有跨文件共享的类型和接口集中在此，作为应用的"类型词汇表"

// ── LLM 配置（与后端 LlmConfig 对应，JSON key 为 snake_case） ──

/**
 * LLM 连接配置。
 * 后端 LlmConfig 未使用 #[serde(rename_all)]，因此序列化为 snake_case。
 * 前端 invoke get_config 返回、set_config 提交，key 命名保持一致。
 */
export interface LlmConfig {
  provider: string;
  /** API 协议类型，决定后端选哪个适配器。默认 openai-completions */
  api_type: ApiType;
  /** 模型名。通过 list_models 下拉选择后写入 */
  model: string;
  api_key: string;
  base_url: string | null;
  /** 额外 HTTP 请求头，用于代理/中转场景。JSON 字符串，null = 不发送 */
  headers: string | null;
  /** 单次请求 token 上限。默认 4096，用户无需手动调整 */
  max_tokens: number;
  /** 思考强度 canonical 档字符串；null/缺省 = 未设置不干预（批 C 接线） */
  thinking_level?: string | null;
}

// ── 模型/供应商类型（与后端 llm/mod.rs 对齐） ──

/** API 协议类型。与 Rust ApiType 枚举对齐 */
export type ApiType =
  | "openai-completions"
  | "anthropic-messages"
  | "openai-responses";

/** 模型所属的 thinking 家族。与 Rust ThinkingFormat 枚举对齐 */
export type ThinkingFormat =
  | "deepseek"
  | "anthropic"
  | "zhipu"
  | "qwen"
  | "kimi"
  | "volcengine"
  | "openrouter";

/** 声明层 compat（models.json 中存储）。模型级覆盖供应商级 */
export interface Compat {
  thinkingFormat?: ThinkingFormat;
  auth?: "bearer" | "none";
}

/** 效果层 compat（resolve_compat 运行时计算，不序列化到 JSON） */
export interface ResolvedCompat {
  carrier: string | null;
  requestParam: string | null;
  policy: "preserve" | "require-tool-call";
  clearable: boolean;
  auth: "bearer" | "none" | null;
}

/** list_providers 命令返回的单条供应商信息 */
export interface ProviderInfo {
  id: string;
  displayName: string;
  apiType: ApiType;
  baseUrl: string;
  compat: Compat;
}

/** list_models 命令返回的单条模型信息 */
export interface ModelInfo {
  id: string;
  displayName: string;
  contextLength: number;
  supportsReasoning: boolean;
  supportsVision: boolean;
  compat: Compat;
  /** 思考强度白名单（camelCase 对齐后端；空 = 无声明 → UI 推断集 [off,medium,high]） */
  thinkingLevels?: string[];
  /** 模型默认思考档（声明；空 = 推断默认 medium） */
  defaultThinkingLevel?: string | null;
  /** 是否已收录能力信息（false = 未收录 → 列表显示 U 徽章；能力字段为缺省值非确认） */
  enriched: boolean;
}

// ── UI 状态机（五状态） ──

/**
 * 前端 UI 状态机：当前画面处于哪个阶段。
 * 无 idle 状态：应用启动时初始为 ready，配合 activeId=null 表现与 idle 完全一致。
 * conversations.length === 0 用于判断首次使用，不单独维护状态。
 */
export type UIState =
  | "ready"
  | "processing"
  | "streaming"
  | "hidden"
  | "error";

/**
 * hidden 状态的子形态：决定隐藏后的渲染行为。
 *  null：hidden 未激活（窗口正常可见时）。
 *  "shortcut"：快捷键隐藏，窗口完全不可见，不渲染任何内容。
 *  "edge_snap"：边缘吸附隐藏，窗口收缩为标签。
 */
export type HiddenMode = null | "shortcut" | "edge_snap";

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
export type AgentEvent = SessionEvent | PhaseEvent | ConversationTitleEvent;

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
}

/** step 开始：一次 LLM 请求 + 其调用的工具。estimatedTokens 为请求前估算（双锚兜底） */
export interface StepStartEvent {
  type: "step/start";
  sessionId: string;
  step: number;
  estimatedTokens: number;
}

/** step 结束：本次调用的 token 用量（最后一次即最终上下文状态） */
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

/** 工具调用展示切片（文本信息 + 可选链接） */
export interface ToolDisplay {
  text?: string;
  link?: ToolDisplayLink;
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
  workspace: WorkspaceSettings;
  window: WindowSettings;
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
  /** 空串 = 未设置（装配回退 data_dir）；修改需重启生效 */
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
  workspace?: WorkspacePatch;
  window?: WindowPatch;
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

/** get_app_meta 载荷（关于页 + 空间页共用只读信息） */
export interface AppMeta {
  version: string;
  data_dir: string;
  workspace_root: string;
  db_path: string;
  knowledge_dir: string;
  skills_dir: string;
}

/** list_skills / open_path 载荷（空间页技能清单；来源 skills md frontmatter）
 *  字段与 Rust SkillEntry `#[serde(rename_all = "camelCase")]` 对齐：
 *  后端序列化为 filePath/name/description。filePath 由后端供给（源 md 绝对
 *  路径，前端不拼接硬编码）；open_path 仅放行资源目录（技能/知识库）内路径。 */
export interface SkillEntry {
  name: string;
  description: string;
  /** 源 md 绝对路径（camelCase 对齐后端；null/缺省 = 无文件可打开） */
  filePath?: string | null;
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

// ── Token 用量 ──

/**
 * 归一化 Token 用量。
 * 与 Rust 后端 TokenUsage 结构对齐（#[serde(rename_all = "camelCase")]）。
 * assistant/message 事件携带最后一次调用的快照（最终上下文状态），
 * 前端按对话覆盖式累积。
 */
export interface TokenUsage {
  inputTokens: number;
  outputTokens: number;
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

/** memory_tidy 返回 */
export interface MemoryTidyResult {
  status: "created" | "rewritten" | "compressed" | "no_delta" | "cooldown" | "failed";
  message: string;
  created: number;
  rewritten: number;
  compressed: boolean;
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
   * 压缩记录（模块 D；undefined = 未压缩过）。压缩后 contextTokens 被置空
   * （进度环归零）——下一次真实轮 usage 自然覆盖。
   */
  compaction?: CompactionMeta;
  /**
   * 环境通知已告知游标（M1 Reminder）：上次拼装进本对话历史的通知最大 seq。
   * sendMessage 时经 advanceNoticeSeq action 推进（拼装即防重复；invoke
   * 失败回滚）。游标为会话元数据的一部分——对话持久化落地时随会话落盘，
   * 接口不变（读写收敛于 ConversationStore action）。缺省 = 0：下轮全量告知。
   */
  lastNoticeSeq?: number;
}
