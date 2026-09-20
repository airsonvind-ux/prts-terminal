// 设置页 · "记忆"子页（Phase 4 任务二 · 批 3，初步）
//
// 两块：
//  - 成长：列表（「关于博士」置顶 + 专题页 + 新建页）；点开单条 → 页内展开查看/
//    编辑（内嵌编辑走命令层，updatedAt/提醒/锁全在）；单条删除（「关于博士」＝复位）。
//  - 经历：当前注入（「近期」+「先前」，与提示词渲染同源）／整理当下记忆／
//    记忆库（查看全部条目 + 清除记忆二次确认）。
//
// 写类操作（新建/编辑/删除/整理/清除）在对话进行中禁用（沿用设置锁前端表现）；
// 查看/读取不受限。界面遵循既有体系与尺寸约束。

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SettingsSection, SectionNote } from "./ui/SettingsSection";
import { showToast } from "../../stores/useToastStore";
import { useConversationStore } from "../../stores/useConversationStore";
import type {
  MemoryDetail,
  MemoryExperienceItem,
  MemoryOverview,
  MemorySetLongtermResult,
  MemoryTidyResult,
} from "../../types";
import styles from "./SettingsMemory.module.css";

/** 「先前」（长期记忆）字数上限：与后端硬闸同源（orchestrator::memory::MAX_LONGTERM_CHARS） */
const LONGTERM_MAX_CHARS = 600;

/** Unicode 码点数（对齐后端 chars().count()；不用 String.length） */
function charCount(s: string): number {
  return [...s].length;
}

export interface SettingsMemoryProps {
  /** 对话处理中（任一对话）：写类操作禁用 */
  busy: boolean;
}

interface EditState {
  mode: "create" | "edit";
  name: string;
  brief: string;
  body: string;
  tagsText: string;
}

/** 标签文本 → 数组（、/逗号分隔） */
function parseTags(text: string): string[] {
  return text
    .split(/[、,，]/)
    .map((t) => t.trim())
    .filter((t) => t.length > 0);
}

export function SettingsMemory({ busy }: SettingsMemoryProps) {
  const activeId = useConversationStore((s) => s.activeId);
  const [overview, setOverview] = useState<MemoryOverview | null>(null);
  const [loadErr, setLoadErr] = useState(false);
  const [edit, setEdit] = useState<EditState | null>(null);
  const [saving, setSaving] = useState(false);

  const [viewer, setViewer] = useState<MemoryExperienceItem[] | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);

  const [confirmClear, setConfirmClear] = useState(false);
  const [tidying, setTidying] = useState(false);
  const [tidyMsg, setTidyMsg] = useState<string | null>(null);
  /** 「先前」（长期记忆）编辑中：非 null = 编辑态（值为草稿） */
  const [preEdit, setPreEdit] = useState<string | null>(null);
  const [savingPre, setSavingPre] = useState(false);

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

  const openEdit = async (name: string) => {
    try {
      const d = await invoke<MemoryDetail>("memory_get", { kind: "growth", name });
      const p = d.page;
      if (!p) return;
      setEdit({
        mode: "edit",
        name: p.name,
        brief: p.brief,
        body: p.body,
        tagsText: p.tags.join("、"),
      });
    } catch (e) {
      showToast(`读取失败：${String(e)}`, { kind: "warning" });
    }
  };

  const startCreate = () => {
    if (!guard()) return;
    setEdit({ mode: "create", name: "", brief: "", body: "", tagsText: "" });
  };

  /** 打开「先前」（长期记忆）编辑：预填现正文 */
  const openPreEdit = () => {
    if (!guard()) return;
    setPreEdit(overview?.previous ?? "");
  };

  /** 保存长期记忆（人工纠偏入口：不经模型，直接覆盖） */
  const savePre = async () => {
    if (preEdit === null || !guard()) return;
    setSavingPre(true);
    try {
      const r = await invoke<MemorySetLongtermResult>("memory_set_longterm", {
        text: preEdit,
      });
      showToast(`长期记忆已重写（版本 ${r.version}，${r.chars} 字）`, {
        kind: "info",
        dedupeKey: "memory-pre-save",
      });
      setPreEdit(null);
      await load();
    } catch (e) {
      showToast(`保存失败：${String(e)}`, { kind: "warning" });
    } finally {
      setSavingPre(false);
    }
  };

  const save = async () => {
    if (!edit || !guard()) return;
    const name = edit.name.trim();
    if (!name) {
      showToast("页名不能为空", { kind: "warning" });
      return;
    }
    setSaving(true);
    try {
      const tags = parseTags(edit.tagsText);
      if (edit.mode === "create") {
        await invoke("memory_create", { name, brief: edit.brief, body: edit.body, tags });
      } else {
        await invoke("memory_update", { name, brief: edit.brief, body: edit.body, tags });
      }
      showToast(edit.mode === "create" ? `已新建「${name}」` : `已保存「${name}」`, {
        kind: "info",
        dedupeKey: "memory-save",
      });
      setEdit(null);
      await load();
    } catch (e) {
      showToast(`保存失败：${String(e)}`, { kind: "warning" });
    } finally {
      setSaving(false);
    }
  };

  const del = async (name: string) => {
    if (!guard()) return;
    try {
      await invoke("memory_delete", { name });
      showToast(`已删除「${name}」`, { kind: "info", dedupeKey: "memory-del" });
      setEdit(null);
      await load();
    } catch (e) {
      showToast(`删除失败：${String(e)}`, { kind: "warning" });
    }
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
      const r = await invoke<MemoryTidyResult>("memory_tidy", { conversationId: activeId });
      setTidyMsg(r.message);
      if (r.status !== "failed") await load();
    } catch (e) {
      setTidyMsg(`整理失败：${String(e)}`);
    } finally {
      setTidying(false);
    }
  };

  const openViewer = async () => {
    try {
      setViewer(await invoke<MemoryExperienceItem[]>("memory_experience_list"));
    } catch (e) {
      showToast(`读取失败：${String(e)}`, { kind: "warning" });
    }
  };

  const toggleExpand = (id: string) => {
    setExpanded((cur) => (cur === id ? null : id));
  };

  const clearMemory = async () => {
    if (!guard()) return;
    try {
      await invoke("memory_experience_clear");
      showToast("已清除经历记忆", { kind: "info", dedupeKey: "memory-clear" });
      setConfirmClear(false);
      setViewer(null);
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

  return (
    <div className={styles.container}>
      {busy && <p className={styles.busyHint}>对话处理中：写操作暂不可用，查看不受限</p>}

      {/* ── 成长 ── */}
      <SettingsSection title="成长">
        <div className={styles.list}>
          {overview.growth.map((g) => (
            <button
              key={g.name}
              className={`${styles.row} ${edit?.mode === "edit" && edit.name === g.name ? styles.rowActive : ""}`}
              onClick={() => void openEdit(g.name)}
              title={g.brief || undefined}
            >
              <span className={styles.rowName}>{g.name}</span>
              {g.brief && <span className={styles.rowBrief}>{g.brief}</span>}
            </button>
          ))}
        </div>
        <div className={styles.actions}>
          <button className={styles.btn} onClick={startCreate} disabled={busy}>
            新建页
          </button>
        </div>

        {edit && (
          <div className={styles.form}>
            <label className={styles.fieldLabel}>页名</label>
            <input
              className={styles.input}
              value={edit.name}
              disabled={edit.mode === "edit"}
              placeholder="如：轴-推演 / 博士偏好"
              onChange={(e) => setEdit({ ...edit, name: e.target.value })}
            />
            <label className={styles.fieldLabel}>简述</label>
            <input
              className={styles.input}
              value={edit.brief}
              placeholder="一句话简述（进主题目录）"
              onChange={(e) => setEdit({ ...edit, brief: e.target.value })}
            />
            <label className={styles.fieldLabel}>正文</label>
            <textarea
              className={styles.textarea}
              value={edit.body}
              onChange={(e) => setEdit({ ...edit, body: e.target.value })}
            />
            <label className={styles.fieldLabel}>标签</label>
            <input
              className={styles.input}
              value={edit.tagsText}
              placeholder="用、或逗号分隔，可留空"
              onChange={(e) => setEdit({ ...edit, tagsText: e.target.value })}
            />
            <div className={styles.formMeta}>创建 / 更新时间与来源由系统记录。</div>
            <div className={styles.actions}>
              <button className={`${styles.btn} ${styles.btnPrimary}`} onClick={() => void save()} disabled={busy || saving}>
                {saving ? "保存中…" : "保存"}
              </button>
              <button className={styles.btn} onClick={() => setEdit(null)}>
                取消
              </button>
              {edit.mode === "edit" && (
                <button
                  className={`${styles.btn} ${styles.btnDanger}`}
                  onClick={() => void del(edit.name)}
                  disabled={busy}
                >
                  {edit.name === "关于博士" ? "复位" : "删除"}
                </button>
              )}
            </div>
          </div>
        )}
      </SettingsSection>

      {/* ── 经历 ── */}
      <SettingsSection title="经历">
        <div className={styles.caption}>当前注入 · 近期</div>
        {overview.recent.length === 0 ? (
          <span className={styles.emptyHint}>（暂无）</span>
        ) : (
          <div className={styles.recentList}>
            {overview.recent.map((r, i) => (
              <div key={`${r.date}-${i}`} className={styles.recentItem}>
                <span className={styles.recentDate}>{r.date}</span>
                {r.title}：{r.summary}
              </div>
            ))}
          </div>
        )}

        <div className={styles.caption}>当前注入 · 先前</div>
        {preEdit === null ? (
          overview.previous.trim().length === 0 ? (
            <span className={styles.emptyHint}>（暂无）</span>
          ) : (
            <div className={styles.preText}>{overview.previous}</div>
          )
        ) : (
          <>
            <textarea
              className={styles.textarea}
              value={preEdit}
              onChange={(e) => setPreEdit(e.target.value)}
              data-pre-edit
            />
            <div className={styles.formMeta}>
              {charCount(preEdit)} 字 / 上限 {LONGTERM_MAX_CHARS} 字（超出会被拒绝）。
              这是注入给模型的「先前」正文：保存后对新对话生效，正在进行的对话靠提醒看到新版。
            </div>
            <div className={styles.actions}>
              <button
                className={`${styles.btn} ${styles.btnPrimary}`}
                onClick={() => void savePre()}
                disabled={busy || savingPre}
              >
                {savingPre ? "保存中…" : "保存"}
              </button>
              <button className={styles.btn} onClick={() => setPreEdit(null)}>
                取消
              </button>
            </div>
          </>
        )}
        {preEdit === null && (
          <div className={styles.actions}>
            <button
              className={styles.btn}
              onClick={openPreEdit}
              disabled={busy || savingPre}
            >
              编辑长期记忆
            </button>
          </div>
        )}

        <div className={styles.actions}>
          <button className={styles.btn} onClick={() => void tidy()} disabled={busy || tidying}>
            {tidying ? "整理中…" : "整理当下记忆"}
          </button>
          <button className={styles.btn} onClick={() => void openViewer()}>
            查看记忆（{overview.experienceCount}）
          </button>
          <button
            className={`${styles.btn} ${styles.btnDanger}`}
            onClick={() => {
              if (!guard()) return;
              setConfirmClear(true);
            }}
            disabled={busy}
          >
            清除记忆
          </button>
        </div>
        {tidyMsg && <p className={styles.tidyMsg}>{tidyMsg}</p>}
        <SectionNote>
          经历记忆由后台自动维护；「整理当下记忆」对当前对话立即提炼一次（忽略轮数阈值）。
        </SectionNote>
      </SettingsSection>

      {/* ── 记忆库浮层 ── */}
      {viewer && (
        <div className={styles.backdrop} onClick={() => setViewer(null)}>
          <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
            <div className={styles.modalHeader}>
              <span>记忆库 · 全部经历（{viewer.length}）</span>
              <button className={styles.btn} onClick={() => setViewer(null)}>
                关闭
              </button>
            </div>
            <div className={styles.modalBody}>
              {viewer.length === 0 ? (
                <span className={styles.emptyHint}>（空）</span>
              ) : (
                viewer.map((item) => (
                  <div key={item.id} className={styles.viewerItem}>
                    <button className={styles.viewerHead} onClick={() => toggleExpand(item.id)}>
                      <span className={styles.viewerTitle}>
                        {item.title}
                        {item.absorbed ? "（已收纳）" : ""}
                      </span>
                      <span className={styles.viewerMeta}>
                        {item.date} · {item.summary}
                      </span>
                    </button>
                    {expanded === item.id && (
                      <div className={styles.viewerDetail}>
                        {`${item.summary}\n标签：${item.tags.length > 0 ? item.tags.join("、") : "（无）"}\n来源：对话 ${item.conversationId} · 轮次 ${item.turnFrom}-${item.turnTo}`}
                      </div>
                    )}
                  </div>
                ))
              )}
            </div>
          </div>
        </div>
      )}

      {/* ── 清除确认浮层 ── */}
      {confirmClear && (
        <div className={styles.backdrop} onClick={() => setConfirmClear(false)}>
          <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
            <div className={styles.modalHeader}>
              <span>清除经历记忆</span>
            </div>
            <div className={styles.modalBody}>
              <SectionNote>
                将清除全部经历条目与长期梗概，并把各对话水位抬平到当前轮数（旧轮次不会被补提炼挖回）。
                冻结的历史不受影响。此操作不可撤销。
              </SectionNote>
            </div>
            <div className={styles.modalFooter}>
              <button className={styles.btn} onClick={() => setConfirmClear(false)}>
                取消
              </button>
              <button className={`${styles.btn} ${styles.btnDanger}`} onClick={() => void clearMemory()}>
                确认清除
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
