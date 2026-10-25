// 发送消息：并发保护、提醒装配、历史派生、上下文硬拦与快照创建。
//
// 不直接持有 store；由 useProcessingStore 把 get/set 传进来。

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentRequest,
  Attachment,
  ReminderData,
  UserMessageEvent,
} from "../types";
import { generateId } from "../utils";
import { useUIStore } from "./useUIStore";
import { useConversationStore } from "./useConversationStore";
import { useCompactionStore } from "./useCompactionStore";
import { showToast } from "./useToastStore";
import { ensureActiveConversation } from "./processingConversation";
import { composeReminderNotice } from "./processingReminder";
import { derivePreviousTurnStatus, willOverflowContext } from "./processingRequest";
import { getModelInfo } from "./processingModelInfo";
import type { ProcessingState, ProcessingStore } from "./useProcessingStore";

function hasProcessingFor(
  processing: Record<string, ProcessingState>,
  conversationId: string
): boolean {
  return Object.values(processing).some(
    (snap) => snap.conversationId === conversationId
  );
}

/** 合成 user/message 事件（前端输入即事实，后端不镜像回传） */
function makeUserEvent(
  sessionId: string,
  content: string,
  attachments?: Attachment[]
): UserMessageEvent {
  return attachments && attachments.length > 0
    ? { type: "user/message", sessionId, content, attachments }
    : { type: "user/message", sessionId, content };
}

type SetStore = (
  partial:
    | Partial<ProcessingStore>
    | ((state: ProcessingStore) => Partial<ProcessingStore>)
) => void;

export async function sendProcessingMessage(
  get: () => ProcessingStore,
  set: SetStore,
  input: string,
  attachments: Attachment[]
): Promise<boolean> {
  // 上一轮错误不锁死发送：用户重新提交即为重试。
  useUIStore.getState().clearError();
  const convStore = useConversationStore.getState();
  const { processing } = get();
  const activeId = convStore.activeId;

  if (activeId !== null && hasProcessingFor(processing, activeId)) return false;

  // ── 模块 D：压缩中禁发（同会话）——压缩进行中 turns 处于状态转换窗口，
  // 新消息若随旧 turns 发送将与压缩结果错位（双向互斥的前端半边；后端
  // busy 闸兜底）。 ──
  if (
    activeId !== null &&
    useCompactionStore.getState().compactingConversationId === activeId
  )
    return false;

  // ── Reminder：拉取提醒数据（后端 get_reminder_data：快照＝值 + 有界事件）──
  // 含块版本随轮归档冻结；游标不在这里提前推进——只有 turn/end 正常完成才提交
  // （见 ProcessingState.noticeAdvanceTo）。拉取失败 → 静默跳过本轮（游标不动 → 下轮重试）。
  let reminderData: ReminderData = { seq: 0, dims: [], events: [] };
  try {
    reminderData = (await invoke<ReminderData>("get_reminder_data")) ?? reminderData;
  } catch {
    // 提醒拉取失败：本轮不带环境通知，游标未动 → 下轮重试；
    // 上一轮状态仍应传达，不能因为环境拉取失败一起丢掉。
  }
  // await 后以最新对话状态为准（拉取期间状态可能被推进）。
  // 上一轮状态走 reminder，不修改冻结的前缀 system prompt。
  const reminderState = useConversationStore.getState();
  const reminderConv = reminderState.activeId !== null
    ? reminderState.conversations.find((c) => c.id === reminderState.activeId)
    : undefined;
  const previousTurnStatus = derivePreviousTurnStatus(reminderConv);
  const notice = composeReminderNotice(
    reminderData,
    reminderConv?.lastNoticeSeq,
    previousTurnStatus
  );
  const noticeBlock = notice.block;
  const noticeAdvanceTo = notice.advanceTo;
  const displayContent = noticeBlock
    ? `${noticeBlock}\n\n${input}`
    : input;

  const st = useConversationStore.getState();
  const isNewNow = st.activeId === null;
  const conv = isNewNow
    ? undefined
    : st.conversations.find((c) => c.id === st.activeId);
  // 设置页可能把供应商切到“尚未选模型”的状态；此时不能静默改用全局模型。
  const sendInfo = getModelInfo();
  const convHasPartialConfig =
    Boolean(conv?.provider || conv?.model) &&
    (!conv?.provider || !conv?.model);
  const draft = st.draftConfig;
  const draftHasPartialConfig =
    isNewNow &&
    Boolean(draft?.provider || draft?.model) &&
    (!draft?.provider || !draft?.model);
  const incompleteConfig = convHasPartialConfig || draftHasPartialConfig;
  if (incompleteConfig) {
    showToast("请先在设置中选择模型", {
      kind: "warning",
      dedupeKey: "model-missing",
    });
    return false;
  }

  if (import.meta.env.DEV && import.meta.env.MODE !== "test" && reminderConv) {
    const last = reminderConv.turns[reminderConv.turns.length - 1];
    console.debug("[previous-turn-status]", previousTurnStatus ?? "none", {
      turns: reminderConv.turns.length,
      tail: last?.slice(-3).map((e) => e.type),
    });
  }

  // ── 模块 D：上下文窗口硬拦（发送前预估）──
  // 拦截为提示而非自动压缩（共识：手动触发）。
  const windowSize =
    conv?.contextLength ?? getModelInfo()?.contextLength ?? 0;
  if (
    !isNewNow &&
    conv &&
    willOverflowContext(conv, displayContent, windowSize)
  ) {
    showToast("上下文接近窗口上限，请先压缩后再继续", {
      kind: "warning",
      dedupeKey: "ctx-window-full",
    });
    return false;
  }

  // 创建或复用对话，并维护自动标题现场
  const conversationId = ensureActiveConversation(isNewNow, conv, input);
  const convNow = useConversationStore
    .getState()
    .conversations.find((c) => c.id === conversationId);

  // 游标不在发送前推进：只有本轮正常完成才由 finishProcessing 提交，
  // 避免 invoke 失败后基线永久缺失（新对话重试仍会重发完整快照）。

  // await/创建期间可能已有本对话的处理中快照（快速连发）→ 重查并发保护
  // （双拉同时完成的理论窗口仍存在，UI 双击主场景被此覆盖）
  if (hasProcessingFor(get().processing, conversationId)) return false;

  const sessionId = generateId();
  // 函数式 set：合并最新 state.processing（await 期间其他对话可能已新增条目）
  set((state) => ({
    processing: {
      ...state.processing,
      [sessionId]: {
        conversationId,
        // 本轮提醒块对应的游标；正常完成后由 finishProcessing 提交
        noticeAdvanceTo:
          noticeAdvanceTo > 0 ? noticeAdvanceTo : undefined,
        // user/message 预置为第一条（含块版本；投影缓存进行态，归档时随轮落库）
        currentEvents: [makeUserEvent(sessionId, displayContent, attachments)],
        agentPhase: "processing",
        // 情绪默认平静（省略）；标签匹配成功时 set mood
        mood: "平静",
        moodIntensity: 0,
      },
    },
  }));
  // processing 字典新增了活跃对话的条目 → deriveUIState 自动返回 "processing"

  // 缓存区按对话分区（模块 C）：每轮随请求携带 conversationId，
  // 后端据此定位本对话的静态/动态桶（跨轮持久）。
  // sessionId 每轮重新生成，不能作为分区键；旧后端不读此字段。
  const metaNow = useConversationStore
    .getState()
    .conversations.find((c) => c.id === conversationId);
  // 旧对话没有会话级配置 → 用当前全局种子固化一次；此后按对话隔离。
  const infoNow = sendInfo;
  const configNow =
    metaNow?.provider && metaNow.model
      ? {
          provider: metaNow.provider,
          model: metaNow.model,
          thinkingLevel: metaNow.thinkingLevel ?? null,
        }
      : infoNow?.provider && infoNow.model
        ? {
            provider: infoNow.provider,
            model: infoNow.model,
            thinkingLevel: infoNow.thinkingLevel ?? null,
          }
        : null;
  if (configNow && (!metaNow?.provider || !metaNow?.model)) {
    useConversationStore
      .getState()
      .updateConversationConfig(
        conversationId,
        configNow,
        infoNow?.name,
        infoNow?.contextLength
      );
  }
  const request: AgentRequest = {
    input,
    sessionId,
    conversationId,
    userContent: displayContent,
    userAttachments: attachments,
    conversationTitle: metaNow?.title ?? convNow?.title,
    conversationCreatedAt: metaNow?.createdAt,
    modelName: metaNow?.modelName,
    contextLength: metaNow?.contextLength,
    provider: configNow?.provider,
    model: configNow?.model,
    thinkingLevel: configNow?.thinkingLevel ?? null,
    lastNoticeSeq: metaNow?.lastNoticeSeq,
  };
  invoke("agent_process", { request }).catch((err) => {
    // invoke 本身的异常（后端进程崩溃、网络不可达等）：后端未跑；游标没有
    // 提前推进，因此无需回滚，下一轮会重发同一份提醒。错误轮归档剥离块
    // （failProcessing 内）——错误轮不对 LLM 可见，块不应残留历史。
    // Agent 处理过程中的错误通过 error 事件到达，不经过此 catch。
    get().failProcessing(sessionId, String(err));
  });
  return true;
}
