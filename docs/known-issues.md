# 已知问题

不是待办清单，是「下次碰到别重新怀疑一遍」的记录。

## 测试

### `MessageList.resizeGuard.test.tsx` 在全量并发下偶发失败

- **现象**：单跑 15 条全过；全量 `npx vitest run` 时偶尔有 1 条断言失败，重跑即过。
- **原因**：这些用例按真实时钟等固定窗口（150–500ms），跑满并发时相邻步骤的定时器错位，断言拿到的是上一步的写入。
- **不是谁引入的**：用 `git stash` 去掉当时改动后同样能复现（2026-11-04 核过）。
- **处理**：重跑；若必须稳定，用 `npx vitest run src/features/chat/MessageList.resizeGuard.test.tsx` 单独跑。不要为了它改生产代码。

## 平台

### 自定义协议的 URL 形态分平台

`prts-view` 在 Windows 的 WebView2 上是 `http://prts-view.localhost/...`，macOS/Linux 才是 `prts-view://localhost/...`。前端不自己判断，走 `view_origin` 命令；`tauri.conf.json` 的 `frame-src` 两形态都写着。

### CSP 只在 `tauri build` 产物里生效

`npx tauri dev` 的页面来自 Vite（`http://localhost:1420`），主窗口 CSP 不生效；验证 CSP、`frame-src`、自定义协议时要用 `npx tauri build --debug --no-bundle` 出的二进制（页面是 `http://tauri.localhost/`）。
