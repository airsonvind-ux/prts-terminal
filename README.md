# PRTS Terminal

面向《明日方舟》的本地领域 Agent。它把游戏数据、机制知识、工具调用和可选的 MAA 自动化，收在一个跑在自己电脑上的工作台里。

> 当前状态：早期开发阶段，Windows x64。版本与发布记录见 [`CHANGELOG.md`](CHANGELOG.md)。

## 它解决什么问题

通用大模型和专用领域之间，隔着两道很难绕开的沟：

- **时效性**：模型的训练记忆有截止时间，游戏却一直在更新。
- **幻觉**：模型不知道自己的知识缺口，会把旧印象当成当前事实，而且说得很确定。

PRTS 的做法不是让模型“更自信”，而是把这两件事分开处理：

- 时效性交给**本地数据库和知识库**。数据和机制随版本维护，需要时现查，不靠模型记忆。
- 幻觉交给**幻觉显化**。查到的、推出来的、没有把握的，在回答里分开；假设要声明，依据要能复核。模型不该用一个像样的答案盖住缺口。

它不替玩家构建思路，也不替玩家拍板。它提供数据、计算和推演，并尽量让“哪里没有把握”也可见。

## 它能做什么

- **查**：干员、敌人、关卡、装置、召唤物、集成战略等结构化数据，以及按条目和章节组织的机制知识库。
- **算**：伤害、数值关系和战场推演。推演交给确定性引擎，按关卡、路线和速度输出格级事件，不靠模型心算。
- **做**：读写文件、运行受控命令、抓取网页、截取游戏画面；接上 MAA 后，可以跑 MAA 支持的自动化流程。
- **记**：两种记忆。经历型记忆记录工作过程中发生过什么，成长型记忆保存被确认或被纠正后的判断准则。
- **聊得下去**：上下文压缩应对单对话上限；前缀冻结让同一对话的 system prompt 与工具定义保持稳定，减少缓存失效。
- **更新**：应用内检查更新，清单带签名，Gitee 主源，GitHub 独立兜底。

MAA 是可选的。没安装 MAA 时，相关工具会明确报告不可用，其他功能不受影响。

## 设计边界

- LLM 只提出工具调用，权限判断、路径检查和命令执行由 Rust 层完成。
- 审批、沙箱、凭据加密和更新签名在后端，不依赖前端自觉。
- 知识库、技能和提示词是可版本化的内容资产，不和 Rust 代码混在一起。
- MAA 桥是独立进程，只通过本机 IPC 通信；核心不静态链接 AGPL 组件。
- 运行时数据落在用户数据目录，仓库只保留源码、资源和测试夹具。

这几条边界的目标不是“限制模型”，而是让模型能碰到什么、做了什么、留下什么，都有明确的位置可查。

## 快速开始

### 环境

- Windows 10/11 x64
- Node.js 22.22.2+（22.x）、24.15+ 或 26+（需满足 jsdom 30；见 `.nvmrc`）
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

本地未签名冒烟构建：

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1 -NoSign
```

`-NoSign` 仍会跑测试、格式、clippy 和打包，但关闭 updater artifact，产物只能用于本地安装/冒烟，不能作为更新包。发布构建可在签名通过后把 `latest.json`/`SHA256SUMS` 写入 `release-staging/<version>/`：

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1 `
  -GiteeRepo <owner>/prts-terminal-release `
  -GitHubRepo <owner>/prts-terminal-release `
  -ReleaseNotes <release-notes.md>
```

## 数据

应用运行时需要 `src-tauri/resources/arknights.db`。它是维护者生成的发布工件，随安装包分发，不随源码仓库提交，也不承诺从公开源码重建。

发布副本带 `meta` 表（`dataVersion`、`generatedAt`、`sourceCommit`、`schemaVersion`）和 `DATA_MANIFEST.json`（DB SHA-256、`dataVersion`、data-contract 结果）。源码可以无数据构建和跑测试；完整应用与发布包需要维护者提供的 DB。

缺少数据库时，应用会以降级状态启动，查询工具返回明确错误，而不是直接崩溃。

## 文档

- [PRTS 使用说明](src-tauri/content/docs/使用说明.md)：面向使用者的完整说明
- [架构](docs/architecture.md)：模块边界、turn 生命周期、事件契约
- [测试](docs/testing.md)：测试分层、手工探针、真实数据契约
- [数据管线](docs/data-pipeline.md)：数据源、产物与更新流程

## 技术构成

Tauri 2 + React 19 + Rust + SQLite。MAA 桥是独立进程；数据管线用 Python 维护。

## 许可证

- PRTS 核心：Apache-2.0，见 [`LICENSE`](LICENSE)
- `bridge/`：AGPL-3.0-or-later，全文见 [`bridge/LICENSE`](bridge/LICENSE)
- 第三方组件与随包内容：见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)
- MAA 本体不随本项目分发；bridge 只在运行时加载用户已安装的 MaaCore

## 参与

- 贡献指南：[`CONTRIBUTING.md`](CONTRIBUTING.md)
- 安全问题：[`SECURITY.md`](SECURITY.md)
- 变更记录：[`CHANGELOG.md`](CHANGELOG.md)
