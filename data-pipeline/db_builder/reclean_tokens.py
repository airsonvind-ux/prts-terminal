# -*- coding: utf-8 -*-
"""tokens 表 reclean：全量爬 PRTS，把页面上的备注同步进库（**一次性修复脚本**）。

全量爬取只出现在这里：更新脚本（update_tokens.py）一律增量，只爬新增。
本脚本的用途是「隔一段时间跟 Wiki 页面统一一次」。

同步什么（页面给什么写什么，含清空）：
  · `备注`           ← {{召唤物信息}}|备注
                       页面没有这个模板 → 不动；模板在而备注为空 → 清空
  · `技能列表[].备注` ← 按技能名匹配（技能 / 技能2 / 无等级技能 / 召唤物技能）
  · `天赋列表[].备注` ← 按天赋名匹配（{{召唤物天赋}}）
其余列一律不动（归 update_tokens.py 与本地源）。页面 404、或页面上找不到该技能/天赋名 → 不动。

用法：
    python reclean_tokens.py --dry-run   # 照常爬取并报告，但不写库、不备份、不写记录
    python reclean_tokens.py
"""

import argparse
import json
import os
import sqlite3
import sys
import time
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent          # data-pipeline/db_builder
_MAINT = HERE.parent / "maintenance"           # 通用模块在 maintenance/lib
for _p in (str(HERE), str(_MAINT / "lib"), str(HERE.parent)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import update_common as uc  # noqa: E402
import update_tokens as ut  # noqa: E402   （复用同一套模板契约与 JSON 口径）

TABLE = "tokens"
RECORD_NAME = "tokens-reclean"
SCRIPT_VERSION = 1
MAX_DETAIL = 500
JOURNAL_MODE = "MEMORY"


def sync_row(row, page, notes):
    """把页面文本同步进行（备注 / 技能备注 / 天赋备注）。返回 (是否变化, 计数)。"""
    note, skill_notes, talent_notes, info_found = page
    counts = {"备注": 0, "技能备注": 0, "技能备注清空": 0, "天赋备注": 0, "天赋备注清空": 0}

    # 备注列：页面有这个模板才动手
    if info_found and (row["备注"] or None) != note:
        row["备注"] = note
        counts["备注"] += 1

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

    talents = ut._parse_json(row["天赋列表"]) or []
    names = {(t.get("天赋名") or "").strip() for t in talents}
    for item in talents:
        tname = (item.get("天赋名") or "").strip()
        if tname not in talent_notes:
            continue
        val = talent_notes[tname]
        if val:
            if item.get("备注") != val:
                item["备注"] = val
                counts["天赋备注"] += 1
        elif item.get("备注"):
            item["备注"] = None
            counts["天赋备注"] += 1
            counts["天赋备注清空"] += 1
    row["天赋列表"] = ut.dump(talents)

    for tname in talent_notes:
        if tname not in names:
            notes.append("%s：页面天赋备注未匹配到库内天赋名 %s" % (row["名称"], tname))
    return any(counts.values()), counts


def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="tokens 表 reclean（全量爬取，同步页面备注）")
    ap.add_argument("--dry-run", action="store_true",
                    help="照常爬取并报告，但不写库、不备份、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== tokens 表 reclean（全量爬取）======")
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
        "脚本": {"文件": "reclean_tokens.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "爬取": {"目标": 0, "成功": 0, "无页面": 0, "有备注": 0, "有技能备注": 0,
                 "有天赋备注": 0, "重定向": [], "无页面明细": []},
        "变更": {"行": 0, "备注": 0, "技能备注": 0, "技能备注清空": 0,
                 "天赋备注": 0, "天赋备注清空": 0, "明细": []},
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
        for probe, marker in (ut.SENTINEL_INFO, ut.SENTINEL_SKILL, ut.SENTINEL_TALENT):
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
        note_rows = skill_note_rows = talent_note_rows = 0
        totals = {"备注": 0, "技能备注": 0, "技能备注清空": 0, "天赋备注": 0, "天赋备注清空": 0}
        t0 = time.time()
        for i, row in enumerate(rows, 1):
            name = row["名称"]
            if i % 10 == 0 or i == len(rows):
                print("  [%d/%d] %.0fs | %s" % (i, len(rows), time.time() - t0, name))
            before = (row["备注"], row["技能列表"], row["天赋列表"])
            try:
                page = ut.crawl_token_page(ut.PAGE_OVERRIDE.get(name, name), redirects=redirects)
            except uc.NoPage:
                no_page.append(name)
                continue
            except Exception as e:
                record["结果"] = "中止（爬取失败：%s）" % e
                path = uc.write_record(RECORD_NAME, record)
                print("!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1

            note_rows += 1 if page[0] else 0
            skill_note_rows += 1 if any(page[1].values()) else 0
            talent_note_rows += 1 if any(page[2].values()) else 0

            is_changed, counts = sync_row(row, page, notes)
            if is_changed:
                changed.append((row["tokenID"], name, before,
                                (row["备注"], row["技能列表"], row["天赋列表"])))
                for k, v in counts.items():
                    totals[k] += v
            time.sleep(0.1)

        record["爬取"]["目标"] = len(rows)
        record["爬取"]["成功"] = len(rows) - len(no_page)
        record["爬取"]["无页面"] = len(no_page)
        record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
        record["爬取"]["重定向"] = ["%s → %s" % ab for ab in redirects[:MAX_DETAIL]]
        record["爬取"]["有备注"] = note_rows
        record["爬取"]["有技能备注"] = skill_note_rows
        record["爬取"]["有天赋备注"] = talent_note_rows
        record["变更"]["行"] = len(changed)
        for k, v in totals.items():
            record["变更"][k] = v
        record["构建提示"] = notes[:MAX_DETAIL]
        record["变更"]["明细"] = []
        for tid, name, old, new in changed[:MAX_DETAIL]:
            item = {"tokenID": tid, "名称": name, "列": []}
            for col, a, b in (("备注", old[0], new[0]), ("技能列表", old[1], new[1]),
                              ("天赋列表", old[2], new[2])):
                if a != b:
                    item["列"].append(col)
                    item[col] = uc.diff_brief(a, b, as_json=col in ut.JSON_COLS)
            record["变更"]["明细"].append(item)
        print("\n  成功 %d | 无页面 %d | 有备注 %d | 有技能备注 %d | 有天赋备注 %d"
              % (len(rows) - len(no_page), len(no_page), note_rows,
                 skill_note_rows, talent_note_rows))
        print("  变更 %d 行：备注 %d | 技能备注 %d（清空 %d）| 天赋备注 %d（清空 %d）"
              % (len(changed), totals["备注"], totals["技能备注"], totals["技能备注清空"],
                 totals["天赋备注"], totals["天赋备注清空"]))
        for tid, name, old, new in changed[:10]:
            cols = [c for c, a, b in (("备注", old[0], new[0]), ("技能列表", old[1], new[1]),
                                      ("天赋列表", old[2], new[2])) if a != b]
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
                for col, a, b in (("备注", old[0], new[0]), ("技能列表", old[1], new[1]),
                                  ("天赋列表", old[2], new[2])):
                    if a != b:
                        conn.execute("UPDATE %s SET %s=? WHERE tokenID=?" % (TABLE, col),
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
