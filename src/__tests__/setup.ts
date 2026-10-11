// src/__tests__/setup.ts
// 全局 Tauri API Mock 层（vitest setupFiles 自动加载，在所有测试前执行一次）
//
// 设计：
//   本文件用 vi.mock() 在模块解析阶段替换 @tauri-apps/api 的两个模块。
//   替换后，任何源代码中 import 的 invoke/listen 都会走 Mock 版本，
//   不需要修改任何源代码。
//
// 提供的测试辅助：
//   __simulateAgentEvent(event)  手动推送一个 AgentEvent，触发 useAgentEvents
//                               注册的回调（等同于 Rust 后端推送 "agent-event"）
//   __invokeMock                 vi.fn 实例，可访问 .mock.calls 检查调用历史

import { vi } from "vitest";

// ── 类型 ──

interface AgentEvent {
  type: string;
  sessionId: string;
  [key: string]: unknown;
}

// ── Mock listen ──

let agentEventCallback: ((event: { payload: AgentEvent }) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    (eventName: string, callback: (event: { payload: AgentEvent }) => void) => {
      if (eventName === "agent-event") {
        agentEventCallback = callback;
      }
      // 返回 Promise<() => void>：resolve 后得到 unlisten 函数
      return Promise.resolve(() => {
        if (eventName === "agent-event") {
          agentEventCallback = null;
        }
      });
    }
  ),
}));

/** 手动推送 AgentEvent（模拟后端通过 Tauri IPC 推送），注入到 useAgentEvents 注册的回调 */
(globalThis as Record<string, unknown>).__simulateAgentEvent = (
  event: AgentEvent
): void => {
  if (!agentEventCallback) {
    console.warn(
      "[Mock] __simulateAgentEvent 被调用，但 useAgentEvents 尚未注册监听器。" +
        "请确保调用方组件已挂载（测试中需先渲染使用 useAgentEvents 的组件）"
    );
    return;
  }
  agentEventCallback({ payload: event });
};

// ── Mock invoke ──

// 默认按命令名路由：get_reminder_data（Reminder 数据源）返回空载荷，
// list_conversations（Phase 4 会话恢复列表）返回空列表（无持久数据/首次使用），
// get_settings（设置页扩展）返回默认 Settings（空 llm/ask/空 workspace/置顶 true），
// get_app_meta 返回占位路径。其余命令 resolve undefined。测试可用
// __invokeMock.mockImplementationOnce / mockResolvedValueOnce 覆盖单次行为。
// append_turn / update_conversation_meta / delete_conversation 为 fire-and-forget
// 写命令（调用方 .catch 静默），默认 resolve undefined 即兼容。
const __defaultInvokeImpl = (cmd: string) => {
  if (cmd === "get_reminder_data") {
    return Promise.resolve({ seq: 0, dims: [], events: [] });
  }
  if (cmd === "list_conversations") return Promise.resolve([]);
  // 设置页扩展：默认空配置（文件缺失 = 默认值语义）
  if (cmd === "get_settings") {
    return Promise.resolve({
      schema_version: 2,
      llm: {
        active_provider: "",
        providers: {},
        thinking_level: null,
      },
      exec: { mode: "ask" },
      workspace: { workspace_root: "" },
      window: { always_on_top: true },
    });
  }
  // 启动自检（启动演出的清单行翻牌）：默认三件就位，测试可覆盖单次行为
  if (cmd === "get_boot_status") {
    return Promise.resolve({
      database: { ok: true, note: "已接入" },
      knowledge: { ok: true, note: "已接入 · 2 条" },
      memory: { ok: true, note: "已接入" },
    });
  }
  if (cmd === "get_app_meta") {
    return Promise.resolve({
      version: "0.1.0",
      dataDir: "C:/fake/data",
      defaultWorkspaceRoot: "C:/fake/workspace",
      dbPath: "C:/fake/db",
      knowledgeDir: "C:/fake/kb",
      skillsDir: "C:/fake/skills",
    });
  }
  if (cmd === "list_skills") return Promise.resolve([]);
  // 模型页：供应商列表默认空（测试按需 mockImplementation）
  if (cmd === "list_providers") return Promise.resolve([]);
  if (cmd === "list_models") return Promise.resolve([]);
  if (cmd === "reveal_api_key") return Promise.resolve("");
  // 记忆页（Phase 4 任务二 · 批 3）：默认空总览/空列表；写命令 resolve undefined
  if (cmd === "memory_overview") {
    return Promise.resolve({
      growth: [],
      recent: [],
      previous: "",
      experienceCount: 0,
    });
  }
  if (cmd === "memory_experience_list") return Promise.resolve([]);
  // open_path（资源文件打开）与目录选择器默认静默/取消
  if (cmd === "open_path") return Promise.resolve(undefined);
  if (cmd === "choose_workspace_directory") return Promise.resolve(null);
  // 技能安装（批 3）：选择器默认取消；安装 / 移除按需 mockImplementation
  if (cmd === "pick_skill_file") return Promise.resolve(null);
  if (cmd === "pick_skill_folder") return Promise.resolve(null);
  if (cmd === "install_skill" || cmd === "remove_skill")
    return Promise.reject("测试未配置技能安装 mock");
  // 空间页（工作区查看）：默认空目录树 / 空搜索 / 资源管理器静默
  if (cmd === "list_workspace_dir") return Promise.resolve([]);
  if (cmd === "search_workspace_files") return Promise.resolve([]);
  if (cmd === "open_workspace_in_explorer") return Promise.resolve(undefined);
  // 引导（说明层）：默认没有看过任何一项（mark_guide_seen 写命令 resolve undefined）
  if (cmd === "get_guide_seen") return Promise.resolve([]);
  return Promise.resolve(undefined);
};

const __invokeMock = vi.fn(__defaultInvokeImpl);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: __invokeMock,
}));

(globalThis as Record<string, unknown>).__invokeMock = __invokeMock;
// 默认路由实现（供测试 beforeEach 恢复——mockImplementation 覆盖后需还原）
(globalThis as Record<string, unknown>).__defaultInvokeImpl = __defaultInvokeImpl;

// ── ResizeObserver polyfill ──
//
// jsdom 没有它，而引导说明层（GuideHost）要观察锚点与根随尺寸重算。
// 这里只做登记（不自动触发）：测试需要重算时手动调 `__trigger()`。
class ResizeObserverMock {
  static instances: ResizeObserverMock[] = [];
  private cb: ResizeObserverCallback;

  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
    ResizeObserverMock.instances.push(this);
  }
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
  /** 测试用：手动触发一次回调 */
  __trigger(): void {
    this.cb([], this as unknown as ResizeObserver);
  }
}

(globalThis as Record<string, unknown>).ResizeObserver = ResizeObserverMock;
