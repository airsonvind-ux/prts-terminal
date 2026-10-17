# -*- coding: utf-8 -*-
"""traps · 更新（增量）

数据源（本地）
    excel/character_table.json   profession == "TRAP"（846 条）
    excel/token_table.json       再补 4 条（其中 3 条 character_table 里没有）
    excel/skill_table.json       技能名 / 类型 / 各级描述与 SP

PRTS 侧（新增首次爬取；recheck 时按页重爬）
    {{装置信息}} ·阻挡半径 → 阻挡半径（非数值记构建提示）
                   ·装置机制 → 装置机制
    {{装置技能}} ·备注     → 技能列表[].备注（按技能名匹配）
    天赋备注不补：装置的天赋写在「装置信息」里，页面上没有这个位置

列分类
    数据源列（14）见 COLUMNS.source
    爬取列（2）   阻挡半径、装置机制
    人工列        —（本表没有人工列）
    子键          技能列表.备注（爬取）、天赋列表.备注（不补，保持 null）

完整性判据
    三处（阻挡半径 / 装置机制 / 技能备注）全空 → partial（待填）
    其余                                       → synced

recheck
    三处都归页面、无人工仲裁 → 页面赢，直接写；天赋备注永不动。

页面定位（三条候选，命中即用）
    ① sources/traps_pages.md（人工源）
    ② 去空白 / 换行后的名称
    ③ 名称 + (装置)
    命中干员页 / 敌人页 / 消歧义页 → 换下一个候选并记录。

用法
    python update.py --dry-run
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

SENTINELS = (("障碍物", "{{装置信息"), ("液压式高台", "{{装置技能"))
SLEEP = uc.SLEEP_BETWEEN_FETCH
DEVICE_SUFFIX = "(装置)"
PAGE_MAP_FILE = paths.MANUAL_SOURCES / "traps_pages.md"
_RE_COMMENT = re.compile(r'<!--[\s\S]*?-->')

# ─── 列声明 ────────────────────────────────────────────────────

COLUMNS = tb.Columns(
    all=[
        "装置ID", "名称", "代号", "阻挡半径", "再部署时间", "部署费用", "嘲讽等级", "阻挡数",
        "技力回复速度", "攻击间隔", "元素抗性", "损伤抗性", "装置机制", "精英化阶段",
        "技能列表", "天赋列表",
    ],
    source=[
        "装置ID", "名称", "代号", "再部署时间", "部署费用", "嘲讽等级", "阻挡数",
        "技力回复速度", "攻击间隔", "元素抗性", "损伤抗性", "精英化阶段",
        "技能列表", "天赋列表",
    ],
    crawl=["阻挡半径", "装置机制"],
    subkeys=[("技能列表", "备注"), ("天赋列表", "备注")],
    json=["精英化阶段", "技能列表", "天赋列表"],
    int=["再部署时间", "部署费用", "嘲讽等级", "阻挡数"],
    float=["技力回复速度", "攻击间隔", "元素抗性", "损伤抗性"],
)

# ─── 枚举映射 ────────────────────────────────────────────────

SKILL_TYPE_MAP = {"MANUAL": "手动触发", "AUTO": "自动触发", "PASSIVE": "被动"}
SP_TYPE_MAP = {"INCREASE_WITH_TIME": "自动回复", "INCREASE_WHEN_ATTACK": "攻击回复",
               "INCREASE_WHEN_TAKEN_DAMAGE": "受击回复"}
DUR_TYPE_MAP = {"NONE": "普通持续", "AMMO": "弹药类"}
PHASE_MAP = {"PHASE_0": 0, "PHASE_1": 1, "PHASE_2": 2}
SP_KEY_MAP = {"spCost": "技力消耗", "initSp": "初始技力",
              "maxChargeTime": "可充能次数", "increment": "技力回复速度"}
ATTR_NAMES = {"maxHp": "生命上限", "atk": "攻击", "def": "防御",
              "magicResistance": "法术抗性"}

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


def clean_name(name):
    """源里 6 个装置名带尾随空格或换行 → 统一去掉。"""
    return (name or "").strip().replace("\n", "").replace("\r", "")


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


def compute_rows(db=None):
    """全部装置 → {装置ID: 16 列}。"""
    with open(paths.EXCEL / "character_table.json", encoding="utf-8") as f:
        ct = json.load(f)
    tpath = paths.EXCEL / "token_table.json"
    tt = {}
    if tpath.exists():
        with open(tpath, encoding="utf-8") as f:
            tt = json.load(f)
    with open(paths.EXCEL / "skill_table.json", encoding="utf-8") as f:
        skill_table = json.load(f)

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
            "阻挡半径": None,
            "再部署时间": data.get("respawnTime", 0),
            "部署费用": data.get("cost", 0),
            "嘲讽等级": data.get("tauntLevel", 0),
            "阻挡数": data.get("blockCnt", 0),
            "技力回复速度": data.get("spRecoveryPerSec", 0),
            "攻击间隔": data.get("baseAttackTime", 0),
            "元素抗性": data.get("epDamageResistance", 0),
            "损伤抗性": data.get("epResistance", 0),
            "装置机制": None,
            "精英化阶段": dump(build_phases(cv)),
            "技能列表": dump(build_skills(cv, skill_table, NOTES, name)),
            "天赋列表": dump(build_talents(cv)),
        }
    if NOTES:
        print("  构建提示 %d 条（已记入更新记录）" % len(NOTES))
    return rows


# ─── 人工源：特殊页面映射 ────────────────────────────────────

def load_page_map():
    """读 sources/traps_pages.md → {装置ID: 页面名}。"""
    out = {}
    if not PAGE_MAP_FILE.exists():
        raise RuntimeError("人工源缺失：%s" % PAGE_MAP_FILE)
    for line in PAGE_MAP_FILE.read_text(encoding="utf-8").splitlines():
        if not line.startswith("| `trap_"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) >= 2:
            out[cells[0].strip("`")] = cells[1]
    return out


# ─── 爬取 ────────────────────────────────────────────────────

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
    """装置页 wikitext → (阻挡半径原值, 装置机制, {技能名: 备注})。"""
    radius_raw, mech = None, None
    tpls = uc.parse_template(text, "装置信息")
    if tpls:
        info = tpls[0]
        if info.get("阻挡半径"):
            radius_raw = info["阻挡半径"]
        if info.get("装置机制"):
            mech = clean_wiki(info["装置机制"]) or None

    skill_notes = {}
    for params in uc.parse_template(text, "装置技能"):
        sname = params.get("技能名", "")
        if not sname:
            continue
        raw = params.get("备注")
        val = (clean_wiki(raw) or None) if raw else None
        if sname not in skill_notes or (val and not skill_notes[sname]):
            skill_notes[sname] = val
    return radius_raw, mech, skill_notes


def crawl(keys, rows):
    """按候选顺序找装置页，取三样内容。返回 {装置ID: Outcome}。"""
    for probe, marker in SENTINELS:
        ok, msg = uc.sentinel_ok(probe, marker)
        if not ok:
            raise RuntimeError(msg)

    page_map = load_page_map()
    out = {}
    start = time.time()
    for i, key in enumerate(keys, 1):
        tid = key[0] if isinstance(key, tuple) else key
        name = (rows.get(key) or {}).get("名称") or tid
        tb.progress(i, len(keys), name, start)

        candidates = []
        if tid in page_map:
            candidates.append(page_map[tid])
        candidates += [name, name + DEVICE_SUFFIX]

        found = None
        for cand in candidates:
            try:
                text = uc.fetch_wikitext(cand)
            except uc.NoPage:
                continue
            kind = _is_foreign_page(_RE_COMMENT.sub("", text))
            if kind:
                NOTES.append("%s：%s 是%s，跳过" % (name, cand, kind))
                continue
            found = (cand, parse_trap_page(text))
            break
            # 命中即用
        time.sleep(SLEEP)

        if found is None:
            out[key] = tb.Outcome("missing", page=candidates[0] if candidates else name,
                                  note="三条候选都没命中装置页")
            continue

        cand, (radius_raw, mech, skill_notes) = found
        values = {"阻挡半径": None, "装置机制": mech, "技能列表": dump(skill_notes)}
        if radius_raw:
            try:
                values["阻挡半径"] = str(float(radius_raw))
            except ValueError:
                NOTES.append("%s：阻挡半径不是数值（%r），已跳过" % (name, radius_raw))

        if values["阻挡半径"] is None and not mech and not any(skill_notes.values()):
            out[key] = tb.Outcome("partial", page=cand,
                                  values={"阻挡半径": None, "装置机制": None, "技能列表": dump({})},
                                  note="三处全空")
        else:
            out[key] = tb.Outcome("synced", page=cand, values=values)
    return out


# ─── 备注合并 / 页面赢 ──────────────────────────────────────

def merge_notes(have, notes):
    """按技能名把页面备注并进库内技能列表。notes 可能是 dict 或 JSON 文本。"""
    obj = _parse(have)
    if not isinstance(obj, list):
        return have if have is not None else ""
    if isinstance(notes, str):
        notes = _parse(notes)
    if not isinstance(notes, dict):
        notes = {}
    obj = copy.deepcopy(obj)
    for item in obj:
        if not isinstance(item, dict):
            continue
        sname = item.get("技能名", "")
        if sname not in notes:
            continue
        note = notes[sname]
        if note:
            item["备注"] = note
        elif "备注" in item:
            item.pop("备注")
    return json.dumps(obj, ensure_ascii=False)


def on_diff(key, col, want, have):
    if col == "技能列表":
        return merge_notes(have, want or {})
    if col == "天赋列表":
        return have          # 天赋备注不补：页面上没有这个位置
    # 阻挡半径 / 装置机制：页面赢，页面没有就清空
    return want if want is not None else ""


SPEC = tb.TableSpec(
    table="traps",
    columns=COLUMNS,
    key_cols=["装置ID"],
    load_rows=compute_rows,
    pending_path=HERE / "pending.json",
    crawl=crawl,
    on_diff=on_diff,
)

if __name__ == "__main__":
    sys.exit(tb.run(SPEC))
