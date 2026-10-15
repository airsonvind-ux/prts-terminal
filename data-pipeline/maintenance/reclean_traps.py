# -*- coding: utf-8 -*-
"""traps 表 reclean：全量爬 PRTS，把页面上的爬取列同步进库（**一次性修复脚本**）。

全量爬取只出现在这里：更新脚本（update_traps.py）一律增量，只爬新增。
本脚本的用途是「隔一段时间跟 Wiki 页面统一一次」。

同步什么：
  · `阻挡半径` ← {{装置信息}}|阻挡半径（不是数值 → 跳过并记入构建提示）
  · `装置机制` ← {{装置信息}}|装置机制（页面给空 → 不动）
  · `技能列表[].备注` ← {{装置技能}}|备注（按技能名匹配，页面给出即为准，含清空）
其余列一律不动（归 update_traps.py 与本地源）。
页面 404、页面是干员/敌人/消歧义页、或页面上找不到该技能名 → 不动。
天赋备注：装置页没有这个字段（天赋只写在 {{装置信息}} 的 天赋名称/天赋描述 里），不同步。

页面查找与更新脚本同一套：PAGE_MAP → 去空白后的名称 → 名称(装置)。

用法：
    python reclean_traps.py --dry-run   # 照常爬取并报告，但不写库、不备份、不写记录
    python reclean_traps.py
"""

import argparse
import json
import os
import sqlite3
import sys
import time
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))   # data-pipeline/
sys.path.insert(0, str(HERE))          # maintenance/

import paths  # noqa: E402
import update_common as uc  # noqa: E402
import update_traps as ut  # noqa: E402

TABLE = "traps"
RECORD_NAME = "traps-reclean"
SCRIPT_VERSION = 1
MAX_DETAIL = 500
JOURNAL_MODE = "MEMORY"


def sync_row(row, page, notes):
    """把页面结果同步进行。返回 (是否变化, 计数)。"""
    radius_raw, mech, skill_notes, _info_found = page
    counts = {"阻挡半径": 0, "装置机制": 0, "技能备注": 0, "技能备注清空": 0}

    if radius_raw:
        try:
            radius = str(float(radius_raw))
        except ValueError:
            notes.append("%s：阻挡半径不是数值（%r），已跳过" % (row["名称"], radius_raw))
        else:
            if (row["阻挡半径"] or None) != radius:
                row["阻挡半径"] = radius
                counts["阻挡半径"] += 1

    if mech and row["装置机制"] != mech:
        row["装置机制"] = mech
        counts["装置机制"] += 1

    skills = ut._parse_json(row["技能列表"]) or []
    for sk in skills:
        sname = sk.get("技能名", "")
        if sname not in skill_notes:
            continue
        val = skill_notes[sname]
        if val:
            if sk.get("备注") != val:
                sk["备注"] = val
                counts["技能备注"] += 1
        elif "备注" in sk:
            del sk["备注"]
            counts["技能备注"] += 1
            counts["技能备注清空"] += 1
    row["技能列表"] = ut.dump(skills)

    return any(counts.values()), counts


def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="traps 表 reclean（全量爬取，同步页面数据）")
    ap.add_argument("--dry-run", action="store_true",
                    help="照常爬取并报告，但不写库、不备份、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== traps 表 reclean（全量爬取）======")
    print("数据库  : %s" % args.db)
    src_ver = uc.read_source_version(paths.DATA_SOURCE)
    print("源版本  : 提交 %s | 游戏版本 %s | 数据时间 %s"
          % (src_ver["提交"], src_ver["游戏版本"], src_ver["数据时间"]))

    record = {
        "表": TABLE,
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {
            "仓库": "Kengxxiao/ArknightsGameData",
            "提交": src_ver["提交"], "提交标题": src_ver["提交标题"],
            "游戏版本": src_ver["游戏版本"], "数据时间": src_ver["数据时间"],
        },
        "脚本": {"文件": "reclean_traps.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "爬取": {"目标": 0, "成功": 0, "无页面": 0, "有阻挡半径": 0, "有装置机制": 0,
                 "有技能备注": 0, "重定向": [], "无页面明细": []},
        "变更": {"行": 0, "阻挡半径": 0, "装置机制": 0, "技能备注": 0,
                 "技能备注清空": 0, "明细": []},
        "构建提示": [],
        "结果": "未执行",
    }

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        rows = [dict(zip(db_cols, r))
                for r in conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), TABLE))]

        print("\n------ 结构哨兵 ------")
        for probe, marker in ut.SENTINELS:
            ok, msg = uc.sentinel_ok(probe, marker)
            if not ok:
                record["结果"] = "中止（结构哨兵失败：%s）" % msg
                path = uc.write_record(RECORD_NAME, record)
                print("!! 中止：%s" % msg)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
            print("  %s ✓" % probe)

        print("\n------ 全量爬取（%d 行）------" % len(rows))
        notes = []
        no_page, redirects, changed = [], [], []
        totals = {"阻挡半径": 0, "装置机制": 0, "技能备注": 0, "技能备注清空": 0}
        n_radius = n_mech = n_note = 0
        t0 = time.time()
        for i, row in enumerate(rows, 1):
            name = row["名称"]
            if i % 20 == 0 or i == len(rows):
                print("  [%d/%d] %.0fs | %s" % (i, len(rows), time.time() - t0, name))
            before = (row["阻挡半径"], row["装置机制"], row["技能列表"])
            try:
                cand, page = ut.fetch_trap_page(row["装置ID"], name, notes, redirects=redirects)
            except Exception as e:
                record["结果"] = "中止（爬取失败：%s）" % e
                path = uc.write_record(RECORD_NAME, record)
                print("!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
            if page is None:
                no_page.append(name)
                continue

            n_radius += 1 if page[0] else 0
            n_mech += 1 if page[1] else 0
            n_note += 1 if any(page[2].values()) else 0

            is_changed, counts = sync_row(row, page, notes)
            if is_changed:
                changed.append((row["装置ID"], name, before,
                                (row["阻挡半径"], row["装置机制"], row["技能列表"])))
                for k, v in counts.items():
                    totals[k] += v
            time.sleep(0.1)

        record["爬取"]["目标"] = len(rows)
        record["爬取"]["成功"] = len(rows) - len(no_page)
        record["爬取"]["无页面"] = len(no_page)
        record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
        record["爬取"]["重定向"] = ["%s → %s" % ab for ab in redirects[:MAX_DETAIL]]
        record["爬取"]["有阻挡半径"] = n_radius
        record["爬取"]["有装置机制"] = n_mech
        record["爬取"]["有技能备注"] = n_note
        record["变更"]["行"] = len(changed)
        for k, v in totals.items():
            record["变更"][k] = v
        record["构建提示"] = notes[:MAX_DETAIL]
        record["变更"]["明细"] = []
        for tid, name, old, new in changed[:MAX_DETAIL]:
            item = {"装置ID": tid, "名称": name, "列": []}
            for col, a, b in (("阻挡半径", old[0], new[0]), ("装置机制", old[1], new[1]),
                              ("技能列表", old[2], new[2])):
                if a != b:
                    item["列"].append(col)
                    item[col] = uc.diff_brief(a, b, as_json=col in ut.JSON_COLS)
            record["变更"]["明细"].append(item)

        print("\n  成功 %d | 无页面 %d | 有阻挡半径 %d | 有装置机制 %d | 有技能备注 %d"
              % (len(rows) - len(no_page), len(no_page), n_radius, n_mech, n_note))
        print("  变更 %d 行：阻挡半径 %d | 装置机制 %d | 技能备注 %d（清空 %d）"
              % (len(changed), totals["阻挡半径"], totals["装置机制"],
                 totals["技能备注"], totals["技能备注清空"]))
        for tid, name, old, new in changed[:10]:
            cols = [c for c, a, b in (("阻挡半径", old[0], new[0]), ("装置机制", old[1], new[1]),
                                      ("技能列表", old[2], new[2])) if a != b]
            print("    · %s %s" % (name, "、".join(cols)))

        if args.dry_run:
            print("\n(干跑：未写库、未备份、未写记录)")
            record["结果"] = "干跑"
            return 0

        # ── 写库（单事务）──
        print("\n------ 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = uc.backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        print("  写回: %d 行" % len(changed))
        conn.execute("PRAGMA journal_mode=%s" % JOURNAL_MODE)
        conn.execute("BEGIN IMMEDIATE")
        try:
            for tid, name, old, new in changed:
                for col, a, b in (("阻挡半径", old[0], new[0]), ("装置机制", old[1], new[1]),
                                  ("技能列表", old[2], new[2])):
                    if a != b:
                        conn.execute("UPDATE %s SET %s=? WHERE 装置ID=?" % (TABLE, col),
                                     (b, tid))
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            path = uc.write_record(RECORD_NAME, record)
            print("\n!! 写库失败并已回滚：%s" % e)
            print("   记录：%s" % path)
            return 1
    finally:
        conn.close()

    record["结果"] = "成功"
    path = uc.write_record(RECORD_NAME, record)

    elapsed = (datetime.now() - started).total_seconds()
    print("\n====== 完成（%.1fs）======" % elapsed)
    print("  变更 %d 行 | 爬取成功 %d | 无页面 %d" % (record["变更"]["行"],
                                                  record["爬取"]["成功"],
                                                  record["爬取"]["无页面"]))
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
