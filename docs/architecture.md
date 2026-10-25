# 架构

PRTS Terminal 的目标不是把 LLM 包进一个聊天窗口，而是把“模型决策”和“本地执行”分开：模型负责提出下一步，Rust 负责判断这一步能不能做、怎么做、做完留下什么。

## 目录

```text
src-tauri/
├─ src/
│  ├─ app/          Tauri 适配与命令
│  ├─ agent/        turn 循环、上下文、记忆、技能、知识索引
│  ├─ llm/          模型协议适配
│  ├─ tools/        工具注册表与领域工具
│  ├─ storage/      会话、数据库、设置、凭据、附件、缓存
│  ├─ runtime/      审批、沙箱、路径策略、取消、插话
│  ├─ integrations/ MAA
│  └─ platform/     更新、通知、可观测性、机器信息
└─ content/         知识库、技能、提示词、随包文档
```

`content/` 是内容资产：`content/prompts/` 通过 `include_str!` 编译进二进制，`knowledge/`、`skills/`、`docs/` 作为 Tauri resources 随包分发；`bridge/` 是独立 AGPL crate。

前端按 feature 组织：

```text
src/
├─ app/shell/      窗口外壳与动画
├─ features/       chat / settings / overlays / guide
├─ shared/ui/      跨 feature 的通用组件
├─ stores/         状态与事件投影
├─ hooks/          生命周期与交互 Hook
└─ animation/      外壳动画引擎
```

## 分层

```text
app        Tauri 适配层：命令、DTO、启动装配
agent      Agent 内核：turn 循环、上下文、事件、压缩、技能与知识索引
llm        模型适配器：OpenAI 兼容、Responses、Anthropic
tools      工具注册表与领域工具
storage    会话、数据库、设置、凭据、附件、记忆
runtime    本地执行与安全：审批、沙箱、路径、取消
integrations  MAA 等外部系统适配
platform   更新、通知、可观测性、机器信息
content    知识库、技能、提示词、随包文档
bridge     MAA 桥：独立进程，只经本机 IPC 通信
```

依赖方向：

```text
app → agent / tools / llm / storage / runtime / platform
agent → llm / tools / storage
tools → storage / integrations
```

`agent` 不依赖 Tauri，`tools` 不依赖 UI；`content/prompts` 被 Rust 编译期嵌入，`knowledge/skills/docs` 不被代码反向依赖。

## 一次 turn 的生命周期

1. 前端提交本轮输入与会话元数据；`primary` 模式下不再提交完整历史。
2. `app` 层检查是否有活跃 turn，从官方 session log 读取历史，组装冻结的 system prompt 与工具定义。
3. `agent` 播种会话日志，进入 step 循环。
4. LLM 流式返回 reasoning、文本和工具调用；`EventSink` 把持久事实写入 session log，同时推送 IPC。
5. 工具调用进入审批与执行阶段：权限判断、路径检查、命令沙箱、取消点。
6. 工具结果回填上下文，进入下一个 step，直到模型不再请求工具或触发停滞/取消。
7. `turn/end` 记录终止原因；后端把本轮事件追加到官方 log，前端把事件投影为消息与状态。

## 事件契约

会话日志是事实来源，IPC 是给前端的投影缓存。

- `assistant/chunk`、`tool/call`、`tool/result`、`step/end`、`turn/end` 属于持久事实
- `assistant/chunk.content` 是展示文本（情绪标签已剥离）；`rawContent` 是模型原文，历史派生优先用它
- `phase` 是纯 UI 信号，不写入日志
- `step/end` 必须同时写日志和推 IPC；只写日志会让前端的上下文指示器停在上一轮
- 前端不得自行推断“后端一定已经收到/处理了某个事件”

## 工具与审批

- 工具通过统一注册表暴露名称、描述、参数 schema 和执行函数
- 执行前先过参数校验，再过权限与路径策略
- 需要审批的调用进入审批卡；用户拒绝、取消或超时都产生明确的工具结果
- 命令执行使用受限令牌、Job 对象和临时 ACL，不把“完整 shell”当成默认能力
- 工具返回结构化 `display` 与文本摘要，前端不解析自然语言来决定状态

## 存储

- `session log` 追加写入，按 turn 归档；派生消息由日志计算，不在两处维护
- `primary` 模式下后端是唯一 writer；前端 `append_turn` 只更新元数据
- 首轮建文件原子替换；追加前修复末尾半行；旧 jsonl 只读兼容
- SQLite 保存领域数据，启动时从随包种子同步到用户数据目录
- 设置、凭据、附件和记忆各自有明确的数据根与迁移规则
- 凭据使用本机用户绑定的加密存储，明文不落盘

## 内容资产

知识库、技能和随包使用说明属于运行时内容，由 `content/` 维护，通过 Tauri `resources` 映射进入安装包。系统提示词（`content/prompts/`）是另一类资产：它们通过 `include_str!` 在编译期嵌入二进制，修改后必须重新编译 Rust。内容更新不应该改变代码模块的路径语义。

## 外部集成

MAA 桥是独立进程，运行时加载用户自己的 `MaaCore.dll`。PRTS 核心只通过本机 TCP 帧协议与它通信，不静态链接 AGPL 组件。桥不可用时，MAA 相关工具降级，不影响其他功能。

## 更新链

更新检查、版本比较、下载、磁盘预检和安装由 Rust 层负责；清单必须带签名，签名版本与清单版本不一致时拒绝安装。Gitee 是主源，GitHub 是独立兜底源。
