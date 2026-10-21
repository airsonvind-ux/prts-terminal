# 数据管线

数据管线负责把公开的游戏数据与 Wiki 内容，转换成应用运行时读取的 SQLite 数据库和 JSON 表。

## 目录

```text
data-pipeline/
├─ maintenance/     当前增量更新系统（权威入口）
├─ transform-to-sqlite.py
└─ verify_db.py
```

早期的 `db_builder/` 一次性脚本已从当前仓库移除，需要参考时查 Git 历史。

当前流程、更新顺序、待办和回滚方式以 `data-pipeline/maintenance/README.md` 为准。

## 数据源

- ArknightsGameData 的 excel 表与关卡文件
- PRTS Wiki 页面
- `maintenance/sources/` 下的人工维护内容

数据源不提交进仓库。首次开发需要按维护文档浅克隆到 `data-pipeline/data-source/`。

## 产物

- `src-tauri/resources/arknights.db`：运行时数据库种子
- `src-tauri/resources/*.json`：术语、异常效果、范围、黑话、格子信息等表

数据库不提交进 Git。发布构建前必须先生成，并在发布记录中写明数据版本或校验和。

## 路径约定

脚本不应写死开发机的绝对路径。数据源根、数据库输出和资源目录应通过以下方式之一解析：

- 仓库相对路径
- 环境变量（如 `PRTS_DATA_ROOT`）
- 命令行参数

`db_builder/` 中的历史脚本已经移出当前仓库，不再作为新脚本的参考。

## 更新原则

- 新增内容只追加，不覆盖人工维护内容
- 有变化或失败才写更新记录
- 写库前自动备份，备份不提交
- 结构哨兵失败时中止，不写入半套数据
- 更新后运行 `verify_db.py`，再运行 `.\scripts\data-contract.ps1`

## 发布前检查

1. 数据源版本已记录
2. `arknights.db` 可被应用打开
3. 关键表行数与上一版对比无异常
4. JSON 表可解析
5. 安装包内资源路径正确
