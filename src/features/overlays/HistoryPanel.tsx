// src/components/overlay/HistoryPanel.tsx
// 历史对话面板
//
// 职责：展示全部对话列表（按时间倒序），支持切换、重命名和删除。
// 自身从 store 读写，不依赖 props——覆盖层体系中的自包含面板。

import { useEffect, useRef, useState } from "react";
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

  const confirmYesRef = useRef<HTMLButtonElement>(null);
  const editBtnRefs = useRef(new Map<string, HTMLButtonElement>());
  const deleteBtnRefs = useRef(new Map<string, HTMLButtonElement>());

  /*
   * 排序：最新对话排最前。
   * 场景判断——用户打开历史面板最常见的动机是"回到刚才的对话"，
   * 按时间倒序把这个最高频操作放在最省力的位置（列表顶部）。
   */
  const sorted = [...conversations].sort(
    (a, b) => b.createdAt - a.createdAt
  );

  // 进入删除确认态时把焦点送到确认按钮；取消后送回删除按钮。
  useEffect(() => {
    if (deletingId) confirmYesRef.current?.focus();
  }, [deletingId]);

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
   * 删除对话（带确认）：首次点击进入确认态，再次点击 ✓ 才真正删除。
   * 确认态下点击 ✗、Escape 或列表其他区域取消。
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
    const id = deletingId;
    setDeletingId(null);
    if (id) {
      window.setTimeout(() => deleteBtnRefs.current.get(id)?.focus(), 0);
    }
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

  const commitEdit = (restoreFocus = false) => {
    const id = editingId;
    if (id && editValue.trim()) {
      const trimmed = editValue.trim();
      renameConversation(id, trimmed.length > 30 ? trimmed.slice(0, 30) : trimmed);
    }
    setEditingId(null);
    if (restoreFocus && id) {
      window.setTimeout(() => editBtnRefs.current.get(id)?.focus(), 0);
    }
  };

  const cancelEdit = (restoreFocus = false) => {
    const id = editingId;
    setEditingId(null);
    if (restoreFocus && id) {
      window.setTimeout(() => editBtnRefs.current.get(id)?.focus(), 0);
    }
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
          const isEditing = conv.id === editingId;
          const relativeTime = formatRelativeTime(conv.createdAt);

          return (
            <div
              key={conv.id}
              className={`${styles.item} ${
                conv.id === activeId && !isDeleting ? styles.active : ""
              } ${isDeleting ? styles.deleting : ""}`}
              onClick={() => {
                if (isDeleting) {
                  cancelDelete();
                } else if (!isEditing) {
                  handleSelect(conv.id);
                }
              }}
              onKeyDown={(e) => {
                if (
                  isDeleting &&
                  e.key === "Escape" &&
                  !e.nativeEvent.isComposing
                ) {
                  e.preventDefault();
                  e.stopPropagation();
                  cancelDelete();
                }
              }}
            >
              {isDeleting ? (
                /* 删除确认态：替换原标题和时间 */
                <>
                  <span className={styles.confirmText}>确认删除？</span>
                  <div className={styles.confirmActions}>
                    <button
                      ref={confirmYesRef}
                      type="button"
                      className={styles.confirmYes}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleDelete(conv.id);
                      }}
                      title="确认删除"
                      aria-label="确认删除"
                    >
                      <svg viewBox="0 0 12 12" width="12" height="12" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                        <polyline points="2,6 5,9 10,3" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      className={styles.confirmNo}
                      onClick={(e) => {
                        e.stopPropagation();
                        cancelDelete();
                      }}
                      title="取消删除"
                      aria-label="取消删除"
                    >
                      <svg viewBox="0 0 12 12" width="12" height="12" stroke="currentColor" strokeWidth="1" fill="none" strokeLinecap="round" aria-hidden="true">
                        <line x1="3" y1="3" x2="9" y2="9" />
                        <line x1="9" y1="3" x2="3" y2="9" />
                      </svg>
                    </button>
                  </div>
                </>
              ) : (
                <>
                  {isEditing ? (
                    <div className={styles.itemInfo}>
                      <input
                        className={styles.editInput}
                        value={editValue}
                        onChange={(e) => setEditValue(e.target.value)}
                        onBlur={() => commitEdit(false)}
                        onKeyDown={(e) => {
                          if (e.nativeEvent.isComposing) return;
                          if (e.key === "Enter") {
                            e.preventDefault();
                            commitEdit(true);
                          } else if (e.key === "Escape") {
                            e.preventDefault();
                            e.stopPropagation();
                            cancelEdit(true);
                          }
                        }}
                        maxLength={30}
                        autoFocus
                        aria-label="编辑对话标题"
                        onClick={(e) => e.stopPropagation()}
                      />
                      <span className={styles.itemTime}>{relativeTime}</span>
                    </div>
                  ) : (
                    <button
                      type="button"
                      data-focus-inset
                      className={styles.selectBtn}
                      data-overlay-autofocus={
                        conv.id === activeId ? "" : undefined
                      }
                      aria-current={conv.id === activeId ? "true" : undefined}
                      aria-label={`${conv.title}，${relativeTime}`}
                    >
                      <span className={styles.itemInfo}>
                        <span className={styles.itemTitle}>{conv.title}</span>
                        <span className={styles.itemTime}>{relativeTime}</span>
                      </span>
                    </button>
                  )}

                  {!isEditing && (
                    <div className={styles.actions}>
                      <button
                        ref={(el) => {
                          if (el) editBtnRefs.current.set(conv.id, el);
                          else editBtnRefs.current.delete(conv.id);
                        }}
                        type="button"
                        className={styles.editBtn}
                        onClick={(e) => {
                          e.stopPropagation();
                          startEdit(conv.id, conv.title);
                        }}
                        title="编辑标题"
                        aria-label="编辑标题"
                      >
                        ✎
                      </button>
                      <button
                        ref={(el) => {
                          if (el) deleteBtnRefs.current.set(conv.id, el);
                          else deleteBtnRefs.current.delete(conv.id);
                        }}
                        type="button"
                        className={styles.deleteBtn}
                        onClick={(e) => {
                          e.stopPropagation();
                          requestDelete(conv.id);
                        }}
                        title="删除对话"
                        aria-label="删除对话"
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
