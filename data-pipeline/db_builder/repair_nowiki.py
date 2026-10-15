# -*- coding: utf-8 -*-
"""一次性修复：把已入库文本里的 `<nowiki/>` 标记清掉。

背景（2026-10-15，Air 报告）：`cleaning_wiki` 的基础规则里漏了 MediaWiki 的
`<nowiki/>` 标记（旧的私有 cleaner 有，合并时漏了）。装置页习惯把 `<nowiki/>`
放在多行参数（`装置机制`）的开头，于是被原样写进了库：
  traps.装置机制 509 行、operators.天赋列表 2 行。
清洗规则已经补上，本脚本只做一件事——把库里已有的这些标记去掉。
**只动 `<nowiki>` 标记本身，不做别的清洗**（不给库引入任何新变化）。

扫描范围：所有表的文本列；值能解析成 JSON 的（如 天赋列表）走进结构，只清字符串。

用法：
    python repair_nowiki.py --dry-run
    python repair_nowiki.py
"""

import argparse
import json
import os
import re
import sqlite3
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))                    # data-pipeline/
sys.path.insert(0, os.path.join(HERE, "..", "maintenance"))     # maintenance/

import paths  # noqa: E402
import update_common as uc  # noqa: E402

RE_NOWIKI = re.compile(r'</?nowiki\s*/?>', re.IGNORECASE)
RECORD_NAME = "nowiki-repair"
MAX_DETAIL = 200


def clean_value(value):
    """清掉字符串里的 nowiki 标记；**含过标记的字符串顺带 strip**——
    新的清洗规则在末尾会 strip，不补上就与“重爬一次”的结果差一个前导换行。"""
    if isinstance(value, str):
        if "nowiki" not in value:
            return value
        return RE_NOWIKI.sub("", value).strip()
    if isinstance(value, list):
        return [clean_value(v) for v in value]
    if isinstance(value, dict):
        return {k: clean_value(v) for k, v in value.items()}
    return value


def repair(raw):
    """返回 (新值, 是否变化)。JSON 列走进结构，其余走纯文本。"""
    if not raw or "nowiki" not in raw:
        return raw, False
    obj = None
    head = raw.lstrip()[:1]
    if head in ("[", "{"):
        try:
            obj = json.loads(raw)
        except (TypeError, ValueError):
            obj = None
    if isinstance(obj, (list, dict)):
        new = json.dumps(clean_value(obj), ensure_ascii=False)
    else:
        new = RE_NOWIKI.sub("", raw).strip()
    return (new, True) if new != raw else (raw, False)


def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="一次性修复：清掉库内文本里的 <nowiki/>")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不备份、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== 修复 <nowiki> 标记 ======")
    print("数据库  : %s" % args.db)

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        tables = [r[0] for r in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' "
            "ORDER BY name")]
        hits, per_place = [], {}
        for t in tables:
            cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % t)]
            for col in cols:
                rows = conn.execute(
                    'SELECT rowid, [%s] FROM %s WHERE [%s] LIKE \'%%nowiki%%\''
                    % (col, t, col)).fetchall()
                for rowid, raw in rows:
                    new, changed = repair(raw)
                    if changed:
                        hits.append((t, col, rowid, raw, new))
                        key = "%s.%s" % (t, col)
                        per_place[key] = per_place.get(key, 0) + 1

        print("\n------ 扫描结果 ------")
        if not hits:
            print("  没有需要修复的文本（已是干净状态）")
        for key, n in sorted(per_place.items()):
            print("  %-24s %d 行" % (key, n))
        for t, col, rowid, raw, new in hits[:6]:
            print("  · %s.%s rowid=%d" % (t, col, rowid))
            print("      旧 %r" % raw[:90])
            print("      新 %r" % new[:90])

        if args.dry_run:
            print("\n(干跑：未写库、未备份、未写记录)")
            return 0
        if not hits:
            return 0

        print("\n------ 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = uc.backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        conn.execute("PRAGMA journal_mode=MEMORY")
        conn.execute("BEGIN IMMEDIATE")
        try:
            for t, col, rowid, _raw, new in hits:
                conn.execute("UPDATE %s SET [%s]=? WHERE rowid=?" % (t, col), (new, rowid))
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            print("\n!! 写库失败并已回滚：%s" % e)
            return 1
    finally:
        conn.close()

    record = {
        "脚本": {"文件": "db_builder/repair_nowiki.py", "版本": 1},
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据库": os.path.basename(args.db),
        "修复": {"行数": len(hits), "分布": per_place,
                 "明细": [{"表": t, "列": col, "旧": uc.brief(raw, 120),
                           "新": uc.brief(new, 120)} for t, col, _r, raw, new in hits[:MAX_DETAIL]]},
        "结果": "成功",
    }
    path = uc.write_record(RECORD_NAME, record)
    print("\n====== 完成 ======")
    print("  修复 %d 行 | 记录: %s" % (len(hits), path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
