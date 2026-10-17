# -*- coding: utf-8 -*-
"""operators 表：三处 wiki 文本的一次性同步清洗。

背景：库内这三处的文本来自**旧的清洗方案**，与现在的 `cleaning_wiki` 规则集不同。
旧 `build_operators_note.py` 的 `clean_note` 遇到 `{{popup|…}}` 整段删掉，旧
`clean_wiki` 又比现在少十几条规则。清洗会丢信息，就地再洗一遍救不回来，所以回
PRTS 页面**重取原文**、用当前规则重洗。

清洗对象只有这三处，**策略按字段分开**（Air 2026-10-15 定）：

    备注                  ← {{CharinfoV2}} 的 特性备注      【保护】
        这里有 Air 手写的算法说明（形如「技能xxx的xxx算法为xxx」），
        也可能只是旧爬取结果。判据：页面给出文本，且（库内为空 或
        `clean_wiki(库内值)` == 页面新清洗结果）才重写；否则保留并记入
        「保留（备注列）」供人工过一遍。
        页面没给出这一项 → 连比都不比，原样不动。

    技能列表[].备注        ← {{技能2}} 的 备注               【以页面为准】
    天赋列表[].备注        ← {{天赋列表3}} 的 备注            【以页面为准】
        Air 不在这两处写补充，库内值只是旧爬取的产物，所以直接还原成页面当前值
        （**包括页面现在没有备注时把它清空**）。
        页面上找不到这一项（技能不在页面 / 天赋份数不够）→ 不动，避免误清空。

另两个开关：`--only-empty` 只填空值（三处都是）；`--page-wins` 把备注列也改成
「以页面为准」（会覆盖手写说明，需显式指定）。

流程：先把全表爬完、算完，再**单事务**写库。跑之前先做结构哨兵，确认 wiki 侧
模板名还在。

用法：
    python reclean_operators.py
    python reclean_operators.py --dry-run        # 只报告
    python reclean_operators.py --only-empty     # 只填空值
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
import update_operators as up  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402

TABLE = "operators"
SCRIPT_VERSION = 2
MAX_DETAIL = 200
MAX_KEPT_DETAIL = 1000
JOURNAL_MODE = "MEMORY"
RECORD_NAME = "operators-reclean"

# 结构哨兵：确认 wiki 侧的干员页模板名没换代
SENTINELS = (("玫兰莎", "{{技能2"), ("香草", "{{天赋列表3"))


def _pure_crawl(old, new):
    """库内值是否就是同一段正文的旧清洗产物（空值也算）。

    判据：拿当前规则再洗一遍库内值，洗出来等于页面的新清洗结果 —— 说明这个字段里
    除了那段正文没别的东西；手写补充洗不掉，等号不成立 → 视为可能含手写内容。
    """
    if not old:
        return True
    return clean_wiki(old) == new


def plan_row(cur_note, skills, talents, charinfo, skill_notes, talent_notes,
             only_empty, page_wins):
    """算这一行要改哪几处、哪几处保留；并给出三列的新值（没变就是 None）。

    返回 (changes, kept, new_note, new_skills_json, new_talents_json)
    """
    changes, kept, wrote_skills, wrote_talents = [], [], False, False
    new_note = cur_note

    def decide(location, old, new, protect):
        nonlocal new_note
        if new == old or (only_empty and old):
            return
        if protect and not page_wins and not _pure_crawl(old, new):
            kept.append((location, old, new))
            return
        changes.append((location, old, new))
        if location == "备注":
            new_note = new

    # 备注列：页面给出才动；默认保护
    if charinfo is not None:
        decide("备注", cur_note, charinfo, protect=True)

    # 技能备注：页面上有该技能就以页面为准（含清空）
    for sk in skills:
        sname = sk.get("技能名", "")
        if sname not in skill_notes:
            continue
        new = skill_notes[sname]
        if new == sk.get("备注") or (only_empty and sk.get("备注")):
            continue
        changes.append(("技能备注[%s]" % sname, sk.get("备注"), new))
        sk["备注"] = new
        wrote_skills = True

    # 天赋备注：页面给出第 i 份就以页面为准（含清空）
    for i, item in enumerate(talents):
        if i >= len(talent_notes):
            continue
        new = talent_notes[i]
        if new == item.get("备注") or (only_empty and item.get("备注")):
            continue
        changes.append(("天赋备注[%s]" % item.get("序号", ""), item.get("备注"), new))
        item["备注"] = new
        wrote_talents = True

    return (changes, kept, new_note,
            json.dumps(skills, ensure_ascii=False) if wrote_skills else None,
            json.dumps(talents, ensure_ascii=False) if wrote_talents else None)


def main():
    ap = argparse.ArgumentParser(description="operators 三处 wiki 文本的同步清洗（重爬重洗）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不写记录、不备份")
    ap.add_argument("--only-empty", action="store_true", help="只填空值，任何已有文本都不动")
    ap.add_argument("--page-wins", action="store_true",
                    help="备注列也以页面为准（会覆盖手写说明）；默认保护")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    if args.only_empty and args.page_wins:
        raise SystemExit("--only-empty 与 --page-wins 不能同时用")
    mode = ("only-empty" if args.only_empty else
            "page-wins" if args.page_wins else "备注列保护/技能与天赋以页面为准")

    started = datetime.now()
    print("====== operators 三处 wiki 文本同步清洗 ======")
    print("数据库  : %s" % args.db)
    print("策略    : %s" % mode)
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
        "字段策略": {"备注": "保护（人工补充与旧页面文本都保留，逐条列出）",
                     "技能列表[].备注": "以页面为准（含清空）",
                     "天赋列表[].备注": "以页面为准（含清空）"},
        "爬取": {"目标干员": 0, "成功": 0, "无页面": 0, "重定向": [], "无页面明细": []},
        "变更": {"行": 0, "处": 0, "备注": 0, "技能备注": 0, "技能备注清空": 0,
                 "天赋备注": 0, "天赋备注清空": 0, "明细": []},
        "保留（备注列）": {"处": 0, "明细": []},
        "结果": "未执行",
    }

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        rows = conn.execute(
            "SELECT 干员ID, 名称, 备注, 技能列表, 天赋列表 FROM %s ORDER BY 名称" % TABLE
        ).fetchall()

        print("\n------ 结构哨兵 ------")
        for probe, marker in SENTINELS:
            ok, msg = uc.sentinel_ok(probe, marker)
            if not ok:
                record["结果"] = "中止（结构哨兵失败：%s）" % msg
                path = uc.write_record(RECORD_NAME, record)
                print("  !! 中止：%s" % msg)
                print("     未对数据库做任何修改；记录：%s" % path)
                return 1
            print("  %s 上的 %s 还在" % (probe, marker))

        print("\n------ 爬取 PRTS（%d 名干员）------" % len(rows))
        no_page, redirects = [], []
        pending = []          # [(干员ID, 备注 或 None, 技能列表 或 None, 天赋列表 或 None)]
        touched = {"备注": 0, "技能备注": 0, "技能备注清空": 0,
                   "天赋备注": 0, "天赋备注清空": 0}
        details, kept_details = [], []
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
                path = uc.write_record(RECORD_NAME, record)
                print("\n!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
            time.sleep(0.1)

            skills = json.loads(sk_json) if sk_json else []
            talents = json.loads(ta_json) if ta_json else []
            changes, kept, new_note, new_skills, new_talents = plan_row(
                cur_note, skills, talents, charinfo, skill_notes, talent_notes,
                args.only_empty, args.page_wins)

            for location, old, new in kept:
                kept_details.append({"干员ID": cid, "名称": name, "位置": location,
                                     "库内": uc.brief(old, 160), "页面": uc.brief(new, 160)})
            for location, old, new in changes:
                if location == "备注":
                    touched["备注"] += 1
                elif location.startswith("技能"):
                    touched["技能备注清空" if new is None else "技能备注"] += 1
                else:
                    touched["天赋备注清空" if new is None else "天赋备注"] += 1
                if len(details) < MAX_DETAIL:
                    details.append({"干员ID": cid, "名称": name, "位置": location,
                                    "旧": uc.brief(old, 120), "新": uc.brief(new, 120)})

            if changes:
                pending.append((cid, new_note if new_note != cur_note else None,
                                new_skills, new_talents))

        record["爬取"]["目标干员"] = len(rows)
        record["爬取"]["无页面"] = len(no_page)
        record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
        record["爬取"]["成功"] = len(rows) - len(no_page)
        record["爬取"]["重定向"] = ["%s → %s" % ab for ab in redirects[:MAX_DETAIL]]
        record["变更"]["行"] = len(pending)
        record["变更"]["处"] = sum(touched.values())
        for k in ("备注", "技能备注", "技能备注清空", "天赋备注", "天赋备注清空"):
            record["变更"][k] = touched[k]
        record["变更"]["明细"] = details
        record["保留（备注列）"]["处"] = len(kept_details)
        record["保留（备注列）"]["明细"] = kept_details[:MAX_KEPT_DETAIL]

        print("\n------ 1. 结果 ------")
        print("  成功 %d | 无页面 %d | 变更行 %d | 变更处 %d | 备注列保留 %d 处"
              % (record["爬取"]["成功"], len(no_page), len(pending),
                 record["变更"]["处"], len(kept_details)))
        print("    技能备注: 改写 %d / 清空 %d | 天赋备注: 改写 %d / 清空 %d"
              % (touched["技能备注"], touched["技能备注清空"],
                 touched["天赋备注"], touched["天赋备注清空"]))
        if no_page:
            print("    无页面: %s" % ", ".join(no_page[:10]))
        for d in details[:8]:
            print("    改 · %s %s | %s → %s" % (d["名称"], d["位置"], d["旧"], d["新"]))
        for d in kept_details[:10]:
            print("    留 · %s %s | 库内 %s | 页面 %s"
                  % (d["名称"], d["位置"], d["库内"], d["页面"]))

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
    print("  更新记录: %s（备注列保留项逐条写在里面）" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
