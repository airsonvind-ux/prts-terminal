"""
PRTS 终端 — stages 表更新脚本（维护用）

三段式，顺序固定：
  1. 校对：以 `关卡ID` 为键，把库内行与「由本地数据源重新算出的值」逐列比对，差异写回。
  2. 新增：库内不存在的键 INSERT（`stage_table.json` 全量 + 六个特殊来源）。
  3. 爬取：只对新增行补 `情报` / `待处理目标数量`（按 PRTS 页面分组，一页一次）。
  4. 记录：单事务，任何异常都不落库；结束写 updates/<时间戳>-stages.json。

校对列 = 除 `情报` / `待处理目标数量` 之外的全部列：
  - `情报` 来自 PRTS 爬取；
  - `待处理目标数量` 来自 PRTS，且允许人工校正（同名条目可能爬到同一页）→ 两者都不校对。

算不出就不改（Air 2026-10-13）：源里找不到该关卡的 level 文件时，`部署上限` / `初始费用` /
`费用上限` / `目标点耐久` / `敌人列表` 不参与比对。游戏资源本身存在 stage→level 映射缺失的
情况（如 act21side_01_m），不可能逐一校对，也不应改动原始信息。

敌人名口径（Air 2026-10-13）：`敌人列表` 里的名字用 `enemy_handbook_table.json`（玩家在
游戏内图鉴能读到的名字）；图鉴里没有的，退回 `enemies` 表的名字，再退回原始 ID。
每项形如 `{名称, 敌人ID, 级别, 覆写属性?}`——名称用于展示与自足查询，敌人ID 用于精确关联
（同名敌人有 66 组，按名字查会撞）。

特殊来源（不在 stage_table.json 里，均无 stageId 的危机合约不在此列）：
  - 集成战略   excel/roguelike_topic_table.json
  - 生息演算   excel/sandbox_table.json + excel/sandbox_perm_table.json
  - 全息作战矩阵 excel/crisis_v2_table.json 的 recalRuneData.seasons
  - 悖论模拟   excel/handbook_info_table.json
  - 训练场    excel/story_review_meta_table.json

用法：
    python update_stages.py --dry-run   # 只报告校对差异与新增，不写库、不爬取、不写记录
    python update_stages.py             # 正式执行
"""

import argparse
import json
import os
import re
import sqlite3
import sys
import time
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import paths  # noqa: E402
import update_common as uc  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402

TABLE = "stages"
SCRIPT_VERSION = 1
MAX_DETAIL = 200

# 不参与校对：PRTS 爬取列 + 人工校正列
SKIP_COLS = {"情报", "待处理目标数量"}
# 需要 level 文件才能算出来的列（源里缺文件时跳过比对）
COMBAT_COLS = {"部署上限", "初始费用", "费用上限", "目标点耐久", "敌人列表"}
JSON_COLS = {"解锁条件", "敌人列表"}
INT_COLS = {"理智消耗", "部署上限", "初始费用", "费用上限", "目标点耐久"}
FLOAT_COLS = set()          # 本表没有浮点列

ALL_COLS = [
    "关卡ID", "关卡名", "关卡描述", "关卡编号", "关卡类型", "解锁条件", "推荐等级",
    "理智消耗", "levelId", "部署上限", "初始费用", "费用上限", "目标点耐久",
    "敌人列表", "情报", "待处理目标数量",
]

STAGE_TYPE_MAP = {
    "MAIN": "主线", "ACTIVITY": "活动", "SUB": "支线", "DAILY": "日常",
    "CAMPAIGN": "剿灭", "GUIDE": "教学", "CLIMB_TOWER": "保全派驻",
    "SPECIAL_STORY": "特殊故事",
}

# 覆写属性：属性字段 → 中文（含 list 类字段，build_stages.py 曾漏掉后由 refine 补上）
ATTR_CN = {
    "name": "名称", "description": "描述", "prefabKey": "模型",
    "applyWay": "攻击方式", "motion": "行动方式", "enemyTags": "种类标签",
    "lifePointReduce": "目标价值", "levelType": "地位", "rangeRadius": "攻击半径",
    "numOfExtraDrops": "额外掉落数", "viewRadius": "视野半径",
    "notCountInTotal": "不计入总数",
    "maxHp": "最大生命值", "atk": "攻击力", "def": "防御力",
    "magicResistance": "法术抗性", "moveSpeed": "移动速度",
    "attackSpeed": "攻击速度", "baseAttackTime": "攻击间隔",
    "hpRecoveryPerSec": "生命自回速度", "spRecoveryPerSec": "SP自回速度",
    "massLevel": "重量", "tauntLevel": "基础嘲讽等级",
    "epDamageResistance": "元素抗性", "epResistance": "损伤抗性",
    "stunImmune": "晕眩抗性", "silenceImmune": "沉默抗性",
    "sleepImmune": "沉睡抗性", "frozenImmune": "冻结抗性",
    "levitateImmune": "浮空抗性", "disarmedCombatImmune": "缴械抗性",
    "fearedImmune": "恐惧抗性", "palsyImmune": "麻痹抗性",
    "attractImmune": "诱导抗性", "teleportImmune": "传送抗性",
    "groundBoundImmune": "缚地抗性",
    "talentBlackboard": "天赋覆写", "skills": "技能覆写",
}

# PRTS 页面名规则（沿 build_stages_prts_v2.py）
URL_RULES = {
    "生息演算·沙洲遗闻": "{code}_{name}(沙洲遗闻)",
    "悖论模拟": "悖论模拟_{name}",
}
DEFAULT_URL = "{code}_{name}"
SENTINEL_MARKER = "关卡信息"
SENTINEL_PAGE = "0-1_坍塌"   # 结构哨兵：实测存在且带关卡模板（1-7_坍塌 实测不存在）

# 爬取结构健康判定（与 enemies 同口径）
NO_DETAIL_RECHECK_EVERY = 5
NO_DETAIL_RATIO_LIMIT = 0.5
NO_DETAIL_MIN_TOTAL = 20

# 人工维护的关卡名：ID 含此串的行，`关卡名` 不参与校对（脚本推不出，改了就抹掉人工成果）。
# 岁·界园 DLC1：这些关卡是在“今昔境”区域对基础关卡做的差分，与基础关卡同名；
# Wiki 与 PRTS 都靠名字后的 (区域名) 区分，而 DLC→区域的映射源里没有，故整列豁免。
EXCLUDE_NAME_FROM_CHECK = "_dlc"

uc.reconfigure_stdout()

STAGE_TABLE = paths.GAMEDATA / "excel" / "stage_table.json"
ENEMY_HANDBOOK = paths.GAMEDATA / "excel" / "enemy_handbook_table.json"
ENEMY_DB = paths.GAMEDATA / "levels" / "enemydata" / "enemy_database.json"
LEVELS_ROOT = paths.GAMEDATA / "levels"


# ─── 数据源读取 ───────────────────────────────────────────────

def load_json(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def build_level_index():
    """levelId → 文件路径。按 obt → activities 顺序走，后者覆盖前者（与旧构建脚本一致）。"""
    index = {}
    for base in ("obt", "activities"):
        root = os.path.join(LEVELS_ROOT, base)
        if not os.path.isdir(root):
            continue
        for dirpath, _, fnames in os.walk(root):
            for fn in fnames:
                if fn.endswith(".json"):
                    index[fn[:-5]] = os.path.join(dirpath, fn)
    return index


class LevelCache:
    """按需加载 level JSON，只留下战斗列所需的字段并缓存（旧脚本一次性全读进内存）。"""

    def __init__(self, index, resolve_name, enemy_base=None):
        self.index = index
        self.resolve = resolve_name
        self.enemy_base = enemy_base
        self.cache = {}

    def combat(self, lid):
        """返回 (部署上限, 初始费用, 费用上限, 目标点耐久, 敌人列表)；文件缺失返回 None。"""
        if not lid:
            return None
        if lid not in self.cache:
            p = self.index.get(lid)
            if p is None:
                self.cache[lid] = None
            else:
                with open(p, encoding="utf-8") as f:
                    lv = json.load(f)
                opts = lv.get("options", {})
                self.cache[lid] = (
                    opts.get("characterLimit"),
                    opts.get("initialCost"),
                    opts.get("maxCost"),
                    opts.get("maxLifePoint"),
                    build_enemy_list(lv.get("enemyDbRefs"), self.resolve, self.enemy_base),
                )
        return self.cache[lid]


def clean_level_id(raw):
    if raw is None:
        return None
    return raw.split("/")[-1]


def clean_unlock_condition(raw):
    if not raw:
        return "[]"
    return json.dumps(
        [{"stageId": c.get("stageId", ""), "completeState": c.get("completeState", "")}
         for c in raw],
        ensure_ascii=False,
    )


def extract_overrides(od):
    """递归取 overwrittenData 中 m_defined=true 的叶节点，键名转中文；list 字段整段保留。"""
    result = {}
    if not isinstance(od, dict):
        return result
    if "m_defined" in od:
        if od["m_defined"]:
            return {"__leaf__": od.get("m_value")}
        return {}
    for key, val in od.items():
        if isinstance(val, dict):
            sub = extract_overrides(val)
            cn = ATTR_CN.get(key, key)
            if "__leaf__" in sub:
                result[cn] = sub["__leaf__"]
            else:
                for sk, sv in sub.items():
                    if sk != "__leaf__":
                        result[sk] = sv
        elif isinstance(val, list) and len(val) > 0:
            result[ATTR_CN.get(key, key)] = val
    return result


def build_enemy_list(refs, resolve_name, enemy_base=None):
    if not refs:
        return "[]"
    out = []
    for ref in refs:
        eid = ref.get("id")
        level = ref.get("level", 0)
        item = {"名称": resolve_name(eid), "敌人ID": eid, "级别": level}
        ov = extract_overrides(ref.get("overwrittenData"))
        if ov:
            if enemy_base is not None:
                ov = _differs(enemy_base.overrides(eid, level), ov)
            if ov:
                item["覆写属性"] = ov
        out.append(item)
    return json.dumps(out, ensure_ascii=False)


class EnemyBase:
    """基准敌人数据：用来判断某关的覆写是不是真的不同于常规值。

    游戏数据的 `overwrittenData` 会把整套字段都标成 m_defined=true，其中大部分与常规
    值完全相同（它标的是“显式给了值”，不是“值变了”），全记下来会让覆写属性形同虚设。
    所以只保留与基准不同的字段；取不到基准就原样保留。
    """

    def __init__(self, path):
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        self.by_key = {}
        for e in data.get("enemies", []):
            for v in e.get("Value", []):
                self.by_key[(e.get("Key"), v.get("level"))] = v.get("enemyData")

    def overrides(self, eid, level):
        """该敌人该级别的基准取值（同一套中文键）；取不到返回 None。"""
        ed = self.by_key.get((eid, level))
        return extract_overrides(ed) if ed is not None else None


def _differs(base, ov):
    """只留与基准不同的字段；base 为 None（取不到基准）时原样保留。"""
    if base is None:
        return ov
    return {k: v for k, v in ov.items()
            if k not in base or not uc.values_equal(base[k], v)}


# ─── 重算全部目标行 ───────────────────────────────────────────

def compute_rows(conn):
    """返回 (rows, no_level_keys)。

    rows: {关卡ID: {列名: 值}}
    no_level_keys: 源里找不到 level 文件的键（这些行的战斗列不参与校对）
    """
    handbook = load_json(ENEMY_HANDBOOK)
    hb_names = {}
    for eid, v in handbook.get("enemyData", {}).items():
        nm = clean_desc(v.get("name"))
        hb_names[eid] = (nm or "").strip() or None
    db_names = {eid: nm for eid, nm in conn.execute("SELECT 敌人ID, 名称 FROM enemies")}

    def resolve_name(eid):
        return hb_names.get(eid) or db_names.get(eid) or eid

    lc = LevelCache(build_level_index(), resolve_name, EnemyBase(ENEMY_DB))
    rows = {}
    no_level = set()

    def fill_combat(row, lid_main, lid_raw, kind):
        """按 level JSON 填战斗列；缺文件则置空并记入 no_level。"""
        row["部署上限"] = row["初始费用"] = row["费用上限"] = None
        row["目标点耐久"] = None
        row["敌人列表"] = None
        if not lid_raw:
            no_level.add(row["关卡ID"])
            return
        multi = "," in lid_raw
        if multi and "集成战略" not in kind:
            no_level.add(row["关卡ID"])
            return
        combat = lc.combat(lid_main)
        if combat is None:
            no_level.add(row["关卡ID"])
            return
        row["部署上限"], row["初始费用"], row["费用上限"], row["目标点耐久"], row["敌人列表"] = combat

    # ── A. stage_table.json ──
    st = load_json(STAGE_TABLE)["stages"]
    for sid, s in st.items():
        lid_raw = clean_level_id(s.get("levelId"))
        kind = STAGE_TYPE_MAP.get(s.get("stageType"), s.get("stageType"))
        row = {
            "关卡ID": sid,
            "关卡名": s.get("name"),
            "关卡描述": clean_desc(s.get("description")),
            "关卡编号": s.get("code"),
            "关卡类型": kind,
            "解锁条件": clean_unlock_condition(s.get("unlockCondition")),
            "推荐等级": s.get("dangerLevel"),
            "理智消耗": s.get("apCost"),
            "levelId": lid_raw,
            "情报": None,
            "待处理目标数量": None,
        }
        fill_combat(row, lid_raw, lid_raw, kind)
        rows[sid] = row

    # ── B. 特殊来源 ──
    def add_special(sid, name, desc, code, kind, lid_raw, lid_main=None):
        row = {
            "关卡ID": sid,
            "关卡名": name or None,
            "关卡描述": clean_desc(desc) if desc else None,
            "关卡编号": code or None,
            "关卡类型": kind,
            "解锁条件": None,
            "推荐等级": None,
            "理智消耗": None,
            "levelId": lid_raw or None,
            "情报": None,
            "待处理目标数量": None,
        }
        fill_combat(row, lid_main if lid_main is not None else lid_raw, lid_raw, kind)
        rows[sid] = row

    def load_optional(rel):
        p = paths.GAMEDATA / "excel" / rel
        return load_json(p) if os.path.exists(p) else None

    # B1. 集成战略
    rl = load_optional("roguelike_topic_table.json")
    if rl:
        topics = rl.get("topics", {})
        for topic_id, topic in topics.items():
            topic_name = topic.get("name", topic_id)
            for sid, s in rl.get("details", {}).get(topic_id, {}).get("stages", {}).items():
                main_lid = clean_level_id(s.get("levelId"))
                lids = [main_lid] + [clean_level_id(r) for r in (s.get("levelReplaceIds") or [])]
                add_special(sid, s.get("name", "").strip(), s.get("description", "").strip(),
                            (s.get("code") or "").strip(), "集成战略·%s" % topic_name,
                            ",".join([l for l in lids if l]), main_lid)

    # B2. 生息演算
    sb0 = load_optional("sandbox_table.json")
    if sb0:
        for _, s in sb0.get("sandboxActTables", {}).get("act1sandbox", {}).get("stageDatas", {}).items():
            add_special(s.get("stageId"), s.get("name"), s.get("description"), s.get("code"),
                        "生息演算·第0期", clean_level_id(s.get("levelId")))
    sbp = load_optional("sandbox_perm_table.json")
    if sbp:
        for key, kind_suffix in (("sandbox_1", None), ("sandbox_2", None)):
            info = sbp.get("basicInfo", {}).get(key, {})
            sub = sbp.get("detail", {}).get("SANDBOX_V2" if key == "sandbox_1" else "SANDBOX_V3", {}).get(key, {})
            kind = "生息演算·%s" % info.get("topicName", key)
            for sid, s in sub.get("stageData", {}).items():
                if key == "sandbox_1":
                    add_special(s.get("stageId"), s.get("name"), s.get("description"),
                                s.get("code"), kind, clean_level_id(s.get("levelId")))
                else:
                    story = sub.get("storyStageData", {}).get(sid, {})
                    sub_ids = [x for x in (story.get("subStageIdList") or []) if x]
                    lids = []
                    for si in sub_ids:
                        lid = clean_level_id(sub.get("subStageData", {}).get(si, {}).get("levelId"))
                        if lid:
                            lids.append(lid)
                    add_special(s.get("stageId"), s.get("name"), s.get("description"),
                                s.get("code"), kind, ",".join(lids), lids[0] if lids else None)

    # B3. 全息作战矩阵：名/编号/描述取自源里的 levelName / levelCode / levelDesc。
    # 编号额外加“全息”前缀——全息关是在常规关卡上做的高难版本、与常规关卡同名，
    # 加前缀以免 PRTS 按 `7-18` 之类的编号误认到全息关上（Air 定，仅本类型适用）。
    cv2 = load_optional("crisis_v2_table.json")
    if cv2:
        for _, season in cv2.get("recalRuneData", {}).get("seasons", {}).items():
            for _, s in season.get("stages", {}).items():
                add_special(s.get("stageId"), s.get("levelName"), s.get("levelDesc"),
                            "全息" + (s.get("levelCode") or ""),
                            "全息作战矩阵", clean_level_id(s.get("levelId")))

    # B4. 悖论模拟
    hbi = load_optional("handbook_info_table.json")
    if hbi:
        for _, s in hbi.get("handbookStageData", {}).items():
            add_special(s.get("stageId"), s.get("name"), s.get("description"), s.get("code"),
                        "悖论模拟", clean_level_id(s.get("levelId")))
    # B5. 训练场
    srm = load_optional("story_review_meta_table.json")
    if srm:
        for _, s in srm.get("trainingCampData", {}).get("stageData", {}).items():
            add_special(s.get("stageId"), s.get("name"), s.get("description"), s.get("code"),
                        "训练场", clean_level_id(s.get("levelId")))

    return rows, no_level


# ─── 比较 ────────────────────────────────────────────────────

def same(col, a, b):
    return uc.same(col, a, b, JSON_COLS, FLOAT_COLS, INT_COLS)


# ─── PRTS 爬取（情报 / 待处理目标数量）─────────────────────────

def build_url_key(sid, code, name, stype):
    eff = (code or "").strip()
    if sid.startswith("tough_"):
        eff = "磨难" + eff
    elif "#s" in sid:
        eff = "险地" + eff
    tpl = URL_RULES.get(stype, DEFAULT_URL)
    key = tpl.format(code=eff, name=(name or "").strip())
    # MediaWiki 页名不能含 `#`（钡点分隔符），PRTS 用全角 `＃` 代替
    return key.replace("#", "＃")


def _intel_from_template(params):
    """从模板参数里取情报（清洗交给 cleaning_wiki 基础规则）。"""
    raw = params.get("情报")
    return clean_wiki(raw) if raw else None


def _template_intel(text):
    """从关卡页 wikitext 里取出各难度身份的情报 + 敌人数量。

    返回 {"NORMAL": str|None, "FOUR_STAR": ..., "SIX_STAR": ..., "IS_EMERGENCY": ..., "ANNIHILATION": ...,
          "_count": str|None}
    """
    out = {}
    # 普通关卡信息（含四星/六星难度变体）
    for tpl in uc.parse_template(text, "普通关卡信息"):
        if tpl.get("全息矩阵化") == "true" or tpl.get("战斗关卡") == "false":
            continue
        mode = tpl.get("关卡难度", "")
        key = {"SIX_STAR": "SIX_STAR", "FOUR_STAR": "FOUR_STAR"}.get(mode, "NORMAL")
        out.setdefault(key, _intel_from_template(tpl))
    for tpl in uc.parse_template(text, "突袭关卡信息"):
        key = "IS_EMERGENCY" if tpl.get("子类型") == "紧急作战" else "FOUR_STAR"
        out.setdefault(key, _intel_from_template(tpl))
    for tpl in uc.parse_template(text, "磨难险地关卡信息"):
        out.setdefault("SIX_STAR", _intel_from_template(tpl))
    for tpl in uc.parse_template(text, "剿灭关卡信息"):
        out.setdefault("ANNIHILATION", _intel_from_template(tpl))

    # 敌人数量：按模板名顺序找第一个带该参数的模板
    for tpl_name in ("普通关卡信息", "突袭关卡信息", "剿灭关卡信息", "保全派驻关卡信息",
                     "集成战略关卡信息", "区块化关卡信息", "教学关卡信息", "活动关卡信息",
                     "主线关卡信息", "支线关卡信息"):
        tpls = uc.parse_template(text, tpl_name)
        if tpls and tpls[0].get("敌人数量"):
            out["_count"] = tpls[0]["敌人数量"]
            break
    else:
        out["_count"] = None
    return out


def match_intel(sid, table):
    """按 stage ID 的难度身份挑对应情报。"""
    if "IS_EMERGENCY" in table and "_e_" in sid:
        return table.get("IS_EMERGENCY")
    if sid.startswith("tough_") or "#s" in sid:
        return table.get("SIX_STAR")
    if "#f#" in sid:
        return table.get("FOUR_STAR")
    if table.get("ANNIHILATION") is not None:
        return table.get("ANNIHILATION")
    return table.get("NORMAL")


def crawl_new(conn, new_keys, rows):
    """对新增行爬 PRTS，返回 {关卡ID: (情报, 待处理目标数量)} 与统计。"""
    targets = [k for k in new_keys
               if rows[k]["关卡编号"] and rows[k]["关卡名"]]
    groups = {}
    for k in targets:
        r = rows[k]
        url = build_url_key(k, r["关卡编号"], r["关卡名"], r["关卡类型"])
        groups.setdefault(url, []).append(k)

    result = {}
    no_page, no_detail, redirects = [], [], []
    total = len(groups)
    print("  待爬页面: %d 个（覆盖 %d 个新增关卡）" % (total, len(targets)))

    ok, why = uc.sentinel_ok(SENTINEL_PAGE, SENTINEL_MARKER)
    if not ok:
        raise RuntimeError(why)

    skipped = 0
    start = time.time()
    for i, (url, keys) in enumerate(groups.items()):
        if i % 25 == 0 or i == total - 1:
            el = time.time() - start
            rate = (i / el) if el > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print("  [%d/%d] %.0fs | ETA %.0fs | %s" % (i, total, el, eta, url[:56]))
        try:
            text = uc.fetch_wikitext(url, redirects)
        except uc.NoPage:
            for k in keys:
                no_page.append(k)
            continue
        table = _template_intel(text)
        count = table.pop("_count", None)
        hit = False
        for k in keys:
            intel = match_intel(k, table)
            if intel is not None:
                result.setdefault(k, [None, None])[0] = intel
                hit = True
            if count:
                result.setdefault(k, [None, None])[1] = count
                hit = True
        if not hit:
            no_detail.extend(keys)
            skipped += 1
            if skipped % NO_DETAIL_RECHECK_EVERY == 0:
                ok2, why2 = uc.sentinel_ok(SENTINEL_PAGE, SENTINEL_MARKER)
                if not ok2:
                    raise RuntimeError("站点结构复核失败（已跳过 %d 页）: %s" % (skipped, why2))
            if skipped >= NO_DETAIL_MIN_TOTAL and skipped > (i + 1) * NO_DETAIL_RATIO_LIMIT:
                raise RuntimeError("无结构页面占比过高（%d/%d），疑似 PRTS 结构变化"
                                   % (skipped, i + 1))
        time.sleep(uc.SLEEP_BETWEEN_FETCH)
    return {k: tuple(v) for k, v in result.items()}, no_page, no_detail, redirects


# ─── 主流程 ──────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description="stages 表更新（校对 + 新增 + 记录）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不爬取、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    paths.ensure_gamedata()
    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== stages 表更新 ======")
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
        "脚本": {"文件": "update_stages.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "校对": {"检查行": 0, "差异行": 0, "差异列数": 0, "明细": [], "计数": {}},
        "新增": {"行": 0, "明细": []},
        "爬取": {"目标关卡": 0, "成功": 0, "无页面": 0, "无详情结构": 0,
                 "重定向": [], "无页面明细": [], "无详情明细": []},
        "结果": "未执行",
    }

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        print("\n------ 重算目标行 ------")
        rows, no_level = compute_rows(conn)
        print("  目标行数: %d | 源里缺 level 文件: %d" % (len(rows), len(no_level)))

        db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        cur = conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), TABLE))
        existing = {r[0]: dict(zip(db_cols, r)) for r in cur.fetchall()}

        # ── 1. 校对 ──
        diffs = []
        per_col = {}
        skipped_no_level = 0
        for k, want in rows.items():
            have = existing.get(k)
            if have is None:
                continue
            for col in db_cols:
                if col in SKIP_COLS:
                    continue
                if col in COMBAT_COLS and k in no_level:
                    skipped_no_level += 1
                    continue
                if col == "关卡名" and EXCLUDE_NAME_FROM_CHECK in k:
                    continue
                if not same(col, have[col], want[col]):
                    diffs.append((k, col, have[col], want[col]))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"]["检查行"] = len(existing)
        record["校对"]["差异行"] = len({d[0] for d in diffs})
        record["校对"]["差异列数"] = len(diffs)
        record["校对"]["计数"] = per_col
        record["校对"]["缺 level 跳过列"] = skipped_no_level
        record["校对"]["明细"] = [
            dict({"键": k, "列": col}, **uc.diff_brief(old, new, as_json=col in JSON_COLS))
            for k, col, old, new in diffs[:MAX_DETAIL]
        ]
        print("\n------ 1. 校对结果 ------")
        print("  库内行数: %d | 差异行: %d | 差异列: %d | 缺 level 跳过列: %d"
              % (len(existing), record["校对"]["差异行"], len(diffs), skipped_no_level))
        for col, n in sorted(per_col.items(), key=lambda x: -x[1]):
            print("    %s: %d 处" % (col, n))

        # ── 2. 新增 ──
        new_keys = sorted(k for k in rows if k not in existing)
        record["新增"]["行"] = len(new_keys)
        record["新增"]["明细"] = [{"键": k, "名": rows[k]["关卡名"], "类型": rows[k]["关卡类型"]}
                                  for k in new_keys[:MAX_DETAIL]]
        print("\n------ 2. 新增 ------")
        print("  新增关卡: %d" % len(new_keys))

        if args.dry_run:
            print("\n(干跑：未写库、未爬取、未写记录)")
            record["结果"] = "干跑"
            return 0

        # ── 3. 爬取（先爬后写）──
        crawl = {}
        if new_keys:
            print("\n------ 3. 爬取 PRTS ------")
            try:
                crawl, no_page, no_detail, redirects = crawl_new(conn, new_keys, rows)
            except Exception as e:
                record["结果"] = "中止（%s）" % e
                path = uc.write_record(TABLE, record)
                print("\n!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
            record["爬取"]["目标关卡"] = len([k for k in new_keys
                                              if rows[k]["关卡编号"] and rows[k]["关卡名"]])
            record["爬取"]["成功"] = len(crawl)
            record["爬取"]["无页面"] = len(no_page)
            record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
            record["爬取"]["无详情结构"] = len(no_detail)
            record["爬取"]["无详情明细"] = no_detail[:MAX_DETAIL]
            record["爬取"]["重定向"] = ["%s → %s" % (a, b) for a, b in redirects[:MAX_DETAIL]]

        # ── 4. 写库（单事务）──
        print("\n------ 4. 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = uc.backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        print("  写回校对差异: %d 列 | 插入新增: %d 行" % (len(diffs), len(new_keys)))
        conn.execute("BEGIN IMMEDIATE")
        try:
            for k, col, old, new in diffs:
                conn.execute("UPDATE %s SET %s=? WHERE 关卡ID=?" % (TABLE, col), (new, k))
            for k in new_keys:
                want = dict(rows[k])
                if k in crawl:
                    want["情报"], want["待处理目标数量"] = crawl[k]
                conn.execute(
                    "INSERT OR IGNORE INTO %s (%s) VALUES (%s)"
                    % (TABLE, ", ".join(ALL_COLS), ", ".join("?" * len(ALL_COLS))),
                    tuple(want[c] for c in ALL_COLS),
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
    print("  校对写回: %d 列 | 新增: %d 行 | 爬取成功: %d | 无页面: %d | 无详情结构: %d"
          % (record["校对"]["差异列数"], record["新增"]["行"], record["爬取"]["成功"],
             record["爬取"]["无页面"], record["爬取"]["无详情结构"]))
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
