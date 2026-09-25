// @vitest-environment jsdom
// 启动演出测试：剧本与真实（restoreInfo）的绑定（fake timers 推进）
//
// 覆盖：
//   - 回归用户：铭牌/清单/三句依次落定，[n/N] 展示真实计数，淡出后 onDone
//   - 首次使用（readTotal = 0）：跳过「调取」，收尾换就绪句

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { BootSequence } from "./BootSequence";
import { useUIStore } from "../../stores/useUIStore";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(() => {
  vi.useFakeTimers();
  useUIStore.setState({
    restoring: false,
    restoreInfo: { phase: "ready", readDone: 0, readTotal: 0 },
  });
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
});

afterEach(() => {
  act(() => {
    root.unmount();
  });
  container.remove();
  vi.useRealTimers();
});

/** 推进到全部落定（剧本总时长 < 3s，5s 富余） */
async function playThrough() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000);
  });
}

describe("BootSequence 启动演出", () => {
  it("回归用户：铭牌/清单入场、三句打字、[2/2] 计数、淡出后 onDone", async () => {
    useUIStore.setState({
      restoreInfo: { phase: "ready", readDone: 2, readTotal: 2 },
    });
    const onDone = vi.fn();
    await act(async () => {
      root.render(<BootSequence onDone={onDone} />);
    });

    // 静态块挂载即渲染（CSS 动画负责入场）
    expect(container.textContent).toContain(
      "PRTS · Preliminary RodesIsland Terminal System"
    );
    expect(container.textContent).toContain("作战数据库 … 已接入");
    expect(container.textContent).toContain("知识库 … 已接入");
    expect(container.textContent).toContain("记忆模块 … 已接入");
    expect(onDone).not.toHaveBeenCalled();

    await playThrough();

    expect(container.textContent).toContain("正在校验作战记录索引…");
    expect(container.textContent).toContain("正在调取作战记录…");
    expect(container.textContent).toContain("[2/2]");
    expect(container.textContent).toContain("链路建立完毕。欢迎回来，博士。");
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it("首次使用（readTotal=0）：跳过「调取」，收尾就绪句", async () => {
    const onDone = vi.fn();
    await act(async () => {
      root.render(<BootSequence onDone={onDone} />);
    });

    await playThrough();

    expect(container.textContent).not.toContain("正在调取作战记录");
    expect(container.textContent).toContain("链路建立完毕。博士，终端就绪。");
    expect(onDone).toHaveBeenCalledTimes(1);
  });
});
