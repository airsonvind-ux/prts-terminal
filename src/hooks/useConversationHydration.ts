// src/hooks/useConversationHydration.ts
// 启动会话恢复 Hook（对话持久化 Phase 4）
//
// 职责：应用启动时从后端恢复会话列表与消息——list_conversations（秒开，
// 来源 index.json）→ 并行 get_conversation 拉全量轮行 → 组装 Conversation
// 装载进 useConversationStore（含恢复 activeId 的导航决策）。
//
// 架构约束（用户拍板）：恢复后的**导航决策**（选哪个会话为 activeId）必须
// 收敛在本模块单点（pickActiveIdAfterRestore + 其调用处），不得散落进 store
// action 或组件。
//
// ▎开屏流程改造点：未来若改为「重启显示欢迎界面 → 虚化 → 变换到上次对话」，
// 只需改本模块——把「恢复后直接 set activeId」换成「先 restoring/欢迎态、
// 动画完成后再 set activeId（或改存 lastActiveId 元数据）」；store action 与
// 组件零改动（组件只消费 conversations/activeId/restoring 三个原始事实）。
//
// 恢复期间 UI：restoring=true（欢迎界面判定被抑制 + 输入禁发）——防恢复完成
// 前闪欢迎界面、防恢复竞态发消息。list 空/失败 → 降级为空历史（欢迎界面照旧）。
//
// 并发口径：restoreConversations 把并发重复调用合并为单次执行（StrictMode
// 在 dev 下会双触发 effect）——既省一次重复读档，也防两条进度线写乱 readDone。
//
// 启动演出数据源：真实工序进度写入 useUIStore.restoreInfo（index → read[n/N]
// → ready），由 BootSequence 消费；演出只读进度，不反向影响流程。
// 历史冻结铁律：轮行是后端落盘的归档原文，JSON.parse 逐行重建——含
// [prts_reminder] 块的 user 消息逐字节保留，零改写。

import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useConversationStore } from "../stores/useConversationStore";
import { useUIStore } from "../stores/useUIStore";
import { showToast } from "../stores/useToastStore";
import type {
  BootStatus,
  Conversation,
  ConversationMeta,
  ConversationRecord,
  SessionEvent,
} from "../types";

/**
 * 恢复后的导航决策（单点，纯函数便于测试）：取 updatedAt 最新会话为 activeId。
 * 依据：重启后"上次活跃对话"≈ 最近有轮提交的对话；欢迎界面无对话列表入口，
 * 一律回欢迎界面会困住用户。列表空 → null（欢迎界面照旧）。
 * 同 updatedAt（同毫秒罕见）→ 保留先遇到者（metas 顺序稳定）。
 * 开屏流程改造点见文件头注释。
 */
export function pickActiveIdAfterRestore(
  metas: ConversationMeta[]
): string | null {
  if (metas.length === 0) return null;
  let best = metas[0];
  for (const m of metas) {
    if (m.updatedAt > best.updatedAt) best = m;
  }
  return best.id;
}

/** 进行中的恢复：并发调用合并为同一 promise（StrictMode 双挂载只跑一次）。
    既省一次全量读档，也避免两条进度线并发写 readDone 导致计数回退。 */
let inflight: Promise<void> | null = null;

/**
 * 执行一次会话恢复（纯 async 逻辑，与 React 解耦——测试可直接 await）。
 */
async function runRestore(): Promise<void> {
  useUIStore.getState().setRestoring(true);
  useUIStore
    .getState()
    .setRestoreInfo({ phase: "index", readDone: 0, readTotal: 0 });

  // 启动自检（只报事实，不参与业务判定）：与索引读取并行，失败静默——
  // 探不到就不显示状态位，不阻断恢复。
  void invoke<BootStatus>("get_boot_status")
    .then((status) => {
      if (status) useUIStore.getState().setBootStatus(status);
    })
    .catch(() => {});

  try {
    const metas = (await invoke<ConversationMeta[]>("list_conversations")) ?? [];
    // header 无法修复的会话不会进入 index；启动时给一条可见提示，
    // 避免文件损坏表现为“对话凭空消失”。
    const unreadable = await invoke<number>("count_unreadable_sessions").catch(
      () => 0
    );
    if (unreadable > 0) {
      showToast(
        `有 ${unreadable} 个会话文件无法读取，未能自动恢复；原文件已保留`,
        { kind: "warning", ttlMs: 8000 }
      );
    }
    if (metas.length === 0) return; // 首次使用/无持久数据 → 欢迎界面照旧

    // 并行拉全量（悬浮窗规模：几十条 × 每轮几 KB ≈ 几十 ms，不引入懒加载状态机）
    // ——逐份完成推进 readDone（启动演出的真实计数）
    useUIStore
      .getState()
      .setRestoreInfo({ phase: "read", readDone: 0, readTotal: metas.length });
    let readDone = 0;
    const records = await Promise.all(
      metas.map(async (m) => {
        const rec = await invoke<ConversationRecord>("get_conversation", {
          conversationId: m.id,
        });
        readDone += 1;
        useUIStore.getState().setRestoreInfo({ readDone });
        return rec;
      })
    );

    // 组装 Conversation（与现有形态完全一致）。防御：重复 id（罕见损坏场景）
    // 后者丢弃 + console.warn。
    const byId = new Map<string, Conversation>();
    for (const r of records) {
      if (byId.has(r.meta.id)) {
        console.warn(`[hydration] 重复会话 id ${r.meta.id}，丢弃后者`);
        continue;
      }
      byId.set(r.meta.id, {
        id: r.meta.id,
        title: r.meta.title,
        createdAt: r.meta.createdAt,
        // 轮行原文逐行 parse——后端保证每行可 parse（坏行已在读取侧跳过）。
        // primary 的官方日志没有前端私有 turn/aborted；按 turn/end 原因补回，
        // 保证重启后中止标记仍可见。
        turns: r.turns.map((line) => {
          const turn = JSON.parse(line) as SessionEvent[];
          const aborted = turn.some(
            (e) => e.type === "turn/end" && e.reason === "aborted"
          );
          const end = turn.find((e) => e.type === "turn/end");
          if (!aborted || turn.some((e) => e.type === "turn/aborted")) {
            // R-07：primary 日志只有 turn/end(max-tokens)，没有前端私有标记；
            // 恢复时补回轮尾提示标记（legacy 日志已有时不重复）。
            if (
              end?.type === "turn/end" &&
              end.reason === "max-tokens" &&
              !turn.some((e) => e.type === "turn/truncated")
            ) {
              return [
                ...turn,
                { type: "turn/truncated", sessionId: end.sessionId },
              ];
            }
            return turn;
          }
          return [
            ...turn,
            { type: "turn/aborted", sessionId: end?.sessionId ?? "" },
          ];
        }),
        // null → undefined 适配（后端可缺省字段以 null 表达）
        modelName: r.meta.modelName ?? undefined,
        contextLength: r.meta.contextLength ?? undefined,
        contextTokens: r.meta.contextTokens ?? undefined,
        lastNoticeSeq: r.meta.lastNoticeSeq ?? undefined,
        compaction: r.meta.compaction ?? undefined,
        usage: r.meta.usage ?? undefined,
      });
    }

    // ▎导航决策单点：恢复 updatedAt 最新会话为 activeId
    // （开屏流程改造点见文件头注释——只改本处与 pickActiveIdAfterRestore）
    useConversationStore.setState({
      conversations: [...byId.values()],
      activeId: pickActiveIdAfterRestore(metas),
      pendingTitle: null,
    });

    // 经历管线启动补偿（Phase 4 任务二）：列出水位落后的会话，逐个
    // fire-and-forget 补提炼（前端不 await；失败静默，下轮轮末/切换会再触发）。
    try {
      const pending =
        (await invoke<string[]>("memory_catch_up", { limit: 5 })) ?? [];
      for (const cid of pending) {
        void invoke("finalize_memory", { conversationId: cid }).catch(() => {});
      }
    } catch {
      /* 补偿失败静默：不阻断恢复 */
    }
  } catch (err) {
    // list/get 失败（后端不可达/损坏）→ 降级为空历史（欢迎界面），不抛给 UI
    console.warn("[hydration] 会话恢复失败（降级为空历史）：", err);
    // 演出据此多报一行（[WARN] 读取: 失败，按新建处理）——降级别没痕迹
    useUIStore.getState().setRestoreFailed(true);
  } finally {
    // 先置 ready（演出据此收尾），再复位 restoring（业务闸）
    useUIStore.getState().setRestoreInfo({ phase: "ready" });
    useUIStore.getState().setRestoring(false);
  }
}

/**
 * 启动恢复入口：并发重复调用（StrictMode 双挂载 / 快速重入）合并为单次执行，
 * 调用方共享同一 promise。恢复完成后解绑，下一次调用重新开始。
 */
export function restoreConversations(): Promise<void> {
  if (!inflight) {
    inflight = runRestore().finally(() => {
      inflight = null;
    });
  }
  return inflight;
}

/** 应用启动时调用一次（仿 useAgentEvents：无 props、仅注册副作用） */
export function useConversationHydration(): void {
  useEffect(() => {
    void restoreConversations();
  }, []);
}
