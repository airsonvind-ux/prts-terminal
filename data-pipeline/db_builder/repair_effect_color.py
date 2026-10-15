# -*- coding: utf-8 -*-
"""一次性修复：清掉库内文本里残留的 `color=…` 片段。

背景（2026-10-15）：`{{异常效果|名称|color=xxx}}` 这条清洗规则有回溯 bug，
第二参数只吃到 `color`，`=xxx` 留在正文里。规则已修（5448dee），但**已经写进库的**
那批救不回来——括号早被吃掉，重洗无效，只能这样按形态清掉。

范围：所有表的文本列；JSON 列走进结构只改字符串。
做法：先跑当前清洗规则（顺手清掉完整的 <color=…> 之类），再删掉裸露的 `color=…`。
只动这两样，不引入别的变化。

用法：
    python repair_effect_color.py --dry-run
    python repair_effect_color.py
"""

import argparse
import json
import os
import re
import sqlite3
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))                     # data-pipeline/
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "maintenance"))

import paths  # noqa: E402
import update_common as uc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402

# 颜色值只在两种形态： #RRGGBB 类十六进制，或已知颜色名。
# 不能用 [A-Za-z]{3,20}：它贪婪，会把 color=orangeBuff 的 Buff 一起吃掉。
COLOR_WORDS = ("orange", "red", "green", "blue", "purple", "grey", "gray", "white",
               "black", "yellow", "cyan", "pink", "gold", "brown", "lime", "navy",
               "teal", "maroon", "olive", "silver", "aqua", "fuchsia")
RE_COLOR = re.compile(r"\|?color=(?:#[0-9a-fA-F]{3,8}|(?:%s))" % "|".join(COLOR_WORDS))
RECORD_NAME = "repair-effect-color"
MAX_DETAIL = 300


def fix_text(s):
    """先按当前规则清洗（清掉完整 <color=…>），再删裸露的 color=…"""
    return RE_COLOR.sub("", clean_wiki(s))


def fix_value(raw):
    """JSON 列走进结构只改字符串；返回 (新值, 是否变化)。"""
    if not isinstance(raw, str) or "color=" not in raw:
        return raw, False
    if raw.lstrip()[:1] in ("[", "{"):
        try:
            obj = json.loads(raw)
        except ValueError:
            obj = None
        if isinstance(obj, (list, dict)):
            def walk(v):
                if isinstance(v, str):
                    return fix_text(v)
                if isinstance(v, list):
                    return [walk(x) for x in v]
                if isinstance(v, dict):
                    return {k: walk(x) for k, x in v.items()}
                return v

            new = json.dumps(walk(obj), ensure_ascii=False)
            return (new, True) if new != raw else (raw, False)
    new = fix_text(raw)
    return (new, True) if new != raw else (raw, False)


def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="一次性修复：清掉库内残留的 color=… 片段")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不备份、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库")
    args = ap.parse_args()

    started = datetime.now()
    print("====== 修复残留 color= 片段 ======")
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
                    "SELECT rowid, [%s] FROM %s WHERE [%s] LIKE '%%color=%%'" % (col, t, col)
                ).fetchall()
                for rowid, raw in rows:
                    new, changed = fix_value(raw)
                    if changed:
                        hits.append((t, col, rowid, raw, new))
                        key = "%s.%s" % (t, col)
                        per_place[key] = per_place.get(key, 0) + 1

        print("\n------ 扫描结果 ------")
        if not hits:
            print("  没有需要修复的文本")
        for key, n in sorted(per_place.items()):
            print("  %-24s %d 行" % (key, n))
        for t, col, rowid, raw, new in hits[:8]:
            print("  · %s.%s rowid=%d" % (t, col, rowid))
            i = str(raw).find("color=")
            print("      旧 …%s…" % str(raw)[max(0, i - 40):i + 30].replace("\n", "⏎"))
            j = str(new).find(str(raw)[max(0, i - 20):max(0, i - 20) + 10])
            print("      新 …%s…" % str(new)[max(0, j - 40):j + 30].replace("\n", "⏎"))

        if args.dry_run:
            print("\n(干跑：未写库、未备份、未写记录)")
            return 0
        if not hits:
            return 0

        print("\n------ 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            print("  备份: %s" % (uc.backup_db(args.db) or "（未生成）"))
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
        "脚本": {"文件": "db_builder/repair_effect_color.py", "版本": 1},
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据库": os.path.basename(args.db),
        "修复": {"行数": len(hits), "分布": per_place,
                 "明细": [{"表": t, "列": col, "旧": uc.brief(raw, 140),
                           "新": uc.brief(new, 140)} for t, col, _r, raw, new in hits[:MAX_DETAIL]]},
        "结果": "成功",
    }
    path = uc.write_record(RECORD_NAME, record)
    print("\n====== 完成 ======")
    print("  修复 %d 行 | 记录: %s" % (len(hits), path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
