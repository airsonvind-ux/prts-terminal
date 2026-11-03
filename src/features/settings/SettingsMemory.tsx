// 设置页 · "记忆"子页（2026-09 结构定稿，2026-11 版式重排）
//
// 结构：
//  - 成长：2 列卡片（关于博士居首，默认露出 4 个，其余收进「全部 N 个」）
//    + [新建专题]；点击卡片 → 二级视图（阅读/编辑）。
//  - 经历：三个读数（经历条目 / 近期 / 长期梗概）+ [查看当下记忆]／[整理当下记忆]；
//    下面一道细线之后是 [查看全部记忆]／[清除经历记忆]，
//    清除确认从遮罩 modal 改成页内确认条（与插件卸载同一套语言）。
//
// 二级视图＝设置内容区内的替换视图（见 SettingsMemoryViews.tsx），不用整窗浮层。
// 写类操作（新建/编辑/删除/整理/清除/长期记忆保存）在对话进行中禁用（沿用设置锁
// 的前端表现）；查看/读取不受限。

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  SettingsSection,
  SectionWarning,
} from "./ui/SettingsSection";
import { Button } from "./ui/Button";
import { Stat } from "./ui/Stat";
import { showToast } from "../../stores/useToastStore";
import { useConversationStore } from "../../stores/useConversationStore";
import type { MemoryOverview, MemoryTidyResult } from "../../types";
import {
  CompiledView,
  GrowthView,
  LibraryView,
  type MemView,
} from "./SettingsMemoryViews";
import { charCount } from "./memoryViewShared";
import styles from "./SettingsMemory.module.css";

export interface SettingsMemoryProps {
  /** 对话处理中（任一对话）：写类操作禁用 */
  busy: boolean;
}

/** 成长专题默认露出几个（两行）；再多就收进「全部 N 个」 */
const GROWTH_PREVIEW = 4;

export function SettingsMemory({ busy }: SettingsMemoryProps) {
  const activeId = useConversationStore((s) => s.activeId);
  const [overview, setOverview] = useState<MemoryOverview | null>(null);
  const [loadErr, setLoadErr] = useState(false);
  const [view, setView] = useState<MemView>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [tidying, setTidying] = useState(false);
  const [tidyMsg, setTidyMsg] = useState<string | null>(null);
  const [showAllGrowth, setShowAllGrowth] = useState(false);

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
            {/* 专题多了不能无限往下排：默认 4 个（两行），其余的收进「全部」。 */}
            <div className={styles.growthGrid}>
              {(showAllGrowth
                ? overview.growth
                : overview.growth.slice(0, GROWTH_PREVIEW)
              ).map((g) => (
                <button
                  key={g.name}
                  type="button"
                  data-focus-inset
                  className={styles.growthCard}
                  onClick={() => setView({ kind: "growth", name: g.name })}
                  title={g.brief || g.name}
                >
                  <span className={styles.growthName}>{g.name}</span>
                  {g.brief && (
                    <span className={styles.growthBrief}>{g.brief}</span>
                  )}
                </button>
              ))}
            </div>
            <div className={styles.actions}>
              <Button
                onClick={() => {
                  if (guard()) setView({ kind: "growthNew" });
                }}
                disabled={busy}
              >
                新建专题
              </Button>
              {overview.growth.length > GROWTH_PREVIEW && (
                <Button onClick={() => setShowAllGrowth((v) => !v)}>
                  {showAllGrowth
                    ? "收起"
                    : `全部 ${overview.growth.length} 个`}
                </Button>
              )}
            </div>
          </SettingsSection>

          {/* ── 经历 ── */}
          <SettingsSection title="经历">
            <div className={styles.stats}>
              <Stat
                label="经历条目"
                value={overview.experienceCount}
                accent
              />
              <Stat label="近期" value={overview.recent.length} />
              <Stat
                label="长期梗概"
                value={
                  overview.previous ? `${charCount(overview.previous)} 字` : "—"
                }
                title={overview.previous || "还没有长期梗概"}
              />
            </div>
            <div className={styles.actions}>
              <Button onClick={() => setView({ kind: "compiled" })}>
                查看当下记忆
              </Button>
              <Button
                onClick={() => void tidy()}
                disabled={busy || tidying}
              >
                {tidying ? "整理中…" : "整理当下记忆"}
              </Button>
            </div>
            {tidyMsg && <p className={styles.tidyMsg}>{tidyMsg}</p>}

            {/* 经历之外还有一份按日期编排的记忆库；清除只作用于经历。
                不再叠标题 + 说明 + 尾注三行文字——按钮名字自己说清楚了。 */}
            <div className={styles.bottomRow}>
              <Button onClick={() => setView({ kind: "library" })}>
                查看全部记忆
              </Button>
              <Button
                variant="danger"
                onClick={() => {
                  if (guard()) setConfirmClear(true);
                }}
                disabled={busy}
              >
                清除经历记忆
              </Button>
            </div>

            {confirmClear && (
              <div
                className={styles.confirmBar}
                role="group"
                aria-label="确认清除经历记忆"
              >
                <span className={styles.confirmText}>
                  将清空经历条目与长期梗概；成长专题不受影响。此操作不可撤销。
                </span>
                <div className={styles.actions}>
                  <Button onClick={() => setConfirmClear(false)}>取消</Button>
                  <Button
                    variant="danger"
                    onClick={() => void clearMemory()}
                  >
                    确认清除
                  </Button>
                </div>
              </div>
            )}
          </SettingsSection>
        </>
      )}
    </div>
  );
}
