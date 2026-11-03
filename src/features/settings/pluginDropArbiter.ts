// 插件安装拖拽区的仲裁。
//
// Tauri 的 webview 级拖放事件是全局的（对话区也在听），两个监听器都会收到同一次
// drop，谁先谁后没有保证。仲裁靠**最后一次 dragover 是否落在安装区**，而不是靠
// 时间窗猜：
//
// - 安装区每次收到 enter / over 都记下命中结果（外面也算一次，会清掉之前的命中）；
// - drop 时两边读同一个值，谁先读都一样（读不是消费）；
// - 落点归安装区时由它自己处理，对话区让路；不是的话对话区照常收附件；
// - drop 之后用 queueMicrotask 清状态——同步清会让后跑的那个监听器读到 false。
//
// 时间窗只是防陈旧值的兜底：dragend 之类的收尾事件收不到时，别让上一次的命中
// 一直挂着。

const STALE_MS = 1500;

let lastOverHit = false;
let overAt = 0;

/** 安装区在每次 enter / over 时上报：这次指针是不是落在它的矩形里。 */
export function notePluginDragOver(hit: boolean): void {
  lastOverHit = hit;
  overAt = Date.now();
}

/** 这次 drop 归不归安装区（以最后一次 dragover 为准，只读不消费）。 */
export function pluginDropTakesPrecedence(): boolean {
  return lastOverHit && Date.now() - overAt <= STALE_MS;
}

/** drop 处理完就清，避免窗口期里下一次投放被误让路。 */
export function finishPluginDrop(): void {
  lastOverHit = false;
  overAt = 0;
}
