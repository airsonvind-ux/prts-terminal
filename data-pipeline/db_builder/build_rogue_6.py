# -*- coding: utf-8 -*-
"""建表并全量入库：沉沦者的黑流树海（rogue_6）—— 一次性，用于首次建表 / 重建。

表结构与列口径见 maintenance/rogue6_source.py（页面解析、JSON 组装都在那里，
与 update_rogue_6.py 共用同一份，避免两套逻辑分叉）。

手填类型（分队 / 节点事件类型）不在本脚本范围内——由 Air 自行补全；
更新脚本同样不碰这些行。
"""

import argparse
import os
import sqlite3
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))                     # data-pipeline/
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "maintenance"))

import paths  # noqa: E402
import update_common as uc  # noqa: E402
from rogue6_source import (  # noqa: E402
    COLS, TABLE, TOPIC, build_rows, check_sentinels, counts_of, fetch_pages, load_json,
    parse_all,
)

SCRIPT_VERSION = 1


def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="建表并全量入库：沉沦者的黑流树海（rogue_6）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不备份、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库")
    args = ap.parse_args()

    started = datetime.now()
    print("====== %s 建表入库 ======" % TOPIC)
    version = uc.read_source_version(str(paths.DATA_SOURCE))
    print("数据源 : %s" % version)
    print("数据库 : %s" % args.db)

    print("\n------ 抓取 PRTS ------")
    pages = fetch_pages()
    parsed = parse_all(pages)

    _, details, modules = load_json()
    print("\n------ 结构哨兵 ------")
    ok, lines = check_sentinels(counts_of(parsed, details, modules))
    for line in lines:
        print("  " + line)
    if not ok:
        raise SystemExit("结构哨兵未通过，中止（页面或数据源结构可能变了）")

    rows, tips = build_rows(pages, details, modules, parsed)

    by_type = {}
    for r in rows:
        by_type[r["类型"]] = by_type.get(r["类型"], 0) + 1
    print("\n------ 组装结果 ------")
    for k, v in sorted(by_type.items()):
        print("  %-26s %d" % (k, v))
    print("  合计 %d 行" % len(rows))
    ids = [r["id"] for r in rows]
    if len(ids) != len(set(ids)):
        raise SystemExit("!! 主键重复，中止")
    for t in tips[:10]:
        print("  · 提示：%s" % t)

    record = {
        "脚本": {"文件": "db_builder/build_rogue_6.py", "版本": SCRIPT_VERSION},
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": version, "数据库": os.path.basename(args.db), "表": TABLE,
        "计数": by_type, "合计": len(rows), "构建提示": tips,
        "结果": "干跑" if args.dry_run else "成功",
    }

    if args.dry_run:
        print("\n(干跑：未写库、未备份、未写记录)")
        return 0

    print("\n------ 写库（单事务）------")
    if args.no_backup:
        print("  (已跳过备份)")
    else:
        print("  备份: %s" % (uc.backup_db(args.db) or "（未生成）"))
    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        conn.execute("PRAGMA journal_mode=MEMORY")
        conn.execute("BEGIN IMMEDIATE")
        try:
            conn.execute("DROP TABLE IF EXISTS %s" % TABLE)
            conn.execute("CREATE TABLE %s (%s)" % (TABLE, ", ".join(
                ["id TEXT PRIMARY KEY", "名称 TEXT NOT NULL"] +
                ["%s TEXT" % c for c in COLS[2:]])))
            conn.executemany(
                "INSERT INTO %s (%s) VALUES (%s)" % (TABLE, ", ".join(COLS),
                                                     ", ".join("?" * len(COLS))),
                [tuple(r.get(c) for c in COLS) for r in rows])
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            uc.write_record(TOPIC, record)
            print("\n!! 写库失败并已回滚：%s" % e)
            return 1
    finally:
        conn.close()

    print("\n====== 完成 ======")
    print("  入库 %d 行 | 记录: %s" % (len(rows), uc.write_record(TOPIC, record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
