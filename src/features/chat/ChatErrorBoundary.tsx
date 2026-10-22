// 对话区渲染错误边界（R-52）。
//
// 恢复数据的形状校验之外，最后一层兜底：投影/渲染抛错时不再让整个应用白屏。
// 外壳、设置覆盖层与 Toast 仍在边界之外，用户还能看到提示并重新加载。

import { Component, type ErrorInfo, type ReactNode } from "react";
import styles from "./ChatErrorBoundary.module.css";

interface Props {
  children: ReactNode;
}

interface State {
  failed: boolean;
}

export class ChatErrorBoundary extends Component<Props, State> {
  state: State = { failed: false };

  static getDerivedStateFromError(): State {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("[chat] 对话界面渲染失败", error, info);
  }

  render(): ReactNode {
    if (this.state.failed) {
      return (
        <div className={styles.container} role="alert">
          <div className={styles.title}>对话界面加载失败</div>
          <button
            className={styles.reload}
            type="button"
            onClick={() => window.location.reload()}
          >
            重新加载
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
