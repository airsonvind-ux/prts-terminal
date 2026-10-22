// PRTS 终端 · LLM 配置、模型能力与 token 用量类型

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
  /** 推理链多轮回传策略覆盖；缺省 = 由 thinkingFormat 家族默认决定 */
  reasoningPolicy?: "preserve" | "require_tool_call";
  /** 流式响应是否接受 stream_options.include_usage；缺省 = 发送并失败降级 */
  supportsUsageInStreaming?: boolean;
}

/** 效果层 compat（resolve_compat 运行时计算，不序列化到 JSON） */
export interface ResolvedCompat {
  carrier: string | null;
  requestParam: string | null;
  policy: "preserve" | "require_tool_call";
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

// ── Token 用量 ──

/**
 * 归一化 Token 用量。
 * 与 Rust 后端 TokenUsage 结构对齐（#[serde(rename_all = "camelCase")]）。
 * assistant/message 事件携带最后一次调用的快照（最终上下文状态），
 * 前端按对话覆盖式累积。
 */
export interface TokenUsage {
  inputTokens: number;
  /**
   * 上下文水位专用输入总量。Anthropic 会加回 cacheRead/cacheCreation；
   * 其他协议缺省时回退到 inputTokens。仅用于圆环与溢出判断。
   */
  contextInputTokens?: number;
  outputTokens: number;
}

/**
 * 统计增量（与后端 UsageStat 对齐）。缓存口径已按协议归一：
 * cachePromptTokens 为命中率分母；无缓存观测时二者缺省。
 */
export interface UsageStat {
  inputTokens: number;
  outputTokens: number;
  cacheHitTokens?: number;
  cachePromptTokens?: number;
}

/** 单个类别的累计用量（后端 CategoryUsage；u64 → number）。 */
export interface CategoryUsage {
  inputTokens: number;
  outputTokens: number;
  /** 命中缓存累计（与 cachePromptTokens 同为 0 时视为未观测） */
  cacheHitTokens: number;
  /** 命中率分母累计（prompt 全量） */
  cachePromptTokens: number;
}

/** 四类累计用量（后端 ConversationUsage）。 */
export interface ConversationUsage {
  session: CategoryUsage;
  compaction: CategoryUsage;
  memory: CategoryUsage;
  title: CategoryUsage;
}

/** 用量类别（与后端 UsageCategory 对齐）。 */
export type UsageCategory = "session" | "compaction" | "memory" | "title";

/** 用量增量（随写命令提交）。 */
export interface UsageDelta {
  category: UsageCategory;
  inputTokens: number;
  outputTokens: number;
  cacheHitTokens?: number;
  cachePromptTokens?: number;
}
