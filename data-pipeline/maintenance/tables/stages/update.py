# -*- coding: utf-8 -*-
"""stages · 更新（增量）

数据源（本地，一个主源 + 五个特殊源）
    excel/stage_table.json                     关卡基本信息（主源）
    excel/roguelike_topic_table.json           集成战略
    excel/sandbox_table.json / sandbox_perm_table.json   生息演算（第0期 / V2 / V3）
    excel/crisis_v2_table.json                 全息作战矩阵
    excel/handbook_info_table.json             悖论模拟
    excel/story_review_meta_table.json         训练场
    levels/{obt,activities}/**/*.json          战斗列：部署上限 / 初始费用 / 费用上限 / 目标点耐久 / 敌人列表
    excel/enemy_handbook_table.json            敌人名（图鉴口径）
    enemies 表                                 敌人表名、覆写属性的基准行

PRTS 侧（一页可覆盖多个难度身份）
    {{普通/突袭/磨难险地/剿灭关卡信息}} · 情报 → 情报
                                        · 敌人数量 → 待处理目标数量

列分类
    数据源列（14）关卡名 关卡描述 关卡编号 关卡类型 解锁条件 推荐等级 理智消耗 levelId
                  部署上限 初始费用 费用上限 目标点耐久 敌人列表
    爬取列（2）   情报 待处理目标数量    ← 只在新增首次爬取时填空，之后归人
    人工列        —（这两列本身就归人；手改后脚本不会覆盖）

格子级豁免
    关卡ID 含 `_dlc` → 关卡名（岁·界园 DLC1 的差分关卡与基础关卡同名，脚本推不出区域名）
    源里找不到 level 文件 → 部署上限 / 初始费用 / 费用上限 / 目标点耐久 / 敌人列表

完整性判据
    页面里「情报」与「敌人数量」都没有 → partial（待填）
    有其中一样                        → synced（另一样就是本就没有）
    页面 404                          → missing

依赖
    **enemies 必须先跑**：敌人列表的覆写属性要拿 enemies 表当基准（EnemyIndex）。

不做 recheck
    本表只爬两列、且都不易变、爬取耗时长；这两列还会被人工修改，重爬风险大于收益。
    新增时由 Air 手动校验 pending 清单。

用法
    python update.py --dry-run    # 本地预分析：不联网、不写任何文件
    python update.py --base       # 数据源 → 库 + 登记待办
    python update.py --crawl      # 只爬 pending
"""

import json
import os
import sqlite3
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
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402
from enemy_attrs import ATK_MAP, IMMUNE_MAP, MOTION_MAP, STATUS_MAP  # noqa: E402

TABLE = "stages"
SENTINEL_PAGE = "0-1_坍塌"     # 结构哨兵：实测存在且带关卡模板
SENTINEL_MARKER = "关卡信息"
SLEEP = uc.SLEEP_BETWEEN_FETCH

NO_DETAIL_RECHECK_EVERY = 5
NO_DETAIL_RATIO_LIMIT = 0.5
NO_DETAIL_MIN_TOTAL = 20

STAGE_TABLE = paths.GAMEDATA / "excel" / "stage_table.json"
ENEMY_HANDBOOK = paths.GAMEDATA / "excel" / "enemy_handbook_table.json"
LEVELS_ROOT = paths.GAMEDATA / "levels"

STAGE_TYPE_MAP = {
    "MAIN": "主线", "ACTIVITY": "活动", "SUB": "支线", "DAILY": "日常",
    "CAMPAIGN": "剿灭", "GUIDE": "教学", "CLIMB_TOWER": "保全派驻",
    "SPECIAL_STORY": "特殊故事",
}

# 人工维护的关卡名：ID 含此串的行，关卡名不参与校对
EXCLUDE_NAME_FROM_CHECK = "_dlc"

# 源里缺 level 文件时不算数的战斗列
COMBAT_COLS = {"部署上限", "初始费用", "费用上限", "目标点耐久", "敌人列表"}
NO_LEVEL_KEYS = set()          # compute_rows 填充：这些键的战斗列跳过

# ─── 列声明 ────────────────────────────────────────────────────

COLUMNS = tb.Columns(
    all=[
        "关卡ID", "关卡名", "关卡描述", "关卡编号", "关卡类型", "解锁条件", "推荐等级",
        "理智消耗", "levelId", "部署上限", "初始费用", "费用上限", "目标点耐久",
        "敌人列表", "情报", "待处理目标数量",
    ],
    source=[
        "关卡名", "关卡描述", "关卡编号", "关卡类型", "解锁条件", "推荐等级", "理智消耗",
        "levelId", "部署上限", "初始费用", "费用上限", "目标点耐久", "敌人列表",
    ],
    crawl=["情报", "待处理目标数量"],
    json=["解锁条件", "敌人列表"],
    int=["理智消耗", "部署上限", "初始费用", "费用上限", "目标点耐久"],
)

# ─── 覆写属性映射（键必须与 enemies 表列名一致，PRTS 才能回表）───

ATTR_COL = {
    "name": "名称", "description": "描述", "levelType": "地位",
    "applyWay": "攻击方式", "motion": "行动方式",
    "rangeRadius": "攻击半径", "lifePointReduce": "目标价值",
    "notCountInTotal": "不计入敌人总数",
    "numOfExtraDrops": "numOfExtraDrops", "viewRadius": "viewRadius",
    "maxHp": "最大生命值", "atk": "攻击力", "def": "防御力",
    "magicResistance": "法术抗性", "moveSpeed": "移动速度",
    "attackSpeed": "攻击速度", "baseAttackTime": "攻击间隔",
    "hpRecoveryPerSec": "生命自回速度", "massLevel": "重量",
    "tauntLevel": "基础嘲讽等级",
    "epResistance": "损伤抗性", "epDamageResistance": "元素抗性",
    "stunImmune": "晕眩抗性", "silenceImmune": "沉默抗性",
    "sleepImmune": "沉睡抗性", "frozenImmune": "冻结抗性",
    "levitateImmune": "浮空抗性", "disarmedCombatImmune": "缴械抗性",
    "fearedImmune": "恐惧抗性", "palsyImmune": "麻痹抗性",
    "attractImmune": "诱导抗性", "teleportImmune": "传送抗性",
    "groundBoundImmune": "缚地抗性",
    "talentBlackboard": "天赋覆写", "skills": "技能覆写",
}
IMMUNE_CN = {ATTR_COL[raw]: cn for raw, cn in IMMUNE_MAP.items() if raw in ATTR_COL}
ENUM_CN = {"攻击方式": ATK_MAP, "行动方式": MOTION_MAP, "地位": STATUS_MAP}
SIBLING_KEYS = ("天赋覆写", "技能覆写")

# PRTS 页面名规则
URL_RULES = {
    "生息演算·沙洲遗闻": "{code}_{name}(沙洲遗闻)",
    "悖论模拟": "悖论模拟_{name}",
}
DEFAULT_URL = "{code}_{name}"


# ─── 数据源读取 ───────────────────────────────────────────────

def load_json(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def handbook_names():
    """图鉴 ID → 图鉴名（清洗后）。"""
    handbook = load_json(ENEMY_HANDBOOK)
    out = {}
    for eid, v in handbook.get("enemyData", {}).items():
        out[eid] = (clean_desc(v.get("name")) or "").strip() or None
    return out


def build_level_index():
    """levelId → 文件路径。按 obt → activities 顺序走，后者覆盖前者。"""
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
    """按需加载 level JSON，只留战斗列所需字段并缓存。"""

    def __init__(self, index, enemy_index):
        self.index = index
        self.enemy_index = enemy_index
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
                lv = load_json(p)
                opts = lv.get("options", {})
                self.cache[lid] = (
                    opts.get("characterLimit"),
                    opts.get("initialCost"),
                    opts.get("maxCost"),
                    opts.get("maxLifePoint"),
                    build_enemy_list(lv.get("enemyDbRefs"), self.enemy_index),
                )
        return self.cache[lid]


def clean_level_id(raw):
    return raw.split("/")[-1] if raw else None


def clean_unlock_condition(raw):
    if not raw:
        return "[]"
    return json.dumps(
        [{"stageId": c.get("stageId", ""), "completeState": c.get("completeState", "")}
         for c in raw],
        ensure_ascii=False,
    )


def extract_overrides(od):
    """取 overwrittenData 的覆写字段，键名转中文（只走顶层 + attributes 两层）。"""
    result = {}
    if not isinstance(od, dict):
        return result

    def leaf(node):
        if isinstance(node, dict):
            if node.get("m_defined") is True:
                return True, node.get("m_value")
            return False, None
        if isinstance(node, list) and node:
            return True, node
        return False, None

    for key, val in od.items():
        if key == "attributes" and isinstance(val, dict):
            for ak, av in val.items():
                ok, v = leaf(av)
                if ok:
                    result[ATTR_COL.get(ak, ak)] = v
            continue
        ok, v = leaf(val)
        if ok:
            result[ATTR_COL.get(key, key)] = v
    return result


def build_enemy_list(refs, index):
    """敌人列表：名称用于展示，敌人ID 用于回表，覆写属性只留真差异。"""
    if not refs:
        return "[]"
    out = []
    for ref in refs:
        eid = ref.get("id")
        level = ref.get("level", 0)
        ov = extract_overrides(ref.get("overwrittenData"))
        tid, base = index.lookup(eid, level, ov.get("名称"))
        item = {"名称": index.display_name(eid, tid, ov), "敌人ID": tid or eid, "级别": level}
        attrs = filter_overrides(ov, base, index.cols)
        if attrs:
            item["覆写属性"] = attrs
        for k in SIBLING_KEYS:
            if k in ov:
                item[k] = ov[k]
        out.append(item)
    return json.dumps(out, ensure_ascii=False)


def _norm_attr(key, val):
    if key in ("名称", "描述") and isinstance(val, str):
        return clean_desc(val) or ""
    if key in ENUM_CN and isinstance(val, str):
        return ENUM_CN[key].get(val, val)
    return val


def _same_attr(a, b):
    if isinstance(a, bool) or isinstance(b, bool):
        return bool(a) == bool(b)
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return abs(float(a) - float(b)) < 1e-9
    return uc.values_equal(a, b)


def _parse_json_list(raw):
    if not raw:
        return []
    try:
        v = json.loads(raw)
        return v if isinstance(v, list) else []
    except (ValueError, TypeError):
        return []


def filter_overrides(ov, base, cols):
    """只留 enemies 表里存在的属性列，且值与该敌人常规值不同的那些。"""
    out = {}
    for key, val in ov.items():
        if key in SIBLING_KEYS:
            continue
        if key in IMMUNE_CN:
            cur = (base or {}).get("异常抗性") or []
            want = bool(val)
            if base is not None and want == (IMMUNE_CN[key] in cur):
                continue
            out[key] = want
            continue
        col = key
        if col not in cols:
            continue
        val = _norm_attr(key, val)
        if base is not None and _same_attr(val, base.get(col)):
            continue
        out[key] = val
    return out


def _strip_chain(eid):
    """逐段去尾的候选 ID（enemy_a_b_c → enemy_a_b → enemy_a）。"""
    parts = (eid or "").split("_")
    out = []
    while len(parts) > 2:
        parts = parts[:-1]
        out.append("_".join(parts))
    return out


class EnemyIndex:
    """enemies 表快照：名称、常规值基准、ID 归属判断。

    `enemyDbRefs[].id` 原样在表里就直接用，绝不去尾；原样不在才逐段去尾找候选，
    且候选的名称须与覆写里的名称一致才算同一敌人（否则算关卡内联）。
    """

    def __init__(self, conn, hb_names):
        self.cols = [d[1] for d in conn.execute("PRAGMA table_info(enemies)")]
        self.hb_names = hb_names
        self.rows = {}
        self.name_by_id = {}
        for r in conn.execute("SELECT * FROM enemies"):
            d = dict(zip(self.cols, r))
            if d["敌人ID"] not in self.name_by_id:
                self.name_by_id[d["敌人ID"]] = d.get("名称")
            d["异常抗性"] = _parse_json_list(d.get("异常抗性"))
            self.rows[(d["敌人ID"], d["级别"])] = d

    def lookup(self, eid, level, want_name):
        if eid in self.name_by_id:
            return eid, self._row(eid, level)
        for cand in _strip_chain(eid):
            if cand in self.name_by_id:
                if (want_name or "").strip() and self.name_by_id[cand] == want_name:
                    return cand, self._row(cand, level)
                return None, None
        return None, None

    def _row(self, eid, level):
        return self.rows.get((eid, level)) or self.rows.get((eid, 0))

    def display_name(self, eid, tid, ov):
        """图鉴名 → 敌人表名 → 覆写里的名称 → 空。"""
        for k in (eid, tid):
            if k and self.hb_names.get(k):
                return self.hb_names[k]
        for k in (eid, tid):
            if k and self.name_by_id.get(k):
                return self.name_by_id[k]
        return _norm_attr("名称", ov.get("名称")) if ov.get("名称") else ""


# ─── 重算目标行 ────────────────────────────────────────────────

def compute_rows(db_path):
    """从数据源 + enemies 表算出全部目标行：{关卡ID: {列名: 值}}。"""
    conn = sqlite3.connect(db_path)
    try:
        lc = LevelCache(build_level_index(), EnemyIndex(conn, handbook_names()))
        rows, no_level = _compute(lc)
    finally:
        conn.close()
    NO_LEVEL_KEYS.clear()
    NO_LEVEL_KEYS.update(no_level)
    return rows


def _compute(lc):
    rows = {}
    no_level = set()

    def fill_combat(row, lid_main, lid_raw, kind):
        """按 level JSON 填战斗列；缺文件则置空并记入 no_level。"""
        row["部署上限"] = row["初始费用"] = row["费用上限"] = None
        row["目标点耐久"] = None
        row["敌人列表"] = None
        if not lid_raw or ("," in lid_raw and "集成战略" not in kind):
            no_level.add(row["关卡ID"])
            return
        combat = lc.combat(lid_main)
        if combat is None:
            no_level.add(row["关卡ID"])
            return
        row["部署上限"], row["初始费用"], row["费用上限"], row["目标点耐久"], row["敌人列表"] = combat

    # ── A. stage_table.json ──
    for sid, s in load_json(STAGE_TABLE)["stages"].items():
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
        for topic_id, topic in rl.get("topics", {}).items():
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
        for key in ("sandbox_1", "sandbox_2"):
            info = sbp.get("basicInfo", {}).get(key, {})
            sub = sbp.get("detail", {}).get(
                "SANDBOX_V2" if key == "sandbox_1" else "SANDBOX_V3", {}).get(key, {})
            kind = "生息演算·%s" % info.get("topicName", key)
            for sid, s in sub.get("stageData", {}).items():
                if key == "sandbox_1":
                    add_special(s.get("stageId"), s.get("name"), s.get("description"),
                                s.get("code"), kind, clean_level_id(s.get("levelId")))
                else:
                    story = sub.get("storyStageData", {}).get(sid, {})
                    lids = []
                    for si in [x for x in (story.get("subStageIdList") or []) if x]:
                        lid = clean_level_id(sub.get("subStageData", {}).get(si, {}).get("levelId"))
                        if lid:
                            lids.append(lid)
                    add_special(s.get("stageId"), s.get("name"), s.get("description"),
                                s.get("code"), kind, ",".join(lids), lids[0] if lids else None)

    # B3. 全息作战矩阵：编号加「全息」前缀（与常规关卡同名，避免 PRTS 误认）
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


# ─── 格子级豁免 ────────────────────────────────────────────────

def skip_column(key, col):
    sid = key[0] if isinstance(key, tuple) else key
    if col == "关卡名" and EXCLUDE_NAME_FROM_CHECK in sid:
        return True
    if col in COMBAT_COLS and sid in NO_LEVEL_KEYS:
        return True
    return False


# ─── PRTS 爬取（情报 / 待处理目标数量）─────────────────────────

def build_url_key(sid, code, name, stype):
    eff = (code or "").strip()
    if sid.startswith("tough_"):
        eff = "磨难" + eff
    elif "#s" in sid:
        eff = "险地" + eff
    tpl = URL_RULES.get(stype, DEFAULT_URL)
    key = tpl.format(code=eff, name=(name or "").strip())
    return key.replace("#", "＃")      # MediaWiki 页名不能含 #，PRTS 用全角代替


def _intel_from_template(params):
    raw = params.get("情报")
    return clean_wiki(raw) if raw else None


def _template_intel(text):
    """关卡页 wikitext → 各难度身份的情报 + 敌人数量。"""
    out = {}
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

    out["_count"] = None
    for tpl_name in ("普通关卡信息", "突袭关卡信息", "剿灭关卡信息", "保全派驻关卡信息",
                     "集成战略关卡信息", "区块化关卡信息", "教学关卡信息", "活动关卡信息",
                     "主线关卡信息", "支线关卡信息"):
        tpls = uc.parse_template(text, tpl_name)
        if tpls and tpls[0].get("敌人数量"):
            out["_count"] = tpls[0]["敌人数量"]
            break
    return out


def match_intel(sid, table):
    if "IS_EMERGENCY" in table and "_e_" in sid:
        return table.get("IS_EMERGENCY")
    if sid.startswith("tough_") or "#s" in sid:
        return table.get("SIX_STAR")
    if "#f#" in sid:
        return table.get("FOUR_STAR")
    if table.get("ANNIHILATION") is not None:
        return table.get("ANNIHILATION")
    return table.get("NORMAL")


def crawl(keys, rows):
    """爬 pending 里的关卡。一个页面覆盖多个难度身份，按页面分组后一页一次。

    判据：页面里情报与敌人数量都没有 → partial（待填）；有其一 → synced。
    """
    groups, unpageable = {}, []
    for k in keys:
        sid = k[0] if isinstance(k, tuple) else k
        r = rows.get(k) or {}
        if not r.get("关卡编号") or not r.get("关卡名"):
            unpageable.append(k)
            continue
        url = build_url_key(sid, r["关卡编号"], r["关卡名"], r["关卡类型"])
        groups.setdefault(url, []).append(k)

    out = {}
    for k in unpageable:
        out[k] = tb.Outcome("partial", note="无编号或名称，拼不出页面")

    pages = list(groups)
    if pages:
        ok, why = uc.sentinel_ok(SENTINEL_PAGE, SENTINEL_MARKER)
        if not ok:
            raise RuntimeError(why)

    skipped = 0
    start = time.time()
    for i, (url, ks) in enumerate(groups.items(), 1):
        tb.progress(i, len(pages), url[:56], start)
        try:
            text = uc.fetch_wikitext(url)
        except uc.NoPage:
            for k in ks:
                out[k] = tb.Outcome("missing", page=url, note="页面不存在")
            time.sleep(SLEEP)
            continue

        table = _template_intel(text)
        count = table.pop("_count", None)
        hit = False
        for k in ks:
            sid = k[0] if isinstance(k, tuple) else k
            intel = match_intel(sid, table)
            if intel is None and not count:
                out[k] = tb.Outcome("partial", page=url, note="情报与敌人数量都没有")
                continue
            hit = True
            out[k] = tb.Outcome("synced", page=url,
                                values={"情报": intel, "待处理目标数量": count})
        if not hit:
            skipped += 1
            if skipped % NO_DETAIL_RECHECK_EVERY == 0:
                ok2, why2 = uc.sentinel_ok(SENTINEL_PAGE, SENTINEL_MARKER)
                if not ok2:
                    raise RuntimeError("站点结构复核失败（已空页 %d 个）: %s" % (skipped, why2))
            if skipped >= NO_DETAIL_MIN_TOTAL and skipped > i * NO_DETAIL_RATIO_LIMIT:
                raise RuntimeError("空页占比过高（%d/%d），疑似 PRTS 结构变化" % (skipped, i))
        time.sleep(SLEEP)
    return out


SPEC = tb.TableSpec(
    table=TABLE,
    columns=COLUMNS,
    key_cols=["关卡ID"],
    load_rows=compute_rows,
    pending_path=HERE / "pending.json",
    crawl=crawl,
    skip_column=skip_column,
    recheck_enabled=False,
)

if __name__ == "__main__":
    sys.exit(tb.run(SPEC))
