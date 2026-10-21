// PRTS 终端 · 附件与消息投影类型

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
