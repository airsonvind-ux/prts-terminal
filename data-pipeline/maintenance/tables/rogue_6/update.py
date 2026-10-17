# -*- coding: utf-8 -*-
"""沉沦者的黑流树海（rogue_6）数据更新脚本 —— 增量，可复用。

三段式：① 校对（库内已有行 vs 重算，差异才写）② 新增（重算里库中没有的行）
③ 记录。页面解析与组行逻辑在 rogue6_source.py（与 build_rogue_6.py 共用同一份）。

**不属于本脚本生成范围的行一律不碰**——分队 / 节点事件类型这些手填行由 Air 自管，
脚本既不比对也不写入。

手改过的值若与页面不一致，校对会把它写回页面口径（脚本只写爬取结果）。
要长期保住某个手改值，把它写进 rogue6_source.MANUAL_FIXES。

用法：
    python update.py --dry-run
    python update.py
"""

import argparse
import os
import sqlite3
import sys
from datetime import datetime

from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import update_common as uc  # noqa: E402
from rogue6_source import (  # noqa: E402
    COLS, TABLE, TOPIC, build_rows, check_sentinels, counts_of, fetch_pages, load_json,
    parse_all,
)

SCRIPT_VERSION = 1
RECORD_NAME = TOPIC
MAX_DETAIL = 300


def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="更新 rogue_6（增量：校对 + 新增）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不备份、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库")
    args = ap.parse_args()

    started = datetime.now()
    print("====== %s 更新 ======" % TOPIC)
    version = uc.read_source_version(str(paths.DATA_SOURCE))
    print("数据源 : %s" % version)
    print("数据库 : %s" % args.db)

    conn = sqlite3.connect(args.db)
    try:
        have = [r[0] for r in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?", (TABLE,))]
    finally:
        conn.close()
    if not have:
        raise SystemExit("表 %s 不存在——先用 build_rogue_6.py 建表" % TABLE)

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

    conn = sqlite3.connect(args.db)
    try:
        live = {}
        for r in conn.execute("SELECT %s FROM %s" % (", ".join(COLS), TABLE)):
            live[r[0]] = dict(zip(COLS, r))
    finally:
        conn.close()

    fresh = {r["id"]: r for r in rows}
    add_rows = [r for r in rows if r["id"] not in live]
    changes = []                       # [(id, {列: (旧, 新)})]
    for iid, r in fresh.items():
        old = live.get(iid)
        if not old:
            continue
        diff = {}
        for c in COLS:
            if not uc.values_equal(old.get(c), r.get(c)):
                diff[c] = (old.get(c), r.get(c))
        if diff:
            changes.append((iid, diff))
    untouched = [i for i in live if i not in fresh]

    print("\n------ 校对结果 ------")
    print("  库内 %d 行；重算 %d 行" % (len(live), len(rows)))
    print("  新增 %d 行 / 变更 %d 行 / 未触碰（非本脚本生成）%d 行"
          % (len(add_rows), len(changes), len(untouched)))
    for iid, diff in changes[:12]:
        for c, (o, n) in diff.items():
            print("  · %s . %s" % (iid, c))
            print("      旧 %s" % uc.brief(o, 110))
            print("      新 %s" % uc.brief(n, 110))
    if len(changes) > 12:
        print("  … 其余 %d 行见记录" % (len(changes) - 12))
    for t in tips[:10]:
        print("  · 提示：%s" % t)

    by_type = {}
    for r in rows:
        by_type[r["类型"]] = by_type.get(r["类型"], 0) + 1

    record = {
        "脚本": {"文件": "maintenance/tables/rogue_6/update.py", "版本": SCRIPT_VERSION},
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": version, "数据库": os.path.basename(args.db), "表": TABLE,
        "计数": by_type, "重算合计": len(rows),
        "新增": [{"id": r["id"], "名称": r["名称"], "类型": r["类型"]} for r in add_rows[:MAX_DETAIL]],
        "新增数": len(add_rows),
        "变更": [{"id": iid, "差异": dict((c, [uc.brief(o, 120), uc.brief(n, 120)])
                                        for c, (o, n) in diff.items())}
                 for iid, diff in changes[:MAX_DETAIL]],
        "变更数": len(changes),
        "未触碰行数": len(untouched),
        "构建提示": tips,
        "结果": "干跑" if args.dry_run else "成功",
    }

    if args.dry_run:
        print("\n(干跑：未写库、未备份、未写记录)")
        return 0
    if not add_rows and not changes:
        print("\n无变化：库内与重算结果一致。未做备份、未写库、未写记录。")
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
            n_ins = n_upd = 0
            for r in add_rows:
                conn.execute("INSERT INTO %s (%s) VALUES (%s)"
                             % (TABLE, ", ".join(COLS), ", ".join("?" * len(COLS))),
                             tuple(r.get(c) for c in COLS))
                n_ins += 1
            for iid, diff in changes:
                sets = ", ".join("%s=?" % c for c in diff)
                conn.execute("UPDATE %s SET %s WHERE id=?" % (TABLE, sets),
                             tuple(diff[c][1] for c in diff) + (iid,))
                n_upd += 1
            conn.execute("COMMIT")
            record["写入"] = {"新增": n_ins, "更新": n_upd}
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            uc.write_record(RECORD_NAME, record)
            print("\n!! 写库失败并已回滚：%s" % e)
            return 1
    finally:
        conn.close()

    print("\n====== 完成 ======")
    print("  新增 %d / 更新 %d | 记录: %s" % (len(add_rows), len(changes),
                                            uc.write_record(RECORD_NAME, record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
