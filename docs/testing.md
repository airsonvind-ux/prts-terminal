# 测试

测试按“保护什么”分层，而不是按文件类型堆在一起。

## 分层

| 层 | 目的 | 运行方式 |
|---|---|---|
| unit | 纯函数、解析、格式化、策略判断 | 默认测试 |
| contract | 事件序列化、工具 schema、数据库 schema | 默认测试 |
| data-contract | 真实 `arknights.db` 的数据快照与查询契约 | 专用命令 |
| integration | 多模块协作：Agent 循环、审批、会话恢复 | 使用临时目录和 mock LLM |
| manual | 真实 Tauri 窗口、MAA、更新安装、游戏数据 | 人工执行并记录结果 |

## 判据

一条测试值得保留，至少满足一条：

- 保护用户可见行为
- 保护一个已经发生或代价很高的失败模式
- 保护安全边界、数据完整性或恢复路径

同时应满足：

- 失败时原因明确，而不是“快照变了”
- 不依赖不稳定的外部网络、真实游戏窗口或未固定版本的数据（数据契约层单独运行）
- 不是对实现细节的复述
- 不与另一条测试重复

以下测试应删除或重写：

- 只验证 mock 被调用了几次，却不验证用户结果
- 依赖真实数据库中的某句文案，数据一更新就红
- 只证明“今天代码这样跑”，换实现就失效
- 没有断言，或断言永远为真

## 命令

```powershell
npm test
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --lib --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo clippy --tests --lib --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --lib --manifest-path src-tauri/Cargo.toml
.\scripts\data-contract.ps1
cargo test --manifest-path bridge/Cargo.toml
```

数据契约套件也可以直接跑：

```powershell
cargo test --lib --manifest-path src-tauri/Cargo.toml --features data-contract
```

CI 覆盖前端测试/构建、Rust 默认单元测试与两种 feature 的测试编译；CI **不执行**数据契约套件，因为真实数据库未进入仓库。

## 当前实际分层（2026-10-21）

- 默认 `cargo test --lib` 不依赖 `src-tauri/resources/arknights.db`；依赖真实库的用例通过 `cfg_attr(not(feature = "data-contract"), ignore = ...)` 跳过。
- 默认套件当前为 1161 passed / 253 ignored。
- 专用 `scripts/data-contract.ps1` 会先检查数据库、打印大小与 SHA-256，再以 `data-contract` feature 运行；当前为 1375 passed / 39 ignored。
- 数据契约测试仍断言具体游戏数据（例如关卡名、干员形态、敌人字段），数据更新后变红是预期行为；不应通过改软断言来“修绿”。
- 没有采用固定夹具是刻意的：固定夹具会把“真实数据更新”这个信号一起冻住。夹具更适合未来给查询逻辑做小样本测试，不替代数据契约。
- 数据库文件被 `.gitignore` 排除；缺少数据库时，默认套件照常运行，专用脚本会明确报错并给出路径。
- Tauri 在编译期会校验 `bundle.resources` 中的数据库路径；debug 构建缺库时，`build.rs` 会创建空占位文件完成校验后立即删除，保证默认测试可跑。release/安装包不走这条放行路径，缺库仍会失败。

## 真实数据

依赖 `arknights.db` 的检查属于数据契约测试，不放进默认单测。当前实现：

- 用 `data-contract` feature 显式开启
- 通过 `scripts/data-contract.ps1` 运行，并输出数据库路径、大小与 SHA-256
- 保留原始断言强度，不改写为“存在即可”
- 失败时能区分“数据库快照变了”和“查询逻辑坏了”

手工查询和真机探针放在 `scripts/manual/`，不冒充自动化测试。

## 接地评测（录制会话）

`evals/grounding/` 提供一套离线评测框架：用 PRTS 已落盘的会话 JSONL，检查工具调用、工具结果、最终回答和压缩摘要之间的对应关系。它不调用模型、不联网、不需要 API Key。

```powershell
python evals/grounding/run.py --check
python evals/grounding/run.py --session <会话文件.jsonl>
.\scripts\grounding-eval.ps1 -Session <会话文件.jsonl>
```

用例格式、失败分类和录制方式见 `evals/grounding/README.md`。当前 3 条合成样例用于验证脚本本身，不代表真实模型评测。

## 前端

- 纯逻辑（投影、清洗、格式化）优先单测
- 组件测试只覆盖用户可见行为，不绑定 DOM 层级和内部 state 名称
- 交互路径用少量集成测试覆盖，不用大量快照替代行为验证

## Agent 路径

至少覆盖：

- 正常文本回复
- 工具调用、审批通过、审批拒绝
- 工具执行失败
- 用户取消
- 上下文压缩
- 会话恢复
- `step/end` 推动上下文用量更新

## 会话历史（C-lite）

- `SessionEvent` 反序列化：旧 jsonl、未知前端事件（mood/turn-aborted）不整体失败
- 投影修复：孤立 `tool_call` 补事实性未执行结果；孤立 `tool_result` 清理；不改写磁盘
- primary 闭环：后端 log 播种 → 处理 → 写官方 jsonl → 重新读出派生
- 前端 primary 路径：`history` 为空、`userContent` 携带本轮正文
- 迁移/崩溃：旧日志可读；首轮原子建文件；追加前修复末尾半行
- primary：后端官方 jsonl 的轮事件与 `derive_messages` 投影一致性；重启恢复
- R-27：Anthropic thinking/signature 解析与请求回传
- R-08：前缀内容指纹变化检测
