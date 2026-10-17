# -*- coding: utf-8 -*-
"""operators · 更新（增量）

数据源（本地）
    excel/character_table.json      干员主体（profession ∈ 八职业）
    excel/char_patch_table.json     升变 / 异格（阿米娅等），并入干员集合
    excel/skill_table.json          技能名 / 类型 / 各级描述与 SP
    excel/uniequip_table.json       职业分支名、模组目录
    excel/battle_equip_table.json   模组各级效果
    sources/模组特性信息.md          模组「特性」文本（人工源，只读；`###` 标题即查表键）

PRTS 侧（新增首次爬取；recheck 时按页重爬）
    {{CharinfoV2}}  · 特性备注 → 备注
    {{技能2}}       · 备注     → 技能列表[].备注（按技能名匹配）
    {{天赋列表3}}   · 备注     → 天赋列表[].备注（按出现顺序）

列分类
    数据源列（18）见 COLUMNS.source
    爬取列（1）   备注
    人工列（2）   描述、模组列表（整列手工调整过，永不比对、永不写入）
    子键          技能列表.备注、天赋列表.备注（比对时剔除，写库时按身份键搬回）

完整性判据
    三处备注（备注列 / 技能备注 / 天赋备注）全空 → partial（待填）
    其余                                        → synced

recheck 三路分流（on_diff）
    技能列表 / 天赋列表  Wiki 为准：把爬到的备注合进库内 JSON，其余字段保留
    备注              库内内容包含爬取结果 → 不动；不包含 → 不替换，进「待人工核对」

依赖
    无跨表依赖；模组特性依赖人工源 sources/模组特性信息.md（文件缺失则脚本不运行）

用法
    python update.py --dry-run    # 本地预分析：不联网、不写任何文件
    python update.py --base
    python update.py --crawl
    python update.py --recheck
"""

import copy
import json
import re
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import table_base as tb  # noqa: E402
import update_common as uc  # noqa: E402
from blackboard import replace as bb_replace  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402

MODULE_TRAITS_MD = "模组特性信息.md"
SENTINEL_SKILL = ("玫兰莎", "{{技能2")
SENTINEL_TALENT = ("香草", "{{天赋列表3")
SLEEP = uc.SLEEP_BETWEEN_FETCH

# ─── 列声明 ────────────────────────────────────────────────────

COLUMNS = tb.Columns(
    all=[
        "干员ID", "名称", "代号", "描述", "部署位置", "标签", "获取方式", "是否不可获取",
        "是否为异格", "稀有度", "职业", "职业分支", "最大潜能", "备注", "信赖加成",
        "精英化阶段", "技能列表", "关联token", "天赋列表", "潜能提升", "模组列表",
    ],
    source=[
        "干员ID", "名称", "代号", "部署位置", "标签", "获取方式", "是否不可获取",
        "是否为异格", "稀有度", "职业", "职业分支", "最大潜能", "信赖加成",
        "精英化阶段", "技能列表", "关联token", "天赋列表", "潜能提升",
    ],
    crawl=["备注"],
    manual=["描述", "模组列表"],
    subkeys=[("技能列表", "备注"), ("天赋列表", "备注")],
    json=["标签", "信赖加成", "精英化阶段", "技能列表", "关联token", "天赋列表",
          "潜能提升", "模组列表"],
    int=["是否不可获取", "是否为异格", "稀有度", "最大潜能"],
)

ITEM_ID = {"技能列表": "技能ID", "天赋列表": "序号"}

# ─── 枚举映射 ────────────────────────────────────────────────

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
ATTR_NAMES = {"maxHp": "生命上限", "atk": "攻击", "def": "防御",
              "magicResistance": "法术抗性", "cost": "部署费用",
              "blockCnt": "阻挡数", "respawnTime": "再部署时间",
              "baseAttackTime": "攻击间隔"}
MODULE_ATTR_MAP = {"max_hp": "生命上限", "atk": "攻击", "def": "防御",
                   "magic_resistance": "法术抗性", "attack_speed": "攻击速度",
                   "block_cnt": "阻挡数", "respawn_time": "再部署时间", "cost": "部署费用"}
PATCH_RENAME = {
    "char_1001_amiya2": "阿米娅(近卫)",
    "char_1037_amiya3": "阿米娅(医疗)",
}
TYPE_REMAP = {"D": "Δ"}

# 本次运行的构建提示（进记录）
NOTES = []


def dump(value):
    """列表/字典 → JSON 文本；空值与空表一律 None。"""
    if value is None:
        return None
    if isinstance(value, (list, dict)) and not value:
        return None
    return json.dumps(value, ensure_ascii=False)


def replace_enhanced(text, blackboard, duration=None):
    """取反 + 大小写不敏感 + {duration} 兜底（语义见 maintenance/blackboard.py）。"""
    return bb_replace(text, blackboard, duration)


# ─── 各列构建 ────────────────────────────────────────────────

def merge_phases(desc_raw, items):
    """多候选（同一 trait 的不同精英化阶段）合并成紧凑格式（既有算法）。"""
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

    if not items:
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
            sp = {cn: (lv.get("spData") or {})[raw]
                  for raw, cn in SP_KEY_MAP.items() if (lv.get("spData") or {}).get(raw) is not None}
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
    """模组列表。特性文本优先取人工源，取不到时退回源数据的 override 文本。"""
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

            # 提到的天赋名不在本干员天赋表里 → 判为新增天赋
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
    """解析《模组特性信息.md》。契约：`### 标题` 就是查表键，其下所有正文行为文本。"""
    lookup = {}
    if not path.is_file():
        return lookup
    key, buf = None, []
    for line in path.read_text(encoding="utf-8").splitlines():
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


# ─── 重算全表 ────────────────────────────────────────────────

def compute_rows(db=None):
    """全部干员 → {干员ID: 21 列}。人工源缺失时直接退出（环境问题，不降级）。"""
    def load_json(name):
        with open(paths.EXCEL / name, encoding="utf-8") as f:
            return json.load(f)

    trait_md = paths.MANUAL_SOURCES / MODULE_TRAITS_MD
    if not trait_md.is_file():
        raise SystemExit("人工源缺失，脚本不运行：%s\n"
                         "  · 模组「特性」文本来自这份人工维护的文件，缺失时无法保证数据正确" % trait_md)
    trait_lookup = load_module_traits(trait_md)
    print("  人工源: %s（%d 条）" % (trait_md, len(trait_lookup)))

    ct = load_json("character_table.json")
    cpt = load_json("char_patch_table.json")
    skt = load_json("skill_table.json")
    ue = load_json("uniequip_table.json")
    be = load_json("battle_equip_table.json")

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
            "描述": build_desc(cv, NOTES),
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
            "技能列表": dump(build_skills(cv.get("skills") or [], skt, NOTES, who)),
            "关联token": dump(list((cv.get("displayTokenDict") or {}).keys())),
            "天赋列表": dump(talents),
            "潜能提升": dump([pr.get("description", "") for pr in (cv.get("potentialRanks") or [])]),
            "模组列表": dump(build_modules((ue.get("charEquip") or {}).get(cid), ue, be,
                                           {t["天赋名"] for t in talents},
                                           trait_lookup, NOTES, who)),
        }
    if NOTES:
        print("  构建提示 %d 条（已记入更新记录）" % len(NOTES))
    return rows


# ─── 爬取 ────────────────────────────────────────────────────

def crawl(keys, rows):
    """抓干员页的三类备注。返回 {干员ID: Outcome}。

    values：
      备注      → 特性备注文本
      技能列表  → {技能名: 备注}（交给 on_diff 合并进 JSON）
      天赋列表  → [备注, ...]（按出现顺序）
    判据：三处全空 → partial（待填）；其余 → synced。
    """
    for probe, marker in (SENTINEL_SKILL, SENTINEL_TALENT):
        ok, msg = uc.sentinel_ok(probe, marker)
        if not ok:
            raise RuntimeError(msg)

    out = {}
    start = time.time()
    for i, key in enumerate(keys, 1):
        cid = key[0] if isinstance(key, tuple) else key
        name = (rows.get(key) or {}).get("名称") or cid
        if i % 10 == 0 or i == len(keys):
            print("  [%d/%d] %.0fs | %s" % (i, len(keys), time.time() - start, name))
        try:
            text = uc.fetch_wikitext(name)
        except uc.NoPage:
            out[key] = tb.Outcome("missing", page=name, note="页面不存在")
            time.sleep(SLEEP)
            continue

        charinfo = None
        tpls = uc.parse_template(text, "CharinfoV2")
        if tpls and tpls[0].get("特性备注"):
            charinfo = clean_wiki(tpls[0]["特性备注"]) or None

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

        if not charinfo and not any(skill_notes.values()) and not any(talent_notes):
            out[key] = tb.Outcome("partial", page=name,
                                  values={"备注": None, "技能列表": {}, "天赋列表": []},
                                  note="三处备注全空")
        else:
            out[key] = tb.Outcome("synced", page=name, values={
                "备注": charinfo, "技能列表": skill_notes, "天赋列表": talent_notes})
        time.sleep(SLEEP)
    return out


# ─── 备注的合并 / 仲裁 ───────────────────────────────────────

def _parse(value):
    if value is None:
        return None
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return None


def merge_skill_notes(have, notes):
    """把 {技能名: 备注} 合进库内 技能列表 JSON，其余字段保留。"""
    obj = _parse(have)
    if not isinstance(obj, list):
        return have
    obj = copy.deepcopy(obj)
    for item in obj:
        if not isinstance(item, dict):
            continue
        note = notes.get(item.get("技能名", ""))
        if note:
            item["备注"] = note
        elif "备注" in item:
            item.pop("备注")
    return json.dumps(obj, ensure_ascii=False)


def merge_talent_notes(have, notes):
    """按出现顺序把备注合进库内 天赋列表 JSON。"""
    obj = _parse(have)
    if not isinstance(obj, list):
        return have
    obj = copy.deepcopy(obj)
    for i, item in enumerate(obj):
        if not isinstance(item, dict):
            continue
        note = notes[i] if i < len(notes) else None
        if note:
            item["备注"] = note
        elif "备注" in item:
            item.pop("备注")
    return json.dumps(obj, ensure_ascii=False)


def on_diff(key, col, want, have):
    """写库前的仲裁。返回 None 表示只记录、不写库。"""
    if col == "备注":
        old = have or ""
        if not old.strip():
            return want if want is not None else old   # 新增 / 库内空 → 直接填（无内容则不动）
        if (want or "") and want in old:
            return old                  # 爬取内容仍在（后面可能追加了人工内容）
        return None                     # 页面变了 → 交人工核对
    if col == "技能列表":
        return merge_skill_notes(have, want or {})
    if col == "天赋列表":
        return merge_talent_notes(have, want or [])
    return want


SPEC = tb.TableSpec(
    table="operators",
    columns=COLUMNS,
    key_cols=["干员ID"],
    load_rows=compute_rows,
    pending_path=HERE / "pending.json",
    crawl=crawl,
    on_diff=on_diff,
)

if __name__ == "__main__":
    sys.exit(tb.run(SPEC))
