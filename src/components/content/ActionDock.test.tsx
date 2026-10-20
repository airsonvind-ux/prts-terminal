// @vitest-environment jsdom
// Action Dock · 审批与更新的优先级、阻塞原因、审批提交

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { ActionDock } from "./ActionDock";
import { useConversationStore } from "../../stores/useConversationStore";
import { useProcessingStore } from "../../stores/useProcessingStore";
import { useUpdaterStore } from "../../stores/useUpdaterStore";
import type { SessionEvent, UpdateStatus } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const mounted: Array<{
  container: HTMLDivElement;
  root: ReturnType<typeof createRoot>;
}> = [];

function renderDock() {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(React.createElement(ActionDock));
  });
  mounted.push({ container, root });
  return {
    text: () => container.textContent ?? "",
    container,
    unmount: () => {
      act(() => root.unmount());
      container.remove();
    },
  };
}

function setApproval(callId = "c1", command = "Get-Process") {
  const conversationId = useConversationStore.getState().activeId!;
  const events: SessionEvent[] = [
    {
      type: "tool/call",
      sessionId: "s1",
      callId,
      toolName: "run_command",
      params: "{}",
    },
    {
      type: "tool/approval-request",
      sessionId: "s1",
      callId,
      command,
      risk: "high",
      mode: "ask",
      ruleId: "run-external-program",
      privilege: "restricted",
      justification: null,
    },
  ];
  useProcessingStore.setState({
    processing: {
      s1: {
        conversationId,
        currentEvents: events,
        agentPhase: "processing",
        mood: "平静",
        moodIntensity: 0,
      } as never,
    },
  });
}

const DOWNLOADED: UpdateStatus = {
  phase: "downloaded",
  currentVersion: "0.1.0",
  devMode: false,
  sources: [],
  available: {
    version: "0.1.1-rc.1",
    currentVersion: "0.1.0",
    notes: "更新说明",
    pubDate: null,
    source: "gitee",
    downloadUrl: "https://example.com/prts.exe",
  },
  source: "gitee",
  downloadedBytes: 1024,
  totalBytes: 1024,
  lastCheckedAt: null,
  warnings: [],
  manualDownloadUrl: null,
  error: null,
};

const CHECK_ERROR: UpdateStatus = {
  ...DOWNLOADED,
  phase: "error",
  available: null,
  downloadedBytes: null,
  totalBytes: null,
  error: "自定义源：error sending request for url (http://127.0.0.1:8765/latest.json)",
  errorStage: "check",
};

beforeEach(() => {
  __invokeMock.mockReset();
  __invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "updater_install_blockers") {
      return Promise.resolve({ activeTurn: true, maaRunning: false });
    }
    return Promise.resolve(true);
  });
  useConversationStore.setState({ conversations: [], activeId: null });
  const id = useConversationStore.getState().createConversation("测试对话");
  useConversationStore.setState({ activeId: id });
  useProcessingStore.setState({ processing: {} });
  useUpdaterStore.setState({
    status: null,
    notice: null,
    blockers: null,
    action: null,
    actionError: null,
    collapsed: false,
    initialized: true,
    dismissedVersion: null,
    dismissedError: false,
  });
});

afterEach(() => {
  for (const item of mounted.splice(0)) {
    try {
      act(() => item.root.unmount());
    } catch {
      // 已卸载
    }
    item.container.remove();
  }
});

describe("Action Dock · 优先级与阻塞", () => {
  it("工具审批优先于已下载更新，消息卡不再承担按钮", () => {
    setApproval("c1", "Remove-Item C:\\tmp\\x.txt");
    useUpdaterStore.setState({ status: DOWNLOADED });
    const dock = renderDock();

    expect(dock.text()).toContain("PRTS 请求运行一条命令");
    expect(dock.text()).toContain("Remove-Item C:\\tmp\\x.txt");
    expect(dock.text()).toContain("确认执行");
    expect(dock.text()).not.toContain("重启并安装");
    dock.unmount();
  });

  it("已下载更新遇到活跃对话 → 安装按钮禁用并说明原因", () => {
    useUpdaterStore.setState({
      status: DOWNLOADED,
      blockers: { activeTurn: true, maaRunning: false },
    });
    const dock = renderDock();

    expect(dock.text()).toContain("重启后生效");
    expect(dock.text()).toContain("有对话正在处理");
    const install = Array.from(dock.container.querySelectorAll("button")).find(
      (button) => button.textContent?.includes("重启并安装")
    );
    expect(install).toBeDefined();
    expect(install!.disabled).toBe(true);
    dock.unmount();
  });

  it("检查失败显示“检查更新失败”，不冒充安装失败", () => {
    useUpdaterStore.setState({ status: CHECK_ERROR });
    const dock = renderDock();

    expect(dock.text()).toContain("检查更新失败");
    expect(dock.text()).toContain("没能连接到更新源");
    expect(dock.text()).not.toContain("更新没有完成");
    dock.unmount();
  });

  it("失败卡可关闭，关闭后不再占住输入区", async () => {
    useUpdaterStore.setState({ status: CHECK_ERROR });
    const dock = renderDock();
    const close = Array.from(dock.container.querySelectorAll("button")).find(
      (button) => button.textContent?.includes("关闭")
    );
    expect(close).toBeDefined();

    await act(async () => {
      close!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(dock.text()).not.toContain("检查更新失败");
    expect(dock.text()).not.toContain("没能连接到更新源");
    dock.unmount();
  });

  it("发现新版本也可关闭，关闭后同一版本不自动弹回", async () => {
    useUpdaterStore.setState({
      status: {
        ...DOWNLOADED,
        phase: "available",
        downloadedBytes: null,
        totalBytes: null,
      },
    });
    const dock = renderDock();
    const close = Array.from(dock.container.querySelectorAll("button")).find(
      (button) => button.textContent?.includes("关闭")
    );
    expect(close).toBeDefined();

    await act(async () => {
      close!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(dock.text()).not.toContain("PRTS 有新版本可用");
    dock.unmount();
  });

  it("点击确认 → invoke resolve_tool_approval 并进入已确认态", async () => {
    setApproval("c2", "Get-Process");
    const dock = renderDock();
    const confirm = Array.from(dock.container.querySelectorAll("button")).find(
      (button) => button.textContent?.includes("确认执行")
    );
    expect(confirm).toBeDefined();

    await act(async () => {
      confirm!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const calls = __invokeMock.mock.calls.filter(
      (call) => call[0] === "resolve_tool_approval"
    );
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({
      request: { sessionId: "s1", callId: "c2", decision: "approved" },
    });
    expect(dock.text()).toContain("已确认，等待执行");
    dock.unmount();
  });
});
