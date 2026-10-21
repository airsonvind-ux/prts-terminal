// src/components/overlay/OverlayContainer.tsx
// 覆盖层调度器
//
// 职责：订阅 useUIStore.overlay，按类型路由到对应面板。
// 自身无状态，不做业务计算——只做两件事：遮罩关闭 + 面板路由。
//
// 渲染位置：frontFace 内部，position:absolute 叠在 ChatView 之上。
// 外壳动画（翅膀、尾巴）不受影响——覆盖层不遮挡立方体的左面、底面和外延元素。

import { useUIStore } from "../../stores/useUIStore";
import { hasPendingApproval, useProcessingStore } from "../../stores/useProcessingStore";
import { HistoryPanel } from "./HistoryPanel";
import { SettingsPanel } from "../settings/SettingsPanel";
import styles from "./OverlayContainer.module.css";

export function OverlayContainer() {
  const overlay = useUIStore((s) => s.overlay);
  const setOverlay = useUIStore((s) => s.setOverlay);
  const pendingApproval = useProcessingStore(hasPendingApproval);

  // 无覆盖层时完全不渲染，DOM 树中无额外节点
  if (overlay === null) return null;

  /*
   * 遮罩关闭逻辑：点击遮罩（即面板之外的区域）→ setOverlay(null)。
   *
   * 具体例子：用户点 ◷ 打开历史面板，看了几条对话后不想操作了，
   * 点击聊天内容区域（此时被遮罩覆盖）→ 历史面板关闭，恢复对话界面。
   * 与移动端"点击遮罩关闭抽屉"的交互模式一致。
   *
   * 面板内部通过 stopPropagation 阻止事件冒泡到遮罩，
   * 所以点击面板本体不会触发关闭。
   */
  const handleBackdropClick = () => setOverlay(null);

  /*
   * 面板路由：switch 语句根据 overlay 类型决定渲染哪个组件。
   *
   * 同时返回面板尺寸（calc 绑定 --face-size，随窗口缩放自动调整）：
   *   history  → 0.67×0.64，窄面板容纳"标题+时间+删除键"三列
   *   settings → 0.86×0.86，双栏表单所需最小宽度
   */
  const renderPanel = () => {
    switch (overlay) {
      case "history":
        return {
          node: <HistoryPanel />,
          style: {
            width: "calc(var(--face-size) * 0.67)",
            maxHeight: "calc(var(--face-size) * 0.64)",
          },
        };

      case "settings":
        return {
          node: <SettingsPanel />,
          style: {
            width: "calc(var(--face-size) * 0.72)",
            height: "calc(var(--face-size) * 0.66)",
          },
        };

      default:
        return null;
    }
  };

  const panel = renderPanel();

  return (
    <div className={styles.overlay}>
      {/* 遮罩层：绝对定位覆盖整个正面，半透明灰底 */}
      <div className={styles.backdrop} onClick={handleBackdropClick} />
      {/* 面板：定位在遮罩之上，内容由 renderPanel 路由 */}
      {panel && (
        <div
          className={styles.panel}
          style={panel.style}
          /* 阻止面板内点击冒泡到遮罩——否则点面板也会关闭 */
          onClick={(e) => e.stopPropagation()}
        >
          {panel.node}
        </div>
      )}
      {pendingApproval && (
        <button
          className={styles.pendingNotice}
          onClick={() => setOverlay(null)}
          type="button"
        >
          命令待确认 · 返回对话
        </button>
      )}
    </div>
  );
}


