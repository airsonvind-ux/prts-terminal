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
import { buildSendHistory, willOverflowContext } from "./processingRequest";
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
): Promise<void> {
  // 上一轮错误不锁死发送：用户重新提交即为重试。
  useUIStore.getState().clearError();
  const convStore = useConversationStore.getState();
  const { processing } = get();
  const activeId = convStore.activeId;

  if (activeId !== null && hasProcessingFor(processing, activeId)) return;

  // ── 模块 D：压缩中禁发（同会话）——压缩进行中 turns 处于状态转换窗口，
  // 新消息若随旧 turns 发送将与压缩结果错位（双向互斥的前端半边；后端
  // busy 闸兜底）。 ──
  if (
    activeId !== null &&
    useCompactionStore.getState().compactingConversationId === activeId
  )
    return;

  // ── Reminder：拉取提醒数据（后端 get_reminder_data：快照＝值 + 有界事件）──
  // 含块版本随轮归档冻结；invoke 失败仅进行中对话回滚（新对话不回滚为 0，
  // 避免重放）。拉取失败 → 静默跳过本轮（游标不动 → 下轮重试）。
  let noticeBlock = "";
  let noticeAdvanceTo = 0;
  let prevNoticeSeq = 0;
  let noticeIsNewConversation = false;
  try {
    const data = (await invoke<ReminderData>("get_reminder_data")) ?? {
      seq: 0,
      dims: [],
      events: [],
    };
    // await 后以最新对话状态为准（拉取期间状态可能被推进）
    const st = useConversationStore.getState();
    const conv = st.activeId !== null
      ? st.conversations.find((c) => c.id === st.activeId)
      : undefined;
    const notice = composeReminderNotice(data, conv?.lastNoticeSeq);
    noticeBlock = notice.block;
    noticeAdvanceTo = notice.advanceTo;
    prevNoticeSeq = notice.prevSeq;
    noticeIsNewConversation = notice.isNewConversation;
  } catch {
    // 提醒拉取失败：本轮不带通知，游标未动 → 下轮重试
  }
  const displayContent = noticeBlock
    ? `${noticeBlock}\n\n${input}`
    : input;

  // 准备传给 LLM 的完整对话历史（含本次 user 消息——含块版本随 history
  // 发送并归档冻结，是 LLM 实际所见）。
  const st = useConversationStore.getState();
  const isNewNow = st.activeId === null;
  const conv = isNewNow
    ? undefined
    : st.conversations.find((c) => c.id === st.activeId);
  const history = buildSendHistory(conv, displayContent, attachments);

  // ── 模块 D：上下文窗口硬拦（发送前预估）──
  // 拦截为提示而非自动压缩（共识：手动触发）。
  const windowSize = getModelInfo()?.contextLength ?? 0;
  if (
    !isNewNow &&
    conv &&
    willOverflowContext(conv, displayContent, windowSize)
  ) {
    showToast("上下文接近窗口上限，请先压缩后再继续", {
      kind: "warning",
      dedupeKey: "ctx-window-full",
    });
    return;
  }

  // 创建或复用对话，并维护自动标题现场
  const conversationId = ensureActiveConversation(isNewNow, conv, input);

  // 游标推进：游标＝payload.seq（快照与增量同口径；含本次未拼入的事件，防下轮补放）。
  if (noticeAdvanceTo > prevNoticeSeq) {
    useConversationStore
      .getState()
      .advanceNoticeSeq(conversationId, noticeAdvanceTo);
  }

  // await/创建期间可能已有本对话的处理中快照（快速连发）→ 重查并发保护
  // （双拉同时完成的理论窗口仍存在，UI 双击主场景被此覆盖）
  if (hasProcessingFor(get().processing, conversationId)) return;

  const sessionId = generateId();
  // 函数式 set：合并最新 state.processing（await 期间其他对话可能已新增条目）
  set((state) => ({
    processing: {
      ...state.processing,
      [sessionId]: {
        conversationId,
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
  const request: AgentRequest = { input, history, sessionId, conversationId };
  invoke("agent_process", { request }).catch((err) => {
    // invoke 本身的异常（后端进程崩溃、网络不可达等）：后端未跑 →
    // 回滚通知游标（下轮重拉重拼，防丢失）。错误轮归档剥离块
    // （failProcessing 内）——错误轮不对 LLM 可见，块不应残留历史。
    // Agent 处理过程中的错误通过 error 事件到达，不经过此 catch。
    // 回滚语义：仅**进行中**对话回滚游标（防丢失）；新对话基线推进
    // 不回滚为 0——否则重试时会把历史通知全量重放。
    if (!noticeIsNewConversation && noticeAdvanceTo > prevNoticeSeq) {
      useConversationStore
        .getState()
        .advanceNoticeSeq(conversationId, prevNoticeSeq);
    }
    get().failProcessing(sessionId, String(err));
  });
}
