# 资源更新系统 · 作业手册

维护 `src-tauri/resources/` 下的库与静态表。

**脚本只做三件事**：算哪些行、比哪些列、怎么解释页面。其余能力一律复用模块，
不在表脚本里另写——系统靠新增表的差异逐步长全，而不是每张表各起一套。

## 模块分工（动手前先读这里）

| 模块 | 负责 | 不负责 |
| --- | --- | --- |
| `paths.py` | 路径唯一来源（数据源、权威库、resources、人工源） | — |
| `cleaning_game.py` | **游戏数据**富文本清洗 `clean_desc`：`<@xx>text</>` → text、`<color=…>` → text、孤立 `</>` 删除、`<浆果虫>` 这类实体引用**保留** | PRTS 页面语法 |
| `cleaning_wiki.py` | **PRTS 页面**清洗：规则表 `BASE_RULES`（`<br>` → 换行等）+ 各表 `extra_rules`；残留标记表 `looks_dirty`（含 `{{`、`[[`、`'''`、`<div`、`<@`、`<$`、`<nowiki`…） | 游戏数据标签 |
| `update_common.py` | 跨表能力：数据源版本 `read_source_version`、写前备份 `backup_file`、更新记录 `write_record`、PRTS 抓取 `fetch_wikitext`（`?action=raw` + api.php 备路）与 `api`、页面解析与取值 `clean_value`（含出库硬门）、列比较 `same`、结构哨兵 `sentinel_ok` | 表结构、枚举映射、列类型集合 |

表脚本命名：`update_*.py` 增量 / `rebuild_*.py` 重建 / `reclean_*.py` 全量重洗。

## 接线表（数据源 → 清洗 → 检查）

| 表 | 数据源 | 清洗 | 检查 |
| --- | --- | --- | --- |
| `arknights.db` 各表 | 游戏数据 + PRTS 页面 | `cleaning_game` / `cleaning_wiki` + 本表 extra_rules | 结构哨兵 + 列比较 |
| `ranges.json` | 游戏数据 `excel/range_table.json`（纯数字） | 无需 | 与现有文件逐字节比对 |
| `tile_info.json` | 游戏数据 `stage_table.tileInfo` | `cleaning_game.clean_desc` | `looks_dirty` |
| `term.json` | PRTS 术语释义 / 特殊机制 / 技能 | `cleaning_wiki` + 本表 `extra_rules` | 出库硬门（`clean_value` 内有残留就抛错） |
| `abnormal_effects.json` | PRTS 异常效果 / 可抵抗状态 | 同上 | 同上 |
| `slang.json` | PRTS 全站重定向 + 库内名称 | 无需 | 新增条目逐条列出 |

## 动作规范

1. **接新表/新数据源，第一步读模块**：列清现有能力（抓取 / 解析 / 清洗 / 备份 / 记录 / 残留检查），
   判断哪些能直接复用。
2. **缺规则就加在模块里**：清洗规则 → `cleaning_*.py` 的规则表（表专属差异走 `extra_rules`）；
   跨表能力 → `update_common`。**不写进表脚本**。
3. **验收用系统自带信号**：`looks_dirty` 残留检查、数据源版本、更新记录 + 写前备份。
   不自己另列检查项——现成的标记表比临时列表可信。
4. **失败即中止**：解析异常、结构哨兵不过、清洗有残留，都不写文件。

## 产物规则

- 更新记录：`updates/<时间戳>-<表>.json`（字段：脚本 / 运行时间 / 模式 / 数据源 / 表 / 计数 / 新增 / 变化 / 结果）
- 写前备份：`backups/<文件名>.<时间戳>.bak`，每个源文件名各留最近 3 份，不同表互不挤占
- `resources/` 下的 JSON 是 `include_str!` 编译期嵌入，改完要**重新构建**才在工具里生效；
  `arknights.db` 是运行时种子，换文件即可
