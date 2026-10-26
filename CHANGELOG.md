# 变更记录

## 0.1.0

首个公开版本。

- Tauri 桌面壳与 React 前端
- Agent turn 循环、工具调用、审批、取消与上下文压缩
- 干员、敌人、关卡、肉鸽、表格查询工具
- 随包知识库、技能与使用说明
- 本地会话、记忆、设置与加密凭据
- MAA 独立桥进程（AGPL）与可选集成
- 签名更新：Gitee 主源、GitHub 兜底、磁盘预检与失败阶段区分
- 上下文指示器按 `step/end` 实时更新
- 关于页可查看核心 Apache-2.0、bridge AGPL-3.0 与第三方声明
- 数据工件带 `meta`（dataVersion/generatedAt/sourceCommit/schemaVersion）与 `DATA_MANIFEST.json`（SHA-256、data-contract 结果）；`db_seed` 按 dataVersion 替换运行时库
- 同步 2026-10-25 游戏数据（dataVersion `2026.10.25`）
- 更新敌人、关卡、装置、术语与随包内容
- 修复 data-contract 在仓库根目录执行时未加载 crt-static 配置的问题
- 干员技能点单支持中文数字与带空格的技能序号（如 `专三二技能`、`专3 二技能`）
