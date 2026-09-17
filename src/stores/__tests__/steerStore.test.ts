// src/stores/__tests__/steerStore.test.ts
// useSteerStore：插话 pending 状态（add/remove/resolveInjected FIFO 双校验/
// clearForSession）——纯 store 行为测试（无 DOM、无 invoke）。

import { describe, it, expect, beforeEach } from "vitest";
import { useSteerStore } from "../useSteerStore";

function resetAll(): void {
  useSteerStore.setState({ pendingBySession: {} });
}

beforeEach(resetAll);

describe("useSteerStore · pending 生命周期", () => {
  it("addPending 按 sessionId 分区追加（FIFO）", () => {
    useSteerStore.getState().addPending("s1", { id: "0", text: "甲" });
    useSteerStore.getState().addPending("s1", { id: "1", text: "乙" });
    useSteerStore.getState().addPending("s2", { id: "0", text: "丙" });
    const s = useSteerStore.getState();
    expect(s.pendingBySession["s1"]).toEqual([
      { id: "0", text: "甲" },
      { id: "1", text: "乙" },
    ]);
    expect(s.pendingBySession["s2"]).toEqual([{ id: "0", text: "丙" }]);
  });

  it("removePending 移除指定条目；清空后删除分区", () => {
    const store = useSteerStore.getState();
    store.addPending("s1", { id: "0", text: "甲" });
    store.addPending("s1", { id: "1", text: "乙" });
    store.removePending("s1", "0");
    expect(useSteerStore.getState().pendingBySession["s1"]).toEqual([
      { id: "1", text: "乙" },
    ]);
    store.removePending("s1", "1");
    expect(
      useSteerStore.getState().pendingBySession["s1"]
    ).toBeUndefined();
    // 幂等：不存在条目空操作
    useSteerStore.getState().removePending("s1", "999");
    expect(useSteerStore.getState().pendingBySession).toEqual({});
  });

  it("resolveInjected：队首内容与事件一致 → 移除队首（转正）", () => {
    const store = useSteerStore.getState();
    store.addPending("s1", { id: "0", text: "插话一" });
    store.addPending("s1", { id: "1", text: "插话二" });
    // 注入顺序 = FIFO：第一条注入移除队首
    useSteerStore.getState().resolveInjected("s1", "插话一");
    expect(useSteerStore.getState().pendingBySession["s1"]).toEqual([
      { id: "1", text: "插话二" },
    ]);
    useSteerStore.getState().resolveInjected("s1", "插话二");
    expect(useSteerStore.getState().pendingBySession).toEqual({});
  });

  it("resolveInjected 双校验：队首不匹配 → 不移除（协议错位防御，clear 兜底）", () => {
    const store = useSteerStore.getState();
    store.addPending("s1", { id: "0", text: "插话一" });
    useSteerStore.getState().resolveInjected("s1", "不相干的内容");
    expect(useSteerStore.getState().pendingBySession["s1"]).toEqual([
      { id: "0", text: "插话一" },
    ]);
    // 无 pending 时无操作
    useSteerStore.getState().resolveInjected("nope", "x");
    expect(useSteerStore.getState().pendingBySession["s1"]).toHaveLength(1);
  });

  it("clearForSession 整清分区（中止/轮收尾防御）；空分区幂等", () => {
    const store = useSteerStore.getState();
    store.addPending("s1", { id: "0", text: "甲" });
    store.addPending("s1", { id: "1", text: "乙" });
    useSteerStore.getState().clearForSession("s1");
    expect(useSteerStore.getState().pendingBySession).toEqual({});
    useSteerStore.getState().clearForSession("s1"); // 幂等
    expect(useSteerStore.getState().pendingBySession).toEqual({});
  });

  it("不同 sessionId 相互隔离", () => {
    const store = useSteerStore.getState();
    store.addPending("s1", { id: "0", text: "甲" });
    store.addPending("s2", { id: "0", text: "乙" });
    useSteerStore.getState().resolveInjected("s1", "甲");
    expect(useSteerStore.getState().pendingBySession["s2"]).toEqual([
      { id: "0", text: "乙" },
    ]);
  });
});
