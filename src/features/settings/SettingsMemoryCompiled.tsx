import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { useConversationStore } from "../../stores/useConversationStore";
import { Button } from "./ui/Button";
import { SectionNote } from "./ui/SettingsSection";
import {
  MemoryViewFrame,
  LONGTERM_MAX_CHARS,
  charCount,
} from "./memoryViewShared";
import type {
  MemoryRecentItem,
  MemorySetLongtermResult,
  MemoryTidyResult,
} from "../../types";
import styles from "./SettingsMemory.module.css";

// ═══════════════════════════════════════════════════════
// 当前记忆（视图①）
//
// 这里放"PRTS 现在记得什么"：近期经历（只读）+ 长期梗概（可改）。
// 整理也在这里——它会调用一次模型、产生 token 消耗，所以要一次明确确认，
// 而不是摆在主页面上等人手滑。
// ═══════════════════════════════════════════════════════

export function CompiledView({
  recent,
  previous,
  busy,
  onBack,
  onSaved,
}: {
  recent: MemoryRecentItem[];
  previous: string;
  busy: boolean;
  onBack: () => void;
  onSaved: () => void;
}) {
  const activeId = useConversationStore((s) => s.activeId);
  const [preEdit, setPreEdit] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [confirmTidy, setConfirmTidy] = useState(false);
  const [tidying, setTidying] = useState(false);
  const [tidyMsg, setTidyMsg] = useState<string | null>(null);
  const preRef = useRef<HTMLTextAreaElement>(null);

  // 编辑框随内容自增（与阅读态展示框同尺寸；CSS 已去原生缩放柄）
  useEffect(() => {
    const el = preRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight + 2}px`;
  }, [preEdit]);

  const savePre = async () => {
    if (preEdit === null) return;
    setSaving(true);
    try {
      const r = await invoke<MemorySetLongtermResult>("memory_set_longterm", {
        text: preEdit,
      });
      if (r.changed) {
        showToast(`长期记忆已重写（版本 ${r.version}，${r.chars} 字）`, {
          kind: "info",
          dedupeKey: "memory-pre-save",
        });
        onSaved();
      } else {
        showToast("无改动，未重写", { kind: "info", dedupeKey: "memory-pre-save" });
      }
      setPreEdit(null);
    } catch (e) {
      showToast(`保存失败：${String(e)}`, { kind: "warning" });
    } finally {
      setSaving(false);
    }
  };

  const tidy = async () => {
    if (busy) {
      showToast("对话处理中不能修改记忆", {
        kind: "warning",
        dedupeKey: "memory-busy",
      });
      return;
    }
    if (!activeId) {
      showToast("当前没有打开的对话，没有可整理的记忆", {
        kind: "warning",
        dedupeKey: "memory-tidy-no-conversation",
      });
      setConfirmTidy(false);
      return;
    }
    setConfirmTidy(false);
    setTidying(true);
    setTidyMsg(null);
    try {
      const r = await invoke<MemoryTidyResult>("memory_tidy", {
        conversationId: activeId,
      });
      setTidyMsg(r.message);
      if (r.status !== "failed") onSaved();
    } catch (e) {
      setTidyMsg(`整理失败：${String(e)}`);
    } finally {
      setTidying(false);
    }
  };

  return (
    <MemoryViewFrame title="当前记忆" onBack={onBack}>
      <div className={styles.viewCaption}>近期经历</div>
      {recent.length === 0 ? (
        <span className={styles.emptyHint}>（暂无）</span>
      ) : (
        <div className={styles.recentList}>
          {recent.map((r, i) => (
            <div key={`${r.date}-${i}`} className={styles.recentItem}>
              <div className={styles.recentDate}>{r.date}</div>
              <div>
                {r.title}：{r.summary}
              </div>
            </div>
          ))}
        </div>
      )}
      <span className={styles.fieldHint}>
        最多列出最近 5 条：这是窗口上限，不代表"近期只有这些"；更早的都收在「所有记忆」里。
      </span>

      <div className={styles.viewCaption}>长期梗概</div>
      {preEdit === null ? (
        <>
          {previous.trim() ? (
            <div className={styles.viewText}>{previous}</div>
          ) : (
            <span className={styles.emptyHint}>（暂无）</span>
          )}
          <div className={styles.actions}>
            <Button
              onClick={() => setPreEdit(previous)}
              disabled={busy || saving}
            >
              编辑
            </Button>
          </div>
        </>
      ) : (
        <>
          <textarea
            className={styles.textarea}
            ref={preRef}
            value={preEdit}
            onChange={(e) => setPreEdit(e.target.value)}
            data-pre-edit
          />
          <div className={styles.formMeta}>
            {charCount(preEdit)} 字 / 上限 {LONGTERM_MAX_CHARS} 字（超出会被拒绝）。保存后对新对话生效。
          </div>
          <div className={styles.actions}>
            <Button
              variant="primary"
              onClick={() => void savePre()}
              disabled={busy || saving}
            >
              {saving ? "保存中…" : "保存"}
            </Button>
            <Button onClick={() => setPreEdit(null)}>取消</Button>
          </div>
        </>
      )}

      {/* 整理：调模型、烧 token，必须自己占一行 + 一次确认 */}
      <div className={styles.actionList}>
        <div className={styles.actionRow}>
          <div className={styles.actionText}>
            <span className={styles.actionLabel}>整理记忆</span>
            <span className={styles.actionHint}>
              把当前对话重新归纳成记忆条目与长期梗概
            </span>
          </div>
          <div className={styles.actions}>
            <Button
              aria-label="整理当下记忆"
              title="会调用一次模型，消耗额外 token"
              onClick={() => setConfirmTidy(true)}
              disabled={busy || tidying}
            >
              {tidying ? "整理中…" : "整理"}
            </Button>
          </div>
        </div>
      </div>

      {confirmTidy && (
        <div
          className={styles.confirmBar}
          role="group"
          aria-label="确认整理当前记忆"
        >
          <span className={styles.confirmText}>
            整理会调用一次模型，把当前对话重新归纳成记忆条目与长期梗概，
            <strong>会消耗额外 token</strong>；归纳结果会直接写入记忆。继续吗？
          </span>
          <div className={styles.actions}>
            <Button onClick={() => setConfirmTidy(false)}>取消</Button>
            <Button variant="primary" onClick={() => void tidy()}>
              开始整理
            </Button>
          </div>
        </div>
      )}

      {tidyMsg && <p className={styles.tidyMsg}>{tidyMsg}</p>}

      <SectionNote>
        「近期经历」只读；「长期梗概」可编辑，保存后对新对话生效。
      </SectionNote>
    </MemoryViewFrame>
  );
}
