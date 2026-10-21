# -*- coding: utf-8 -*-
"""PRTS 终端 — 数据管线路径的唯一来源。

为什么集中到一处：数据源目录名换过一次（网页 ZIP 版 `ArknightsGameData-master`
→ git 克隆版 `ArknightsGameData`），而几十个一次性脚本里曾各自写死它的绝对路径。
**长期脚本（维护脚本、更新脚本）一律从这里取路径，不再各自写死。**

约定：
- 项目根 = 本文件的上一级目录（data-pipeline/ 的上一级）
- 数据源目录默认 `<项目根>/data-source/ArknightsGameData`
  可用环境变量 `PRTS_DATA_SOURCE` 覆盖（指向解包库根，即含 `zh_CN` 的那一级）
- 数据源只读：不要手改解包库内容，更新走 `git fetch` + `git reset --hard`
"""

import os
from pathlib import Path

HERE = Path(__file__).resolve().parent            # data-pipeline/
PROJECT_ROOT = HERE.parent                       # 仓库根

# 旧 db_builder/ 一次性脚本已移出当前仓库；路径统一从这里取。

DATA_SOURCE = Path(
    os.environ.get("PRTS_DATA_SOURCE") or (PROJECT_ROOT / "data-source" / "ArknightsGameData")
).resolve()

GAMEDATA = DATA_SOURCE / "zh_CN" / "gamedata"    # 解包数据根（excel / levels 都在这里）
EXCEL = GAMEDATA / "excel"
LEVELS = GAMEDATA / "levels"

RESOURCES = PROJECT_ROOT / "src-tauri" / "resources"

MAINT = HERE / "maintenance"
# 权威副本：数据更新脚本唯一操作对象（maintenance/db/arknights.db）
DB_PATH = MAINT / "db" / "arknights.db"
# 更新系统的产物目录（被 lib/update_common 引用；脚本里不要自己拼）
BACKUPS = MAINT / "backups"
UPDATES = MAINT / "updates"

# 人工维护的数据源（人工更新，脚本只读；放在仓库内，更新系统不依赖项目目录外的文件）
MANUAL_SOURCES = MAINT / "sources"

# 便于脚本做"数据源是否就位"的显式检查，而不是等到 open() 才报错
def ensure_gamedata() -> None:
    """数据源缺失时给出可执行的提示（而不是裸 FileNotFoundError）。"""
    if not EXCEL.is_dir():
        raise SystemExit(
            "数据源未就位：{}\n"
            "  · 首次准备：见 README / 作业手册的「数据源获取」\n"
            "  · 更新：git fetch --depth 1 origin master && git reset --hard FETCH_HEAD\n"
            "  · 或设置环境变量 PRTS_DATA_SOURCE 指向解包库根目录".format(DATA_SOURCE)
        )
