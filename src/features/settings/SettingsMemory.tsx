// 设置页 · "记忆"子页（2026-09 结构定稿）
//
// 结构：
//  - 成长：专题页列表（关于博士居首）+ [新建专题]；点击条目 → 二级视图（阅读/编辑）。
//  - 经历：[查看当下记忆]（视图①：近期只读＋先前可编辑）／[整理当下记忆]；
//    「所有记忆」子块：[查看记忆]（视图②：全部条目按日期编排）／[清除记忆]（二次确认）。
//
// 二级视图＝设置内容区内的替换视图（见 SettingsMemoryViews.tsx），不用整窗浮层。
// 写类操作（新建/编辑/删除/整理/清除/长期记忆保存）在对话进行中禁用（沿用设置锁
// 的前端表现）；查看/读取不受限。

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SettingsSection, SectionNote, SectionWarning } from "./ui/SettingsSection";
import { showToast } from "../../stores/useToastStore";
import { useConversationStore } from "../../stores/useConversationStore";
import type { MemoryOverview, MemoryTidyResult } from "../../types";
import {
  CompiledView,
  GrowthView,
  LibraryView,
  type MemView,
} from "./SettingsMemoryViews";
import { useConfirmFocus } from "./useConfirmFocus";
import styles from "./SettingsMemory.module.css";

export interface SettingsMemoryProps {
  /** 对话处理中（任一对话）：写类操作禁用 */
  busy: boolean;
}

export function SettingsMemory({ busy }: SettingsMemoryProps) {
  const activeId = useConversationStore((s) => s.activeId);
  const [overview, setOverview] = useState<MemoryOverview | null>(null);
  const [loadErr, setLoadErr] = useState(false);
  const [view, setView] = useState<MemView>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [tidying, setTidying] = useState(false);
  const [tidyMsg, setTidyMsg] = useState<string | null>(null);
  const { modalRef, handleKeyDown } = useConfirmFocus(confirmClear, () =>
    setConfirmClear(false)
  );

  const load = useCallback(async () => {
    try {
      const o = await invoke<MemoryOverview>("memory_overview");
      setOverview(o);
      setLoadErr(false);
    } catch (e) {
      console.error("[memory] overview 失败", e);
      setLoadErr(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 写类操作守卫：对话处理中拒绝（沿用设置锁前端表现） */
  const guard = (): boolean => {
    if (busy) {
      showToast("对话处理中不能修改记忆", {
        kind: "warning",
        dedupeKey: "memory-busy",
      });
      return false;
    }
    return true;
  };

  const tidy = async () => {
    if (!guard()) return;
    if (!activeId) {
      showToast("当前没有打开的对话", { kind: "warning" });
      return;
    }
    setTidying(true);
    setTidyMsg(null);
    try {
      const r = await invoke<MemoryTidyResult>("memory_tidy", {
        conversationId: activeId,
      });
      setTidyMsg(r.message);
      if (r.status !== "failed") await load();
    } catch (e) {
      setTidyMsg(`整理失败：${String(e)}`);
    } finally {
      setTidying(false);
    }
  };

  const clearMemory = async () => {
    if (!guard()) return;
    try {
      await invoke("memory_experience_clear");
      showToast("已清除经历记忆", { kind: "info", dedupeKey: "memory-clear" });
      setConfirmClear(false);
      await load();
    } catch (e) {
      showToast(`清除失败：${String(e)}`, { kind: "warning" });
    }
  };

  if (loadErr) {
    return <span className={styles.emptyHint}>记忆加载失败，请重启后重试</span>;
  }
  if (!overview) {
    return <span className={styles.emptyHint}>加载中…</span>;
  }

  const closeView = () => setView(null);

  return (
    <div className={styles.container}>
      {busy && (
        <SectionWarning>对话处理中：写操作暂不可用，查看不受限</SectionWarning>
      )}

      {view ? (
        view.kind === "growth" || view.kind === "growthNew" ? (
          <GrowthView
            name={view.kind === "growth" ? view.name : undefined}
            busy={busy}
            onBack={closeView}
            onSaved={() => void load()}
          />
        ) : view.kind === "compiled" ? (
          <CompiledView
            recent={overview.recent}
            previous={overview.previous}
            busy={busy}
            onBack={closeView}
            onSaved={() => void load()}
          />
        ) : (
          <LibraryView onBack={closeView} />
        )
      ) : (
        <>
          {/* ── 成长 ── */}
          <SettingsSection title="成长">
            <div className={styles.list}>
              {overview.growth.map((g) => (
                <button
                  key={g.name}
                  type="button"
                  data-focus-inset
                  className={styles.row}
                  onClick={() => setView({ kind: "growth", name: g.name })}
                  title={g.brief || undefined}
                >
                  <span className={styles.rowName}>{g.name}</span>
                  {g.brief && <span className={styles.rowBrief}>{g.brief}</span>}
                </button>
              ))}
            </div>
            <div className={styles.actions}>
              <button
                className={styles.btn}
                onClick={() => {
                  if (guard()) setView({ kind: "growthNew" });
                }}
                disabled={busy}
              >
                新建专题
              </button>
            </div>
          </SettingsSection>

          {/* ── 经历 ── */}
          <SettingsSection title="经历">
            <div className={styles.actions}>
              <button
                className={styles.btn}
                onClick={() => setView({ kind: "compiled" })}
              >
                查看当下记忆
              </button>
              <button
                className={styles.btn}
                onClick={() => void tidy()}
                disabled={busy || tidying}
              >
                {tidying ? "整理中…" : "整理当下记忆"}
              </button>
            </div>
            {tidyMsg && <p className={styles.tidyMsg}>{tidyMsg}</p>}

            <div className={styles.subTitle}>所有记忆</div>
            <div className={styles.actions}>
              <button
                className={styles.btn}
                onClick={() => setView({ kind: "library" })}
              >
                查看记忆
              </button>
              <button
                className={`${styles.btn} ${styles.btnDanger}`}
                onClick={() => {
                  if (guard()) setConfirmClear(true);
                }}
                disabled={busy}
              >
                清除记忆
              </button>
            </div>

            <SectionNote>
              经历记忆由后台自动维护；「整理当下记忆」对当前对话立即整理一次。
            </SectionNote>
          </SettingsSection>
        </>
      )}

      {/* ── 清除确认浮层 ── */}
      {confirmClear && (
        <div className={styles.backdrop} onClick={() => setConfirmClear(false)}>
          <div
            ref={modalRef}
            className={styles.modal}
            role="dialog"
            aria-modal="true"
            aria-labelledby="memory-clear-title"
            tabIndex={-1}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={handleKeyDown}
          >
            <div className={styles.modalHeader}>
              <span id="memory-clear-title">清除经历记忆</span>
            </div>
            <div className={styles.modalBody}>
              <SectionNote>
                将清除全部经历记忆：经历条目与长期梗概都会清空；成长页（关于博士与专题）不受影响。此操作不可撤销。
              </SectionNote>
            </div>
            <div className={styles.modalFooter}>
              <button
                type="button"
                className={styles.btn}
                data-confirm-cancel
                onClick={() => setConfirmClear(false)}
              >
                取消
              </button>
              <button
                className={`${styles.btn} ${styles.btnDanger}`}
                onClick={() => void clearMemory()}
              >
                确认清除
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
