// src/stores/__tests__/toast.test.ts
// M3-B §5.2 Toast store：show / dismiss / ttl 自动消失 / dedupe 重置 ttl

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  useToastStore,
  showToast,
  DEFAULT_TTL_MS,
} from "../useToastStore";

beforeEach(() => {
  vi.useFakeTimers();
  useToastStore.setState({ toasts: [] });
});

afterEach(() => {
  // 跑完所有挂起定时器再回到真实时钟（防 pending timer 泄漏到其他测试）
  vi.runAllTimers();
  vi.useRealTimers();
  useToastStore.setState({ toasts: [] });
});

function toastMessages(): string[] {
  return useToastStore.getState().toasts.map((t) => t.message);
}

describe("useToastStore", () => {
  it("show 追加条目（默认 kind=warning、ttl=4000）", () => {
    showToast("对话处理中不能修改模型");
    const { toasts } = useToastStore.getState();
    expect(toasts).toHaveLength(1);
    expect(toasts[0].message).toBe("对话处理中不能修改模型");
    expect(toasts[0].kind).toBe("warning");
    expect(toasts[0].ttlMs).toBe(DEFAULT_TTL_MS);
    expect(typeof toasts[0].id).toBe("string");
  });

  it("dismiss 移除条目并清定时器", () => {
    showToast("a");
    const id = useToastStore.getState().toasts[0].id;
    expect(vi.getTimerCount()).toBe(1);
    useToastStore.getState().dismiss(id);
    expect(useToastStore.getState().toasts).toHaveLength(0);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("默认 ttl 后自动消失", () => {
    showToast("自动消失");
    expect(useToastStore.getState().toasts).toHaveLength(1);
    vi.advanceTimersByTime(DEFAULT_TTL_MS - 1);
    expect(useToastStore.getState().toasts).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });

  it("自定义 ttlMs 生效", () => {
    showToast("短命", { ttlMs: 100 });
    vi.advanceTimersByTime(100);
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });

  it("kind=info 透传", () => {
    showToast("信息", { kind: "info" });
    expect(useToastStore.getState().toasts[0].kind).toBe("info");
  });

  it("dedupe：同 key 命中现存条目 → 不新增仅更新文案并重置 ttl", () => {
    showToast("第一次", { dedupeKey: "config-busy-模型" });
    expect(vi.getTimerCount()).toBe(1);

    // 同 key 再来：条目数不变、文案更新、定时器仍只有一个（重置非新增）
    showToast("第二次", { dedupeKey: "config-busy-模型" });
    expect(useToastStore.getState().toasts).toHaveLength(1);
    expect(toastMessages()).toEqual(["第二次"]);
    expect(vi.getTimerCount()).toBe(1);

    // 重置后的 ttl 从第二次 show 起算：推进满一个默认 ttl 后消失
    vi.advanceTimersByTime(DEFAULT_TTL_MS);
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });

  it("不同 dedupeKey / 无 key → 各自独立堆积", () => {
    showToast("模型", { dedupeKey: "config-busy-模型" });
    showToast("API Key", { dedupeKey: "config-busy-API Key" });
    showToast("无 key 一条");
    expect(useToastStore.getState().toasts).toHaveLength(3);
    expect(vi.getTimerCount()).toBe(3);
  });

  it("模块级 showToast 与 store.show 等价", () => {
    showToast("模块导出");
    useToastStore.getState().show("store 方法");
    expect(useToastStore.getState().toasts).toHaveLength(2);
  });
});
