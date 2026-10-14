# -*- coding: utf-8 -*-
"""规则升级后的全库重清洗（`天赋文本` / `技能文本`）。

性质：**规则升级时的一次性全库迁移**。
  - 规则不变时不用跑——清洗在每次爬取时都会执行，新数据天生干净；
  - 规则再升级后重跑即可，用途完全相同。

只改 `天赋文本` / `技能文本` 两列，不碰 `描述` / `能力列表` / `备注` 等列。

清洗是**幂等**的：已洗净的文本再洗一次结果相同。所以重复运行不会产生新变化，
脚本会直接报「无变化」并跳过备份、写库、写记录。

用法：
    python reclean_wiki.py --dry-run   # 只看会改多少、改成什么样
    python reclean_wiki.py             # 正式执行（先备份，单事务，失败回滚）
"""

import argparse
import os
import sqlite3
import sys
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import paths  # noqa: E402
from cleaning_wiki import clean_wiki, looks_dirty  # noqa: E402
from update_common import backup_db, write_record  # noqa: E402

TABLE = "enemies"
COLS = ("天赋文本", "技能文本")
MAX_SAMPLE = 5


def main():
    ap = argparse.ArgumentParser(description="重清洗 enemies 的天赋/技能文本")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库")
    ap.add_argument("--no-backup", action="store_true", help="跳过自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== wikitext 重清洗（%s）======" % " / ".join(COLS))
    print("数据库: %s" % args.db)

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        for c in COLS:
            if c not in cols:
                raise SystemExit("缺少列：%s" % c)

        rows = conn.execute(
            "SELECT rowid, 敌人ID, 级别, %s FROM %s" % (", ".join(COLS), TABLE)
        ).fetchall()
        print("扫描行数: %d" % len(rows))

        plan = []          # (rowid, [新值...], 旧值...)
        checked = 0
        dirty_before = 0
        samples = []
        for rowid, eid, lv, *values in rows:
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
                        i = min([v.find(x) for x in ("{{", "<!--", "[[") if v.find(x) >= 0] or [0])
                        samples.append({
                            "键": "%s|%s" % (eid, lv),
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
            "表": TABLE,
            "列": list(COLS),
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
            for rowid, news, _old in plan:
                sets = ", ".join("%s=?" % c for c in COLS)
                conn.execute(
                    "UPDATE %s SET %s WHERE rowid=?" % (TABLE, sets),
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
        for r in conn.execute("SELECT %s FROM %s" % (", ".join(COLS), TABLE)):
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
