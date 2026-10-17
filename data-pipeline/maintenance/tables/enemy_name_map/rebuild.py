"""
PRTS 终端 — 重建 `enemy_name_map`（图鉴名 ↔ 数据名）

背景
----
敌人的名字有两个来源：

| 来源                          | 名字     | 谁在用                        |
|-------------------------------|----------|------------------------------|
| `enemy_handbook_table.json`   | 图鉴名    | 玩家可见；PRTS 页面名；PRTS 查询用 |
| `enemy_database.json`         | 数据名    | 游戏内部；`enemies` 表记录的就是它 |

两边名字大多相同，少数不同（本轮 19 组）。PRTS 按图鉴名查，而 `enemies` 表里存的是数据名，
直接查会落空 —— 所以要一张映射表把两边接起来。

本表只放**两边都有名字、且不同**的那些（其余的用 `enemies.名称` 直接就能查到）。

一个例外（Air 2026-10-14 指出）
------------------------------
多数名字差异是同一个敌人的两个名字（如 图鉴名“浆果磐蟹” / 数据名“带果壳的磐蟹”）。
但有一类不同：**同一个图鉴下真有两只敌人** —— 常规形态与友方形态，各自是不同的 ID，
图鉴却把它们都标成同一个名字。例如：

    enemy_7051_xbcas     拟南兽       ← 常规
    enemy_7053_xbcasz    拟南兽（友方）  ← 图鉴名也是“拟南兽”

这种不属于“名字不一致”，而是两个不同的单位。拿它做映射，会让“拟南兽（友方）”
在查询里显示成“拟南兽”（就是 Air 遇到的那个毛病）。所以判定为这种形态的**不入表**：
数据名 = 图鉴名 + 括号后缀，且 `enemies` 表里另有一条 名称 = 图鉴名 的常规敌人。

用法
----
    python rebuild_enemy_name_map.py --dry-run
    python rebuild_enemy_name_map.py

表结构：`enemy_name_map(敌人ID TEXT PRIMARY KEY, 图鉴名 TEXT, 数据名 TEXT)`
"""

import argparse
import json
import os
import sqlite3
import sys
from datetime import datetime

from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)
import paths  # noqa: E402
import update_common as uc  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402

TABLE = "enemy_name_map"
SCRIPT_VERSION = 1
DDL = """
CREATE TABLE %s (
    敌人ID TEXT PRIMARY KEY,
    图鉴名 TEXT NOT NULL,
    数据名 TEXT NOT NULL
)
""" % TABLE

ENEMY_HANDBOOK = paths.GAMEDATA / "excel" / "enemy_handbook_table.json"
ENEMY_DB = paths.GAMEDATA / "levels" / "enemydata" / "enemy_database.json"

uc.reconfigure_stdout()


def norm(text):
    return (clean_desc(text) or "").strip()


def load_names():
    """→ (图鉴名 {ID: 名}, 数据名 {ID: 名})，都已清洗。"""
    handbook = json.load(open(ENEMY_HANDBOOK, encoding="utf-8"))
    hb = {}
    for eid, v in handbook.get("enemyData", {}).items():
        nm = norm(v.get("name"))
        if nm:
            hb[eid] = nm

    database = json.load(open(ENEMY_DB, encoding="utf-8"))
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
    """数据名 = 图鉴名 + 括号后缀，且表里另有一条同名常规敌人 → 是“同一图鉴下的另一种形态”。

    这种不映射：图鉴名指向的是常规形态，友方形态有自己的名字（数据名）。
    """
    if not data.startswith(display):
        return False
    suffix = data[len(display):]
    return suffix.startswith(("（", "(")) and display in plain_names


def main():
    ap = argparse.ArgumentParser(description="重建 enemy_name_map（图鉴名 ↔ 数据名）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认权威库）")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== 重建 enemy_name_map ======")
    print("数据库 : %s" % args.db)

    hb, db = load_names()
    print("图鉴 ID %d | 数据名 ID %d | 交集 %d" % (len(hb), len(db), len(set(hb) & set(db))))

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None

    plain_names = {row[0] for row in conn.execute("SELECT DISTINCT 名称 FROM enemies") if row[0]}

    rows, skipped = [], []
    for eid in sorted(set(hb) & set(db)):
        if hb[eid] == db[eid]:
            continue
        if is_friendly_variant(hb[eid], db[eid], plain_names):
            skipped.append((eid, hb[eid], db[eid]))
            continue
        rows.append((eid, hb[eid], db[eid]))

    print("\n两边名字不同（需要映射）: %d 组" % len(rows))
    for eid, display, data in rows:
        print("   %-26s 图鉴名=%-18s 数据名=%s" % (eid, display, data))
    if skipped:
        print("\n同一图鉴下的另一种形态（不映射，共 %d 组）:" % len(skipped))
        for eid, display, data in skipped:
            print("   %-26s 图鉴名=%-18s 数据名=%s" % (eid, display, data))

    existing = conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name=?", (TABLE,)
    ).fetchone()
    old = []
    if existing:
        old = sorted(tuple(r) for r in conn.execute("SELECT 敌人ID, 图鉴名, 数据名 FROM %s" % TABLE))

    record = {
        "表": TABLE,
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {
            "图鉴名": "excel/enemy_handbook_table.json",
            "数据名": "levels/enemydata/enemy_database.json",
        },
        "脚本": {"文件": "rebuild_enemy_name_map.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "原表行数": len(old),
        "新表行数": len(rows),
        "同一图鉴下的另一种形态（不映射）": [list(x) for x in skipped],
        "新增": [list(x) for x in rows if x not in old],
        "移除": [list(x) for x in old if x not in rows],
        "结果": "未执行",
    }

    if args.dry_run:
        print("\n(干跑：未写库、未写记录)")
        record["结果"] = "干跑"
        return 0

    record["结果"] = "成功"
    if not args.no_backup:
        print("\n  备份: %s" % (uc.backup_db(args.db) or "（未生成）"))
    conn.execute("BEGIN IMMEDIATE")
    try:
        conn.execute("DROP TABLE IF EXISTS %s" % TABLE)
        conn.execute(DDL)
        conn.executemany("INSERT INTO %s VALUES (?, ?, ?)" % TABLE, rows)
        conn.execute("COMMIT")
    except Exception as e:
        conn.execute("ROLLBACK")
        record["结果"] = "中止（%s）" % e
        print("\n!! 中止：%s（已回滚）" % e)
        print("   记录：%s" % uc.write_record(TABLE, record))
        return 1
    conn.close()
    print("\n  写入 %d 行" % len(rows))
    print("  记录: %s" % uc.write_record(TABLE, record))
    return 0


if __name__ == "__main__":
    sys.exit(main())
