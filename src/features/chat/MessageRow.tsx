import { memo, useState } from "react";
import type { DisplayItem } from "./messageItems";
import { AttachmentStrip } from "./AttachmentStrip";
import { EventBadge } from "./EventBadge";
import { Markdown } from "./Markdown";
import { WelcomeMark } from "./WelcomeMark";
import styles from "./MessageList.module.css";

/**
 * 压缩点提示卡（模块 D · 上下文衰减）。
 *
 * 本地条目，非 LLM 消息：turns 全量保留（回看自由），本卡标记「从这一位置起
 * 之前的对话已压缩归档」——压缩点之后为模型当前可见的保留对话。
 * 展开可读压缩摘要（LLM checkpoint 正文）。
 */
function CompactionNotice({
  foldedCount,
  summary,
}: {
  foldedCount: number;
  summary: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={styles.compactionCard} data-compaction-card>
      <button
        type="button"
        className={styles.compactionHeader}
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span>上下文已压缩 · {foldedCount} 轮并入摘要</span>
        <span className={styles.compactionChevron} aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      <div className={styles.compactionDesc}>
        被压缩的对话已并入上方摘要，不再进入模型上下文（原历史保留，仅供回看）；
        从下一条消息起为当前对话。
      </div>
      {open && (
        <pre className={styles.compactionSummary} data-compaction-summary>
          {summary}
        </pre>
      )}
    </div>
  );
}

/** 首部连续错误轮的折叠记录（不删除数据，只是不再占据对话首屏）。 */
function PreflightNotice({
  count,
  errors,
}: {
  count: number;
  errors: string[];
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={styles.preflightCard} data-preflight-card>
      <button
        type="button"
        className={styles.preflightHeader}
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span>启动阶段失败 {count} 次 · 已恢复</span>
        <span className={styles.compactionChevron} aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div className={styles.preflightErrors}>
          {errors.map((message, index) => (
            <div key={index}>{message}</div>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * 一行展示项（消息气泡 / 事件卡 / pending / 压缩卡）。
 *
 * memo 的前提是归档 item 引用跨刷新稳定（buildArchivedItems 按 turns 缓存 +
 * 单轮投影缓存）：流式刷新时归档行整段跳过重渲染，只有当前轮的行会更新。
 */
export const MessageRow = memo(function MessageRow({
  item,
  onCancelPending,
}: {
  item: DisplayItem;
  onCancelPending?: (entryId: string) => void;
}) {
  switch (item.type) {
    case "welcome":
      return (
        <div className={styles.welcome}>
          <WelcomeMark />
        </div>
      );
    case "events":
      return (
        <EventBadge
          events={item.events}
          interactive={item.interactive}
          showApprovalControls={false}
          className={styles.row}
        />
      );
    case "message":
      if (item.role === "assistant") {
        return (
          <div
            data-message-area
            className={`${styles.bubble} ${styles.assistant} ${styles.row}`}
          >
            {item.segments.map((seg, segIdx) => (
              <div
                key={segIdx}
                className={seg.transition ? styles.streaming : undefined}
              >
                <Markdown content={seg.content} />
                {item.isStreaming && segIdx === item.segments.length - 1 && (
                  <span className={styles.cursor} />
                )}
              </div>
            ))}
          </div>
        );
      }
      // M3-B：轮中止标记条（灰/斜体小字，独立样式——不走 error 分支；
      // 「正在中止…」与「该轮已中止」同一套样式，前者是过程、后者是结果）
      if (item.role === "aborted" || item.role === "aborting") {
        return (
          <div
            data-message-area
            className={`${styles.bubble} ${styles.aborted} ${styles.row}`}
          >
            {item.content}
          </div>
        );
      }
      return (
        <div
          data-message-area
          className={`${styles.bubble} ${styles[item.role]} ${styles.row}`}
        >
          {item.role === "user" && item.attachments && item.attachments.length > 0 && (
            <AttachmentStrip files={item.attachments} variant="message" />
          )}
          <Markdown content={item.content} />
        </div>
      );
    case "pending":
      return (
        <button
          type="button"
          className={`${styles.steerPending} ${styles.row}`}
          title="点击取消"
          data-steer-pending
          onClick={() => onCancelPending?.(item.id)}
        >
          {item.text}
        </button>
      );
    case "preflight":
      return <PreflightNotice count={item.count} errors={item.errors} />;
    case "compaction":
      return (
        <CompactionNotice
          foldedCount={item.foldedCount}
          summary={item.summary}
        />
      );
  }
});
