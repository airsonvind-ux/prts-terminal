# PRTS Terminal

面向《明日方舟》的本地领域 Agent。它把游戏数据、机制知识、工具调用和可选的 MAA 自动化，收在一个跑在自己电脑上的工作台里。

> 当前状态：早期开发，Windows 10/11 x64。版本与发布记录见 [`CHANGELOG.md`](CHANGELOG.md)。

## 设计意图

通用大模型和专用领域之间，通常隔着两道沟：

- **时效性**：模型的训练记忆有截止时间，游戏却一直在更新。
- **不确定性**：模型不知道自己的知识缺口，容易把旧印象当成当前事实。

PRTS 不试图让模型“更自信”，而是把事实、推理和不确定分开：

- 时效性交给**本地数据库、知识库和数据管线**，需要时现查，不靠模型记忆。
- 不确定性交给**幻觉显化**：查到的、推出来的、没有把握的分开写，假设和依据可复核。
- 执行交给**受权限约束的工具**：模型只提出调用，路径、权限、命令和更新签名由 Rust 层判断。

它不替玩家构建思路，也不替玩家拍板。它提供数据、计算和推演，并尽量让“哪里没有把握”也可见。

## 概念

| 概念 | 在这里指什么 |
|---|---|
| PRTS | 本项目的桌面 Agent，不是游戏本体，也不是云端服务 |
| turn | 一次用户输入到模型停止请求工具的完整执行轮次 |
| 事件契约 | 后端向前端发送的稳定事件形状，前端据此投影消息和状态 |
| 幻觉显化 | 把已知、推断、未知分开表达，并声明假设和依据 |
| 知识库 | 按条目和章节组织的机制知识，属于可版本化内容资产 |
| 数据管线 | 从游戏数据源重建 SQLite 数据库、manifest 和随包内容的流程 |
| MAA | MaaAssistantArknights，可选的外部自动化工具；PRTS 通过独立 bridge 进程与本机 IPC 通信 |
| 模型协议适配 | 把不同模型协议转换成统一请求/事件结构的后端层 |

## 现在能做什么

- **查**：干员、敌人、关卡、装置、召唤物、集成战略等结构化数据，以及机制知识库。
- **算**：伤害、数值关系和战场推演；推演由确定性引擎输出格级事件，不靠模型心算。
- **做**：读写文件、运行受控命令、抓取网页、截取游戏画面；接上 MAA 后可执行其支持的自动化流程。
- **记**：经历型记忆记录过程，成长型记忆保存被确认或纠正后的判断准则。
- **聊得下去**：上下文压缩应对单对话上限；前缀冻结减少重复请求导致的缓存失效。
- **更新**：应用内检查更新，清单带签名，Gitee 主源，GitHub 独立兜底。

MAA 是可选的。没有安装 MAA 时，相关工具会明确报告不可用，其他功能不受影响。

## 快速开始

### 环境

- Windows 10/11 x64
- Node.js 22.22.2+（22.x）、24.15+ 或 26+（需满足 jsdom 30；见 `.nvmrc`）
- Rust stable（MSVC toolchain）
- Python 3.11+（构建 MAA bridge 和维护数据管线需要）

### 开发

```powershell
npm ci
npm run tauri dev
```

`npm run tauri dev` 会先按需构建 MAA bridge；bridge 源码没变时复用已有产物。

只跑 Rust 检查时，先构建 bridge（干净克隆必须做这一步）：

```powershell
npm run build:sidecar
cargo test --lib --manifest-path src-tauri/Cargo.toml
```

只跑前端：

```powershell
npm run dev
```

### 测试与构建

```powershell
npm test
npm run build
```

本地构建安装包：

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1
```

正式发布构建需要 Tauri 更新签名私钥。只想本地冒烟时：

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1 -NoSign
```

`-NoSign` 产物不能作为更新包。构建与本地验证见 [`CONTRIBUTING.md`](CONTRIBUTING.md)，签名和发布细节见 `build.ps1`。

## 仓库结构

```text
src/                 React 前端
src-tauri/           Rust 后端、Tauri 配置、资源与测试
bridge/              MAA bridge（独立进程，AGPL-3.0-or-later）
data-pipeline/       数据源同步、数据库重建、维护记录
scripts/             数据/应用发布、签名校验与辅助脚本
evals/               离线评测与接地检查
docs/                架构、测试、数据管线、安全等开发文档
public/              静态资源
```

运行时数据库不在源码仓中。`src-tauri/resources/arknights.db` 是维护者生成的发布工件，随安装包分发；源码可以无数据构建和跑测试。

## 数据与发布

应用版本和数据版本独立，README 不写死具体版本号：

- 应用版本：见 [`CHANGELOG.md`](CHANGELOG.md) 和发布页
- 数据版本：由 DB 的 `meta` 与 `DATA_MANIFEST.json` 记录，随数据 Release 发布

数据库、`DATA_MANIFEST.json` 和校验信息作为数据 Release 的资产提供，发布工作流按数据版本拉取固定输入。这样数据刷新不需要把数据库提交进源码仓，也不需要让应用版本和数据版本互相绑死。

发布说明和版本记录见 [`CHANGELOG.md`](CHANGELOG.md)。缺少数据库时，应用会以降级状态启动，查询工具返回明确错误，而不是直接崩溃。

## 文档

- [使用说明](src-tauri/content/docs/使用说明.md)：面向使用者的完整说明
- [架构](docs/architecture.md)：模块边界、turn 生命周期、事件契约
- [测试](docs/testing.md)：测试分层、手工探针、真实数据契约
- [数据管线](docs/data-pipeline.md)：数据源、产物与更新流程
- [安全](docs/security.md)：信任模型、权限边界、更新与凭据

## 技术构成

Tauri 2 + React 19 + Rust + SQLite。MAA bridge 是独立进程；数据管线用 Python 维护。

## 仓库与下载

- 源码仓库：<https://github.com/airsonvind-ux/prts-terminal>
- GitHub 发布页：<https://github.com/airsonvind-ux/prts-terminal-release/releases>
- Gitee 发布页：<https://gitee.com/airsonvind0/prts-terminal-release/releases>

源码在 GitHub 维护；Gitee 只保留发布版与更新清单，作为国内下载源。

## 参与

- 贡献指南：[`CONTRIBUTING.md`](CONTRIBUTING.md)
- 行为准则：[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md)
- 安全问题：[`SECURITY.md`](SECURITY.md)
- 变更记录：[`CHANGELOG.md`](CHANGELOG.md)
- 第三方组件：[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)

## 许可证

- PRTS 核心：Apache-2.0，见 [`LICENSE`](LICENSE)
- `bridge/`：AGPL-3.0-or-later，全文见 [`bridge/LICENSE`](bridge/LICENSE)
- MAA 本体不随本项目分发；bridge 只在运行时加载用户已安装的 MaaCore
