// src/hooks/__tests__/useConversationHydration.test.ts
// 会话恢复（Phase 4）测试：恢复 hook 的纯逻辑（restoreConversations /
// pickActiveIdAfterRestore——与 React 解耦，测试直接 await）
//
// 覆盖：
//   F6 恢复 hydrate：list + get → conversations/turns/activeId 正确、
//      null → undefined、含块 user 消息逐字节保留（历史冻结铁律）
//   F7 恢复失败降级（list reject → restoring=false 空列表不抛）
//      + 首次使用（空列表）→ 欢迎界面照旧
//   F9 restoring 状态切换（恢复期间 true、完成/失败后 false）
//   F10 启动演出进度（restoreInfo：read 逐份推进 → ready）
//   F11 并发调用合并（StrictMode 双挂载只跑一轮：list/get 各一次、进度不回退）
//   导航决策：pickActiveIdAfterRestore 单测（updatedAt 最新 = activeId）

import { describe, it, expect, beforeEach } from "vitest";
import {
  pickActiveIdAfterRestore,
  restoreConversations,
} from "../useConversationHydration";
import { useConversationStore } from "../../stores/useConversationStore";
import { useUIStore } from "../../stores/useUIStore";
import { useToastStore } from "../../stores/useToastStore";
import type { ConversationMeta } from "../../types";

// ── 辅助 ──

/** setup.ts 默认路由的复刻（mockReset 后恢复默认行为，防跨用例污染） */
function defaultRoute(cmd: string) {
  if (cmd === "get_reminder_data") {
    return Promise.resolve({ seq: 0, dims: [], events: [] });
  }
  if (cmd === "list_conversations") return Promise.resolve([]);
  if (cmd === "count_unreadable_sessions") return Promise.resolve(0);
  return Promise.resolve(undefined);
}

function resetAll(): void {
  useConversationStore.setState({
    conversations: [],
    activeId: null,
    pendingTitle: null,
  });
  useUIStore.setState({
    errorMsg: null,
    overlay: null,
    restoring: false,
  });
  useToastStore.setState({ toasts: [] });
  __invokeMock.mockReset();
  __invokeMock.mockImplementation(defaultRoute);
}

beforeEach(resetAll);

// ── 数据 ──

function meta(over: Partial<ConversationMeta>): ConversationMeta {
  return {
    id: "x",
    title: "X",
    createdAt: 1,
    updatedAt: 1,
    modelName: null,
    contextLength: null,
    contextTokens: null,
    lastNoticeSeq: null,
    compaction: null,
    ...over,
  };
}

// 含 [prts_reminder] 块的 user 消息（冻结历史）：内存形态为真实换行，
// 落盘轮行为 JSON.stringify 单行（\n 转义）；恢复 parse 后必须逐字节还原
const blockedUserContent =
  "[prts_reminder]\n- 模型：deepseek\n[/prts_reminder]\n\n继续";
const blockedTurnLine = JSON.stringify([
  { type: "user/message", sessionId: "s1", content: blockedUserContent },
]);

/** 等一轮宏任务（让 list/get 的 await 链推进到下一稳定态） */
const tick = () => new Promise<void>((r) => setTimeout(r, 0));

// ═══════════════════════════════════════════════════════
// 导航决策单点
// ═══════════════════════════════════════════════════════

describe("pickActiveIdAfterRestore（导航决策单点）", () => {
  it("空列表 → null（欢迎界面照旧）", () => {
    expect(pickActiveIdAfterRestore([])).toBeNull();
  });

  it("取 updatedAt 最新会话为 activeId", () => {
    const old = meta({ id: "a", updatedAt: 3000 });
    const fresh = meta({ id: "b", title: "新B", updatedAt: 5000 });
    expect(pickActiveIdAfterRestore([old, fresh])).toBe("b");
    // 乱序输入同样取最新
    expect(pickActiveIdAfterRestore([fresh, old])).toBe("b");
  });

  it("同 updatedAt → 保留先遇到者（列表顺序稳定）", () => {
    const a = meta({ id: "a", updatedAt: 7 });
    const b = meta({ id: "b", updatedAt: 7 });
    expect(pickActiveIdAfterRestore([a, b])).toBe("a");
  });
});

// ═══════════════════════════════════════════════════════
// F6：恢复 hydrate
// ═══════════════════════════════════════════════════════

describe("F6 恢复 hydrate", () => {
  it("list + get → conversations/turns/activeId 正确、null→undefined、块逐字节保留", async () => {
    const metaA = meta({
      id: "a",
      title: "旧A",
      createdAt: 1000,
      updatedAt: 3000,
    });
    const metaB = meta({
      id: "b",
      title: "新B",
      createdAt: 2000,
      updatedAt: 5000,
      modelName: "deepseek-chat",
      contextLength: 64000,
      contextTokens: { inputTokens: 1, outputTokens: 2 },
      lastNoticeSeq: 9,
    });
    __invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_conversations") {
        return Promise.resolve([metaA, metaB]);
      }
      if (cmd === "get_conversation") {
        const conversationId = (args as { conversationId: string }).conversationId;
        if (conversationId === "a") {
          return Promise.resolve({ meta: metaA, turns: [blockedTurnLine] });
        }
        return Promise.resolve({ meta: metaB, turns: [] });
      }
      return Promise.resolve(undefined);
    });

    await restoreConversations();

    const { conversations, activeId, pendingTitle } =
      useConversationStore.getState();
    expect(conversations).toHaveLength(2);
    // 导航决策：updatedAt 最新（b: 5000 > a: 3000）
    expect(activeId).toBe("b");
    expect(pendingTitle).toBeNull();

    // a：含块轮行逐字节还原（历史冻结铁律）
    const a = conversations.find((c) => c.id === "a")!;
    expect(a.turns).toHaveLength(1);
    expect(a.turns[0][0]).toEqual({
      type: "user/message",
      sessionId: "s1",
      content: blockedUserContent,
    });
    // 可缺省字段 null → undefined 适配
    expect(a.modelName).toBeUndefined();
    expect(a.contextLength).toBeUndefined();
    expect(a.contextTokens).toBeUndefined();
    expect(a.lastNoticeSeq).toBeUndefined();

    // b：完整 meta 恢复
    const b = conversations.find((c) => c.id === "b")!;
    expect(b.title).toBe("新B");
    expect(b.createdAt).toBe(2000);
    expect(b.modelName).toBe("deepseek-chat");
    expect(b.contextLength).toBe(64000);
    expect(b.contextTokens).toEqual({ inputTokens: 1, outputTokens: 2 });
    expect(b.lastNoticeSeq).toBe(9);

    // 恢复完成 restoring 复位
    expect(useUIStore.getState().restoring).toBe(false);
    // 启动演出数据源：工序进度收敛 ready（两份记录读完）
    expect(useUIStore.getState().restoreInfo).toEqual({
      phase: "ready",
      readDone: 2,
      readTotal: 2,
    });
  });

  it("list 空（首次使用）→ 空历史 + restoring 复位（欢迎界面照旧）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_conversations") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });

    await restoreConversations();

    const { conversations, activeId } = useConversationStore.getState();
    expect(conversations).toHaveLength(0);
    expect(activeId).toBeNull();
    expect(useUIStore.getState().restoring).toBe(false);
    // 首次使用：无记录可读（readTotal 保持 0）
    expect(useUIStore.getState().restoreInfo).toEqual({
      phase: "ready",
      readDone: 0,
      readTotal: 0,
    });
  });

  it("primary 日志的 turn/end(max-tokens) 恢复为轮尾截断标记", async () => {
    const metaX = meta({ id: "t", updatedAt: 2 });
    const turnLine = JSON.stringify([
      { type: "user/message", sessionId: "t", content: "长回答" },
      {
        type: "assistant/message",
        sessionId: "t",
        content: "半截回答",
        reasoningContent: null,
        usage: null,
      },
      { type: "turn/end", sessionId: "t", reason: "max-tokens" },
    ]);
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_conversations") return Promise.resolve([metaX]);
      if (cmd === "get_conversation") {
        return Promise.resolve({ meta: metaX, turns: [turnLine] });
      }
      return Promise.resolve(undefined);
    });

    await restoreConversations();

    const turns = useConversationStore.getState().conversations[0].turns;
    expect(turns[0]).toEqual(
      expect.arrayContaining([{ type: "turn/truncated", sessionId: "t" }])
    );
  });
});

// ═══════════════════════════════════════════════════════
// F7：恢复失败降级
// ═══════════════════════════════════════════════════════

describe("F7 恢复失败降级", () => {
  it("list reject → 不抛、空历史、restoring 复位", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_conversations") {
        return Promise.reject(new Error("后端不可达"));
      }
      return Promise.resolve(undefined);
    });

    await expect(restoreConversations()).resolves.toBeUndefined();

    const { conversations, activeId } = useConversationStore.getState();
    expect(conversations).toHaveLength(0);
    expect(activeId).toBeNull();
    expect(useUIStore.getState().restoring).toBe(false);
  });

  it("get reject（部分拉取失败）→ 整体降级空历史、不抛", async () => {
    const metaX = meta({ id: "x", updatedAt: 2 });
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_conversations") return Promise.resolve([metaX]);
      if (cmd === "count_unreadable_sessions") return Promise.resolve(0);
      if (cmd === "get_conversation") {
        return Promise.reject(new Error("会话文件损坏"));
      }
      return Promise.resolve(undefined);
    });

    await expect(restoreConversations()).resolves.toBeUndefined();
    expect(useConversationStore.getState().conversations).toHaveLength(0);
    expect(useUIStore.getState().restoring).toBe(false);
  });

  it("存在无法读取的会话文件 → warning toast（不静默）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_conversations") return Promise.resolve([]);
      if (cmd === "count_unreadable_sessions") return Promise.resolve(2);
      return Promise.resolve(undefined);
    });

    await restoreConversations();

    const toasts = useToastStore.getState().toasts;
    expect(
      toasts.some(
        (t) =>
          t.kind === "warning" &&
          t.message.includes("2 个会话文件无法读取")
      )
    ).toBe(true);
  });
});

// ═══════════════════════════════════════════════════════
// F9：restoring 状态切换
// ═══════════════════════════════════════════════════════

describe("F9 restoring 状态切换", () => {
  it("恢复开始时 true、get 挂起期间保持 true、完成后 false", async () => {
    const metaX = meta({ id: "x", updatedAt: 2 });
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_conversations") return Promise.resolve([metaX]);
      if (cmd === "get_conversation") {
        return gate.then(() => ({ meta: metaX, turns: [] }));
      }
      return Promise.resolve(undefined);
    });

    const p = restoreConversations();
    // setRestoring(true) 同步执行；等 list 微任务消化后 get 挂起在 gate 上
    expect(useUIStore.getState().restoring).toBe(true);
    await Promise.resolve();
    expect(useUIStore.getState().restoring).toBe(true);

    release();
    await p;
    expect(useUIStore.getState().restoring).toBe(false);
  });
});

// ═══════════════════════════════════════════════════════
// F10：启动演出进度（restoreInfo 的工序推进）
// ═══════════════════════════════════════════════════════

describe("F10 启动演出进度", () => {
  it("read 阶段逐份推进 readDone；结束后收敛 ready", async () => {
    const metaA = meta({ id: "a", updatedAt: 2 });
    const metaB = meta({ id: "b", updatedAt: 1 });
    let releaseA!: () => void;
    let releaseB!: () => void;
    const gateA = new Promise<void>((r) => (releaseA = r));
    const gateB = new Promise<void>((r) => (releaseB = r));
    __invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_conversations") return Promise.resolve([metaA, metaB]);
      if (cmd === "get_conversation") {
        const id = (args as { conversationId: string }).conversationId;
        const gate = id === "a" ? gateA : gateB;
        return gate.then(() => ({
          meta: id === "a" ? metaA : metaB,
          turns: [],
        }));
      }
      return Promise.resolve(undefined);
    });

    const p = restoreConversations();
    await tick();
    // 进入 read 阶段：0/2
    expect(useUIStore.getState().restoreInfo).toEqual({
      phase: "read",
      readDone: 0,
      readTotal: 2,
    });

    releaseA();
    await tick();
    expect(useUIStore.getState().restoreInfo.readDone).toBe(1);

    releaseB();
    await p;
    expect(useUIStore.getState().restoreInfo).toEqual({
      phase: "ready",
      readDone: 2,
      readTotal: 2,
    });
  });
});

// ═══════════════════════════════════════════════════════
// F11：并发调用合并（StrictMode 双挂载）
// ═══════════════════════════════════════════════════════

describe("F11 并发调用合并", () => {
  it("并发重复调用合并为一次执行：list/get 只发一轮，进度不回退", async () => {
    const metaA = meta({ id: "a", updatedAt: 2 });
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_conversations") return Promise.resolve([metaA]);
      if (cmd === "get_conversation") {
        return Promise.resolve({ meta: metaA, turns: [] });
      }
      return Promise.resolve(undefined);
    });

    const p1 = restoreConversations();
    const p2 = restoreConversations();
    expect(p2).toBe(p1); // 复用同一 promise（第二次调用不重新起跑）

    await Promise.all([p1, p2]);

    const listCalls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "list_conversations"
    );
    const getCalls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "get_conversation"
    );
    expect(listCalls).toHaveLength(1);
    expect(getCalls).toHaveLength(1);
    expect(useUIStore.getState().restoreInfo).toEqual({
      phase: "ready",
      readDone: 1,
      readTotal: 1,
    });
  });
});
