# -*- coding: utf-8 -*-
"""operators 表更新：校对 + 新增 + 记录。

数据源（除爬取外全在本地）：
  <EXCEL>/character_table.json     干员主体（21 列里绝大多数）
  <EXCEL>/char_patch_table.json    升变/异格形态（阿米娅等），并入干员集合
  <EXCEL>/skill_table.json         技能名/类型/各级描述与 SP
  <EXCEL>/uniequip_table.json      职业分支名、模组目录
  <EXCEL>/battle_equip_table.json  模组各级效果
  maintenance/sources/模组特性信息.md   模组的「特性」文本（人工维护，脚本只读）

PRTS 侧（**只对新增干员爬**，每行一次请求）：
  {{天赋列表3}}   天赋表模板，取 `备注` 参数（一页一个实例）
  {{技能2}}       单个技能表模板，取 `备注` 参数（一页多个，按技能名匹配）
  {{CharinfoV2}}  取 `特性备注` 参数，写入 operators.备注 列

三段式：
  ① 校对：重算每一列，与库内比；差异写回。本地源列基本不变，这一段主要是守结构。
  ② 新增：源有库无的干员建档，并补爬三类备注（否则新干员是空的）。
  ③ 记录：updates/<时间戳>-operators.json。
失败即中止；写库单事务；干跑不写库、不爬取、不写记录。

跳过清单（对**已有**干员永不比对、永不写入）：
  · 描述                        人工调整过（Air 2026-10-15 定）
  · 备注                        爬取的特性备注 + 人工补充的算法说明
  · 技能列表[].备注             爬取 + 人工
  · 天赋列表[].备注             爬取 + 人工
  · 模组列表                    整列人工调整过（Air 2026-10-15 定）
新增行例外：这些列在新增时照常由源数据 / 爬取 / 人工源填上（描述与模组列表随后由 Air 手工调整）。

用法：
    python update_operators.py --dry-run
    python update_operators.py
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
sys.path.insert(0, str(HERE.parent))   # data-pipeline/（paths.py 在这一层）
sys.path.insert(0, str(HERE))          # maintenance/

import paths  # noqa: E402
import update_common as uc  # noqa: E402
from blackboard import replace as bb_replace  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402

TABLE = "operators"
SCRIPT_VERSION = 1
MAX_DETAIL = 200
PROGRESS_EVERY = 50
JOURNAL_MODE = "MEMORY"
MODULE_TRAITS_MD = "模组特性信息.md"

# 哨兵：抓一个稳定页面，确认 wiki 侧模板名还在（PRTS 改版会换模板代号）
SENTINEL_SKILL = ("玫兰莎", "{{技能2")
SENTINEL_TALENT = ("香草", "{{天赋列表3")

# ─── 列分类 ──────────────────────────────────────────────────

ALL_COLS = [
    "干员ID", "名称", "代号", "描述", "部署位置", "标签", "获取方式", "是否不可获取",
    "是否为异格", "稀有度", "职业", "职业分支", "最大潜能", "备注", "信赖加成",
    "精英化阶段", "技能列表", "关联token", "天赋列表", "潜能提升", "模组列表",
]
JSON_COLS = {"标签", "信赖加成", "精英化阶段", "技能列表", "关联token", "天赋列表",
             "潜能提升", "模组列表"}
INT_COLS = {"是否不可获取", "是否为异格", "稀有度", "最大潜能"}
# 对已有干员永不比对、永不写入的列（描述与模组列表是 Air 手工调整过的）
SKIP_COLS = {"描述", "备注", "模组列表"}

# 顶层跳过列的「子键」：比对时剔除、写库时保留（内容归爬取 / 人工）
KEEP_SUBKEY = {"技能列表": "备注", "天赋列表": "备注"}
# 各表项的身份键（用于把子键搬回对应项）
ITEM_ID = {"技能列表": "技能ID", "天赋列表": "序号"}

# ─── 枚举映射（与 db_builder/build_operators.py 同一套语义）────

OPS_PROFS = {"WARRIOR", "SNIPER", "TANK", "MEDIC", "SUPPORT", "CASTER", "SPECIAL", "PIONEER"}
POSITION_MAP = {"MELEE": "近战位", "RANGED": "远程位"}
PROF_MAP = {"WARRIOR": "近卫", "SNIPER": "狙击", "TANK": "重装", "MEDIC": "医疗",
            "SUPPORT": "辅助", "CASTER": "术师", "SPECIAL": "特种", "PIONEER": "先锋"}
SKILL_TYPE_MAP = {"MANUAL": "手动触发", "AUTO": "自动触发", "PASSIVE": "被动"}
SP_TYPE_MAP = {"INCREASE_WITH_TIME": "自动回复", "INCREASE_WHEN_ATTACK": "攻击回复",
               "INCREASE_WHEN_TAKEN_DAMAGE": "受击回复"}
DUR_TYPE_MAP = {"NONE": "普通持续", "AMMO": "弹药类"}
PHASE_MAP = {"PHASE_0": 0, "PHASE_1": 1, "PHASE_2": 2}
SP_KEY_MAP = {"spCost": "技力消耗", "initSp": "初始技力",
              "maxChargeTime": "可充能次数", "increment": "技力回复速度"}

# 属性名：前者来自 build_operators，后五行来自 refine_modules 的补正
ATTR_NAMES = {"maxHp": "生命上限", "atk": "攻击", "def": "防御",
              "magicResistance": "法术抗性", "cost": "部署费用",
              "blockCnt": "阻挡数", "respawnTime": "再部署时间",
              "baseAttackTime": "攻击间隔"}
MODULE_ATTR_MAP = {"max_hp": "生命上限", "atk": "攻击", "def": "防御",
                   "magic_resistance": "法术抗性", "attack_speed": "攻击速度",
                   "block_cnt": "阻挡数", "respawn_time": "再部署时间", "cost": "部署费用"}

# 重名/重号修正：源里同名同字的形态靠这张表区分（人工确认过的，半角括号）
PATCH_RENAME = {
    "char_1001_amiya2": "阿米娅(近卫)",
    "char_1037_amiya3": "阿米娅(医疗)",
}
# 人工源里的类型记号
TYPE_REMAP = {"D": "Δ"}


def dump(value):
    """列表/字典 → JSON 文本；空值与空表一律 None（与旧构建脚本的口径一致）。"""
    if value is None:
        return None
    if isinstance(value, (list, dict)) and not value:
        return None
    return json.dumps(value, ensure_ascii=False)


# ─── 文本处理 ────────────────────────────────────────────────

_RE_PAT = re.compile(r'\{(-?)([^}:]+)(?::([^}]+))?\}')


def replace_enhanced(text, blackboard, duration=None):
    """取反 + 大小写不敏感 + {duration} 兜底（语义见 maintenance/blackboard.py）。"""
    return bb_replace(text, blackboard, duration)


def merge_phases(desc_raw, items):
    """多候选（同一 trait 的不同精英化阶段）合并成紧凑格式。

    变化处用 `/` 连接，末尾标 `（精英化0/1/2）`。文本长度不一致时返回 None。
    与 db_builder/refine_operators.py 的 merge_phases 同一算法。
    """
    texts = [replace_enhanced(desc_raw, bb)[0] for _, bb in items]
    if len(set(texts)) == 1:
        return texts[0]
    n = len(texts[0])
    if any(len(t) != n for t in texts):
        return None
    diff = {i for i in range(n) if len({t[i] for t in texts}) > 1}
    if not diff:
        return texts[0]
    spans, start, prev = [], None, None
    for i in range(n):
        if i in diff:
            if start is None:
                start = i
            prev = i
        elif start is not None:
            spans.append((start, prev))
            start = None
    if start is not None:
        spans.append((start, prev))
    num_chars = set('0123456789.%')
    spans2 = []
    for s, e in spans:
        while s - 1 >= 0 and texts[0][s - 1] in num_chars | {'-'}:
            s -= 1
        while e + 1 < n and texts[0][e + 1] in num_chars:
            e += 1
        spans2.append((s, e))
    merged = []
    for s, e in spans2:
        if merged and s <= merged[-1][1] + 1:
            merged[-1] = (merged[-1][0], max(merged[-1][1], e))
        else:
            merged.append((s, e))
    spans = merged
    out = texts[0][:spans[0][0]]
    for si, (s, e) in enumerate(spans):
        out += '/'.join(t[s:e + 1] for t in texts)
        nxt = spans[si + 1][0] if si + 1 < len(spans) else n
        out += texts[0][e + 1:nxt]
    out += '（精英化%s）' % '/'.join(str(p) for p, _ in items)
    return out


# ─── 各列的构建 ──────────────────────────────────────────────

def build_favor(cv):
    favor = cv.get("favorKeyFrames") or []
    if not favor:
        return None
    data = favor[-1].get("data", {})
    frame = {"生命上限": data.get("maxHp", 0), "攻击": data.get("atk", 0),
             "防御": data.get("def", 0), "法术抗性": data.get("magicResistance", 0)}
    if not any(v != 0 for v in frame.values()):
        return None
    return dump(frame)


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
    return dump(phases)


def build_desc(cv, notes):
    """干员描述：占位符按 trait 候选解析；多候选按精英化阶段合并。"""
    raw = cv.get("description") or ""
    items = []
    for cand in ((cv.get("trait") or {}).get("candidates") or []):
        phase = (cand.get("unlockCondition") or {}).get("phase", "PHASE_0")
        bb = {b["key"]: b["value"] for b in (cand.get("blackboard") or [])}
        items.append((PHASE_MAP.get(phase, 0), bb))
    items.sort(key=lambda x: x[0])

    text = None
    if not items:
        # 306/463 的干员在源里没有 trait 候选（描述本来就是纯文本），是正常路径，不记提示
        text = clean_desc(raw)
    elif len(items) > 1:
        text = merge_phases(raw, items)
        if text is None:
            notes.append("描述：多候选长度不一致，退回游戏文本清洗")
            text = clean_desc(raw)
    else:
        text = replace_enhanced(raw, items[0][1])[0]
    return text or None


def build_skills(skills_ref, skill_table, notes, who):
    out = []
    for sr in skills_ref:
        sid = sr.get("skillId", "")
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
            text, _ = replace_enhanced(lv.get("description") or "", bb, lv.get("duration"))
            if text and re.search(r'\{[^{}]*\}|</>', text):
                notes.append("%s / %s Lv%d：描述有未解析残留" % (who, lv0.get("name", ""), i + 1))
            sp = {}
            for raw_key, cn_key in SP_KEY_MAP.items():
                val = (lv.get("spData") or {}).get(raw_key)
                if val is not None:
                    sp[cn_key] = val
            details.append({"等级": i + 1, "范围ID": lv.get("rangeId"),
                            "描述": text, "持续时间": lv.get("duration"), "SP": sp})

        out.append({
            "技能ID": sid,
            "技能名": lv0.get("name", ""),
            "触发类型": SKILL_TYPE_MAP.get(lv0.get("skillType", ""), lv0.get("skillType", "")),
            "回复类型": "被动" if isinstance(sp_type, int) else SP_TYPE_MAP.get(sp_type, str(sp_type)),
            "持续类型": DUR_TYPE_MAP.get(lv0.get("durationType", ""), lv0.get("durationType", "")),
            "备注": None,
            "各级详情": details,
        })
    return out


def build_talents(talents):
    groups = {}
    for talent in talents or []:
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


def build_modules(equip_ids, ue, be, talent_names, trait_lookup, notes, who):
    """模组列表。特性文本优先取人工源，取不到时退回源数据里的 override 文本。"""
    if not equip_ids:
        return []
    out = []
    for equip_id in equip_ids:
        ed = (ue.get("equipDict") or {}).get(equip_id)
        if not ed or ed.get("type") != "ADVANCED":
            continue
        bed = be.get(equip_id) or {}
        phases = []
        for phase_idx, bp in enumerate(bed.get("phases") or []):
            lv = bp.get("equipLevel")

            stat_updates = []
            for b in bp.get("attributeBlackboard") or []:
                name = MODULE_ATTR_MAP.get(b.get("key", ""), b.get("key", ""))
                val = b.get("value", 0)
                stat_updates.append({"属性": name,
                                     "增加值": int(val) if val == int(val) else val})

            trait_parts, talent_parts = [], []
            for part in bp.get("parts") or []:
                if part.get("overrideTraitDataBundle"):
                    for item in (part["overrideTraitDataBundle"].get("candidates") or []):
                        item_bb = {b["key"]: b["value"] for b in (item.get("blackboard") or [])}
                        add = item.get("additionalDescription")
                        ovr = item.get("overrideDescripton")
                        if add:
                            trait_parts.append("特性追加：" + replace_enhanced(add, item_bb)[0])
                        if ovr:
                            trait_parts.append("特性覆盖：" + replace_enhanced(ovr, item_bb)[0])
                for bundle_key in ("addOrOverrideTalentDataBundle", "overrideTalentDataBundle"):
                    bundle = part.get(bundle_key)
                    if not bundle:
                        continue
                    cands = bundle.get("candidates") or []
                    if cands and cands[0].get("upgradeDescription"):
                        item = cands[0]
                        tname = item.get("name", "")
                        txt = clean_desc(item["upgradeDescription"])
                        talent_parts.append("%s：%s" % (tname, txt) if tname else txt)

            trait_text = "\n".join(trait_parts) or None
            talent_text = "\n".join(talent_parts) or None
            effects = {"数值更新": stat_updates, "特性": trait_text if phase_idx == 0 else None}

            # 天赋更新文本 / 新增天赋：提到的天赋名不在本干员天赋表里 → 判为新增天赋
            if talent_text and lv in (2, 3):
                sep = "：" if "：" in talent_text else (":" if ":" in talent_text else None)
                mentioned = talent_text.split(sep)[0].strip() if sep else ""
                key = "天赋更新文本" if (mentioned and mentioned in talent_names) else "新增天赋"
                effects[key] = talent_text
            else:
                effects["天赋更新文本"] = None
            phases.append(effects)

        # 人工源覆盖：按模组名（特限证章）或「分支名1-分支名2」查
        mod_name = ed.get("uniEquipName", "")
        branch1, type2 = ed.get("typeName1", ""), ed.get("typeName2", "")
        trait = trait_lookup.get(mod_name)
        if not trait and branch1 and type2:
            trait = trait_lookup.get("%s-%s" % (branch1, TYPE_REMAP.get(type2, type2)))
        if trait:
            if phases:
                phases[0]["特性"] = trait
        elif mod_name:
            notes.append("%s / 模组「%s」(%s-%s) 未在人工源中找到特性文本"
                         % (who, mod_name, branch1, type2))

        out.append({"模组ID": equip_id, "名称": mod_name, "分支名1": branch1,
                    "分支名2": type2, "各级效果": phases})
    return out


# ─── 人工源 ──────────────────────────────────────────────────

def load_module_traits(path):
    """解析《模组特性信息.md》。契约：`### 标题` 就是查表键，其下所有正文行（含 ※）为文本。"""
    lookup = {}
    if not os.path.isfile(path):
        return lookup
    key, buf = None, []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip()
            m = re.match(r'^### (.+)', line)
            if m:
                if key and buf:
                    lookup[key] = "\n".join(buf)
                key, buf = m.group(1).strip(), []
                continue
            if not line or line.startswith("#"):
                continue
            if key:
                buf.append(line)
    if key and buf:
        lookup[key] = "\n".join(buf)
    return lookup


# ─── 子键的剔除 / 搬回（备注类的比对与保留）───────────────────

def _parse_json(value):
    if value is None:
        return None
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return None


def strip_keep(col, value):
    """比对用：去掉只由爬取 / 人工源提供的子键。"""
    if col not in KEEP_SUBKEY or value is None:
        return value
    obj = _parse_json(value)
    if not isinstance(obj, list):
        return value
    obj = copy.deepcopy(obj)
    sub = KEEP_SUBKEY[col]
    for item in obj:
        if isinstance(item, dict):
            item.pop(sub, None)
    return json.dumps(obj, ensure_ascii=False)


def merge_keep(col, old_value, new_value):
    """写库用：把旧值里那些子键搬回新值的对应项。"""
    if col not in KEEP_SUBKEY:
        return new_value
    old_obj, new_obj = _parse_json(old_value), _parse_json(new_value)
    if not isinstance(old_obj, list) or not isinstance(new_obj, list):
        return new_value
    id_key = ITEM_ID[col]
    sub = KEEP_SUBKEY[col]
    old_by_id = {it.get(id_key): it for it in old_obj
                 if isinstance(it, dict) and it.get(id_key) is not None}
    for item in new_obj:
        if not isinstance(item, dict):
            continue
        old_item = old_by_id.get(item.get(id_key))
        if not isinstance(old_item, dict):
            continue
        if sub in old_item:
            item[sub] = old_item[sub]
        else:
            item.pop(sub, None)
    return json.dumps(new_obj, ensure_ascii=False)


# ─── 重算全表 ────────────────────────────────────────────────

def compute_rows(ct, cpt, skt, ue, be, trait_lookup, notes):
    ops = [(k, v) for k, v in ct.items() if v.get("profession") in OPS_PROFS]
    for cid, cv in (cpt.get("patchChars") or {}).items():
        if cv.get("profession") in OPS_PROFS:
            ops.append((cid, cv))

    subprof_map = {v.get("subProfessionId"): v.get("subProfessionName")
                   for v in (ue.get("subProfDict") or {}).values()}

    rows = {}
    for i, (cid, cv) in enumerate(ops, 1):
        if i % 100 == 0 or i == len(ops):
            print("  重算 %d/%d" % (i, len(ops)))
        who = PATCH_RENAME.get(cid) or cv.get("name", cid)
        talents = build_talents(cv.get("talents"))
        rows[cid] = {
            "干员ID": cid,
            "名称": PATCH_RENAME.get(cid, cv.get("name", "")),
            "代号": cv.get("appellation", ""),
            "描述": build_desc(cv, notes),
            "部署位置": POSITION_MAP.get(cv.get("position", ""), cv.get("position", "")),
            "标签": dump(cv.get("tagList")),
            "获取方式": cv.get("itemObtainApproach") or None,
            "是否不可获取": 1 if cv.get("isNotObtainable") else 0,
            "是否为异格": 1 if cv.get("isSpChar") else 0,
            "稀有度": {"TIER_%d" % n: n for n in range(1, 7)}.get(cv.get("rarity", ""), 0),
            "职业": PROF_MAP.get(cv.get("profession", ""), cv.get("profession", "")),
            "职业分支": subprof_map.get(cv.get("subProfessionId", ""), cv.get("subProfessionId", "")),
            "最大潜能": cv.get("maxPotentialLevel", 0),
            "备注": None,
            "信赖加成": build_favor(cv),
            "精英化阶段": build_phases(cv),
            "技能列表": dump(build_skills(cv.get("skills") or [], skt, notes, who)),
            "关联token": dump(list((cv.get("displayTokenDict") or {}).keys())),
            "天赋列表": dump(talents),
            "潜能提升": dump([pr.get("description", "") for pr in (cv.get("potentialRanks") or [])]),
            "模组列表": dump(build_modules((ue.get("charEquip") or {}).get(cid), ue, be,
                                           {t["天赋名"] for t in talents},
                                           trait_lookup, notes, who)),
        }
    return rows


# ─── PRTS 爬取 ───────────────────────────────────────────────

def crawl_operator_page(name, redirects=None):
    """抓一个干员页，取三类文本。返回 (备注列, {技能名: 备注}, [天赋备注...])。"""
    text = uc.fetch_wikitext(name, redirects=redirects)

    charinfo = None
    tpls = uc.parse_template(text, "CharinfoV2")
    if tpls:
        raw = tpls[0].get("特性备注")
        if raw:
            charinfo = clean_wiki(raw) or None

    skill_notes = {}
    for params in uc.parse_template(text, "技能2") + uc.parse_template(text, "技能\n"):
        sname = params.get("技能名", "")
        if not sname:
            continue
        raw = params.get("备注")
        note = (clean_wiki(raw) or None) if raw else None
        if sname not in skill_notes or (note and not skill_notes[sname]):
            skill_notes[sname] = note

    talent_notes = []
    for params in uc.parse_template(text, "天赋列表3"):
        raw = params.get("备注")
        talent_notes.append((clean_wiki(raw) or None) if raw else None)

    return charinfo, skill_notes, talent_notes


def fill_new_row(row, charinfo, skill_notes, talent_notes):
    """把爬到的三类备注写进新行的结构里。"""
    if charinfo:
        row["备注"] = charinfo

    skills = _parse_json(row["技能列表"]) or []
    for sk in skills:
        note = skill_notes.get(sk.get("技能名", ""))
        if note:
            sk["备注"] = note
    row["技能列表"] = dump(skills)

    talents = _parse_json(row["天赋列表"]) or []
    for i, item in enumerate(talents):
        if i < len(talent_notes) and talent_notes[i]:
            item["备注"] = talent_notes[i]
    row["天赋列表"] = dump(talents)


# ─── 主流程 ──────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description="operators 表更新（校对 + 新增 + 记录）")
    ap.add_argument("--dry-run", action="store_true",
                    help="只报告，不写库、不爬取、不写记录、不备份")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    paths.ensure_gamedata()
    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== operators 表更新 ======")
    print("数据库  : %s" % args.db)
    print("数据源  : %s" % paths.DATA_SOURCE)
    src_ver = uc.read_source_version(paths.DATA_SOURCE)
    print("源版本  : 提交 %s | 游戏版本 %s | 数据时间 %s"
          % (src_ver["提交"], src_ver["游戏版本"], src_ver["数据时间"]))

    trait_md = os.path.join(str(paths.MANUAL_SOURCES), MODULE_TRAITS_MD)
    trait_lookup = load_module_traits(trait_md)
    print("人工源  : %s（%d 条）" % (trait_md, len(trait_lookup)))
    if not trait_lookup:
        print("  · 提示：人工源为空或不存在，模组「特性」将退回源数据的 override 文本")

    record = {
        "表": TABLE,
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {
            "仓库": "Kengxxiao/ArknightsGameData",
            "提交": src_ver["提交"], "提交标题": src_ver["提交标题"],
            "游戏版本": src_ver["游戏版本"], "数据时间": src_ver["数据时间"],
        },
        "脚本": {"文件": "update_operators.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "人工源": {"文件": os.path.basename(trait_md), "条目": len(trait_lookup)},
        "校对": {"检查行": 0, "差异行": 0, "差异列数": 0, "计数": {}, "明细": []},
        "新增": {"干员": 0, "明细": []},
        "爬取": {"目标干员": 0, "成功": 0, "无页面": 0, "有特性备注": 0,
                 "有技能备注": 0, "有天赋备注": 0, "重定向": [], "无页面明细": []},
        "跳过": {
            "列": sorted(SKIP_COLS),
            "子键": {"技能列表": ["备注"], "天赋列表": ["备注"]},
            "说明": "描述与模组列表由 Air 手工调整；备注类为爬取 / 人工产物。"
                     "对已有干员一律不比对、不写入；新增行照常由源数据 / 爬取 / 人工源填充",
        },
        "构建提示": [],
        "结果": "未执行",
    }

    print("\n------ 读取源数据 ------")
    def load_json(name):
        path = paths.EXCEL / name
        with open(path, encoding="utf-8") as f:
            return json.load(f)

    ct = load_json("character_table.json")
    cpt = load_json("char_patch_table.json")
    skt = load_json("skill_table.json")
    ue = load_json("uniequip_table.json")
    be = load_json("battle_equip_table.json")
    print("  干员表 %d 条 | 补丁 %d 条 | 技能 %d 条 | 模组目录 %d 条"
          % (len(ct), len(cpt.get("patchChars") or {}), len(skt), len(ue.get("equipDict") or {})))

    notes = []
    print("\n------ 重算目标行 ------")
    rows = compute_rows(ct, cpt, skt, ue, be, trait_lookup, notes)
    print("  目标行数: %d" % len(rows))
    if notes:
        record["构建提示"] = notes[:MAX_DETAIL]
        print("  构建提示 %d 条（已记入更新记录）" % len(notes))

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        existing = {}
        for r in conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), TABLE)):
            d = dict(zip(db_cols, r))
            existing[d["干员ID"]] = d

        # ── 1. 校对 ──
        print("\n------ 1. 校对 ------")
        diffs, per_col = [], {}
        for n, (cid, want) in enumerate(rows.items(), 1):
            if n % 100 == 0 or n == len(rows):
                print("  比对 %d/%d" % (n, len(rows)))
            have = existing.get(cid)
            if have is None:
                continue
            for col in ALL_COLS:
                if col in SKIP_COLS:
                    continue
                if not uc.same(col, strip_keep(col, have[col]), strip_keep(col, want[col]),
                               json_cols=JSON_COLS, int_cols=INT_COLS):
                    write_value = merge_keep(col, have[col], want[col])
                    diffs.append((cid, col, have[col], write_value))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"]["检查行"] = len(existing)
        record["校对"]["差异行"] = len({d[0] for d in diffs})
        record["校对"]["差异列数"] = len(diffs)
        record["校对"]["计数"] = per_col
        record["校对"]["明细"] = [
            dict({"干员ID": cid, "名称": rows[cid]["名称"], "列": col},
                 **uc.diff_brief(old, new, as_json=col in JSON_COLS))
            for cid, col, old, new in diffs[:MAX_DETAIL]
        ]
        print("  库内 %d 行 | 差异行 %d | 差异列 %d"
              % (len(existing), record["校对"]["差异行"], len(diffs)))
        for col, cnt in sorted(per_col.items(), key=lambda x: -x[1]):
            print("    %s: %d 处" % (col, cnt))
        for cid, col, old, new in diffs[:10]:
            print("    · %s %s" % (rows[cid]["名称"], col))

        # ── 2. 新增 ──
        new_ids = sorted(k for k in rows if k not in existing)
        record["新增"]["干员"] = len(new_ids)
        record["新增"]["明细"] = [
            {"干员ID": cid, "名称": rows[cid]["名称"]} for cid in new_ids[:MAX_DETAIL]
        ]
        print("\n------ 2. 新增 ------")
        print("  新增干员: %d" % len(new_ids))
        for cid in new_ids[:20]:
            print("    %s  %s" % (cid, rows[cid]["名称"]))
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
            print("\n------ 3. 爬取 PRTS（%d 名新增干员）------" % len(new_ids))
            for probe, marker in (SENTINEL_SKILL, SENTINEL_TALENT):
                ok, msg = uc.sentinel_ok(probe, marker)
                if not ok:
                    record["结果"] = "中止（结构哨兵失败：%s）" % msg
                    path = uc.write_record(TABLE, record)
                    print("\n!! 中止：%s" % msg)
                    print("   未对数据库做任何修改；记录：%s" % path)
                    return 1

            no_page, redirects = [], []
            t0 = time.time()
            for i, cid in enumerate(new_ids, 1):
                name = rows[cid]["名称"]
                if i % 10 == 0 or i == len(new_ids):
                    print("  [%d/%d] %.0fs | %s" % (i, len(new_ids), time.time() - t0, name))
                try:
                    crawled[cid] = crawl_operator_page(name, redirects=redirects)
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

            record["爬取"]["目标干员"] = len(new_ids)
            record["爬取"]["成功"] = len(crawled)
            record["爬取"]["无页面"] = len(no_page)
            record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
            record["爬取"]["有特性备注"] = sum(1 for v in crawled.values() if v[0])
            record["爬取"]["有技能备注"] = sum(1 for v in crawled.values()
                                              if any(v[1].values()))
            record["爬取"]["有天赋备注"] = sum(1 for v in crawled.values()
                                              if any(v[2]))
            record["爬取"]["重定向"] = ["%s → %s" % ab for ab in redirects[:MAX_DETAIL]]
            for cid, (charinfo, skill_notes, talent_notes) in crawled.items():
                fill_new_row(rows[cid], charinfo, skill_notes, talent_notes)
            print("  成功 %d | 无页面 %d | 有特性备注 %d | 有技能备注 %d | 有天赋备注 %d"
                  % (len(crawled), len(no_page), record["爬取"]["有特性备注"],
                     record["爬取"]["有技能备注"], record["爬取"]["有天赋备注"]))

        # ── 4. 写库（单事务）──
        print("\n------ 4. 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = uc.backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        print("  写回校对差异: %d 列 | 插入新增: %d 行" % (len(diffs), len(new_ids)))
        conn.execute("PRAGMA journal_mode=%s" % JOURNAL_MODE)
        conn.execute("BEGIN IMMEDIATE")
        try:
            for cid, col, _old, new in diffs:
                conn.execute("UPDATE %s SET %s=? WHERE 干员ID=?" % (TABLE, col), (new, cid))
            for cid in new_ids:
                conn.execute(
                    "INSERT OR IGNORE INTO %s (%s) VALUES (%s)"
                    % (TABLE, ", ".join(ALL_COLS), ", ".join("?" * len(ALL_COLS))),
                    tuple(rows[cid][c] for c in ALL_COLS),
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

    record["结果"] = "成功"
    path = uc.write_record(TABLE, record)

    elapsed = (datetime.now() - started).total_seconds()
    print("\n====== 完成（%.1fs）======" % elapsed)
    print("  校对写回: %d 列 | 新增: %d 名 | 爬取成功: %d | 无页面: %d"
          % (record["校对"]["差异列数"], record["新增"]["干员"],
             record["爬取"]["成功"], record["爬取"]["无页面"]))
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
