// 设置页 · "用量"子页（Phase 5）
//
// 数据来源：useConversationStore.conversations（后端 index.json 的投影缓存，
// 随应用启动全量载入 + 随轮/压缩/标题增量同步）——纯读取，无 IPC、无加载态。
//
// 结构（单页，两个 scope 共用一套布局）：
//  - 顶部：范围选择器（全部对话 / 单对话下拉，最近在前，超出滚动）
//  - Hero：总用量大字 + 小圆环（缓存命中率，未观测显示 —）
//  - 分类区：四行（对话 / 压缩 / 记忆 / 标题），每行 = 类别名 + 占比条 + 用量 · 命中
// 交互仅三：范围切换、滚动、（对话列表由轮末自动更新，无手动刷新）。

import { useMemo, useState } from "react";
import { useConversationStore } from "../../stores/useConversationStore";
import type {
  CategoryUsage,
  Conversation,
  ConversationUsage,
  UsageCategory,
} from "../../types";
import styles from "./SettingsUsage.module.css";

const CATEGORIES: { key: UsageCategory; label: string }[] = [
  { key: "session", label: "对话" },
  { key: "compaction", label: "压缩" },
  { key: "memory", label: "记忆" },
  { key: "title", label: "标题" },
];

function zeroCategory(): CategoryUsage {
  return { inputTokens: 0, outputTokens: 0, cacheHitTokens: 0, cachePromptTokens: 0 };
}

function zeroUsage(): ConversationUsage {
  return {
    session: zeroCategory(),
    compaction: zeroCategory(),
    memory: zeroCategory(),
    title: zeroCategory(),
  };
}

function addCategory(a: CategoryUsage, b: CategoryUsage): CategoryUsage {
  return {
    inputTokens: a.inputTokens + b.inputTokens,
    outputTokens: a.outputTokens + b.outputTokens,
    cacheHitTokens: a.cacheHitTokens + b.cacheHitTokens,
    cachePromptTokens: a.cachePromptTokens + b.cachePromptTokens,
  };
}

function sumUsage(list: Conversation[]): ConversationUsage {
  return list.reduce<ConversationUsage>((acc, c) => {
    if (!c.usage) return acc;
    return {
      session: addCategory(acc.session, c.usage.session),
      compaction: addCategory(acc.compaction, c.usage.compaction),
      memory: addCategory(acc.memory, c.usage.memory),
      title: addCategory(acc.title, c.usage.title),
    };
  }, zeroUsage());
}

function catTokens(c: CategoryUsage): number {
  return c.inputTokens + c.outputTokens;
}

function usageTotal(u: ConversationUsage | undefined): number {
  if (!u) return 0;
  return CATEGORIES.reduce((s, c) => s + catTokens(u[c.key]), 0);
}

function formatCompact(n: number): string {
  const r = Math.round(n);
  if (r >= 1_000_000) return `${(r / 1_000_000).toFixed(2).replace(/\.?0+$/, "")}M`;
  if (r >= 1_000) return `${(r / 1_000).toFixed(1).replace(/\.?0+$/, "")}K`;
  return String(r);
}

function formatNumber(n: number): string {
  return Math.round(n).toLocaleString();
}

const DONUT_CIRCUMFERENCE = 2 * Math.PI * 18;

export function SettingsUsage() {
  const conversations = useConversationStore((s) => s.conversations);
  const [scope, setScope] = useState<string>("all");
  const [open, setOpen] = useState(false);

  const allUsage = useMemo(() => sumUsage(conversations), [conversations]);
  const hasAny = useMemo(() => conversations.some((c) => c.usage), [conversations]);

  // scope 指向的对话可能已被删除 → 回退全部
  const activeConv =
    scope === "all" ? null : conversations.find((c) => c.id === scope) ?? null;
  const effectiveScope = scope !== "all" && !activeConv ? "all" : scope;
  const usage = effectiveScope === "all" ? allUsage : activeConv?.usage ?? zeroUsage();

  const total = CATEGORIES.reduce((s, c) => s + catTokens(usage[c.key]), 0);
  const hit = CATEGORIES.reduce((s, c) => s + usage[c.key].cacheHitTokens, 0);
  const prompt = CATEGORIES.reduce((s, c) => s + usage[c.key].cachePromptTokens, 0);
  const hitRate = prompt > 0 ? hit / prompt : null;

  const scopeLabel =
    effectiveScope === "all" ? "全部对话" : activeConv?.title ?? "全部对话";

  // 清单：最近在前（Conversation 无 updatedAt，用 createdAt 近似）
  const listed = useMemo(
    () => [...conversations].sort((a, b) => b.createdAt - a.createdAt),
    [conversations]
  );

  return (
    <div className={styles.container}>
      <button
        className={styles.scopeBtn}
        onClick={() => setOpen((v) => !v)}
        aria-label="选择统计范围"
      >
        <span className={styles.scopeLabel} title={scopeLabel}>
          {scopeLabel}
        </span>
        <svg className={styles.chev} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" aria-hidden="true">
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>

      {open && (
        <div className={styles.scopeList}>
          <button
            className={`${styles.scopeItem}${effectiveScope === "all" ? ` ${styles.scopeItemActive}` : ""}`}
            onClick={() => {
              setScope("all");
              setOpen(false);
            }}
          >
            <span className={styles.itemTitle}>全部对话</span>
            <span className={styles.itemValue}>{formatCompact(usageTotal(allUsage))}</span>
          </button>
          {listed.map((c) => (
            <button
              key={c.id}
              className={`${styles.scopeItem}${effectiveScope === c.id ? ` ${styles.scopeItemActive}` : ""}`}
              onClick={() => {
                setScope(c.id);
                setOpen(false);
              }}
            >
              <span className={styles.itemTitle} title={c.title}>
                {c.title}
              </span>
              <span className={styles.itemValue}>{formatCompact(usageTotal(c.usage))}</span>
            </button>
          ))}
        </div>
      )}

      {!hasAny ? (
        <p className={styles.empty}>暂无用量记录</p>
      ) : (
        <>
          <div className={styles.hero}>
            <div>
              <span className={styles.heroLabel}>总用量</span>
              <span className={styles.heroValue} title={formatNumber(total)}>
                {formatCompact(total)}
              </span>
            </div>
            <div className={styles.donutWrap}>
              <svg className={styles.donut} viewBox="0 0 44 44" role="img">
                <title>缓存命中率</title>
                <circle cx="22" cy="22" r="18" fill="none" stroke="var(--depth-1)" strokeWidth="4" />
                <circle
                  cx="22"
                  cy="22"
                  r="18"
                  fill="none"
                  stroke="var(--accent)"
                  strokeWidth="4"
                  transform="rotate(-90 22 22)"
                  strokeDasharray={`${((hitRate ?? 0) * DONUT_CIRCUMFERENCE).toFixed(1)} ${DONUT_CIRCUMFERENCE.toFixed(1)}`}
                />
                <text x="22" y="26" textAnchor="middle" className={styles.donutText}>
                  {hitRate === null ? "—" : `${Math.round(hitRate * 100)}%`}
                </text>
              </svg>
              <span className={styles.heroLabel}>
                {hitRate === null ? "缓存未观测" : `命中 ${formatCompact(hit)}`}
              </span>
            </div>
          </div>

          <div className={styles.cats}>
            {CATEGORIES.map((c) => {
              const v = usage[c.key];
              const t = catTokens(v);
              const share = total > 0 ? t / total : 0;
              return (
                <div className={styles.catRow} key={c.key}>
                  <span className={styles.catName}>{c.label}</span>
                  <span className={styles.catBar}>
                    <span
                      className={styles.catFill}
                      style={{ width: `${(share * 100).toFixed(1)}%` }}
                    />
                  </span>
                  <span className={styles.catVal}>
                    <span className={styles.catTokens} title={formatNumber(t)}>
                      {formatCompact(t)}
                    </span>
                    <span className={styles.catHit}>
                      {v.cachePromptTokens > 0 ? `命中 ${formatCompact(v.cacheHitTokens)}` : "未观测"}
                    </span>
                  </span>
                </div>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}
