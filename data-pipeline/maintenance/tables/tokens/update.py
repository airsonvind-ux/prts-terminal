# -*- coding: utf-8 -*-
"""tokens 表更新：校对 + 新增 + 记录。

数据源（本地）：
  <EXCEL>/character_table.json   profession == "TOKEN" 的条目（当前 75 条）
  <EXCEL>/skill_table.json       技能名 / 类型 / 各级描述与 SP
  operators 表                    持有者（反查 `关联token`，该列存 tokenID 列表）

PRTS 侧（**只对新增行爬**，每行一次请求）：
  {{召唤物信息}}|备注                                  → tokens.备注
  {{技能}} / {{技能2}} / {{无等级技能}} / {{召唤物技能}}|备注 → 技能列表[].备注（按技能名匹配）
  {{召唤物天赋}}|备注                                  → 天赋列表[].备注（按天赋名匹配）

三段式：
  ① 校对：重算结构列并与库内比，差异写回；
  ② 新增：源有库无的召唤物建档，并补爬三类备注；
  ③ 记录：updates/<时间戳>-tokens.json。

跳过清单（对**已有**行永不比对、永不写入）：
  · 备注             PRTS 爬取列（Air 2026-10-15 定）
  · 技能列表[].备注   PRTS 爬取列（同上）
  · 天赋列表[].备注   PRTS 爬取列（同上）
这三处都归页面：只在新增行填；**全量同步只在 reclean_tokens.py（一次性修复脚本）里做**，
更新脚本一律增量——全量爬取只出现在 reclean 脚本里。

口径（Air 2026-10-15 定）：
  · 持有者 = 反查 `operators.关联token`（纯脚本列，校对时重算）。
    反查不到的（仅用于技能实现、游戏内不向玩家展示的召唤物）留空就是正确答案。
    **顺序依赖：operators 必须先更新。**
  · 技能列表[].备注 / 天赋列表[].备注 = 爬取列，跳过清单成员；
    全量同步见 reclean_tokens.py（按技能名 / 天赋名匹配，页面上找不到则不动）。
  · 源里 profession == "TOKEN" 的一律进表（与「是否向玩家展示」无关）。

用法：
    python update_tokens.py --dry-run
    python update_tokens.py
"""

import argparse
import copy
import json
import os
import re
import sqlite3
import sys
import time
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import update_common as uc  # noqa: E402
from blackboard import replace as bb_replace  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402

TABLE = "tokens"
SCRIPT_VERSION = 1
MAX_DETAIL = 200
JOURNAL_MODE = "MEMORY"

# 哨兵：抓已验证过的稳定页面，确认 wiki 侧模板名还在（PRTS 改版会换模板代号）
SENTINEL_INFO = ("医疗探机", "{{召唤物信息")
SENTINEL_SKILL = ("医疗探机", "{{无等级技能")
SENTINEL_TALENT = ("牙猎犬", "{{召唤物天赋")

# 重名 token 的 PRTS 页面名（与干员页同名时使用）
PAGE_OVERRIDE = {
    "Mon3tr": "Mon3tr(凯尔希的召唤物)",
}

# ─── 列分类 ──────────────────────────────────────────────────

ALL_COLS = [
    "tokenID", "名称", "代号", "特性", "持有者", "部署位置", "部署费用", "阻挡数",
    "嘲讽等级", "再部署时间", "攻击速度", "攻击间隔", "精英化阶段", "技能列表",
    "天赋列表", "备注",
]
JSON_COLS = {"精英化阶段", "技能列表", "天赋列表"}
INT_COLS = {"部署费用", "阻挡数", "嘲讽等级", "再部署时间"}
FLOAT_COLS = {"攻击速度", "攻击间隔"}
# 对已有行永不比对、永不写入的列
SKIP_COLS = {"备注"}
# 寄居在 JSON 列里的爬取子键：比对时剔除、写库时沿用库内值。
# 内容归 PRTS 页面：只在新增行填，全量同步归 reclean_tokens.py。
SKIP_SUBKEY = {"技能列表": "备注", "天赋列表": "备注"}

# ─── 枚举映射（与 db_builder/build_tokens.py 同一套语义）────

POSITION_MAP = {"MELEE": "近战位", "RANGED": "远程位", "ALL": "全部位", "NONE": "无"}
SKILL_TYPE_MAP = {"MANUAL": "手动触发", "AUTO": "自动触发", "PASSIVE": "被动"}
SP_TYPE_MAP = {"INCREASE_WITH_TIME": "自动回复", "INCREASE_WHEN_ATTACK": "攻击回复",
               "INCREASE_WHEN_TAKEN_DAMAGE": "受击回复"}
DUR_TYPE_MAP = {"NONE": "普通持续", "AMMO": "弹药类"}
PHASE_MAP = {"PHASE_0": 0, "PHASE_1": 1, "PHASE_2": 2}
SP_KEY_MAP = {"spCost": "技力消耗", "initSp": "初始技力",
              "maxChargeTime": "可充能次数", "increment": "技力回复速度"}
# 关键帧属性：tokens 只保留这四项（与库内结构一致）
ATTR_NAMES = {"maxHp": "生命上限", "atk": "攻击", "def": "防御",
              "magicResistance": "法术抗性"}

# 页面模板名：技能模板的版本号会变（技能 / 技能2 / 技能3…），按前缀扫
SKILL_TPL_NAMES = ("技能", "无等级技能", "召唤物技能")
TALENT_TPL = "召唤物天赋"
_RE_TALENT_NAME_KEY = re.compile(r"天赋\d+$")


def dump(value):
    """列表/字典 → JSON 文本；空值与空表一律 None（与库内口径一致）。"""
    if value is None:
        return None
    if isinstance(value, (list, dict)) and not value:
        return None
    return json.dumps(value, ensure_ascii=False)


def _parse_json(value):
    if value is None:
        return None
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return None


def strip_notes(col, value):
    """比对用：去掉 JSON 列里寄居的爬取子键（它归页面，不由源数据决定）。"""
    if col not in SKIP_SUBKEY or value is None:
        return value
    obj = _parse_json(value)
    if not isinstance(obj, list):
        return value
    obj = copy.deepcopy(obj)
    for item in obj:
        if isinstance(item, dict):
            item.pop(SKIP_SUBKEY[col], None)
    return json.dumps(obj, ensure_ascii=False)


# ─── 各列构建 ────────────────────────────────────────────────

def first_frame(cv):
    try:
        return cv["phases"][0]["attributesKeyFrames"][0]["data"]
    except (KeyError, IndexError, TypeError):
        return {}


def build_phases(cv):
    phases = []
    for ph in cv.get("phases") or []:
        frames = []
        for af in ph.get("attributesKeyFrames") or []:
            data = af.get("data", {})
            frames.append({"等级": af.get("level"),
                           "属性": {cn: data.get(k, 0) for k, cn in ATTR_NAMES.items()}})
        phases.append({"范围ID": ph.get("rangeId", ""),
                       "最高等级": ph.get("maxLevel"), "关键帧": frames})
    return phases


def build_skills(cv, skill_table, notes, who):
    """召唤物的 skills 数组按精英化阶段占位（同一 skillId 重复出现），按 skillId 去重。"""
    out, seen = [], set()
    for sr in cv.get("skills") or []:
        sid = sr.get("skillId", "")
        if not sid or sid in seen:
            continue
        seen.add(sid)
        sk = skill_table.get(sid)
        if not sk:
            continue
        levels = sk.get("levels") or []
        lv0 = levels[0] if levels else {}
        sp0 = lv0.get("spData") or {}
        sp_type = sp0.get("spType", "")

        details = []
        for i, lv in enumerate(levels):
            bb = {b["key"]: b["value"] for b in (lv.get("blackboard") or [])}
            text, _ = bb_replace(lv.get("description") or "", bb, lv.get("duration"))
            if text and re.search(r'\{[^{}]*\}|</>', text):
                notes.append("%s / %s Lv%d：描述有未解析残留" % (who, lv0.get("name", ""), i + 1))
            sp = {}
            for raw_key, cn_key in SP_KEY_MAP.items():
                val = (lv.get("spData") or {}).get(raw_key)
                sp[cn_key] = 0 if val is None else val
            details.append({"等级": i + 1, "范围ID": lv.get("rangeId"),
                            "描述": text, "持续时间": lv.get("duration"), "SP": sp})

        out.append({
            "技能ID": sid,
            "技能名": lv0.get("name", ""),
            "触发类型": SKILL_TYPE_MAP.get(lv0.get("skillType", ""), lv0.get("skillType", "")),
            "回复类型": "被动" if isinstance(sp_type, int) else SP_TYPE_MAP.get(sp_type, str(sp_type)),
            "持续类型": DUR_TYPE_MAP.get(lv0.get("durationType", ""), lv0.get("durationType", "")),
            "各级详情": details,
        })
    return out


def build_talents(cv):
    groups = {}
    for talent in cv.get("talents") or []:
        for cand in (talent.get("candidates") or []):
            if cand.get("isHideTalent"):
                continue
            pk = cand.get("prefabKey", "")
            g = groups.setdefault(pk, {"name": cand.get("name", ""), "candidates": []})
            cond = cand.get("unlockCondition") or {}
            g["candidates"].append({
                "描述": clean_desc(cand.get("description", "")),
                "条件": {"精英化阶段": PHASE_MAP.get(cond.get("phase", "PHASE_0"), 0),
                          "潜能等级": cand.get("requiredPotentialRank", 0)},
            })
    return [{"序号": pk, "天赋名": groups[pk]["name"], "备注": None,
             "备选": groups[pk]["candidates"]} for pk in sorted(groups)]


def load_owners(conn, notes):
    """持有者 = 反查 operators.关联token（该列存的是 tokenID 列表）。"""
    owners, dup = {}, []
    for name, link in conn.execute("SELECT 名称, 关联token FROM operators"):
        if not link:
            continue
        obj = _parse_json(link)
        if not isinstance(obj, list):
            if isinstance(obj, str) and obj:
                notes.append("operators 关联token 不是列表：%s → %r" % (name, link))
            continue
        for tid in obj:
            if not isinstance(tid, str):
                continue
            if tid in owners and owners[tid] != name:
                dup.append("%s ← %s / %s" % (tid, owners[tid], name))
            else:
                owners.setdefault(tid, name)
    if dup:
        notes.append("同一 token 被多名干员引用（取先出现者）：%s" % "；".join(dup[:20]))
    return owners


def compute_rows(ct, skill_table, owners, notes):
    tokens = [(k, v) for k, v in ct.items() if v.get("profession") == "TOKEN"]
    rows = {}
    for i, (tid, cv) in enumerate(tokens, 1):
        if i % 20 == 0 or i == len(tokens):
            print("  重算 %d/%d" % (i, len(tokens)))
        data = first_frame(cv)
        name = cv.get("name", "") or tid
        rows[tid] = {
            "tokenID": tid,
            "名称": name,
            "代号": cv.get("appellation", ""),
            "特性": clean_desc(cv.get("description")) or None,
            "持有者": owners.get(tid),
            "部署位置": POSITION_MAP.get(cv.get("position", ""), cv.get("position", "")),
            "部署费用": data.get("cost", 0),
            "阻挡数": data.get("blockCnt", 0),
            "嘲讽等级": data.get("tauntLevel", 0),
            "再部署时间": data.get("respawnTime", 0),
            "攻击速度": data.get("attackSpeed", 0),
            "攻击间隔": data.get("baseAttackTime", 0),
            "精英化阶段": dump(build_phases(cv)),
            "技能列表": dump(build_skills(cv, skill_table, notes, name)),
            "天赋列表": dump(build_talents(cv)),
            "备注": None,
        }
    return rows


# ─── PRTS 爬取 ───────────────────────────────────────────────

def crawl_token_page(name, redirects=None):
    """抓一个召唤物页，取三类文本。

    返回 (备注列, {技能名: 备注}, {天赋名: 备注}, 页面是否带召唤物信息模板)。
    第四项给 reclean 用：区分「页面没这个模板」（不动）与「模板在但备注为空」（清空）。
    """
    text = uc.fetch_wikitext(name, redirects=redirects)

    note = None
    tpls = uc.parse_template(text, "召唤物信息")
    info_found = bool(tpls)
    if tpls:
        raw = tpls[0].get("备注")
        if raw:
            note = clean_wiki(raw) or None

    skill_notes = {}
    for tpl_name in SKILL_TPL_NAMES:
        for params in uc.parse_template(text, tpl_name):
            sname = params.get("技能名", "")
            if not sname:
                continue
            raw = params.get("备注")
            val = (clean_wiki(raw) or None) if raw else None
            # 同一技能可能同时挂在多个版本模板上：有备注的优先
            if sname not in skill_notes or (val and not skill_notes[sname]):
                skill_notes[sname] = val

    talent_notes = {}
    for params in uc.parse_template(text, TALENT_TPL):
        raw = params.get("备注")
        val = (clean_wiki(raw) or None) if raw else None
        for key, tname in params.items():
            if _RE_TALENT_NAME_KEY.search(key) and tname:
                talent_notes[tname.strip()] = val

    return note, skill_notes, talent_notes, info_found


def carry_db_notes(col, want_value, have_value):
    """重建值先继承库内已有的备注：页面上找不到的技能 / 天赋，备注保持不动。"""
    want, have = _parse_json(want_value), _parse_json(have_value)
    if not isinstance(want, list) or not isinstance(have, list):
        return want_value
    if col == "技能列表":
        def key_of(item):
            return item.get("技能名", "")
    else:
        def key_of(item):
            return (item.get("天赋名") or "").strip()
    notes_by_key = {}
    for item in have:
        if isinstance(item, dict) and item.get("备注"):
            notes_by_key.setdefault(key_of(item), item["备注"])
    for item in want:
        if isinstance(item, dict):
            note = notes_by_key.get(key_of(item))
            if note:
                item["备注"] = note
    return dump(want)


def apply_page_notes(row, note, skill_notes, talent_notes, is_new, notes, who):
    """把页面文本按「以页面为准」写进行结构。返回 (技能备注变更处, 天赋备注变更处)。

    · 技能：页面上找到该技能名 → 以页面为准（含清空）；没找到 → 不动。
    · 天赋：天赋名在页面出现 → 以页面为准（含清空）；没出现 → 不动。
    · 备注列：只在新增行写。
    """
    counts = {"技能备注变更": 0, "技能备注清空": 0, "天赋备注变更": 0, "天赋备注清空": 0}

    if is_new and note:
        row["备注"] = note

    skills = _parse_json(row["技能列表"]) or []
    for sk in skills:
        sname = sk.get("技能名", "")
        if sname not in skill_notes:
            continue
        val = skill_notes[sname]
        if val:
            if sk.get("备注") != val:
                sk["备注"] = val
                counts["技能备注变更"] += 1
        elif "备注" in sk:
            del sk["备注"]
            counts["技能备注变更"] += 1
            counts["技能备注清空"] += 1
    row["技能列表"] = dump(skills)

    talents = _parse_json(row["天赋列表"]) or []
    names = {(i.get("天赋名") or "").strip() for i in talents}
    for item in talents:
        tname = (item.get("天赋名") or "").strip()
        if tname not in talent_notes:
            continue
        val = talent_notes[tname]
        if val:
            if item.get("备注") != val:
                item["备注"] = val
                counts["天赋备注变更"] += 1
        elif item.get("备注"):
            item["备注"] = None
            counts["天赋备注变更"] += 1
            counts["天赋备注清空"] += 1
    row["天赋列表"] = dump(talents)

    # 页面给了天赋备注、但天赋名一个都对不上：记一笔，别默默丢掉
    unmatched = [t for t in talent_notes if t not in names]
    if unmatched:
        notes.append("%s：页面天赋备注未匹配到库内天赋名 %s" % (who, unmatched))
    return counts


# ─── 主流程 ──────────────────────────────────────────────────

def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="tokens 表更新（校对 + 新增 + 记录）")
    ap.add_argument("--dry-run", action="store_true",
                    help="只报告，不写库、不爬取、不写记录、不备份")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    paths.ensure_gamedata()
    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== tokens 表更新 ======")
    print("数据库  : %s" % args.db)
    print("数据源  : %s" % paths.DATA_SOURCE)
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
        "脚本": {"文件": "update_tokens.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "校对": {"检查行": 0, "差异行": 0, "差异列数": 0, "计数": {}, "明细": []},
        "新增": {"召唤物": 0, "明细": []},
        "爬取": {"目标": 0, "成功": 0, "无页面": 0, "有备注": 0,
                 "有技能备注": 0, "有天赋备注": 0, "重定向": [], "无页面明细": []},
        "跳过": {
            "列": sorted(SKIP_COLS),
            "子键": {"技能列表": ["备注"], "天赋列表": ["备注"]},
            "说明": "备注类（含技能/天赋的备注子键）为 PRTS 爬取列，对已有行一律不比对、"
                     "不写入；新增行照常爬。全量同步见 reclean_tokens.py。",
        },
        "构建提示": [],
        "结果": "未执行",
    }

    print("\n------ 读取源数据 ------")
    def load_json(name):
        with open(paths.EXCEL / name, encoding="utf-8") as f:
            return json.load(f)

    ct = load_json("character_table.json")
    skill_table = load_json("skill_table.json")
    print("  干员表/召唤物表 %d 条 | 技能 %d 条" % (len(ct), len(skill_table)))

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        notes = []
        owners = load_owners(conn, notes)
        print("  持有者来源: operators.关联token → %d 个 token" % len(owners))

        print("\n------ 重算目标行 ------")
        rows = compute_rows(ct, skill_table, owners, notes)
        dangling = sorted(set(owners) - set(rows))
        if dangling:
            notes.append("operators.关联token 引用了源里没有的 token：%s" % "、".join(dangling))
        print("  目标行数: %d" % len(rows))

        db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        existing = {}
        for r in conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), TABLE)):
            d = dict(zip(db_cols, r))
            existing[d["tokenID"]] = d

        # ── 1. 校对（结构）──
        print("\n------ 1. 校对 ------")
        struct_diffs, per_col = [], {}
        for tid, want in rows.items():
            have = existing.get(tid)
            if have is None:
                continue
            for col in ALL_COLS:
                if col in SKIP_COLS:
                    continue
                a, b = have[col], want[col]
                if col in SKIP_SUBKEY:
                    a, b = strip_notes(col, a), strip_notes(col, b)
                if not uc.same(col, a, b, json_cols=JSON_COLS,
                               int_cols=INT_COLS, float_cols=FLOAT_COLS):
                    struct_diffs.append((tid, col))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"]["检查行"] = len(existing)
        record["校对"]["差异行"] = len({d[0] for d in struct_diffs})
        record["校对"]["差异列数"] = len(struct_diffs)
        record["校对"]["计数"] = per_col
        record["校对"]["明细"] = [
            dict({"tokenID": tid, "名称": rows[tid]["名称"], "列": col},
                 **uc.diff_brief(existing[tid][col], rows[tid][col], as_json=col in JSON_COLS))
            for tid, col in struct_diffs[:MAX_DETAIL]
        ]
        print("  库内 %d 行 | 差异行 %d | 差异列 %d"
              % (len(existing), record["校对"]["差异行"], len(struct_diffs)))
        for col, cnt in sorted(per_col.items(), key=lambda x: -x[1]):
            print("    %s: %d 处" % (col, cnt))
        for tid, col in struct_diffs[:10]:
            print("    · %s %s" % (rows[tid]["名称"], col))

        # ── 2. 新增 ──
        new_ids = sorted(k for k in rows if k not in existing)
        record["新增"]["召唤物"] = len(new_ids)
        record["新增"]["明细"] = [
            {"tokenID": tid, "名称": rows[tid]["名称"]} for tid in new_ids[:MAX_DETAIL]
        ]
        print("\n------ 2. 新增 ------")
        print("  新增召唤物: %d" % len(new_ids))
        for tid in new_ids[:20]:
            print("    %s  %s" % (tid, rows[tid]["名称"]))
        gone = sorted(k for k in existing if k not in rows)
        if gone:
            print("  库有源无（不删，仅记录）: %d 个" % len(gone))

        if args.dry_run:
            print("\n(干跑：未写库、未爬取、未写记录)")
            record["结果"] = "干跑"
            return 0

        # ── 3. 爬取（只爬新增；失败整体中止）──
        crawled = {}
        if new_ids:
            print("\n------ 3. 爬取 PRTS（%d 个新增召唤物）------" % len(new_ids))
            for probe, marker in (SENTINEL_INFO, SENTINEL_SKILL, SENTINEL_TALENT):
                ok, msg = uc.sentinel_ok(probe, marker)
                if not ok:
                    record["结果"] = "中止（结构哨兵失败：%s）" % msg
                    path = uc.write_record(TABLE, record)
                    print("\n!! 中止：%s" % msg)
                    print("   未对数据库做任何修改；记录：%s" % path)
                    return 1

            no_page, redirects = [], []
            note_rows = skill_note_rows = talent_note_rows = 0
            t0 = time.time()
            for i, tid in enumerate(new_ids, 1):
                name = rows[tid]["名称"]
                if i % 10 == 0 or i == len(new_ids):
                    print("  [%d/%d] %.0fs | %s" % (i, len(new_ids), time.time() - t0, name))
                try:
                    page = crawl_token_page(PAGE_OVERRIDE.get(name, name), redirects=redirects)
                except uc.NoPage:
                    no_page.append(name)
                    continue
                except Exception as e:
                    record["结果"] = "中止（爬取失败：%s）" % e
                    path = uc.write_record(TABLE, record)
                    print("\n!! 中止：%s" % e)
                    print("   未对数据库做任何修改；记录：%s" % path)
                    return 1

                note_rows += 1 if page[0] else 0
                skill_note_rows += 1 if any(page[1].values()) else 0
                talent_note_rows += 1 if any(page[2].values()) else 0
                apply_page_notes(rows[tid], page[0], page[1], page[2], True, notes, name)
                crawled[tid] = page
                time.sleep(0.1)

            record["爬取"]["目标"] = len(new_ids)
            record["爬取"]["成功"] = len(crawled)
            record["爬取"]["无页面"] = len(no_page)
            record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
            record["爬取"]["重定向"] = ["%s → %s" % ab for ab in redirects[:MAX_DETAIL]]
            record["爬取"]["有备注"] = note_rows
            record["爬取"]["有技能备注"] = skill_note_rows
            record["爬取"]["有天赋备注"] = talent_note_rows
            print("  成功 %d | 无页面 %d | 有备注 %d | 有技能备注 %d | 有天赋备注 %d"
                  % (len(crawled), len(no_page), note_rows, skill_note_rows, talent_note_rows))

        # ── 4. 写库（单事务）──
        # JSON 列若因结构差异要重写，先把库内已有的备注子键搬回来（它归页面，不归源）
        for tid, col in struct_diffs:
            if col in SKIP_SUBKEY:
                rows[tid][col] = carry_db_notes(col, rows[tid][col], existing[tid][col])
        print("\n------ 4. 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = uc.backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        print("  写回校对差异: %d 列 | 插入新增: %d 行" % (len(struct_diffs), len(new_ids)))
        conn.execute("PRAGMA journal_mode=%s" % JOURNAL_MODE)
        conn.execute("BEGIN IMMEDIATE")
        try:
            for tid, col in struct_diffs:
                conn.execute("UPDATE %s SET %s=? WHERE tokenID=?" % (TABLE, col),
                             (rows[tid][col], tid))
            for tid in new_ids:
                conn.execute(
                    "INSERT OR IGNORE INTO %s (%s) VALUES (%s)"
                    % (TABLE, ", ".join(ALL_COLS), ", ".join("?" * len(ALL_COLS))),
                    tuple(rows[tid][c] for c in ALL_COLS),
                )
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

    record["构建提示"] = notes[:MAX_DETAIL]
    record["结果"] = "成功"
    path = uc.write_record(TABLE, record)

    elapsed = (datetime.now() - started).total_seconds()
    print("\n====== 完成（%.1fs）======" % elapsed)
    print("  校对写回: %d 列 | 新增: %d | 爬取成功: %d | 无页面: %d"
          % (record["校对"]["差异列数"], record["新增"]["召唤物"],
             record["爬取"]["成功"], record["爬取"]["无页面"]))
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
