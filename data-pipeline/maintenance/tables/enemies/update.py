# -*- coding: utf-8 -*-
"""enemies · 更新（增量）

数据源（本地）
    levels/enemydata/enemy_database.json   数值 / 属性 / 名称 / 描述 / 级别
    excel/enemy_handbook_table.json        图鉴：能力列表 / 种类 / 伤害类型 / 关联敌人
    图鉴条目缺失时：HB_FALLBACK_IDS 里六个变体 ID 回退到本体条目（名称须一致）

PRTS 侧（每页一次请求，按页面去重）
    {{敌人信息/levelcontent}} · 天赋    → 天赋文本
                              · 技能1..N → 技能文本（JSON）
    页面定位：sources/enemies_pages.md（人工源）→ 否则（名称, 级别）
    取块：映射表按节位置（0 基）取，默认按 index 参数取；非首节与首节合并
          （PRTS 约定后面的节只写差异项）

列分类
    数据源列（29）见 COLUMNS.source                    ← 参与校对
    爬取列（2）   天赋文本 技能文本                    ← 已有行只在 --recheck 时比对
    人工列（1）   备注                                 ← 永不比对、永不写入
    行级豁免      enemy_190 / enemy_50 / enemy_55 / enemy_150（254 行）
                  永不校对、永不爬取、不进 pending

列类型
    JSON：能力列表 种类 关联敌人 伤害类型 异常抗性
    整数：级别 目标价值 不计入敌人总数 numOfExtraDrops 最大生命值 攻击力 防御力 重量 基础嘲讽等级
    浮点：攻击半径 法术抗性 移动速度 攻击速度 攻击间隔 生命自回速度 损伤抗性 元素抗性 viewRadius

完整性判据（保守）
    天赋与技能均为空                              → partial（暂未填写）
    至少一个非空                                  → synced
    页面 404                                      → missing
    页面在但无 levelcontent / 该 index 不在页面   → partial

失败口径
    网络 / 解析 / 哨兵 / 写库 → 中止（写库前回滚）
    404 / 空壳                → 记 pending，继续
    站点结构：首页哨兵 + 每 5 个空壳复核一次；空壳占比 >50%（≥20 个后）中止

用法
    python update.py --dry-run      # 本地预分析：不联网、不写任何文件
    python update.py --base         # 数据源 → 库 + 登记待办
    python update.py --crawl        # 只爬 pending
    python update.py --recheck      # 全表重爬，只比对天赋/技能
"""

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
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402
from enemy_attrs import ATK_MAP, DAMAGE_MAP, IMMUNE_MAP, MOTION_MAP, STATUS_MAP  # noqa: E402

TABLE = "enemies"
SENTINEL_MARKER = "敌人信息/levelcontent"
SLEEP = uc.SLEEP_BETWEEN_FETCH
NO_DETAIL_RECHECK_EVERY = 5    # 每累计这么多个空壳，复核一次站点结构
NO_DETAIL_RATIO_LIMIT = 0.5    # 空壳页面占比超过这个值（且已试够 NO_DETAIL_MIN_TOTAL 个）→ 中止
NO_DETAIL_MIN_TOTAL = 20       # 比例兜底的生效下限

# 行级豁免：这些 ID 前缀属于特殊模式下的敌人，天赋/技能由人工补充
ROW_EXEMPT_PREFIXES = ("enemy_190", "enemy_50", "enemy_55", "enemy_150")

ENEMY_DB = paths.GAMEDATA / "levels" / "enemydata" / "enemy_database.json"
HANDBOOK = paths.GAMEDATA / "excel" / "enemy_handbook_table.json"
PAGE_MAP_FILE = paths.MANUAL_SOURCES / "enemies_pages.md"

# 图鉴变体回退：这些 ID 图鉴未收录条目，三字段与本体一致（Air 2026-10-14 定的范围）
HB_FALLBACK_IDS = (
    "enemy_1331_cbsisy_3",   # “十字路口”量产型（生息演算特殊版本）
    "enemy_1529_dsdevr_2",   # 屠谕者，大群意志
    "enemy_1547_blord_2",    # 杜卡雷，“君主之红”第二形态
    "enemy_1553_lrdead_2",   # 引火的死魂灵第一形态
    "enemy_7027_xbbtl_2",    # 晶玲兽
    "enemy_7028_xbscp_2",    # 晶珑兽
)

# ─── 列声明 ────────────────────────────────────────────────────

COLUMNS = tb.Columns(
    all=[
        "敌人ID", "名称", "级别", "描述", "地位", "能力列表", "种类", "关联敌人",
        "攻击方式", "伤害类型", "行动方式", "攻击半径", "目标价值", "不计入敌人总数",
        "numOfExtraDrops", "viewRadius", "最大生命值", "攻击力", "防御力", "法术抗性",
        "移动速度", "攻击速度", "攻击间隔", "生命自回速度", "重量", "基础嘲讽等级",
        "损伤抗性", "元素抗性", "异常抗性", "天赋文本", "技能文本", "备注",
    ],
    source=[
        "名称", "级别", "描述", "地位", "能力列表", "种类", "关联敌人", "攻击方式",
        "伤害类型", "行动方式", "攻击半径", "目标价值", "不计入敌人总数",
        "numOfExtraDrops", "viewRadius", "最大生命值", "攻击力", "防御力", "法术抗性",
        "移动速度", "攻击速度", "攻击间隔", "生命自回速度", "重量", "基础嘲讽等级",
        "损伤抗性", "元素抗性", "异常抗性",
    ],
    crawl=["天赋文本", "技能文本"],
    manual=["备注"],
    json=["能力列表", "种类", "关联敌人", "伤害类型", "异常抗性"],
    int=["级别", "目标价值", "不计入敌人总数", "numOfExtraDrops", "最大生命值",
         "攻击力", "防御力", "重量", "基础嘲讽等级"],
    float=["攻击半径", "法术抗性", "移动速度", "攻击速度", "攻击间隔",
           "生命自回速度", "损伤抗性", "元素抗性", "viewRadius"],
)


# ─── 人工源：特殊页面映射 ──────────────────────────────────────

def load_page_map():
    """读 sources/enemies_pages.md 的映射表 → {(敌人ID, 级别): (页面名, index)}。

    文件是唯一事实来源；这张表代替了原先写在脚本里的 PAGE_MAP。
    """
    out = {}
    if not PAGE_MAP_FILE.exists():
        raise RuntimeError("人工源缺失：%s" % PAGE_MAP_FILE)
    for line in PAGE_MAP_FILE.read_text(encoding="utf-8").splitlines():
        if not line.startswith("| `enemy_"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) < 4:
            continue
        eid = cells[0].strip("`")
        try:
            level, index = int(cells[1]), int(cells[3])
        except ValueError:
            continue
        out[(eid, level)] = (cells[2], index)
    return out


# ─── 数据源读取 ────────────────────────────────────────────────

def load_source():
    with open(ENEMY_DB, encoding="utf-8") as f:
        database = json.load(f)
    with open(HANDBOOK, encoding="utf-8") as f:
        handbook = json.load(f)
    return database, handbook


# ─── 取值辅助 ──────────────────────────────────────────────────

def mv(obj, default=None):
    """解包 m_defined/m_value，始终返回 m_value。"""
    if obj is None:
        return default
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_value"]
    return obj


def is_defined(obj):
    if obj is None:
        return False
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_defined"]
    return True


def val(ed, lv0_ed, key, default=None):
    """取 enemyData 直接字段，level>0 时 m_defined=false 从 lv0 继承。"""
    obj = ed.get(key)
    if not is_defined(obj) and lv0_ed:
        return mv(lv0_ed.get(key), default)
    return mv(obj, default)


def aval(ed, lv0_ed, key, default=None):
    """取 attributes 子字段，继承规则同 val。"""
    attrs = ed.get("attributes", {})
    obj = attrs.get(key)
    if not is_defined(obj) and lv0_ed:
        return mv(lv0_ed.get("attributes", {}).get(key), default)
    return mv(obj, default)


def pick_hb_entry(hb_data, enemy_id, name):
    """该敌人的图鉴条目；HB_FALLBACK_IDS 里的变体 ID 回退到本体（名称须一致）。"""
    entry = hb_data.get(enemy_id.lower())
    if entry is not None:
        return entry
    if enemy_id not in HB_FALLBACK_IDS:
        return None
    cur = enemy_id
    while True:
        m = re.search(r"_\d+$", cur)
        if not m:
            return None
        cur = cur[:m.start()]
        entry = hb_data.get(cur.lower())
        if entry is not None:
            if (entry.get("name") or "").strip() == (name or "").strip():
                return entry
            return None


# ─── 重算目标行 ────────────────────────────────────────────────

def compute_rows(db_path=None):
    """从本地数据源算出全部目标行：{(敌人ID, 级别): {列名: 值}}。

    db_path 用不到（本表只读数据源）；参数是为了与 table_base 的约定一致。
    """
    database, handbook = load_source()
    race_map = {k: v.get("raceName") for k, v in handbook.get("raceData", {}).items()}
    hb_data = handbook.get("enemyData", {})

    name_map = {}
    for entry in database["enemies"]:
        for v in entry["Value"]:
            if v["level"] == 0:
                nm = (clean_desc(mv(v["enemyData"].get("name"))) or "").strip()
                name_map[entry["Key"].lower()] = nm or None
                break

    rows = {}
    for entry in database["enemies"]:
        eid = entry["Key"]
        lv0_ed = None
        for v in entry["Value"]:
            if v["level"] == 0:
                lv0_ed = v["enemyData"]
                break

        hb_entry = pick_hb_entry(hb_data, eid, name_map.get(eid.lower())) or {}

        # 关联敌人：同一敌人所有 level 行相同
        link_ids = hb_entry.get("linkEnemies", [])
        links = json.dumps(
            [name_map.get(lid.lower()) or lid for lid in link_ids], ensure_ascii=False
        ) if link_ids else None

        for variant in entry["Value"]:
            level = variant["level"]
            ed = variant["enemyData"]

            name = (clean_desc(val(ed, lv0_ed, "name")) or "").strip() or None
            desc = clean_desc(val(ed, lv0_ed, "description"))

            raw_level = val(ed, lv0_ed, "levelType") or hb_entry.get("enemyLevel")
            status = STATUS_MAP.get(raw_level, raw_level)

            raw_abilities = hb_entry.get("abilityList", [])
            abilities = json.dumps(
                [clean_desc(a.get("text")) for a in raw_abilities], ensure_ascii=False
            ) if raw_abilities else None
            dmg_types = hb_entry.get("damageType", [])
            dmg_cn = json.dumps([DAMAGE_MAP.get(t, t) for t in dmg_types], ensure_ascii=False)

            tags = val(ed, lv0_ed, "enemyTags", [])
            tags_cn = json.dumps([race_map.get(t, t) for t in tags],
                                 ensure_ascii=False) if tags else None

            atk_raw = val(ed, lv0_ed, "applyWay")
            mot_raw = val(ed, lv0_ed, "motion")

            immunes = [cn for key, cn in IMMUNE_MAP.items() if aval(ed, lv0_ed, key) is True]

            rows[(eid, level)] = {
                "敌人ID": eid,
                "名称": name,
                "级别": level,
                "描述": desc,
                "地位": status,
                "能力列表": abilities,
                "种类": tags_cn,
                "关联敌人": links,
                "攻击方式": ATK_MAP.get(atk_raw, atk_raw),
                "伤害类型": dmg_cn,
                "行动方式": MOTION_MAP.get(mot_raw, mot_raw),
                "攻击半径": val(ed, lv0_ed, "rangeRadius"),
                "目标价值": val(ed, lv0_ed, "lifePointReduce"),
                "不计入敌人总数": 1 if val(ed, lv0_ed, "notCountInTotal") else 0,
                "numOfExtraDrops": val(ed, lv0_ed, "numOfExtraDrops"),
                "viewRadius": val(ed, lv0_ed, "viewRadius"),
                "最大生命值": aval(ed, lv0_ed, "maxHp"),
                "攻击力": aval(ed, lv0_ed, "atk"),
                "防御力": aval(ed, lv0_ed, "def"),
                "法术抗性": aval(ed, lv0_ed, "magicResistance"),
                "移动速度": aval(ed, lv0_ed, "moveSpeed"),
                "攻击速度": aval(ed, lv0_ed, "attackSpeed"),
                "攻击间隔": aval(ed, lv0_ed, "baseAttackTime"),
                "生命自回速度": aval(ed, lv0_ed, "hpRecoveryPerSec"),
                "重量": aval(ed, lv0_ed, "massLevel"),
                "基础嘲讽等级": aval(ed, lv0_ed, "tauntLevel"),
                "损伤抗性": aval(ed, lv0_ed, "epResistance"),
                "元素抗性": aval(ed, lv0_ed, "epDamageResistance"),
                "异常抗性": json.dumps(immunes, ensure_ascii=False) if immunes else None,
                "天赋文本": None,   # 爬取列
                "技能文本": None,   # 爬取列
                "备注": None,       # 人工列，脚本不写
            }
    return rows


# ─── 页面解析 ──────────────────────────────────────────────────

def extract_skills(params):
    """从模板参数中提取技能列表。"""
    skills = []
    for key in sorted(params.keys()):
        m = re.match(r"^技能(\d+)$", key)
        if not m:
            continue
        si = m.group(1)
        skills.append({
            "名称": clean_wiki(params.get(key, "")),
            "初始冷却": params.get("技能%s初始" % si, ""),
            "周期冷却": params.get("技能%s消耗" % si, ""),
            "技力消耗": params.get("技能%s技力" % si, "/"),
            "效果": clean_wiki(params.get("技能%s效果" % si, "")),
        })
    return skills


def level_index(lv):
    try:
        return int(str(lv.get("index", "0")).strip() or "0")
    except (TypeError, ValueError):
        return 0


def pick_level(levels, index):
    """取 index 对应的块，并与 index=0 合并（后面的节只写差异项）。"""
    base = None
    for lv in levels:
        if level_index(lv) == 0:
            base = lv
            break
    for lv in levels:
        if level_index(lv) == index:
            merged = dict(base or {})
            merged.update(lv)
            return merged
    return None


def pick_level_by_pos(levels, pos):
    """按页面上的节位置取块（0 基），与首节合并。

    映射表用位置而不是 index 参数：参数是人工填的，复制模板时容易忘改
    （沉沦之人第三节就还写着 index=1）。
    """
    if pos < 0 or pos >= len(levels):
        return None
    merged = dict(levels[0])
    if pos:
        merged.update(levels[pos])
    return merged


def text_from_level(block):
    """一个 levelcontent 块 → (天赋文本, 技能文本)。"""
    skills = extract_skills(block)
    return (
        clean_wiki(block.get("天赋", "")) or None,
        json.dumps(skills, ensure_ascii=False) if skills else None,
    )


# ─── 爬取 ──────────────────────────────────────────────────────

def crawl(keys, rows):
    """爬待爬/重爬的键。返回 {(敌人ID, 级别): Outcome}。

    真·事故向上抛（触发整体中止）：哨兵失败 / 网络异常 / 空壳占比过高。
    404 与空壳属可预见情况，按 missing / partial 记录，不抛。
    """
    page_map = load_page_map()
    by_page = {}
    for key in keys:
        name = (rows.get(key) or {}).get("名称") or ""
        mapped = page_map.get(key)
        if mapped:
            page, index = mapped
            by_page.setdefault(page, []).append((key, index, True))
            continue
        page, index = name, key[1]
        if not page:
            continue
        by_page.setdefault(page, []).append((key, index, False))
    pages = list(by_page)
    if not pages:
        return {}

    ok, why = uc.sentinel_ok(pages[0], SENTINEL_MARKER)
    if not ok:
        raise RuntimeError(why)

    out = {}
    no_detail = 0
    start = time.time()
    for i, page in enumerate(pages, 1):
        tb.progress(i, len(pages), page, start)

        try:
            wikitext = uc.fetch_wikitext(page)
        except uc.NoPage:
            wikitext = None
        levels = uc.parse_template(wikitext, SENTINEL_MARKER) if wikitext else []

        if not levels:
            if wikitext is None:
                state, note = "missing", "页面不存在"
            else:
                state, note = "partial", "无 levelcontent 结构"
                no_detail += 1
                if no_detail % NO_DETAIL_RECHECK_EVERY == 0:
                    ok2, why2 = uc.sentinel_ok(pages[0], SENTINEL_MARKER)
                    if not ok2:
                        raise RuntimeError("站点结构复核失败（已空壳 %d 个）: %s" % (no_detail, why2))
                if no_detail >= NO_DETAIL_MIN_TOTAL and no_detail > i * NO_DETAIL_RATIO_LIMIT:
                    raise RuntimeError("空壳页面占比过高（%d/%d），疑似 PRTS 结构变化"
                                       % (no_detail, i))
            for key, _idx, _pos in by_page[page]:
                out[key] = tb.Outcome(state, page=page, note=note)
            time.sleep(SLEEP)
            continue

        for key, index, by_pos in by_page[page]:
            block = (pick_level_by_pos(levels, index) if by_pos
                     else pick_level(levels, index))
            if block is None:
                note = ("第 %d 节不在页面（共 %d 节）" % (index, len(levels))
                        if by_pos else "index %s 不在页面" % index)
                out[key] = tb.Outcome("partial", page=page, note=note)
                continue
            tianfu, jinneng = text_from_level(block)
            if not tianfu and not jinneng:
                out[key] = tb.Outcome("partial", page=page,
                                      values={"天赋文本": None, "技能文本": None},
                                      note="天赋与技能均为空")
            else:
                out[key] = tb.Outcome("synced", page=page,
                                      values={"天赋文本": tianfu, "技能文本": jinneng})
        time.sleep(SLEEP)
    return out


# ─── 行级豁免 ──────────────────────────────────────────────────

def row_exempt(key):
    return str(key[0]).startswith(ROW_EXEMPT_PREFIXES)


SPEC = tb.TableSpec(
    table=TABLE,
    columns=COLUMNS,
    key_cols=["敌人ID", "级别"],
    load_rows=compute_rows,
    pending_path=HERE / "pending.json",
    crawl=crawl,
    row_exempt=row_exempt,
)

if __name__ == "__main__":
    sys.exit(tb.run(SPEC))
