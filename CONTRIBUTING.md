# 贡献

## 开发环境

- Windows 10/11 x64
- Node.js 20+
- Rust stable（MSVC）
- Python 3.11+（只在维护数据管线时需要）

```powershell
npm ci
npm run tauri dev
```

## 提交前

```powershell
npm test
npm run build
cargo test --lib --manifest-path src-tauri/Cargo.toml
```

涉及桥的改动：

```powershell
cargo test --manifest-path bridge/Cargo.toml
```

## 代码约定

- Rust 负责权限、路径、执行、持久化和更新；前端只负责展示与交互
- 工具必须经过统一注册表和参数 schema
- 需要审批的能力不能绕过审批直接执行
- 新行为优先补行为测试，不补实现细节测试
- 不把开发机绝对路径、个人目录、内部工具名写进源码、注释或文档

## 内容资产

知识库、技能、提示词和随包文档属于内容资产。修改内容时：

- 保持 frontmatter 和目录索引一致
- 只追加或修正条目，不覆盖人工维护内容
- 不把临时笔记、个人路径或未验证结论写进随包内容

## 提交

- 一个提交只做一件事；结构迁移和行为修改分开
- 结构迁移提交必须能独立编译和测试
- 删除测试时在提交说明里写清判据
- 不提交 `.tmp/`、数据库、构建日志、本地工具目录

## 文档

- 架构：`docs/architecture.md`
- 测试：`docs/testing.md`
- 数据：`docs/data-pipeline.md`
- 安全：`docs/security.md`
