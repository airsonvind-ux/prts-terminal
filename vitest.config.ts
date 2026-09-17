import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Node 环境：测试纯逻辑（Store + Hook），不涉及 DOM
    environment: "node",
    // 全局 setup 文件：在全部测试运行前执行一次，注册 Tauri API Mock
    setupFiles: ["./src/__tests__/setup.ts"],
    // CSS 真实处理（默认关闭时 ?raw 导入为空串）：契约测试读取样式原文
    css: true,
    // 默认 globals: false，需显式 import describe/it/expect
  },
});
