# 测试

测试按“保护什么”分层，而不是按文件类型堆在一起。

## 四层

| 层 | 目的 | 运行方式 |
|---|---|---|
| unit | 纯函数、解析、格式化、策略判断 | 默认测试 |
| contract | 事件序列化、工具 schema、数据库 schema、数据契约 | 默认可快速运行 |
| integration | 多模块协作：Agent 循环、审批、会话恢复 | 使用临时目录和 mock LLM |
| manual | 真实 Tauri 窗口、MAA、更新安装、游戏数据 | 人工执行并记录结果 |

## 判据

一条测试值得保留，至少满足一条：

- 保护用户可见行为
- 保护一个已经发生或代价很高的失败模式
- 保护安全边界、数据完整性或恢复路径

同时应满足：

- 失败时原因明确，而不是“快照变了”
- 不依赖不稳定的外部网络、真实游戏窗口或未固定版本的数据
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
cargo test --manifest-path bridge/Cargo.toml
```

CI 覆盖前端测试/构建、Rust 编译和测试编译；当前 CI **不执行** Rust 单元测试，因为真实数据库未进入仓库。

## 当前实际分层（2026-10-21）

- 默认 `cargo test --lib` 仍包含依赖 `src-tauri/resources/arknights.db` 的数据契约用例；运行前需要本地准备该文件（已被 `.gitignore` 排除）。
- 这些用例断言具体游戏数据（例如关卡名、干员形态、敌人字段），数据更新后变红是预期行为；不应通过改软断言来“修绿”。
- 数据契约测试尚未拆成独立命令或固定夹具，这是已知债务；CI 只执行 `cargo test --lib --no-run`，因此不依赖数据库。
- 没有数据库时，默认 Rust 测试会失败，而不是静默跳过；这是当前有意保留的显式失败。

## 真实数据

依赖 `arknights.db` 的检查属于数据契约测试，不放进默认单测。它们应该：

- 使用固定版本的数据库或固定夹具
- 通过单独命令运行
- 在失败时报告数据库版本/哈希和缺失的字段

手工查询和真机探针放在 `scripts/manual/`，不冒充自动化测试。

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
