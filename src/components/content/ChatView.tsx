// src/components/content/ChatView.tsx
// 内容层编排组件 · 对话界面入口
//
// 职责：从三个 store 读取数据，转化为子组件 props，flex column 布局。
// 自身无状态——不做任何业务计算，仅做 props 桥接。
//
// 架构升级 · 模块 1：对话数据为投影缓存（turns: SessionEvent[][]），
// 消息气泡/事件卡片由 MessageList 内部投影派生。

import { useCallback, useMemo, type MouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useConversationStore } from "../../stores/useConversationStore";
import { useProcessingStore } from "../../stores/useProcessingStore";
import { useUIStore } from "../../stores/useUIStore";
import { useUIState } from "../../hooks/useUIState";
import { useCompactionStore } from "../../stores/useCompactionStore";
import { EMPTY_STEER_PENDING, useSteerStore } from "../../stores/useSteerStore";
import { showToast } from "../../stores/useToastStore";
import { Toolbar } from "./Toolbar";
import { MessageList } from "./MessageList";
import { FindBar } from "./FindBar";
import { InputArea } from "./InputArea";
import { ContextIndicator } from "./ContextIndicator";
import styles from "./ChatView.module.css";

interface Props {
  /** 窗口拖拽手柄回调（由 App.tsx 通过 useDrag Hook 注入） */
  onDragHandle?: (e: MouseEvent) => void;
}

export function ChatView({ onDragHandle }: Props) {
  // ── 活跃对话数据（投影缓存） ──
  const activeId = useConversationStore((s) => s.activeId);
  const conversations = useConversationStore((s) => s.conversations);

  const activeConv = activeId
    ? conversations.find((c) => c.id === activeId)
    : undefined;
  const turns = useMemo(() => activeConv?.turns ?? [], [activeConv?.turns]);

  // ── 活跃对话的处理中快照（流式输出期间非空） ──
  // 与 useUIState 中查找 agentPhase 的模式一致：
  // processing 字典的键是 sessionId，需要按 conversationId 遍历匹配
  const processingSnapshot = useProcessingStore((s) => {
    if (!activeId) return undefined;
    return Object.values(s.processing).find(
      (p) => p.conversationId === activeId
    );
  });
  const currentEvents = useMemo(
    () => processingSnapshot?.currentEvents ?? [],
    [processingSnapshot?.currentEvents]
  );

  // ── 模块 D：压缩状态（压缩中禁发 + 提示卡数据源） ──
  const compactingConversationId = useCompactionStore(
    (s) => s.compactingConversationId
  );
  const compacting = activeId !== null && compactingConversationId === activeId;
  const activeCompaction = activeConv?.compaction;

  // ── 发送与状态 ──
  const sendMessage = useProcessingStore((s) => s.sendMessage);
  const uiState = useUIState();
  // 启动会话恢复中（Phase 4）：期间抑制欢迎界面判定 + 禁发（防竞态）
  const restoring = useUIStore((s) => s.restoring);

  // ── 当前对话处理中轮次的 sessionId（processing 字典 key = 后端
  // abort_turn / steer_message 的活动 turn 目标）──
  const processingSessionId = useProcessingStore((s) => {
    if (!activeId) return null;
    const entries = Object.entries(s.processing);
    for (let i = 0; i < entries.length; i++) {
      if (entries[i][1].conversationId === activeId) return entries[i][0];
    }
    return null;
  });
  const generating = processingSessionId !== null;

  // 硬禁（非生成态）：错误 / 会话恢复中 / 压缩中——不发送也不插话不中止
  const hardDisabled = restoring || compacting || uiState === "error";

  // ── 插话：pending 待注入气泡（仅当前进行中轮；空列表复用常量引用）──
  const pending = useSteerStore((s) =>
    processingSessionId
      ? s.pendingBySession[processingSessionId] ?? EMPTY_STEER_PENDING
      : EMPTY_STEER_PENDING
  );

  const handleSubmit = useCallback(
    (text: string) => {
      sendMessage(text);
    },
    [sendMessage]
  );

  // ── 插话提交（生成中）：入队成功（返回条目 id）→ 登记 pending → 清输入；
  // 失败 toast → 保留输入供修改 ──
  const handleSteer = useCallback(
    async (text: string): Promise<boolean> => {
      if (!processingSessionId) return false;
      try {
        const id = await invoke<string>("steer_message", {
          sessionId: processingSessionId,
          text,
        });
        useSteerStore.getState().addPending(processingSessionId, { id, text });
        return true;
      } catch (err) {
        showToast(String(err), { kind: "warning", dedupeKey: "steer-failed" });
        return false;
      }
    },
    [processingSessionId]
  );

  // ── 中止（生成中无字方块）：后端 abort_turn 已清插话队列；前端乐观清
  // pending（气泡立即消失），轮尾 turn/end(aborted) 走现有归档链路 ──
  const handleStop = useCallback(() => {
    if (!processingSessionId) return;
    useSteerStore.getState().clearForSession(processingSessionId);
    invoke("abort_turn", { sessionId: processingSessionId }).catch(() => {});
  }, [processingSessionId]);

  // ── pending 气泡点击取消：cancel_steer 成功才移除（消失即反馈，不回填）──
  const handleCancelSteer = useCallback(
    (entryId: string) => {
      if (!processingSessionId) return;
      invoke<boolean>("cancel_steer", {
        sessionId: processingSessionId,
        entryId,
      })
        .then((ok) => {
          if (ok) {
            useSteerStore.getState().removePending(processingSessionId, entryId);
          }
        })
        .catch(() => {});
    },
    [processingSessionId]
  );

  // ── 首次使用 ──
  // conversations.length === 0 且 activeId === null → 欢迎界面；
  // restoring 期间抑制（防恢复完成前闪欢迎界面）
  const isFirstUse = conversations.length === 0 && !restoring;

  return (
    <div className={styles.container}>
      <Toolbar onDragHandle={onDragHandle} />
      {restoring && turns.length === 0 ? (
        /* 恢复中且尚无内容：给一个明确的等待态。
           启动要读全量会话（长对话单文件可达十余 MB），这段工作占满主线程，
           期间没有任何内容可画——用占位把“空”变成“在等”。 */
        <div className={styles.restoring} data-restoring>
          正在恢复上次对话…
        </div>
      ) : (
        <MessageList
          turns={turns}
          currentEvents={currentEvents}
          isFirstUse={isFirstUse}
          compaction={activeCompaction}
          pending={pending}
          onCancelPending={handleCancelSteer}
          conversationId={activeId}
        />
      )}
      {!isFirstUse && <FindBar />}
      <div className={styles.inputRow}>
        <ContextIndicator />
        <InputArea
          onSubmit={handleSubmit}
          onSteer={handleSteer}
          onStop={handleStop}
          generating={generating}
          disabled={hardDisabled}
        />
      </div>
    </div>
  );
}
