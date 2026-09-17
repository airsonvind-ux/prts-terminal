// src/components/overlay/HistoryPanel.tsx
// 历史对话面板
//
// 职责：展示全部对话列表（按时间倒序），支持切换和删除。
// 自身从 store 读写，不依赖 props——覆盖层体系中的自包含面板。

import { useState } from "react";
import { useConversationStore } from "../../stores/useConversationStore";
import { useUIStore } from "../../stores/useUIStore";
import styles from "./HistoryPanel.module.css";

export function HistoryPanel() {
  const conversations = useConversationStore((s) => s.conversations);
  const activeId = useConversationStore((s) => s.activeId);
  const switchConversation = useConversationStore((s) => s.switchConversation);
  const deleteConversation = useConversationStore((s) => s.deleteConversation);
  const renameConversation = useConversationStore((s) => s.renameConversation);
  const setOverlay = useUIStore((s) => s.setOverlay);

  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [deletingId, setDeletingId] = useState<string | null>(null);

  /*
   * 排序：最新对话排最前。
   * 场景判断——用户打开历史面板最常见的动机是"回到刚才的对话"，
   * 按时间倒序把这个最高频操作放在最省力的位置（列表顶部）。
   */
  const sorted = [...conversations].sort(
    (a, b) => b.createdAt - a.createdAt
  );

  /*
   * 选择对话：切换 activeId 后立即关闭覆盖层。
   *
   * 流程示例：用户当前在"1-7关卡攻略"对话中，点 ◷ 打开历史，
   * 看到列表中有"源石虫数值查询"，点击它 →
   *   switchConversation 将 activeId 从 "对话A" 切到 "对话B"
   *   setOverlay(null) 关闭覆盖层
   *   ChatView 重渲染：Toolbar 标题更新、MessageList 显示对话B的消息
   *
   * 为什么不保持覆盖层打开？因为"选择"是一个完整的操作闭环——用户选完就想看到结果。
   * 如果还需要手动关闭，等于每个选择操作多一次点击。
   */
  const handleSelect = (id: string) => {
    switchConversation(id);
    setOverlay(null);
  };

  /*
   * 删除对话：移除对话记录后关闭覆盖层。
   *
   * 如果删除的是当前活跃对话，useConversationStore 内部会将 activeId 置为 null，
   * ChatView 自动退回欢迎界面。
   *
   * stopPropagation 阻止事件冒泡到父级 div（否则会同时触发 handleSelect 和 handleDelete）。
   */
  /*
   * 删除对话（带确认）：首次点击进入确认态，再次点击 ✓ 才真正删除。
   * 确认态下点击 ✗ 或列表其他区域取消。
   */
  const handleDelete = (id: string) => {
    deleteConversation(id);
    setOverlay(null);
    setDeletingId(null);
  };

  const requestDelete = (id: string) => {
    setDeletingId(id);
    setEditingId(null); // 关闭可能同时打开的编辑态
  };

  const cancelDelete = () => {
    setDeletingId(null);
  };

  /*
   * 标题编辑：点击编辑按钮进入编辑模式，Enter/失焦保存，Escape 取消。
   * 标题文字本身不挂载点击事件——编辑操作通过独立的编辑按钮触发，
   * 避免与"点击整行切换对话"的交互冲突。
   */
  const startEdit = (id: string, title: string) => {
    setEditingId(id);
    setEditValue(title);
    setDeletingId(null);
  };

  const commitEdit = () => {
    if (editingId && editValue.trim()) {
      const trimmed = editValue.trim();
      renameConversation(editingId, trimmed.length > 30 ? trimmed.slice(0, 30) : trimmed);
    }
    setEditingId(null);
  };

  const cancelEdit = () => {
    setEditingId(null);
  };

  // 空状态：没有任何对话记录
  if (sorted.length === 0) {
    return (
      <div className={styles.empty}>
        <p className={styles.emptyText}>暂无历史对话</p>
        <p className={styles.emptyHint}>发送第一条消息后将自动保存</p>
      </div>
    );
  }

  return (
    <>
      <div className={styles.header}>
        <span className={styles.headerTitle}>历史对话</span>
        <span className={styles.headerCount}>{sorted.length} 条</span>
      </div>
      <div className={styles.list}>
        {sorted.map((conv) => {
          const isDeleting = conv.id === deletingId;

          return (
            <div
              key={conv.id}
              className={`${styles.item} ${
                conv.id === activeId && !isDeleting ? styles.active : ""
              } ${isDeleting ? styles.deleting : ""}`}
              onClick={() => {
                if (isDeleting) {
                  cancelDelete();
                } else {
                  handleSelect(conv.id);
                }
              }}
            >
              {isDeleting ? (
                /* 删除确认态：替换原标题和时间 */
                <>
                  <span className={styles.confirmText}>确认删除？</span>
                  <div className={styles.confirmActions}>
                    <button
                      className={styles.confirmYes}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleDelete(conv.id);
                      }}
                      title="确认删除"
                    >
                      <svg viewBox="0 0 12 12" width="12" height="12" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="2,6 5,9 10,3" />
                      </svg>
                    </button>
                    <button
                      className={styles.confirmNo}
                      onClick={(e) => {
                        e.stopPropagation();
                        cancelDelete();
                      }}
                      title="取消删除"
                    >
                      <svg viewBox="0 0 12 12" width="12" height="12" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round">
                        <line x1="3" y1="3" x2="9" y2="9" />
                        <line x1="9" y1="3" x2="3" y2="9" />
                      </svg>
                    </button>
                  </div>
                </>
              ) : (
                <>
                  <div className={styles.itemInfo}>
                    {conv.id === editingId ? (
                      <input
                        className={styles.editInput}
                        value={editValue}
                        onChange={(e) => setEditValue(e.target.value)}
                        onBlur={commitEdit}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") commitEdit();
                          if (e.key === "Escape") cancelEdit();
                        }}
                        maxLength={30}
                        autoFocus
                        onClick={(e) => e.stopPropagation()}
                      />
                    ) : (
                      <span className={styles.itemTitle}>{conv.title}</span>
                    )}
                    <span className={styles.itemTime}>{formatRelativeTime(conv.createdAt)}</span>
                  </div>

                  {conv.id !== editingId && (
                    <div className={styles.actions}>
                      <button
                        className={styles.editBtn}
                        onClick={(e) => {
                          e.stopPropagation();
                          startEdit(conv.id, conv.title);
                        }}
                        title="编辑标题"
                      >
                        ✎
                      </button>
                      <button
                        className={styles.deleteBtn}
                        onClick={(e) => {
                          e.stopPropagation();
                          requestDelete(conv.id);
                        }}
                        title="删除对话"
                      >
                        ×
                      </button>
                    </div>
                  )}
                </>
              )}
            </div>
          );
        })}
      </div>
    </>
  );
}

// ── 工具函数 ──

/**
 * 将时间戳转为相对时间描述。
 *
 * 粒度阶梯：
 *   1 分钟内       → "刚刚"
 *   1 小时以内     → "N 分钟前"
 *   24 小时以内    → "N 小时前"
 *   48 小时以内    → "昨天"
 *   7 天以内       → "N 天前"
 *   超过 7 天      → "M/D"（如 "6/28"）
 *
 * 不使用"年/月/日 时:分"完整格式的原因：
 * 悬浮窗空间有限（面板宽度仅 216px），长日期字符串会挤压标题空间。
 * 超过一周的对话，精确到"哪天"已经足够——你不会在意三天前的对话是上午还是下午聊的。
 */
function formatRelativeTime(timestamp: number): string {
  const now = Date.now();
  const diff = now - timestamp;

  const minutes = Math.floor(diff / 60_000);
  const hours = Math.floor(diff / 3_600_000);
  const days = Math.floor(diff / 86_400_000);

  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes}分钟前`;
  if (hours < 24) return `${hours}小时前`;
  if (days === 1) return "昨天";
  if (days < 7) return `${days}天前`;

  const date = new Date(timestamp);
  return `${date.getMonth() + 1}/${date.getDate()}`;
}
