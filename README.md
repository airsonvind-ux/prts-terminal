# PRTS Terminal

面向《明日方舟》的桌面 Agent 终端。把游戏机制知识、结构化数据查询、本地工具调用和可选的 MAA 自动化，收进一个可审计的本地工作台。

> 当前版本：`0.1.0`，Windows x64，早期开发阶段。

## 它是什么

PRTS Terminal 不是聊天壳，而是一个带工具循环的领域 Agent：

- **机制问答**：读取随包知识库，按条目和章节组织上下文
- **数据查询**：干员、敌人、关卡、肉鸽、表格等结构化查询
- **本地工具**：文件读写、命令执行、网页抓取，受审批与沙箱约束
- **MAA 集成**：通过独立桥进程调用用户已安装的 MAA，不打包 MAA 本体
- **会话与记忆**：本地会话日志、上下文压缩、长期记忆与技能
- **应用内更新**：签名清单、双源检查、下载与安装保护

## 设计边界

- LLM 只提出工具调用，权限判断、路径检查和命令执行由 Rust 层完成
- 审批、沙箱、凭据加密和更新签名都在后端，不依赖前端自觉
- 知识库、技能和提示词是可版本化的内容资产，不和 Rust 代码混在一起
- MAA 桥是独立进程，只通过本机 IPC 通信；核心不静态链接 AGPL 组件
- 所有运行时数据落在用户数据目录，仓库只保留源码、资源和测试夹具

## 技术栈

- 桌面壳：Tauri 2
- 前端：React 19 + TypeScript + Vite + Zustand
- 后端：Rust 2021 + SQLite
- 数据管线：Python，负责游戏数据与 Wiki 内容的增量维护
- 安装包：Windows NSIS

## 快速开始

### 环境

- Windows 10/11 x64
- Node.js 20+
- Rust stable（MSVC toolchain）
- Python 3.11+（构建 MAA 桥需要；维护数据管线也需要）

### 开发

```powershell
npm ci
npm run tauri dev
```

`npm run tauri dev` 会先按需构建 MAA 桥；桥源码没变时直接复用已有产物。

只跑 Rust 检查时，先构建桥（干净克隆必须做这一步）：

```powershell
npm run build:sidecar
cargo test --lib --manifest-path src-tauri/Cargo.toml
```

只跑前端：

```powershell
npm run dev
```

### 构建安装包

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1
```

正式发布构建需要更新签名私钥；构建前可用 `-DryRun` 检查版本与资源。

### 数据

应用运行时需要 `src-tauri/resources/arknights.db`。它是维护者生成的发布工件，随安装包分发，不随源码仓库提交，也不承诺从公开源码重建。

发布副本带 `meta` 表（`dataVersion`、`generatedAt`、`sourceCommit`、`schemaVersion`）和 `DATA_MANIFEST.json`（DB SHA-256、`dataVersion`、data-contract 结果）。源码可以无数据构建和跑测试；完整应用与发布包需要维护者提供的 DB。

- 数据更新流程：`data-pipeline/maintenance/README.md`
- 数据源、工件与推送：`docs/data-pipeline.md`
- 版本规则：`docs/release-versioning.md`

缺少数据库时，应用会以降级状态启动，查询工具返回明确错误，而不是直接崩溃。

## 测试

```powershell
npm test
npm run build:sidecar
cargo test --lib --manifest-path src-tauri/Cargo.toml
```

测试分层、手工探针和真实数据契约测试见 `docs/testing.md`。

## 目录

```text
src/                 React 前端
src-tauri/           Tauri + Rust 后端
bridge/              MAA 桥（独立 AGPL 进程）
data-pipeline/       游戏数据与知识内容维护
docs/                架构、测试、数据、安全文档
scripts/             开发与人工验证脚本
```

更完整的模块边界见 `docs/architecture.md`。

## MAA

MAA 是可选的。未安装 MAA 时，相关工具报告不可用，应用其他功能不受影响。

`bridge/` 因运行时链接 MaaCore 而以 AGPL-3.0-or-later 发布，和 Apache-2.0 的 PRTS 核心只通过进程间通信交互。详见 `bridge/README.md`。

## 许可证

- PRTS 核心：Apache-2.0，见 `LICENSE`
- `bridge/`：AGPL-3.0-or-later，全文见 `bridge/LICENSE`
- 第三方组件与随包内容：见 `THIRD_PARTY_NOTICES.md`
- MAA 本体不随本项目分发；bridge 只在运行时加载用户已安装的 MaaCore

## 参与

- 贡献指南：`CONTRIBUTING.md`
- 安全问题：`SECURITY.md`
- 变更记录：`CHANGELOG.md`
