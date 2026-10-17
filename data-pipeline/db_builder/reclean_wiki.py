# -*- coding: utf-8 -*-
"""规则升级后的全库重清洗（按表选要洗的列）。

性质：**规则升级时的一次性全库迁移**。
  - 规则不变时不用跑——清洗在每次爬取时都会执行，新数据天生干净；
  - 规则再升级后重跑即可，用途完全相同；
  - 已存在、但当初是按旧规则洗过的列，靠这里补洗。

目标（`--table` 选，默认 enemies）：

| 表 | 列 | 为什么要补洗 |
|---|---|---|
| `enemies` | `天赋文本` / `技能文本` | 规则升级后统一重洗 |
| `stages` | `情报` | 情报只对**新增行**爬取，且不参与校对（`SKIP_COLS`）——2026-10-13 补齐规则之前的行会一直带着旧清洗的残留（`{{Popup|…}}`、`{{color|…}}`、`'''…'''`），补洗一次后残留归零 |

只改指定列，不碰其它列；清洗是**幂等**的，已洗净的文本再洗一次结果相同，
所以重复运行不会产生新变化，脚本会直接报「无变化」并跳过备份、写库、写记录。

用法：
    python reclean_wiki.py --dry-run                  # enemies（默认）：只看会改多少、改成什么样
    python reclean_wiki.py
    python reclean_wiki.py --table stages --dry-run    # stages.情报
    python reclean_wiki.py --table stages
"""

import argparse
import os
import sqlite3
import sys
from datetime import datetime

from pathlib import Path

HERE = Path(__file__).resolve().parent          # data-pipeline/db_builder
_MAINT = HERE.parent / "maintenance"           # 通用模块在 maintenance/lib
for _p in (str(HERE), str(_MAINT / "lib"), str(HERE.parent)):
    if _p not in sys.path:
        sys.path.insert(0, _p)
import paths  # noqa: E402
from cleaning_wiki import clean_wiki, looks_dirty  # noqa: E402
from update_common import backup_db, write_record  # noqa: E402

# 表名: (要洗的列, 键列)
TARGETS = {
    "enemies": (("天赋文本", "技能文本"), ("敌人ID", "级别")),
    "stages": (("情报",), ("关卡ID",)),
}
MAX_SAMPLE = 5


def main():
    ap = argparse.ArgumentParser(description="规则升级后的全库重清洗")
    ap.add_argument("--table", choices=sorted(TARGETS), default="enemies",
                    help="要洗哪张表（默认 enemies）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库")
    ap.add_argument("--no-backup", action="store_true", help="跳过自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    table = args.table
    cols, key_cols = TARGETS[table]

    started = datetime.now()
    print("====== wikitext 重清洗（%s：%s）======" % (table, " / ".join(cols)))
    print("数据库: %s" % args.db)

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        have = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % table)]
        for c in cols:
            if c not in have:
                raise SystemExit("缺少列：%s" % c)

        key_sel = ", ".join(key_cols)
        rows = conn.execute(
            "SELECT rowid, %s, %s FROM %s" % (key_sel, ", ".join(cols), table)
        ).fetchall()
        print("扫描行数: %d" % len(rows))

        plan = []          # (rowid, [新值...], 旧值...)
        checked = 0
        dirty_before = 0
        samples = []
        for rowid, *rest in rows:
            keys = rest[:len(key_cols)]
            values = rest[len(key_cols):]
            news = []
            changed_here = False
            for v in values:
                if v is None:
                    news.append(None)
                    continue
                checked += 1
                if looks_dirty(v):
                    dirty_before += 1
                nv = clean_wiki(v)
                news.append(nv)
                if nv != v:
                    changed_here = True
                    if len(samples) < MAX_SAMPLE:
                        i = min([v.find(x) for x in ("{{", "<!--", "[[", "'''") if v.find(x) >= 0] or [0])
                        samples.append({
                            "键": "|".join(str(k) for k in keys),
                            "前": v[max(0, i - 40):i + 160],
                            "后": clean_wiki(v[max(0, i - 40):i + 160]),
                        })
            if changed_here:
                plan.append((rowid, news, values))

        print("有文本的列数: %d | 会变化: %d | 清洗前含脏标记: %d"
              % (checked, len(plan), dirty_before))
        print("\n--- 变化样例（前 %d 条）---" % len(samples))
        for s in samples:
            print("  [%s]" % s["键"])
            print("    前: %s" % s["前"].replace("\n", "\\n"))
            print("    后: %s" % s["后"].replace("\n", "\\n"))

        record = {
            "任务": "reclean_wiki",
            "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
            "数据库": os.path.basename(args.db),
            "表": table,
            "列": list(cols),
            "有文本列数": checked,
            "变化列数": len(plan),
            "清洗前含脏标记": dirty_before,
            "样例": samples,
            "结果": "未执行",
        }

        if args.dry_run:
            print("\n(干跑：未写库)")
            record["结果"] = "干跑"
            return 0

        if not plan:
            print("\n无变化：所有文本都已是清洗后的形态（清洗幂等）。")
            print("未做备份、未写库、未写记录。")
            return 0

        if args.no_backup:
            print("\n(已跳过备份)")
        else:
            bak = backup_db(args.db)
            print("\n备份: %s" % (bak or "（未生成）"))

        print("------ 写库（单事务）------")
        conn.execute("BEGIN IMMEDIATE")
        try:
            sets = ", ".join("%s=?" % c for c in cols)
            for rowid, news, _old in plan:
                conn.execute(
                    "UPDATE %s SET %s WHERE rowid=?" % (table, sets),
                    tuple(news) + (rowid,),
                )
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            write_record("reclean-wiki", record)
            print("\n!! 写库失败并已回滚：%s" % e)
            return 1

        # 复核
        after_dirty = 0
        for r in conn.execute("SELECT %s FROM %s" % (", ".join(cols), table)):
            for v in r:
                if v and looks_dirty(v):
                    after_dirty += 1
        record["清洗后残留"] = after_dirty
        record["结果"] = "成功"
        path = write_record("reclean-wiki", record)
        print("\n====== 完成 ======")
        print("  更新列数: %d | 清洗后仍残留: %d" % (len(plan), after_dirty))
        print("  记录: %s" % path)
        return 0
    finally:
        conn.close()


if __name__ == "__main__":
    sys.exit(main())
