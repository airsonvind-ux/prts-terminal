// @vitest-environment jsdom
// 启动演出测试：剧本与真实（restoreInfo）的绑定（fake timers 推进）
//
// 覆盖：
//   - 回归用户：铭牌/清单/三句依次落定，[n/N] 展示真实计数；
//     收尾句停留 → 日志退场波 → 字标收拢入场（欢迎页）→ 淡出后 onDone
//   - 首次使用（readTotal = 0）：跳过「调取」，收尾换就绪句；欢迎幕与回归一致

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
    bootStatus: null,
    restoreFailed: false,
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

/** 当前幕（挂载在根节点上的 data-scene） */
function sceneNow(): string | null {
  return (
    container.querySelector("[data-boot]")?.getAttribute("data-scene") ?? null
  );
}

/** 推进到全部落定（剧本总时长 < 6s，8s 富余） */
async function playThrough() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(8000);
  });
}

describe("BootSequence 启动演出", () => {
  it("回归用户：三句打字、[2/2] 计数、收尾停留、退场波 → 欢迎页、淡出后 onDone", async () => {
    useUIStore.setState({
      restoreInfo: { phase: "ready", readDone: 2, readTotal: 2 },
    });
    const onDone = vi.fn();
    await act(async () => {
      root.render(<BootSequence onDone={onDone} />);
    });

    // 静态块挂载即渲染（CSS 动画负责入场）；初始为日志幕
    // 自检报告未到：清单三行的值先是「接入中」（状态不编造）
    expect(container.textContent).toContain(
      "PRTS · Preliminary Rhodes Island Terminal System"
    );
    expect(container.textContent).toContain("数据库 … 接入中");
    expect(container.textContent).toContain("知识库 … 接入中");
    expect(container.textContent).toContain("记忆模块 … 接入中");
    expect(sceneNow()).toBe("log");
    expect(onDone).not.toHaveBeenCalled();

    await playThrough();

    expect(container.textContent).toContain("正在校验作战记录索引…");
    expect(container.textContent).toContain("正在调取作战记录…");
    expect(container.textContent).toContain("[2/2]");
    expect(container.textContent).toContain("链路建立完毕。欢迎回来，博士。");
    // 幕二：欢迎页（字标 + 标语）且已停在欢迎幕
    expect(container.textContent).toContain("明日方舟AI辅助终端");
    expect(sceneNow()).toBe("welcome");
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it("首次使用（readTotal=0）：跳过「调取」，收尾就绪句，欢迎幕照常播放", async () => {
    const onDone = vi.fn();
    await act(async () => {
      root.render(<BootSequence onDone={onDone} />);
    });

    await playThrough();

    expect(container.textContent).not.toContain("正在调取作战记录");
    expect(container.textContent).toContain("链路建立完毕。博士，终端就绪。");
    // 首次使用同样播幕二（幕二字标与欢迎页同源，淡出衔接无缝）
    expect(sceneNow()).toBe("welcome");
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it("自检报告到位：清单行的值定音（已接入 / 未就绪）；揭示时回调 onReveal", async () => {
    useUIStore.setState({
      bootStatus: {
        database: { ok: true, note: "已接入" },
        knowledge: { ok: true, note: "已接入 · 128 条" },
        memory: { ok: false, note: "目录不可写" },
      },
    });
    const onDone = vi.fn();
    const onReveal = vi.fn();
    await act(async () => {
      root.render(<BootSequence onDone={onDone} onReveal={onReveal} />);
    });

    expect(container.textContent).toContain("数据库 … 已接入");
    expect(container.textContent).toContain("知识库 … 已接入");
    expect(container.textContent).toContain("记忆模块 … 未就绪");
    // 揭示回调在淡出时才触发
    expect(onReveal).not.toHaveBeenCalled();

    await playThrough();

    expect(onReveal).toHaveBeenCalledTimes(1);
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it("恢复失败：多报一行，且不进入「调取」", async () => {
    useUIStore.setState({ restoreFailed: true });
    const onDone = vi.fn();
    await act(async () => {
      root.render(<BootSequence onDone={onDone} />);
    });

    await playThrough();

    expect(container.textContent).toContain("读取失败，按新建处理");
    expect(container.textContent).not.toContain("正在调取作战记录");
    expect(onDone).toHaveBeenCalledTimes(1);
  });
});
