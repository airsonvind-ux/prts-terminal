# -*- coding: utf-8 -*-
"""tokens · 更新（增量）

数据源（本地）
    excel/character_table.json   profession == TOKEN 的条目（当前 75 条）
    excel/skill_table.json       技能名 / 类型 / 各级描述与 SP
    operators 表                 持有者（反查 关联token，该列存 tokenID 列表）

PRTS 侧（新增首次爬取；recheck 时按页重爬）
    {{召唤物信息}} ·备注                        → 备注
    {{技能}} / {{无等级技能}} / {{召唤物技能}} ·备注 → 技能列表[].备注（按技能名）
    {{召唤物天赋}} ·备注                        → 天赋列表[].备注（按天赋名）

列分类
    数据源列（15）见 COLUMNS.source
    爬取列（1）   备注
    人工列        —（本表没有人工列）
    子键          技能列表.备注、天赋列表.备注

完整性判据
    三处备注（备注列 / 技能备注 / 天赋备注）全空 → partial（待填）
    其余                                        → synced

recheck
    三处备注都归页面，不存在人工仲裁 → 页面赢，直接写。
    技能 / 天赋按技能名、天赋名合并进库内 JSON，其余字段保留。

口径
    持有者 = 反查 operators.关联token（纯脚本列）。反查不到的是不向玩家展示的召唤物，
    留空即正确答案。**顺序依赖：operators 必须先更新。**
    源里 profession == "TOKEN" 的一律进表（与「是否向玩家展示」无关）。

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

SENTINEL_INFO = ("医疗探机", "{{召唤物信息")
SENTINEL_SKILL = ("医疗探机", "{{无等级技能")
SENTINEL_TALENT = ("牙猎犬", "{{召唤物天赋")
SLEEP = uc.SLEEP_BETWEEN_FETCH

# 重名 token 的 PRTS 页面名（与干员页同名时使用）
PAGE_OVERRIDE = {
    "Mon3tr": "Mon3tr(凯尔希的召唤物)",
}

# ─── 列声明 ────────────────────────────────────────────────────

COLUMNS = tb.Columns(
    all=[
        "tokenID", "名称", "代号", "特性", "持有者", "部署位置", "部署费用", "阻挡数",
        "嘲讽等级", "再部署时间", "攻击速度", "攻击间隔", "精英化阶段", "技能列表",
        "天赋列表", "备注",
    ],
    source=[
        "tokenID", "名称", "代号", "特性", "持有者", "部署位置", "部署费用", "阻挡数",
        "嘲讽等级", "再部署时间", "攻击速度", "攻击间隔", "精英化阶段", "技能列表",
        "天赋列表",
    ],
    crawl=["备注"],
    subkeys=[("技能列表", "备注"), ("天赋列表", "备注")],
    json=["精英化阶段", "技能列表", "天赋列表"],
    int=["部署费用", "阻挡数", "嘲讽等级", "再部署时间"],
    float=["攻击速度", "攻击间隔"],
)

# ─── 枚举映射 ────────────────────────────────────────────────

POSITION_MAP = {"MELEE": "近战位", "RANGED": "远程位", "ALL": "全部位", "NONE": "无"}
SKILL_TYPE_MAP = {"MANUAL": "手动触发", "AUTO": "自动触发", "PASSIVE": "被动"}
SP_TYPE_MAP = {"INCREASE_WITH_TIME": "自动回复", "INCREASE_WHEN_ATTACK": "攻击回复",
               "INCREASE_WHEN_TAKEN_DAMAGE": "受击回复"}
DUR_TYPE_MAP = {"NONE": "普通持续", "AMMO": "弹药类"}
PHASE_MAP = {"PHASE_0": 0, "PHASE_1": 1, "PHASE_2": 2}
SP_KEY_MAP = {"spCost": "技力消耗", "initSp": "初始技力",
              "maxChargeTime": "可充能次数", "increment": "技力回复速度"}
ATTR_NAMES = {"maxHp": "生命上限", "atk": "攻击", "def": "防御",
              "magicResistance": "法术抗性"}

SKILL_TPL_NAMES = ("技能", "无等级技能", "召唤物技能")
TALENT_TPL = "召唤物天赋"
_RE_TALENT_NAME_KEY = re.compile(r"天赋\d+$")

NOTES = []


def dump(value):
    if value is None:
        return None
    if isinstance(value, (list, dict)) and not value:
        return None
    return json.dumps(value, ensure_ascii=False)


def _parse(value):
    if value is None:
        return None
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return None


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
    """召唤物的 skills 按精英化阶段占位（同一 skillId 重复出现），按 skillId 去重。"""
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


# ─── 持有者 ──────────────────────────────────────────────────

def load_owners(db, notes):
    """持有者 = 反查 operators.关联token。"""
    import sqlite3
    owners, dup = {}, []
    conn = sqlite3.connect(db)
    try:
        for name, link in conn.execute("SELECT 名称, 关联token FROM operators"):
            if not link:
                continue
            obj = _parse(link)
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
    finally:
        conn.close()
    if dup:
        notes.append("同一 token 被多名干员引用（取先出现者）：%s" % "；".join(dup[:20]))
    return owners


def compute_rows(db=None):
    """全部召唤物 → {tokenID: 16 列}。"""
    with open(paths.EXCEL / "character_table.json", encoding="utf-8") as f:
        ct = json.load(f)
    with open(paths.EXCEL / "skill_table.json", encoding="utf-8") as f:
        skill_table = json.load(f)

    owners = load_owners(db, NOTES)
    print("  持有者来源: operators.关联token → %d 个 token" % len(owners))

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
            "技能列表": dump(build_skills(cv, skill_table, NOTES, name)),
            "天赋列表": dump(build_talents(cv)),
            "备注": None,
        }

    dangling = sorted(set(owners) - set(rows))
    if dangling:
        NOTES.append("operators.关联token 引用了源里没有的 token：%s" % "、".join(dangling))
    if NOTES:
        print("  构建提示 %d 条（已记入更新记录）" % len(NOTES))
    return rows


# ─── 爬取 ────────────────────────────────────────────────────

def crawl(keys, rows):
    """抓召唤物页的三类备注。返回 {tokenID: Outcome}。"""
    for probe, marker in (SENTINEL_INFO, SENTINEL_SKILL, SENTINEL_TALENT):
        ok, msg = uc.sentinel_ok(probe, marker)
        if not ok:
            raise RuntimeError(msg)

    out = {}
    start = time.time()
    for i, key in enumerate(keys, 1):
        tid = key[0] if isinstance(key, tuple) else key
        name = (rows.get(key) or {}).get("名称") or tid
        tb.progress(i, len(keys), name, start)
        try:
            text = uc.fetch_wikitext(PAGE_OVERRIDE.get(name, name))
        except uc.NoPage:
            out[key] = tb.Outcome("missing", page=name, note="页面不存在")
            time.sleep(SLEEP)
            continue

        note = None
        tpls = uc.parse_template(text, "召唤物信息")
        if tpls and tpls[0].get("备注"):
            note = clean_wiki(tpls[0]["备注"]) or None

        skill_notes = {}
        for tpl_name in SKILL_TPL_NAMES:
            for params in uc.parse_template(text, tpl_name):
                sname = params.get("技能名", "")
                if not sname:
                    continue
                raw = params.get("备注")
                val = (clean_wiki(raw) or None) if raw else None
                if sname not in skill_notes or (val and not skill_notes[sname]):
                    skill_notes[sname] = val

        talent_notes = {}
        for params in uc.parse_template(text, TALENT_TPL):
            raw = params.get("备注")
            val = (clean_wiki(raw) or None) if raw else None
            for k, tname in params.items():
                if _RE_TALENT_NAME_KEY.search(k) and tname:
                    talent_notes[tname.strip()] = val

        if not note and not any(skill_notes.values()) and not any(talent_notes.values()):
            out[key] = tb.Outcome("partial", page=name,
                                  values={"备注": None, "技能列表": {}, "天赋列表": {}},
                                  note="三处备注全空")
        else:
            out[key] = tb.Outcome("synced", page=name, values={
                "备注": note, "技能列表": skill_notes, "天赋列表": talent_notes})
        time.sleep(SLEEP)
    return out


# ─── 备注合并（recheck 页面赢）────────────────────────────────

def merge_skill_notes(have, notes):
    obj = _parse(have)
    if not isinstance(obj, list):
        return have if have is not None else ""
    obj = copy.deepcopy(obj)
    for item in obj:
        if not isinstance(item, dict):
            continue
        sname = item.get("技能名", "")
        if sname not in notes:
            continue                    # 页面没提到这个技能 → 不动
        note = notes[sname]
        if note:
            item["备注"] = note
        elif "备注" in item:
            item.pop("备注")
    return json.dumps(obj, ensure_ascii=False)


def merge_talent_notes(have, notes):
    obj = _parse(have)
    if not isinstance(obj, list):
        return have if have is not None else ""
    obj = copy.deepcopy(obj)
    for item in obj:
        if not isinstance(item, dict):
            continue
        tname = (item.get("天赋名") or "").strip()
        if tname not in notes:
            continue                    # 页面没提到这个天赋 → 不动
        note = notes[tname]
        if note:
            item["备注"] = note
        elif item.get("备注"):
            item["备注"] = None
    return json.dumps(obj, ensure_ascii=False)


def on_diff(key, col, want, have):
    if col == "技能列表":
        return merge_skill_notes(have, want or {})
    if col == "天赋列表":
        return merge_talent_notes(have, want or {})
    # 备注列：页面赢；页面没有备注就清空（空串与 NULL 等价）
    return want if want is not None else ""


SPEC = tb.TableSpec(
    table="tokens",
    columns=COLUMNS,
    key_cols=["tokenID"],
    load_rows=compute_rows,
    pending_path=HERE / "pending.json",
    crawl=crawl,
    on_diff=on_diff,
)

if __name__ == "__main__":
    sys.exit(tb.run(SPEC))
