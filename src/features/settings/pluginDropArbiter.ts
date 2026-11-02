// 插件安装拖拽区的仲裁。
//
// Tauri 的 webview 级拖放事件是全局的（对话区也在听），两个监听器都会收到同一次
// drop，谁先谁后没有保证。这里用「短时武装」代替顺序假设：指针在安装区上方时武装
// 1.5 秒，对话区在这段时间里放弃这次投放。

let armedUntil = 0;

/** 指针进入安装区：武装（覆盖随后到达的 drop）。 */
export function armPluginDrop(): void {
  armedUntil = Date.now() + 1500;
}

/** 指针离开安装区：解除。 */
export function disarmPluginDrop(): void {
  armedUntil = 0;
}

/** 对话区在 drop 前问一次：这次投放是不是归安装区。 */
export function isPluginDropArmed(): boolean {
  return Date.now() < armedUntil;
}
