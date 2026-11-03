import { useEffect, useMemo, useRef, useState } from "react";
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
// 回答一个问题："PRTS 现在记得什么？"
//  - 近期经历：最近 5 条，与「记忆库」同一套条目卡，日期成组；
//  - 长期梗概：默认只给四行预览（长文不用把整页撑满），编辑是显式的一次切换；
//  - 整理：调模型、烧 token，独占一块并要一次确认。
// ═══════════════════════════════════════════════════════

/** 预览超过这个长度才给「展开全文」——短文本不值得多一次点击 */
const PREVIEW_CHARS = 180;

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
  const [expanded, setExpanded] = useState(false);
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

  // 日期分组，组内保持后端顺序（新在前）
  const groups = useMemo(() => {
    const map = new Map<string, MemoryRecentItem[]>();
    for (const r of recent) {
      const arr = map.get(r.date);
      if (arr) arr.push(r);
      else map.set(r.date, [r]);
    }
    return [...map.entries()];
  }, [recent]);

  const longTerm = charCount(previous) > PREVIEW_CHARS;

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
      setExpanded(false);
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
      {/* ── 近期经历 ── */}
      <section className={styles.block}>
        <div className={styles.blockHead}>
          <span className={styles.blockTitle}>近期经历</span>
          <span className={styles.blockSpacer} />
          <span className={styles.blockMeta}>{recent.length} 条</span>
        </div>

        {recent.length === 0 ? (
          <span className={styles.emptyHint}>（暂无）</span>
        ) : (
          groups.map(([date, list]) => (
            <div key={date} className={styles.dateGroup}>
              <div className={styles.dateLabel}>
                {date}
                <span className={styles.dateCount}>{list.length} 条</span>
              </div>
              {list.map((r, i) => (
                <div key={`${r.date}-${i}`} className={styles.entry}>
                  <div className={styles.entryHeadStatic}>
                    <span className={styles.recentTitle}>
                      {r.title}：{r.summary}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          ))
        )}

        <span className={styles.fieldHint}>
          最多列出最近 5 条：这是窗口上限，不代表"近期只有这些"；更早的都收在「所有记忆」里。
        </span>
      </section>

      {/* ── 长期梗概 ── */}
      <section className={styles.block}>
        <div className={styles.blockHead}>
          <span className={styles.blockTitle}>长期梗概</span>
          <span className={styles.blockSpacer} />
          <span className={styles.blockMeta}>{charCount(previous)} 字</span>
          {preEdit === null && (
            <Button
              className={styles.blockButton}
              onClick={() => setPreEdit(previous)}
              disabled={busy || saving}
            >
              编辑
            </Button>
          )}
        </div>

        {preEdit === null ? (
          <>
            {previous.trim() ? (
              <div
                className={`${styles.prePreview} ${
                  expanded ? styles.prePreviewOpen : ""
                }`}
              >
                {previous}
              </div>
            ) : (
              <span className={styles.emptyHint}>（暂无）</span>
            )}
            {longTerm && (
              <button
                type="button"
                className={styles.linkButton}
                onClick={() => setExpanded((v) => !v)}
              >
                {expanded ? "收起" : "展开全文"}
              </button>
            )}
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
            <div className={styles.blockActions}>
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
      </section>

      {/* ── 整理记忆 ── */}
      <section className={styles.block}>
        <div className={styles.blockHead}>
          <span className={styles.blockTitle}>整理记忆</span>
          <span className={styles.blockSpacer} />
          <Button
            aria-label="整理当下记忆"
            title="会调用一次模型，消耗额外 token"
            onClick={() => setConfirmTidy(true)}
            disabled={busy || tidying}
          >
            {tidying ? "整理中…" : "整理"}
          </Button>
        </div>
        <span className={styles.blockHint}>
          把当前对话重新归纳成记忆条目与长期梗概
        </span>

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
            <div className={styles.blockActions}>
              <Button onClick={() => setConfirmTidy(false)}>取消</Button>
              <Button variant="primary" onClick={() => void tidy()}>
                开始整理
              </Button>
            </div>
          </div>
        )}

        {tidyMsg && <p className={styles.tidyMsg}>{tidyMsg}</p>}
      </section>

      <SectionNote>
        「近期经历」只读；「长期梗概」可编辑，保存后对新对话生效。
      </SectionNote>
    </MemoryViewFrame>
  );
}
