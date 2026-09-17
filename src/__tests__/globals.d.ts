// 测试全局辅助函数类型声明
// __simulateAgentEvent / __invokeMock 在 setup.ts 中挂载到 globalThis

interface AgentEvent {
  type: string;
  sessionId: string;
  [key: string]: unknown;
}

declare global {
  var __simulateAgentEvent: (event: AgentEvent) => void;
  var __invokeMock: import("vitest").Mock;
}

export {};
