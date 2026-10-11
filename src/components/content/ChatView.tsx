// src/components/content/ChatView.tsx
// 内容层编排组件 · 对话界面入口
//
// 职责：从三个 store 读取数据，转化为子组件 props，flex column 布局。
// 自身无状态——不做任何业务计算，仅做 props 桥接。
//
// 架构升级 · 模块 1：对话数据为投影缓存（turns: SessionEvent[][]），
// 消息气泡/事件卡片由 MessageList 内部投影派生。

import { useCallback, useEffect, useMemo, useState, type MouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Attachment } from "../../types";
import { useConversationStore } from "../../stores/useConversationStore";
import { useProcessingStore, getModelInfo, markAborting } from "../../stores/useProcessingStore";
import { useUIStore } from "../../stores/useUIStore";
import { useUIState } from "../../hooks/useUIState";
import { useCompactionStore } from "../../stores/useCompactionStore";
import { EMPTY_STEER_PENDING, useSteerStore } from "../../stores/useSteerStore";
import { showToast } from "../../stores/useToastStore";
import { useGuideStore } from "../../stores/useGuideStore";
import { Toolbar } from "./Toolbar";
import { MessageList } from "./MessageList";
import { BootSequence } from "./BootSequence";
import { FindBar } from "./FindBar";
import { InputArea } from "./InputArea";
import { AttachmentStrip } from "./AttachmentStrip";
import { DragOverlay } from "./DragOverlay";
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
  // 中止已发出、轮未收尾（乐观态，见 handleStop）
  const aborting = processingSnapshot?.aborting ?? false;

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
  // 覆盖层面板（设置 / 历史）：开着时欢迎页导览不弹——锚点会被盖住
  const overlay = useUIStore((s) => s.overlay);
  // 启动演出覆盖层（2026-09 定稿）：播完（含淡出）才撤下、露出对话
  const [bootVisible, setBootVisible] = useState(true);
  const handleBootDone = useCallback(() => setBootVisible(false), []);
  // 标题与指示环的揭示（启动演出开始淡出时置位）：演出期间它们不显示，
  // 否则“已有对话”会在载入过程中就露面，载入过程反而看不出来。
  const [chromeVisible, setChromeVisible] = useState(false);
  const handleBootReveal = useCallback(() => setChromeVisible(true), []);

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

  // 硬禁（非生成态）：错误 / 会话恢复中 / 启动演出未撤 / 压缩中——不发送也不插话不中止
  const hardDisabled =
    restoring || bootVisible || compacting || uiState === "error";

  // ── 插话：pending 待注入气泡（对话级，2026-09-25 修订）──
  // 不再要求「进行中轮」：中止/错误后未交付条目跨轮存活（后端保留、
  // 下一轮领取），气泡任何时候都显示当前对话的待交付项（可点击取消；
  // 空列表复用常量引用）。
  const pending = useSteerStore((s) =>
    activeId
      ? s.pendingByConversation[activeId] ?? EMPTY_STEER_PENDING
      : EMPTY_STEER_PENDING
  );

  // ── 附件（旁路输入：拖入 / 粘贴；无按钮）──
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const addAttachments = useCallback((list: Attachment[]) => {
    if (list.length > 0) setAttachments((prev) => [...prev, ...list]);
  }, []);
  const removeAttachment = useCallback((index: number) => {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  }, []);

  /** 拖入的路径 → 上传（落点按当前对话分区）。 */
  const uploadPaths = useCallback(
    (paths: string[]) => {
      if (!paths || paths.length === 0) return;
      void invoke<Attachment[]>("upload_attachment", {
        paths,
        conversationId: activeId ?? null,
      })
        .then(addAttachments)
        .catch((err) =>
          showToast(String(err), { kind: "warning", dedupeKey: "attach-failed" })
        );
    },
    [activeId, addAttachments]
  );

  /** 粘贴图片 blob → 上传。 */
  const uploadBlob = useCallback(
    async (file: File) => {
      try {
        const base64 = await new Promise<string>((resolve, reject) => {
          const reader = new FileReader();
          reader.onerror = () => reject(reader.error ?? new Error("读取失败"));
          reader.onload = () => {
            const s = typeof reader.result === "string" ? reader.result : "";
            const comma = s.indexOf(",");
            resolve(comma >= 0 ? s.slice(comma + 1) : s);
          };
          reader.readAsDataURL(file);
        });
        const item = await invoke<Attachment>("upload_attachment_blob", {
          name: file.name || "pasted.png",
          base64Data: base64,
          mimeType: file.type || "image/png",
          conversationId: activeId ?? null,
        });
        addAttachments([item]);
      } catch (err) {
        showToast(String(err), { kind: "warning", dedupeKey: "attach-failed" });
      }
    },
    [activeId, addAttachments]
  );

  // 拖入（Tauri webview 事件；拖到窗口任意处即上传，拖拽中现形遮罩）
  const [dragActive, setDragActive] = useState(false);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const off = await getCurrentWebview().onDragDropEvent((event) => {
          const type = event.payload.type;
          if (type === "enter" || type === "over") {
            setDragActive(true);
          } else if (type === "leave") {
            setDragActive(false);
          } else if (type === "drop") {
            setDragActive(false);
            uploadPaths(event.payload.paths);
          }
        });
        if (disposed) off();
        else unlisten = off;
      } catch {
        // 非 Tauri 环境（测试 / 浏览器）：无拖拽事件
      }
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [uploadPaths]);

  const handleSubmit = useCallback(
    (text: string) => {
      const hasImage = attachments.some((a) => a.mimeType.startsWith("image/"));
      if (hasImage && getModelInfo()?.supportsVision !== true) {
        showToast("当前模型不支持图像输入，图片将按文件发送（模型看不到内容）", {
          kind: "warning",
          dedupeKey: "text-model-image-file-only",
        });
      }
      sendMessage(text, attachments);
      setAttachments([]);
    },
    [attachments, sendMessage]
  );

  // ── 插话提交（生成中）：入队成功（返回条目 id）→ 登记 pending → 清输入；
  // 关窗降级（2026-09-25）：轮恰在提交时收束 → 后端拒绝 → 转普通发送
  // （对齐 DSH 尽力而为语义，不打扰用户）；其余失败 toast → 保留输入 ──
  const handleSteer = useCallback(
    async (text: string): Promise<boolean> => {
      // 无活动轮（竞态窗口）：直接降级为普通发送
      if (!processingSessionId || !activeId) {
        await sendMessage(text);
        return true;
      }
      try {
        const id = await invoke<string>("steer_message", {
          sessionId: processingSessionId,
          text,
        });
        useSteerStore.getState().addPending(activeId, {
          id,
          text,
          sessionId: processingSessionId,
        });
        return true;
      } catch (err) {
        const msg = String(err);
        // 判定标记与后端 steer_message 的拒绝文案保持同步
        if (msg.includes("没有处理中的对话")) {
          // 轮收尾事件在途（processing 残留）→ 不清输入、不发送——
          // 防 sendMessage 并发保护吞消息；待模式切回「发送」后由用户提交
          const stillProcessing = Object.values(
            useProcessingStore.getState().processing
          ).some((p) => p.conversationId === activeId);
          if (stillProcessing) return false;
          await sendMessage(text);
          return true;
        }
        showToast(msg, { kind: "warning", dedupeKey: "steer-failed" });
        return false;
      }
    },
    [processingSessionId, activeId, sendMessage]
  );

  // ── 中止（生成中无字方块）：中止不再清插话队列（2026-09-25 保留语义）
  // ——未交付 pending 气泡留存（可点击取消），后端保留、下一轮领取交付；
  // 轮尾 turn/end(aborted) 走现有归档链路。
  // 反馈（2026-10-08）：点击即乐观置「正在中止」（按钮形变 + 输入框提示 + 轮尾
  // 一条），后端报告没有活动轮（返回 false）时撤回——否则界面空等，像出了 bug ──
  const handleStop = useCallback(() => {
    if (!processingSessionId) return;
    const sid = processingSessionId;
    markAborting(sid, true);
    invoke<boolean>("abort_turn", { sessionId: sid })
      .then((found) => {
        if (!found) markAborting(sid, false);
      })
      .catch(() => markAborting(sid, false));
  }, [processingSessionId]);

  // ── pending 气泡点击取消：cancel_steer 成功才移除（消失即反馈，不回填）；
  // 寻址用条目自带的提交轮 sessionId（跨轮保留件为旧轮 id，仍可命中）──
  const handleCancelSteer = useCallback(
    (entryId: string) => {
      if (!activeId) return;
      const list = useSteerStore.getState().pendingByConversation[activeId];
      const entry = list?.find((e) => e.id === entryId);
      if (!entry) return;
      invoke<boolean>("cancel_steer", {
        sessionId: entry.sessionId,
        entryId,
      })
        .then((ok) => {
          if (ok) {
            useSteerStore.getState().removePending(activeId, entryId);
          }
        })
        .catch(() => {});
    },
    [activeId]
  );

  // ── 首次使用 ──
  // 当前没有对话（草稿态：启动后未选、或点了新对话）→ 欢迎界面；
  // restoring 期间抑制（防恢复完成前闪欢迎界面）
  const showWelcome = activeId === null && !restoring;

  // ── 引导（说明层）触发 ──
  // ① 欢迎卡：欢迎页真的渲染 + 启动演出已完全撤下 + 没有别的面板开着 + 键集已拉到。
  //    「由表面真的出现过决定」，不看装完第几次启动。
  //    三步导览不再自动触发，由这张卡上的「开始」启动（Air 2026-10-11）。
  const guideHydrated = useGuideStore((s) => s.hydrated);
  const enqueueGuide = useGuideStore((s) => s.enqueue);
  useEffect(() => {
    if (!guideHydrated || !showWelcome || bootVisible || overlay !== null) return;
    enqueueGuide(["welcome.intro"]);
  }, [guideHydrated, showWelcome, bootVisible, overlay, enqueueGuide]);

  // ② 首次出现型（对话标题 / 上下文进度环）：元素真的在画面上，且本轮没在生成
  //    ——生成中才出现的（新对话的标题占位）要等这一轮结束
  const convTitle = activeConv?.title;
  useEffect(() => {
    if (!guideHydrated || restoring || bootVisible || !chromeVisible) return;
    if (generating || aborting) return;
    if (!convTitle) return; // 标题还没落（后台模型生成中）
    enqueueGuide(["first.conv-title", "first.context-ring"]);
  }, [
    guideHydrated,
    restoring,
    bootVisible,
    chromeVisible,
    generating,
    aborting,
    convTitle,
    enqueueGuide,
  ]);

  return (
    <div className={styles.container}>
      <Toolbar onDragHandle={onDragHandle} showTitle={chromeVisible} />
      <DragOverlay active={dragActive} />
      <div className={styles.messageArea}>
        {/* 恢复中且尚无内容：对话区留白（启动演出覆盖其上）；恢复后内容先挂载、
            在遮盖期间完成首帧与沉底，演出淡出时直接露出 */}
        {restoring && turns.length === 0 ? null : (
          <MessageList
            turns={turns}
            currentEvents={currentEvents}
            showWelcome={showWelcome}
            aborting={aborting}
            compaction={activeCompaction}
            pending={pending}
            onCancelPending={handleCancelSteer}
            conversationId={activeId}
          />
        )}
        {bootVisible && (
          <BootSequence onDone={handleBootDone} onReveal={handleBootReveal} />
        )}
      </div>
      {!showWelcome && <FindBar />}
      <div className={styles.bottomArea}>
        {attachments.length > 0 && (
          <AttachmentStrip files={attachments} onRemove={removeAttachment} />
        )}
        <div className={styles.inputRow}>
          {/* 指示环自己负责揭示：启动演出期间宽 0（与无对话时同态，不占位），
              揭示时靠它自己的 width 过渡把输入框自然地向右缩过去 */}
          <ContextIndicator reveal={chromeVisible} />
          <InputArea
            onSubmit={handleSubmit}
            onSteer={handleSteer}
            onStop={handleStop}
            aborting={aborting}
            generating={generating}
            disabled={hardDisabled}
            hasAttachments={attachments.length > 0}
            onPasteImage={uploadBlob}
          />
        </div>
      </div>
    </div>
  );
}
