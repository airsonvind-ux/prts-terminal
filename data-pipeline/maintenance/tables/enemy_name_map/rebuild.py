# -*- coding: utf-8 -*-
"""enemy_name_map · 重建（图鉴名 ↔ 数据名）

背景
    敌人的名字有两个来源：`enemy_handbook_table.json` 的图鉴名（玩家可见、PRTS 页面名、
    PRTS 查询用）与 `enemy_database.json` 的数据名（游戏内部，`enemies` 表存的是它）。
    两边大多相同，少数不同；PRTS 按图鉴名查，直接查 `enemies` 会落空，故需要这张映射。

数据源（本地，无 PRTS、无人工列）
    excel/enemy_handbook_table.json       图鉴名
    levels/enemydata/enemy_database.json  数据名（只取 level 0 那一条）
    enemies 表                             名称集合（判「同一图鉴下的另一种形态」）

判定（既有方案，不动）
    只放两边都有名字、且不同的那些；名字相同的直接走 `enemies.名称`
    排除「同一图鉴下的另一种形态」：数据名 = 图鉴名 + 括号后缀，且表里另有一条
    同名常规敌人 → 不入表（否则「拟南兽（友方）」会被查成「拟南兽」）

输出
    arknights.db 的 enemy_name_map(敌人ID, 图鉴名, 数据名)
    写库：备份 → DROP → CREATE → 全量 INSERT（单事务）

依赖
    **enemies 必须先跑**：友方形态的判定要用 enemies 表的名称集合。

跳过条件
    重建结果与库内逐行逐列完全一致 → 不写库、不写记录

用法
    python rebuild.py --dry-run
    python rebuild.py
"""

import json
import sqlite3
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import table_base as tb  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402

TABLE = "enemy_name_map"
MAX_DETAIL = 200

DDL = """
CREATE TABLE enemy_name_map (
    敌人ID TEXT PRIMARY KEY,
    图鉴名 TEXT NOT NULL,
    数据名 TEXT NOT NULL
)
"""

INSERT_COLS = [("敌人ID", "敌人ID"), ("图鉴名", "图鉴名"), ("数据名", "数据名")]

ENEMY_HANDBOOK = paths.GAMEDATA / "excel" / "enemy_handbook_table.json"
ENEMY_DB = paths.GAMEDATA / "levels" / "enemydata" / "enemy_database.json"

STATS = {}


def norm(text):
    return (clean_desc(text) or "").strip()


def load_names():
    """→ (图鉴名 {ID: 名}, 数据名 {ID: 名})，都已清洗。"""
    with open(ENEMY_HANDBOOK, encoding="utf-8") as f:
        handbook = json.load(f)
    hb = {}
    for eid, v in handbook.get("enemyData", {}).items():
        nm = norm(v.get("name"))
        if nm:
            hb[eid] = nm

    with open(ENEMY_DB, encoding="utf-8") as f:
        database = json.load(f)
    db = {}
    for entry in database.get("enemies", []):
        eid = entry.get("Key")
        if not eid:
            continue
        for variant in entry.get("Value", []):
            raw = variant.get("enemyData", {}).get("name")
            if isinstance(raw, dict):
                raw = raw.get("m_value")
            nm = norm(raw)
            if nm and eid not in db:
                db[eid] = nm
    return hb, db


def is_friendly_variant(display, data, plain_names):
    """数据名 = 图鉴名 + 括号后缀，且表里另有一条同名常规敌人 → 同一图鉴下的另一种形态。"""
    if not data.startswith(display):
        return False
    suffix = data[len(display):]
    return suffix.startswith(("（", "(")) and display in plain_names


def build(old=None, db=None):
    """两边都有名且不同 → 排除友方形态 → {敌人ID: 行}。"""
    hb, db_names = load_names()
    conn = sqlite3.connect(db)
    try:
        plain_names = {r[0] for r in conn.execute("SELECT DISTINCT 名称 FROM enemies") if r[0]}
    finally:
        conn.close()

    rows, skipped = {}, []
    for eid in sorted(set(hb) & set(db_names)):
        if hb[eid] == db_names[eid]:
            continue
        if is_friendly_variant(hb[eid], db_names[eid], plain_names):
            skipped.append([eid, hb[eid], db_names[eid]])
            continue
        rows[eid] = {"敌人ID": eid, "图鉴名": hb[eid], "数据名": db_names[eid]}

    STATS.update({
        "图鉴ID数": len(hb),
        "数据名ID数": len(db_names),
        "交集": len(set(hb) & set(db_names)),
        "同一图鉴下的另一种形态（不映射）": skipped[:MAX_DETAIL],
    })
    print("  图鉴 %d | 数据名 %d | 交集 %d | 需映射 %d | 不映射 %d"
          % (len(hb), len(db_names), len(set(hb) & set(db_names)), len(rows), len(skipped)))
    for eid, display, data in skipped[:10]:
        print("    不映射: %-26s 图鉴名=%s 数据名=%s" % (eid, display, data))
    return rows


def verify(conn):
    total = conn.execute("SELECT COUNT(*) FROM enemy_name_map").fetchone()[0]
    out = {"行数": total}
    out.update(STATS)
    print("  复核: enemy_name_map %d 行" % total)
    return out


SPEC = tb.RebuildDbSpec(
    table=TABLE,
    create_sql=DDL,
    insert_cols=INSERT_COLS,
    build=build,
    key="敌人ID",
    verify=verify,
)

if __name__ == "__main__":
    sys.exit(tb.run_rebuild_db(SPEC))
