# -*- coding: utf-8 -*-
"""operators 表：三处 wiki 文本的一次性同步清洗。

背景：库内这三处的文本来自**旧的清洗方案**，与现在的 `cleaning_wiki` 规则集不同。
旧 `build_operators_note.py` 的 `clean_note` 遇到 `{{popup|…}}` 整段删掉（注释里写着
「浮窗内容过于复杂」），旧 `clean_wiki` 又比现在少十几条规则。清洗会丢信息，就地再洗
一遍救不回来，所以回 PRTS 页面**重取原文**、用当前规则重洗。

清洗对象只有这三处（其余列一律不碰）：
  · 备注                 ← {{CharinfoV2}} 的 特性备注
  · 技能列表[].备注       ← {{技能2}} 的 备注
  · 天赋列表[].备注       ← {{天赋列表3}} 的 备注
（干员描述、模组列表是 Air 手工调整过的，本脚本不涉及。）

口径（Air 2026-10-15 定：不得覆盖人工修改）：
  · 页面没给出这一项          → 保持库内原值
  · 库内为空                  → 写入
  · 库内已有且 looks_dirty()  → 重写为当前清洗结果
  · 库内已有且看起来干净      → **跳过**，记入「疑似人工修改」（可能是你补的内容）
`--page-wins` 改为以页面为准（会覆盖人工补充，需显式指定）；`--only-empty` 只填空值。

流程：先把全表爬完、算完，再**单事务**写库。干跑会打印改动与跳过项。

用法：
    python reclean_operators.py --dry-run
    python reclean_operators.py
    python reclean_operators.py --only-empty
    python reclean_operators.py --page-wins      # 以页面为准，覆盖人工补充
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
import update_operators as up  # noqa: E402
from cleaning_wiki import looks_dirty  # noqa: E402

TABLE = "operators"
SCRIPT_VERSION = 1
MAX_DETAIL = 200
JOURNAL_MODE = "MEMORY"


def plan_row(cur_note, skills, talents, charinfo, skill_notes, talent_notes,
             only_empty, page_wins):
    """算这一行该改哪几处、该跳过哪几处。

    返回 (changes, skipped)：changes = [(位置, 旧值, 新值)]；
    skipped = [(位置, 旧值, 新值)]，即页面与库内不同但库内看起来干净、疑似人工修改的。
    """
    changes, skipped = [], []

    def want(location, old, new):
        if not new or new == old:
            return
        if only_empty:
            if not old:
                changes.append((location, old, new))
            return
        if not page_wins and old and not looks_dirty(old):
            skipped.append((location, old, new))
            return
        changes.append((location, old, new))

    want("备注", cur_note, charinfo)
    for sk in skills:
        sname = sk.get("技能名", "")
        want("技能备注[%s]" % sname, sk.get("备注"), skill_notes.get(sname))
    for i, item in enumerate(talents):
        note = talent_notes[i] if i < len(talent_notes) else None
        want("天赋备注[%s]" % item.get("序号", ""), item.get("备注"), note)
    return changes, skipped


def apply_skills(skills, skill_notes, only_empty, page_wins):
    for sk in skills:
        note = skill_notes.get(sk.get("技能名", ""))
        if not note:
            continue
        if only_empty and sk.get("备注"):
            continue
        if not page_wins and sk.get("备注") and not looks_dirty(sk["备注"]):
            continue
        sk["备注"] = note
    return json.dumps(skills, ensure_ascii=False)


def apply_talents(talents, talent_notes, only_empty, page_wins):
    for i, item in enumerate(talents):
        note = talent_notes[i] if i < len(talent_notes) else None
        if not note:
            continue
        if only_empty and item.get("备注"):
            continue
        if not page_wins and item.get("备注") and not looks_dirty(item["备注"]):
            continue
        item["备注"] = note
    return json.dumps(talents, ensure_ascii=False)


def main():
    ap = argparse.ArgumentParser(description="operators 三处 wiki 文本的同步清洗（重爬重洗）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不写记录、不备份")
    ap.add_argument("--only-empty", action="store_true", help="只填空值，任何已有文本都不动")
    ap.add_argument("--page-wins", action="store_true",
                    help="以页面为准重写（会覆盖人工补充）；不加则为保护模式")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    if args.only_empty and args.page_wins:
        raise SystemExit("--only-empty 与 --page-wins 不能同时用")
    mode = "only-empty" if args.only_empty else ("page-wins" if args.page_wins else "protect")

    started = datetime.now()
    print("====== operators 三处 wiki 文本同步清洗 ======")
    print("数据库  : %s" % args.db)
    print("模式    : %s" % {"only-empty": "只填空值", "page-wins": "以页面为准（会覆盖人工补充）",
                          "protect": "保护模式：只重写明显没洗干净的"}[mode])
    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    src_ver = uc.read_source_version(paths.DATA_SOURCE)
    record = {
        "表": TABLE,
        "任务": "reclean（三处 wiki 文本重爬重洗）",
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "脚本": {"文件": "reclean_operators.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "数据源": {"提交": src_ver["提交"], "游戏版本": src_ver["游戏版本"],
                   "数据时间": src_ver["数据时间"]},
        "模式": mode,
        "范围": ["备注", "技能列表[].备注", "天赋列表[].备注"],
        "爬取": {"目标干员": 0, "成功": 0, "无页面": 0, "重定向": [], "无页面明细": []},
        "变更": {"行": 0, "处": 0, "备注": 0, "技能备注": 0, "天赋备注": 0, "明细": []},
        "跳过（疑似人工修改）": {"处": 0, "明细": []},
        "结果": "未执行",
    }

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        rows = conn.execute(
            "SELECT 干员ID, 名称, 备注, 技能列表, 天赋列表 FROM %s ORDER BY 名称" % TABLE
        ).fetchall()

        print("\n------ 爬取 PRTS（%d 名干员）------" % len(rows))
        no_page, redirects = [], []
        pending = []          # [(干员ID, 备注 或 None, 技能列表 或 None, 天赋列表 或 None)]
        touched = {"备注": 0, "技能备注": 0, "天赋备注": 0}
        details, skipped_details = [], []
        skipped_total = 0
        t0 = time.time()

        for i, (cid, name, cur_note, sk_json, ta_json) in enumerate(rows, 1):
            if i % 20 == 0 or i == len(rows):
                print("  [%d/%d] %.0fs | %s" % (i, len(rows), time.time() - t0, name))
            try:
                charinfo, skill_notes, talent_notes = up.crawl_operator_page(
                    name, redirects=redirects)
            except uc.NoPage:
                no_page.append(name)
                continue
            except Exception as e:
                record["结果"] = "中止（爬取失败：%s）" % e
                path = uc.write_record(TABLE, record)
                print("\n!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
            time.sleep(0.1)

            skills = json.loads(sk_json) if sk_json else []
            talents = json.loads(ta_json) if ta_json else []
            changes, skipped = plan_row(cur_note, skills, talents, charinfo, skill_notes,
                                        talent_notes, args.only_empty, args.page_wins)
            skipped_total += len(skipped)
            for location, old, new in skipped:
                if len(skipped_details) < MAX_DETAIL:
                    skipped_details.append({"干员ID": cid, "名称": name, "位置": location,
                                            "库内": uc.brief(old, 120), "页面": uc.brief(new, 120)})
            if not changes:
                continue

            new_note = None
            for location, old, new in changes:
                if location == "备注":
                    touched["备注"] += 1
                    new_note = new
                else:
                    touched["技能备注" if location.startswith("技能") else "天赋备注"] += 1
                    if len(details) < MAX_DETAIL:
                        details.append({"干员ID": cid, "名称": name, "位置": location,
                                        "旧": uc.brief(old, 120), "新": uc.brief(new, 120)})
            if new_note is not None and len(details) < MAX_DETAIL:
                details.append({"干员ID": cid, "名称": name, "位置": "备注",
                                "旧": uc.brief(cur_note, 120), "新": uc.brief(new_note, 120)})

            want_skills = any(loc.startswith("技能") for loc, _, _ in changes)
            want_talents = any(loc.startswith("天赋") for loc, _, _ in changes)
            pending.append((
                cid, new_note,
                apply_skills(skills, skill_notes, args.only_empty, args.page_wins)
                if want_skills else None,
                apply_talents(talents, talent_notes, args.only_empty, args.page_wins)
                if want_talents else None,
            ))

        record["爬取"]["目标干员"] = len(rows)
        record["爬取"]["无页面"] = len(no_page)
        record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
        record["爬取"]["成功"] = len(rows) - len(no_page)
        record["爬取"]["重定向"] = ["%s → %s" % ab for ab in redirects[:MAX_DETAIL]]
        record["变更"]["行"] = len(pending)
        record["变更"]["处"] = sum(touched.values())
        record["变更"]["备注"] = touched["备注"]
        record["变更"]["技能备注"] = touched["技能备注"]
        record["变更"]["天赋备注"] = touched["天赋备注"]
        record["变更"]["明细"] = details
        record["跳过（疑似人工修改）"]["处"] = skipped_total
        record["跳过（疑似人工修改）"]["明细"] = skipped_details

        print("\n------ 1. 结果 ------")
        print("  成功 %d | 无页面 %d | 变更行 %d | 变更处 %d | 跳过（疑似人工）%d 处"
              % (record["爬取"]["成功"], len(no_page), len(pending),
                 record["变更"]["处"], skipped_total))
        print("    备注列 %d | 技能备注 %d | 天赋备注 %d"
              % (touched["备注"], touched["技能备注"], touched["天赋备注"]))
        if no_page:
            print("    无页面: %s" % ", ".join(no_page[:10]))
        for d in details[:10]:
            print("    改 · %s %s | %s → %s" % (d["名称"], d["位置"], d["旧"], d["新"]))
        for d in skipped_details[:10]:
            print("    留 · %s %s | 库内 %s | 页面 %s" % (d["名称"], d["位置"], d["库内"], d["页面"]))

        if args.dry_run:
            print("\n(干跑：未写库、未写记录、未备份)")
            record["结果"] = "干跑"
            return 0

        # ── 2. 写库（单事务）──
        print("\n------ 2. 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = uc.backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        print("  写回: %d 行" % len(pending))
        conn.execute("PRAGMA journal_mode=%s" % JOURNAL_MODE)
        conn.execute("BEGIN IMMEDIATE")
        try:
            for cid, new_note, new_skills, new_talents in pending:
                if new_note is not None:
                    conn.execute("UPDATE %s SET 备注=? WHERE 干员ID=?" % TABLE, (new_note, cid))
                if new_skills is not None:
                    conn.execute("UPDATE %s SET 技能列表=? WHERE 干员ID=?" % TABLE,
                                 (new_skills, cid))
                if new_talents is not None:
                    conn.execute("UPDATE %s SET 天赋列表=? WHERE 干员ID=?" % TABLE,
                                 (new_talents, cid))
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            path = uc.write_record(TABLE, record)
            print("\n!! 写库失败并已回滚：%s" % e)
            print("   记录：%s" % path)
            return 1
    finally:
        conn.close()

    record["结果"] = "成功"
    path = uc.write_record(TABLE, record)
    elapsed = (datetime.now() - started).total_seconds()
    print("\n====== 完成（%.1fs）======" % elapsed)
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
