# -*- coding: utf-8 -*-
"""traps 表更新：校对 + 新增 + 记录（**增量**）。

数据源（本地）：
  <EXCEL>/character_table.json   profession == "TRAP"（846 条）
  <EXCEL>/token_table.json       再补 4 条（其中 3 条 character_table 里没有）
  <EXCEL>/skill_table.json       技能名 / 类型 / 各级描述与 SP

PRTS 侧（**只对新增行爬**，每行一次请求）：
  {{装置信息}}|阻挡半径  → traps.阻挡半径（不是数值 → 跳过并记入构建提示）
  {{装置信息}}|装置机制  → traps.装置机制
  {{装置技能}}|备注      → 技能列表[].备注（按技能名匹配）

天赋备注：装置的天赋写在 `{{装置信息}}` 的 `天赋名称` / `天赋描述` 里，页面上**没有**
「天赋备注」这个位置（补充说明一律写在 `装置机制` 里，如「天赋【爆炸】：…」）。
→ 不补，天赋列表[].备注 保持 null。

页面查找（按优先级，# 表示命中即用）：
  ① PAGE_MAP 里该装置 ID 的精确页面名（整理自旧 patch_traps_prts.py 的 PATCH_MAP）
  ② 去空白 / 换行后的名称（源里 6 个名字带尾随空格或换行）
  ③ 名称 + `(装置)`（与干员同名的装置走这条，如 Mon3tr / 佩佩 / 可露希尔）
命中干员页 / 敌人页 / 消歧义页 → 换下一个候选，并记入记录。

三段式：① 校对 ② 新增 ③ 记录。失败即中止；写库单事务。
跳过清单（对**已有**行永不比对、永不写入）：阻挡半径、装置机制、技能列表[].备注、天赋列表[].备注。
**全量同步只在 reclean_traps.py（一次性修复脚本）里做**，更新脚本一律增量。

用法：
    python update_traps.py --dry-run
    python update_traps.py
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

TABLE = "traps"
SCRIPT_VERSION = 1
MAX_DETAIL = 200
JOURNAL_MODE = "MEMORY"

# 哨兵：已验证过的两个稳定页面（后者同时证明「去空白后的名字能命中页面」）
SENTINELS = (("障碍物", "{{装置信息"), ("液压式高台", "{{装置技能"))

# 装置 ID → PRTS 页面名（游戏数据名与页面名不一致的那批）
PAGE_MAP = {
    "trap_1043_acarm043": "坚定维式重锤",
    "trap_1048_acarm048": "口袋法阵",
    "trap_1066_acarm066": "幸运硬币",
    "trap_065_normbox": "宝箱(水月)",
    "trap_108_smbox": "宝箱(萨米)",
    "trap_757_skzbox": "宝箱(萨卡兹)",
    "trap_066_rarebox": "尖刺宝箱(水月)",
    "trap_109_smrbox": "尖刺宝箱(萨米)",
    "trap_1005_acarm005": "信标(卫戍协议)",
    "trap_1109_acarm109": "信标(装置)",
    "trap_1500_lhgras": "草丛",
}
# 与干员同名的装置走这个后缀（旧 patch 的 FALLBACK_NAMES 实质就是这个规则）
DEVICE_SUFFIX = "(装置)"

# ─── 列分类 ──────────────────────────────────────────────────

ALL_COLS = [
    "装置ID", "名称", "代号", "阻挡半径", "再部署时间", "部署费用", "嘲讽等级", "阻挡数",
    "技力回复速度", "攻击间隔", "元素抗性", "损伤抗性", "装置机制", "精英化阶段",
    "技能列表", "天赋列表",
]
JSON_COLS = {"精英化阶段", "技能列表", "天赋列表"}
INT_COLS = {"再部署时间", "部署费用", "嘲讽等级", "阻挡数"}
FLOAT_COLS = {"技力回复速度", "攻击间隔", "元素抗性", "损伤抗性"}
# 对已有行永不比对、永不写入的列（爬取列）
SKIP_COLS = {"阻挡半径", "装置机制"}
# 寄居在 JSON 列里的爬取子键：比对时剔除、写库时沿用库内值
SKIP_SUBKEY = {"技能列表": "备注", "天赋列表": "备注"}

# ─── 枚举映射（与 db_builder/build_traps.py 同一套语义）─────

SKILL_TYPE_MAP = {"MANUAL": "手动触发", "AUTO": "自动触发", "PASSIVE": "被动"}
SP_TYPE_MAP = {"INCREASE_WITH_TIME": "自动回复", "INCREASE_WHEN_ATTACK": "攻击回复",
               "INCREASE_WHEN_TAKEN_DAMAGE": "受击回复"}
DUR_TYPE_MAP = {"NONE": "普通持续", "AMMO": "弹药类"}
PHASE_MAP = {"PHASE_0": 0, "PHASE_1": 1, "PHASE_2": 2}
SP_KEY_MAP = {"spCost": "技力消耗", "initSp": "初始技力",
              "maxChargeTime": "可充能次数", "increment": "技力回复速度"}
ATTR_NAMES = {"maxHp": "生命上限", "atk": "攻击", "def": "防御",
              "magicResistance": "法术抗性"}

_RE_COMMENT = re.compile(r'<!--[\s\S]*?-->')


def dump(value):
    if value is None:
        return None
    if isinstance(value, (list, dict)) and not value:
        return None
    return json.dumps(value, ensure_ascii=False)


def clean_name(name):
    """源里 6 个装置名带尾随空格或换行 → 统一去掉（Air 2026-10-15 定）。"""
    return (name or "").strip().replace("\n", "").replace("\r", "")


def _parse_json(value):
    if value is None:
        return None
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return None


def strip_notes(col, value):
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


def carry_db_notes(col, want_value, have_value):
    """重建值先继承库内已有的备注：页面上找不到的技能 / 天赋，备注保持不动。"""
    want, have = _parse_json(want_value), _parse_json(have_value)
    if not isinstance(want, list) or not isinstance(have, list):
        return want_value
    key = "技能名" if col == "技能列表" else "天赋名"

    def key_of(item):
        return (item.get(key) or "").strip() if key == "天赋名" else item.get(key, "")

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


# ─── 各列构建 ────────────────────────────────────────────────

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


def compute_rows(ct, tt, skill_table, notes):
    raw = {k: v for k, v in ct.items() if v.get("profession") == "TRAP"}
    for k, v in tt.items():
        if k not in raw and v.get("profession") == "TRAP":
            raw[k] = v

    rows = {}
    for i, (tid, cv) in enumerate(raw.items(), 1):
        if i % 100 == 0 or i == len(raw):
            print("  重算 %d/%d" % (i, len(raw)))
        data = ((cv.get("phases") or [{}])[0].get("attributesKeyFrames") or [{}])[0].get("data", {})
        name = clean_name(cv.get("name")) or tid
        rows[tid] = {
            "装置ID": tid,
            "名称": name,
            "代号": cv.get("appellation", ""),
            "阻挡半径": None,          # 爬取列
            "再部署时间": data.get("respawnTime", 0),
            "部署费用": data.get("cost", 0),
            "嘲讽等级": data.get("tauntLevel", 0),
            "阻挡数": data.get("blockCnt", 0),
            "技力回复速度": data.get("spRecoveryPerSec", 0),
            "攻击间隔": data.get("baseAttackTime", 0),
            "元素抗性": data.get("epDamageResistance", 0),
            "损伤抗性": data.get("epResistance", 0),
            "装置机制": None,          # 爬取列
            "精英化阶段": dump(build_phases(cv)),
            "技能列表": dump(build_skills(cv, skill_table, notes, name)),
            "天赋列表": dump(build_talents(cv)),
        }
    return rows


# ─── PRTS 爬取 ───────────────────────────────────────────────

def _is_foreign_page(text):
    """消歧义页 / 干员页 / 敌人页：不是装置页。"""
    stripped = text.lstrip()
    if stripped.startswith("{{消歧义页"):
        return "消歧义页"
    if "{{干员信息}}" in text or "{{CharinfoV2" in text:
        return "干员页"
    if "{{敌人信息" in text:
        return "敌人页"
    return None


def parse_trap_page(text):
    """从装置页 wikitext 取 (阻挡半径原值, 装置机制, {技能名: 备注}, 是否有装置信息模板)。"""
    radius_raw, mech, info_found = None, None, False
    tpls = uc.parse_template(text, "装置信息")
    if tpls:
        info_found = True
        info = tpls[0]
        raw = info.get("阻挡半径", "")
        if raw:
            radius_raw = raw
        raw = info.get("装置机制", "")
        if raw:
            mech = clean_wiki(raw) or None

    skill_notes = {}
    for params in uc.parse_template(text, "装置技能"):
        sname = params.get("技能名", "")
        if not sname:
            continue
        raw = params.get("备注")
        val = (clean_wiki(raw) or None) if raw else None
        if sname not in skill_notes or (val and not skill_notes[sname]):
            skill_notes[sname] = val
    return radius_raw, mech, skill_notes, info_found


def fetch_trap_page(tid, name, notes, redirects=None):
    """按候选顺序找真正的装置页。返回 (页面名, 页面数据) 或 (None, None)。"""
    clean = clean_name(name)
    candidates = []
    if tid in PAGE_MAP:
        candidates.append(PAGE_MAP[tid])
    candidates.append(clean)
    candidates.append(clean + DEVICE_SUFFIX)

    for cand in candidates:
        try:
            text = uc.fetch_wikitext(cand, redirects=redirects)
        except uc.NoPage:
            notes.append("%s：页面不存在（%s）" % (clean, cand))
            continue
        text = _RE_COMMENT.sub("", text)
        kind = _is_foreign_page(text)
        if kind:
            notes.append("%s：%s 是%s，跳过" % (clean, cand, kind))
            continue
        return cand, parse_trap_page(text)
    return None, None


def apply_page(rows_item, page, notes):
    """把页面结果写进**新增**行：阻挡半径 / 装置机制 / 技能备注。

    已有行不走这里——半径、机制属跳过列，技能备注也归 reclean_traps.py。
    """
    radius_raw, mech, skill_notes, _info_found = page
    changed_skills = 0

    if radius_raw:
        try:
            rows_item["阻挡半径"] = str(float(radius_raw))
        except ValueError:
            notes.append("%s：阻挡半径不是数值（%r），已跳过"
                         % (rows_item["名称"], radius_raw))
    rows_item["装置机制"] = mech

    skills = _parse_json(rows_item["技能列表"]) or []
    for sk in skills:
        sname = sk.get("技能名", "")
        if sname not in skill_notes:
            continue
        val = skill_notes[sname]
        if val:
            sk["备注"] = val
        elif "备注" in sk:
            del sk["备注"]
        changed_skills += 1
    rows_item["技能列表"] = dump(skills)
    return changed_skills


# ─── 主流程 ──────────────────────────────────────────────────

def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="traps 表更新（校对 + 新增 + 记录）")
    ap.add_argument("--dry-run", action="store_true",
                    help="只报告，不写库、不爬取、不写记录、不备份")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    paths.ensure_gamedata()
    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== traps 表更新 ======")
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
        "脚本": {"文件": "update_traps.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "校对": {"检查行": 0, "差异行": 0, "差异列数": 0, "计数": {}, "明细": []},
        "新增": {"装置": 0, "明细": []},
        "爬取": {"目标": 0, "成功": 0, "无页面": 0, "候选名": {},
                 "有阻挡半径": 0, "有装置机制": 0, "有技能备注": 0,
                 "重定向": [], "无页面明细": []},
        "跳过": {
            "列": sorted(SKIP_COLS),
            "子键": {"技能列表": ["备注"], "天赋列表": ["备注"]},
            "说明": "阻挡半径 / 装置机制 / 技能备注为 PRTS 爬取列，对已有行一律不比对、"
                     "不写入；新增行照常爬。全量同步见 reclean_traps.py。",
        },
        "构建提示": [],
        "结果": "未执行",
    }

    print("\n------ 读取源数据 ------")
    def load_json(name):
        with open(paths.EXCEL / name, encoding="utf-8") as f:
            return json.load(f)

    ct = load_json("character_table.json")
    tt = load_json("token_table.json") if (paths.EXCEL / "token_table.json").exists() else {}
    skill_table = load_json("skill_table.json")
    print("  干员/装置表 %d 条 | token_table %d 条 | 技能 %d 条"
          % (len(ct), len(tt), len(skill_table)))

    notes = []
    print("\n------ 重算目标行 ------")
    rows = compute_rows(ct, tt, skill_table, notes)
    print("  目标行数: %d" % len(rows))

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        existing = {}
        for r in conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), TABLE)):
            d = dict(zip(db_cols, r))
            existing[d["装置ID"]] = d

        # ── 1. 校对 ──
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
                # 「名称」是唯一被有意归一化的列（去掉尾随空格/换行）。
                # 这里不能走 same() 的「去空白后比较」——那样归一化就永远写不进去。
                if col == "名称":
                    same_ok = a == b
                else:
                    same_ok = uc.same(col, a, b, json_cols=JSON_COLS,
                                      int_cols=INT_COLS, float_cols=FLOAT_COLS)
                if not same_ok:
                    struct_diffs.append((tid, col))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"]["检查行"] = len(existing)
        record["校对"]["差异行"] = len({d[0] for d in struct_diffs})
        record["校对"]["差异列数"] = len(struct_diffs)
        record["校对"]["计数"] = per_col
        record["校对"]["明细"] = [
            dict({"装置ID": tid, "名称": rows[tid]["名称"], "列": col},
                 **uc.diff_brief(existing[tid][col], rows[tid][col], as_json=col in JSON_COLS))
            for tid, col in struct_diffs[:MAX_DETAIL]
        ]
        print("  库内 %d 行 | 差异行 %d | 差异列 %d"
              % (len(existing), record["校对"]["差异行"], len(struct_diffs)))
        for col, cnt in sorted(per_col.items(), key=lambda x: -x[1]):
            print("    %s: %d 处" % (col, cnt))
        for tid, col in struct_diffs[:12]:
            print("    · %s %s" % (rows[tid]["名称"], col))

        # ── 2. 新增 ──
        new_ids = sorted(k for k in rows if k not in existing)
        record["新增"]["装置"] = len(new_ids)
        record["新增"]["明细"] = [
            {"装置ID": tid, "名称": rows[tid]["名称"]} for tid in new_ids[:MAX_DETAIL]
        ]
        print("\n------ 2. 新增 ------")
        print("  新增装置: %d" % len(new_ids))
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
        if new_ids:
            print("\n------ 3. 爬取 PRTS（%d 个新增装置）------" % len(new_ids))
            for probe, marker in SENTINELS:
                ok, msg = uc.sentinel_ok(probe, marker)
                if not ok:
                    record["结果"] = "中止（结构哨兵失败：%s）" % msg
                    path = uc.write_record(TABLE, record)
                    print("\n!! 中止：%s" % msg)
                    print("   未对数据库做任何修改；记录：%s" % path)
                    return 1

            no_page, redirects, cand_used = [], [], {}
            n_radius = n_mech = n_note = 0
            t0 = time.time()
            for i, tid in enumerate(new_ids, 1):
                name = rows[tid]["名称"]
                if i % 10 == 0 or i == len(new_ids):
                    print("  [%d/%d] %.0fs | %s" % (i, len(new_ids), time.time() - t0, name))
                try:
                    cand, page = fetch_trap_page(tid, name, notes, redirects=redirects)
                except Exception as e:
                    record["结果"] = "中止（爬取失败：%s）" % e
                    path = uc.write_record(TABLE, record)
                    print("\n!! 中止：%s" % e)
                    print("   未对数据库做任何修改；记录：%s" % path)
                    return 1
                if page is None:
                    no_page.append(name)
                    continue
                cand_used[name] = cand
                n_radius += 1 if page[0] else 0
                n_mech += 1 if page[1] else 0
                n_note += 1 if any(page[2].values()) else 0
                apply_page(rows[tid], page, notes)
                time.sleep(0.1)

            record["爬取"]["目标"] = len(new_ids)
            record["爬取"]["成功"] = len(new_ids) - len(no_page)
            record["爬取"]["无页面"] = len(no_page)
            record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
            record["爬取"]["候选名"] = cand_used
            record["爬取"]["有阻挡半径"] = n_radius
            record["爬取"]["有装置机制"] = n_mech
            record["爬取"]["有技能备注"] = n_note
            record["爬取"]["重定向"] = ["%s → %s" % ab for ab in redirects[:MAX_DETAIL]]
            print("  成功 %d | 无页面 %d | 有阻挡半径 %d | 有装置机制 %d | 有技能备注 %d"
                  % (record["爬取"]["成功"], len(no_page), n_radius, n_mech, n_note))

        # ── 4. 写库（单事务）──
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
                conn.execute("UPDATE %s SET %s=? WHERE 装置ID=?" % (TABLE, col),
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
          % (record["校对"]["差异列数"], record["新增"]["装置"],
             record["爬取"]["成功"], record["爬取"]["无页面"]))
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
