// src/scrollActivity.ts
// Windows 按需滚动条：滚动中给滚动元素短暂添加 prts-scroll-active，
// 停止滚动 1000ms 后移除（显色由 .prts-scroll-active 驱动）。
//
// 悬停/拖动加深由伪元素 :hover 独立驱动（styles/scrollbar.css）：
// 指针压在滑块上时始终显示加深态——"加深 ⇔ 可拖动"精确一致，且
// :hover 由浏览器按部件判定，不受「滚动条槽吞鼠标事件」影响（无需
// 任何坐标判定）。悬停保持显示的意图同样由 :hover 天然满足。
//
// 实现：document 捕获阶段监听 scroll（scroll 不冒泡，捕获可覆盖
// 全部滚动容器）；停止滚动 1s 后移除类。

const IDLE_MS = 1000;

const timers = new WeakMap<Element, number>();

/** 安装全局滚动监听。返回卸载函数（测试用；生产在 main.tsx 调用一次）。 */
export function installScrollActivity(): () => void {
  const onScroll = (event: Event): void => {
    const target = event.target;
    if (!(target instanceof Element)) return;

    target.classList.add("prts-scroll-active");
    const prev = timers.get(target);
    if (prev !== undefined) window.clearTimeout(prev);
    timers.set(
      target,
      window.setTimeout(() => {
        timers.delete(target);
        target.classList.remove("prts-scroll-active");
      }, IDLE_MS)
    );
  };

  document.addEventListener("scroll", onScroll, {
    capture: true,
    passive: true,
  });

  return () => {
    document.removeEventListener("scroll", onScroll, true);
  };
}
