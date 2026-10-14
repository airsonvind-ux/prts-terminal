# -*- coding: utf-8 -*-
"""PRTS 终端 — enemies 表更新脚本（维护用）

三段式，顺序固定：
  1. 校对：以 (敌人ID, 级别) 为键，把库内行与「由本地数据源重新算出的值」逐列比对，
          差异写回库并记入更新记录。
          校对列 = 除 `天赋文本` / `技能文本` / `备注` 之外的全部列
          （前两者来源于 PRTS 爬取，逐次重爬太慢；`备注` 是人工补充列）。
  2. 新增：库内不存在的键 INSERT（同一套继承与回填规则，含 `关联敌人`）；
          这些敌人的名称去重后爬一次 PRTS wiki，取回天赋/技能文本，过 wiki 清洗后写入。
          已有敌人不重爬。
  3. 记录：单事务，任何异常都不落库；结束写 updates/<时间戳>-enemies.json。

数据源（只读，来自 paths.GAMEDATA）：
  levels/enemydata/enemy_database.json   数值与属性
  excel/enemy_handbook_table.json        图鉴字段（能力列表 / 种类 / 伤害类型 / 关联敌人）
游戏版本号取自数据源仓库最新提交信息（形如 [CN UPDATE] Client:2.7.81 Data:26-10-08-...）。
通用件（源版本 / 备份 / 记录 / PRTS 抓取与哨兵 / 列比较 / 输出环境）见 update_common.py。

特殊页面映射（PAGE_MAP）：
  少数敌人的 PRTS 页面与库内条目不是一一对应：一个页面里装着多个形态/级别，
  或页面名不是敌人名（带后缀、差分页、差分索引页）。默认规则「按名称取页 +
  取 index==级别 的 levelcontent 块」对这些敌人会抓错或抓重，故在 PAGE_MAP 里为
  这些 (敌人ID, 级别) 显式写明 (页面名, index)。逐条依据见
  20.Projects/PRTS终端助手/04-数据/特殊页面爬取映射清单.md

  取块规则：PRTS 约定 index=0 那节写全参数，之后的每节只写与它不同的参数
  （缺的参数表示继承），所以取 index>0 的节时要与 index=0 合并。

失败语义（Air 定，2026-10-13）：
  - 网络异常 / PRTS 站点结构变化 → 中止本次更新（不改库），记录里写明原因；
  - PRTS 404（页面不存在，多为新敌人尚无页面）→ 跳过并记录，不计失败。

用法：
    python update_enemies.py --dry-run            # 只报告校对差异与新增，不写库、不爬取、不写记录
    python update_enemies.py                      # 正式执行（校对 + 新增 + 记录）
    python update_enemies.py --recrawl-special    # 只重抓 PAGE_MAP 里的敌人：按映射重写其 天赋文本/技能文本
    python update_enemies.py --recrawl-special --dry-run
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

TABLE = "enemies"
SCRIPT_VERSION = 4
MAX_DETAIL = 200          # 记录里明细最多留多少条（超出只计数）
SLEEP_BETWEEN_FETCH = uc.SLEEP_BETWEEN_FETCH
NO_DETAIL_RECHECK_EVERY = 5   # 每累计跳过这么多个，复核一次哨兵（站点结构还健康吗）
NO_DETAIL_RATIO_LIMIT = 0.5   # 无结构页面占比超过这个值（且已试够 NO_DETAIL_MIN_TOTAL 个）→ 中止
NO_DETAIL_MIN_TOTAL = 20      # 比例兜底的生效下限
SENTINEL_MARKER = "敌人信息/levelcontent"

# ID 段豁免：这些段不参与「校对」与「PRTS 爬取」，只做新增。
# 190 段是奇象生物（活动捕捉后可在特定玩法召唤的敌人 + 捕捉关卡中出现的同名非常规敌人）：
#   ① PRTS 上只有装置页/图鉴页，没有 {{敌人信息/levelcontent}} 结构，本来就没有技能与天赋；
#   ② 段内 18 个名字与正常敌人重名，按名称爬会重定向到正常敌人页、抓到别人的数据；
#   ③ 描述 / 能力列表 / 天赋文本 / 技能文本 由人工维护，校对会用数据源重算覆盖它们。
# 用 ID 前缀判定是唯一可靠的信号（页面行为分辨不了同名实体）。
EXCLUDE_FROM_CHECK_AND_CRAWL = ("enemy_190",)

# 特殊页面映射：键 (敌人ID, 级别)，值 (页面名, index)。未列出的键走默认规则
# （页面名 = 名称，index = 级别）。要改哪一条，请连同
# 04-数据/特殊页面爬取映射清单.md 一起改。
PAGE_MAP = {
    # 净浊之焰 / 焦竭之火：页面把两个不同内部 id 合并成「级别0 / 级别1」两节
    ("enemy_1177_dufrbl", 0): ("净浊之焰", 0),
    ("enemy_1177_dufrbl_2", 0): ("净浊之焰", 1),
    ("enemy_1294_duchns", 0): ("焦竭之火", 0),
    ("enemy_1294_duchns_2", 0): ("焦竭之火", 1),
    # 熔戈者，护王余火：页面分「级别B / 级别A / 级别S」三节
    ("enemy_8001_flmlod", 0): ("熔戈者，护王余火", 0),
    ("enemy_8001_flmlod_2", 0): ("熔戈者，护王余火", 1),
    ("enemy_8001_flmlod_3", 0): ("熔戈者，护王余火", 2),
    # 结构性原理：敌方页带后缀（无后缀那页是我方召唤物，没有 levelcontent），分「Rank Ⅰ / Ⅱ / Ⅲ」
    ("enemy_8011_mcndog", 0): ("结构性原理(敌方)", 0),
    ("enemy_8011_mcndog_2", 0): ("结构性原理(敌方)", 1),
    ("enemy_8011_mcndog_3", 0): ("结构性原理(敌方)", 2),
    # 假想敌：胄 / 铳 / 管：页面多出「级别0(隐秘核心)」一节，对应 _2
    ("enemy_9013_acstmk", 0): ("假想敌：胄", 0),
    ("enemy_9013_acstmk_2", 0): ("假想敌：胄", 1),
    ("enemy_9017_achunt", 0): ("假想敌：铳", 0),
    ("enemy_9017_achunt_2", 0): ("假想敌：铳", 1),
    ("enemy_9021_acduml", 0): ("假想敌：管", 0),
    ("enemy_9021_acduml_2", 0): ("假想敌：管", 1),
    # 乌萨斯战争术师：页面第二节标题就是「特殊版本」
    ("enemy_10210_muwiz", 0): ("乌萨斯战争术师", 0),
    ("enemy_10210_muwiz_2", 0): ("乌萨斯战争术师", 1),
    # 落石：页面分「落石(小型) / 落石(大型)」
    ("enemy_1343_ltrock", 0): ("落石", 0),
    ("enemy_1343_ltrock_2", 0): ("落石", 1),
    # 同构碎片：无后缀那页是差分索引页（无 levelcontent），真正的敌人在「同构碎片1/2/3」
    ("enemy_1542_wdslm_2", 0): ("同构碎片1", 0),
    ("enemy_1542_wdslm_2", 1): ("同构碎片1", 1),
    ("enemy_1542_wdslm_3", 0): ("同构碎片2", 0),
    ("enemy_1542_wdslm_3", 1): ("同构碎片2", 1),
    ("enemy_1542_wdslm_4", 0): ("同构碎片3", 0),
    ("enemy_1542_wdslm_4", 1): ("同构碎片3", 1),
    # 某种天命：两个内部 id 各有一个同名带后缀的页面
    ("enemy_10111_ymgpck", 0): ("“某种天命”", 0),
    ("enemy_10111_ymgpck_2", 0): ("“某种天命”(HI02)", 0),
    # 杜卡雷，“君主之红”：页面分「第一形态(级别0/1) / 第二形态(级别0/1)」共四节
    ("enemy_1547_blord", 0): ("杜卡雷，“君主之红”", 0),
    ("enemy_1547_blord", 1): ("杜卡雷，“君主之红”", 1),
    ("enemy_1547_blord_2", 0): ("杜卡雷，“君主之红”", 2),
    ("enemy_1547_blord_2", 1): ("杜卡雷，“君主之红”", 3),
}

uc.reconfigure_stdout()

# 三字段补齐：这些是同一实体的变体 ID，图鉴未收录其条目、但本体有 —— `能力列表` /
# `关联敌人` / `伤害类型` 应与本体条目一致（不再为空）。只对下列 ID 生效，不做成全表规则
# （Air 2026-10-14 定：范围就是这批）；`--recrawl-special` 会顺带补上。
# 依据见 04-数据/特殊页面爬取映射清单.md。
HB_FALLBACK_IDS = (
    "enemy_1331_cbsisy_3",   # “十字路口”量产型（生息演算特殊版本）
    "enemy_1529_dsdevr_2",   # 屠谕者，大群意志
    "enemy_1547_blord_2",    # 杜卡雷，“君主之红”第二形态
    "enemy_1553_lrdead_2",   # 引火的死魂灵第一形态
    "enemy_7027_xbbtl_2",    # 晶玲兽
    "enemy_7028_xbscp_2",    # 晶珑兽
)

ENEMY_DB = paths.GAMEDATA / "levels" / "enemydata" / "enemy_database.json"
HANDBOOK = paths.GAMEDATA / "excel" / "enemy_handbook_table.json"

# ─── 枚举映射 ────────────────────────────────────────────────
# IMMUNE_MAP 是「免疫字段 → 中文状态」的唯一事实来源：游戏新增异常状态时只改这里。
# 顺序 = 游戏 attributes 里的字段顺序（追加新状态就追加在末尾，已有条目顺序不变，
# 避免校对把历史行整列判成差异）。
# db_builder/build_enemies.py 里那份是构建期快照，已归档、不再随游戏更新。

STATUS_MAP = {"NORMAL": "普通", "ELITE": "精英", "BOSS": "领袖"}
ATK_MAP = {"MELEE": "近战", "RANGED": "远程", "NONE": "不攻击"}
MOTION_MAP = {"WALK": "地面", "FLY": "飞行"}
DAMAGE_MAP = {"PHYSIC": "物理", "MAGIC": "法术", "HEAL": "治疗", "NO_DAMAGE": "无"}

IMMUNE_MAP = {
    "stunImmune": "晕眩", "silenceImmune": "沉默",
    "sleepImmune": "沉睡", "frozenImmune": "冻结",
    "levitateImmune": "浮空", "disarmedCombatImmune": "战栗",
    "fearedImmune": "恐惧", "palsyImmune": "麻痹",
    "attractImmune": "诱导",
    "teleportImmune": "传送", "groundBoundImmune": "缚地",
}

# ─── 列分类（比较方式不同）──────────────────────────────────────

# 不参与校对：PRTS 爬取列 + 人工补充列
SKIP_COLS = {"天赋文本", "技能文本", "备注"}

JSON_COLS = {"能力列表", "种类", "关联敌人", "伤害类型", "异常抗性"}
FLOAT_COLS = {"攻击半径", "法术抗性", "移动速度", "攻击速度", "攻击间隔",
              "生命自回速度", "损伤抗性", "元素抗性", "viewRadius"}
INT_COLS = {"级别", "目标价值", "不计入敌人总数", "numOfExtraDrops", "最大生命值",
            "攻击力", "防御力", "重量", "基础嘲讽等级"}

# INSERT 列顺序（与实际库 schema 一致，含 `备注`；不含已废弃的 `模式标记`）
ALL_COLS = [
    "敌人ID", "名称", "级别", "描述", "地位", "能力列表", "种类", "关联敌人",
    "攻击方式", "伤害类型", "行动方式", "攻击半径", "目标价值", "不计入敌人总数",
    "numOfExtraDrops", "viewRadius", "最大生命值", "攻击力", "防御力", "法术抗性",
    "移动速度", "攻击速度", "攻击间隔", "生命自回速度", "重量", "基础嘲讽等级",
    "损伤抗性", "元素抗性", "异常抗性", "天赋文本", "技能文本", "备注",
]


# ─── 数据源读取 ────────────────────────────────────────────────

def load_source():
    with open(ENEMY_DB, encoding="utf-8") as f:
        database = json.load(f)
    with open(HANDBOOK, encoding="utf-8") as f:
        handbook = json.load(f)
    return database, handbook


# ─── 取值辅助（与 db_builder/build_enemies.py 同一套语义）─────────

def mv(obj, default=None):
    """解包 m_defined/m_value，始终返回 m_value"""
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
    """取 enemyData 直接字段，level>0 时 m_defined=false 从 lv0 继承"""
    obj = ed.get(key)
    if not is_defined(obj) and lv0_ed:
        return mv(lv0_ed.get(key), default)
    return mv(obj, default)


def aval(ed, lv0_ed, key, default=None):
    """取 attributes 子字段，继承规则同 val"""
    attrs = ed.get("attributes", {})
    obj = attrs.get(key)
    if not is_defined(obj) and lv0_ed:
        return mv(lv0_ed.get("attributes", {}).get(key), default)
    return mv(obj, default)


def pick_hb_entry(hb_data, enemy_id, name):
    """该敌人的图鉴条目；HB_FALLBACK_IDS 里的变体 ID 回退到本体条目。

    图鉴按敌人 ID 建条目，同一实体的变体 ID（如 enemy_7027_xbbtl_2）常常不在其中，
    此时去掉尾部 `_N` 逐级回退。**回退候选的 name 必须与本行的名称一致**才算同一敌人：
    例如 enemy_1542_wdslm_2 去尾缀得到 enemy_1542_wdslm，而那一条是「“变形者集群”」，
    不是同构碎片，不能拿它的字段。

    只对 HB_FALLBACK_IDS 里列出的 ID 回退（Air 定：范围就是清单点名的这批，
    不做成全表通用规则）；其余 ID 找不到条目就返回 None，字段如实留空。
    """
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


# ─── 重算全部目标行 ────────────────────────────────────────────

def compute_rows():
    """按与构建脚本一致的规则，从本地数据源算出全部目标行。

    返回 {(敌人ID, 级别): {列名: 值}}。
    """
    database, handbook = load_source()
    race_map = {k: v.get("raceName") for k, v in handbook.get("raceData", {}).items()}
    hb_data = handbook.get("enemyData", {})

    # 名称映射（level 0 的清洗后名称），供 `关联敌人` 转中文名
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

        # 图鉴条目：变体 ID 回退到本体（名称需一致），见 pick_hb_entry
        hb_entry = pick_hb_entry(hb_data, eid, name_map.get(eid.lower())) or {}

        # 关联敌人：同一敌人所有 level 行相同（与 build_enemies_link.py 的 UPDATE 效果一致）
        link_ids = hb_entry.get("linkEnemies", [])
        links = json.dumps(
            [name_map.get(lid.lower()) or lid for lid in link_ids], ensure_ascii=False
        ) if link_ids else None

        for variant in entry["Value"]:
            level = variant["level"]
            ed = variant["enemyData"]

            name = (clean_desc(val(ed, lv0_ed, "name")) or "").strip() or None
            desc = clean_desc(val(ed, lv0_ed, "description"))

            # 地位：优先游戏数据 levelType（未定义时 val 已回退 level 0），再回退图鉴
            raw_level = val(ed, lv0_ed, "levelType") or hb_entry.get("enemyLevel")
            status = STATUS_MAP.get(raw_level, raw_level)

            raw_abilities = hb_entry.get("abilityList", [])
            abilities = json.dumps(
                [clean_desc(a.get("text")) for a in raw_abilities], ensure_ascii=False
            ) if raw_abilities else None
            dmg_types = hb_entry.get("damageType", [])
            dmg_cn = json.dumps([DAMAGE_MAP.get(t, t) for t in dmg_types], ensure_ascii=False)

            tags = val(ed, lv0_ed, "enemyTags", [])
            tags_cn = json.dumps([race_map.get(t, t) for t in tags], ensure_ascii=False) if tags else None

            atk_raw = val(ed, lv0_ed, "applyWay")
            mot_raw = val(ed, lv0_ed, "motion")

            immunes = []
            for key, cn_name in IMMUNE_MAP.items():
                if aval(ed, lv0_ed, key) is True:
                    immunes.append(cn_name)
            immune_json = json.dumps(immunes, ensure_ascii=False) if immunes else None

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
                "异常抗性": immune_json,
                "天赋文本": None,   # 由爬取写入
                "技能文本": None,   # 由爬取写入
                "备注": None,       # 人工列，脚本不写
            }
    return rows


# ─── 比较 ────────────────────────────────────────────────────

def same(col, a, b):
    """按列类型比较库内值与重算值（列类型集合取本表的）。"""
    return uc.same(col, a, b, JSON_COLS, FLOAT_COLS, INT_COLS)


# ─── PRTS 爬取（抓取 / 模板解析在 update_common）───────────────

def extract_skills(params):
    """从模板参数中提取技能列表。"""
    skills = []
    for key in sorted(params.keys()):
        m = re.match(r'^技能(\d+)$', key)
        if not m:
            continue
        si = m.group(1)
        skills.append({
            "名称": clean_wiki(params.get(key, '')),
            "初始冷却": params.get("技能%s初始" % si, ''),
            "周期冷却": params.get("技能%s消耗" % si, ''),
            "技力消耗": params.get("技能%s技力" % si, '/'),
            "效果": clean_wiki(params.get("技能%s效果" % si, '')),
        })
    return skills


def crawl_target(eid, level, name):
    """该行该抓哪个页面、取页面里哪个 index 的 levelcontent 块。

    默认：页面名 = 名称、index = 级别；PAGE_MAP 里列了的用它写明的。
    """
    hit = PAGE_MAP.get((eid, level))
    if hit is not None:
        return hit
    return (name, level)


def level_index(lv):
    """levelcontent 块里的 index（缺失或非法一律当 0）。"""
    try:
        return int(str(lv.get("index", "0")).strip() or "0")
    except (TypeError, ValueError):
        return 0


def pick_level(levels, index):
    """从一页的 levelcontent 列表里取出 index 对应的块，并与 index=0 合并。

    PRTS 约定 index=0 那节写全参数，之后的每节只写与它不同的参数（缺的表示继承），
    所以要合并。该 index 不在页面上时返回 None，由调用方决定怎么处理。
    """
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


def text_from_level(block):
    """从一个 levelcontent 块生成 (天赋文本, 技能文本)。"""
    skills = extract_skills(block)
    return (
        clean_wiki(block.get("天赋", "")) or None,
        json.dumps(skills, ensure_ascii=False) if skills else None,
    )


def crawl_new(pages, sample_name):
    """爬取若干页面。

    返回 (结果字典, 404 无页面列表, 有页面但无结构列表)。
    真·事故向上抛异常（触发整体中止）：哨兵失败 / 单页重试后仍网络异常 /
    跳过数达阈值时哨兵复核失败 / 无结构占比超过比例上限。
    注意：不能用「连续多少个无结构」当信号——爬取名单按 ID 排序，同类对象（如
    19xxx 段的奇象生物）天然相邻，连续失败只说明“这一批是同一类”。
    个别页面「有页面但无 {{敌人信息/levelcontent}}」属可预见情况——奇象生物等特殊敌人
    在 PRTS 只建了装置页、本无技能与天赋——跳过并记录，不计失败。
    """
    if not pages:
        return {}, [], [], []

    ok, why = uc.sentinel_ok(sample_name, SENTINEL_MARKER)
    if not ok:
        raise RuntimeError(why)

    results = {}
    no_page = []
    no_detail = []
    redirects = []
    skipped = 0
    total = len(pages)
    start = time.time()
    for i, page in enumerate(pages):
        if i % 10 == 0:
            elapsed = time.time() - start
            rate = (i / elapsed) if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print("  [%d/%d] %.0fs | ETA %.0fs | %s" % (i, total, elapsed, eta, page))

        # 抓取与重试在 update_common：404 → NoPage（算「无页面」），
        # 网络类异常重试后仍失败会抛 RuntimeError → 整体中止
        try:
            wikitext = uc.fetch_wikitext(page, redirects)
        except uc.NoPage:
            wikitext = None

        levels = uc.parse_template(wikitext, "敌人信息/levelcontent") if wikitext else []
        if not levels:
            if wikitext is None:
                no_page.append(page)
            else:
                no_detail.append(page)
            skipped += 1
            print("      · 跳过（无详情结构）: %s" % page)
            # 每累计跳过若干，复核站点结构：哨兵通过 ⇒ 站点健康，是本批页面自身的情况
            if skipped % NO_DETAIL_RECHECK_EVERY == 0:
                ok2, why2 = uc.sentinel_ok(sample_name, SENTINEL_MARKER)
                if not ok2:
                    raise RuntimeError(
                        "站点结构复核失败（已跳过 %d 个）: %s" % (skipped, why2)
                    )
                print("      · 站点结构复核通过（已跳过 %d 个）" % skipped)
            # 比例兜底
            if skipped >= NO_DETAIL_MIN_TOTAL and skipped > (i + 1) * NO_DETAIL_RATIO_LIMIT:
                raise RuntimeError(
                    "无结构页面占比过高（%d/%d），疑似 PRTS 结构变化" % (skipped, i + 1)
                )
            time.sleep(SLEEP_BETWEEN_FETCH)
            continue
        results[page] = levels
        time.sleep(SLEEP_BETWEEN_FETCH)
    return results, no_page, no_detail, redirects


# ─── 只重抓：特殊页面映射里的那一批 ───────────────────────────

def recrawl_special(args):
    """把 PAGE_MAP 列出的敌人的 天赋文本/技能文本 重抓到页面里正确的那一节。

    只写这两列（不动 `备注` 与其它列），不跑校对与新增。
    页面 404 / 页面无 levelcontent / 映射的 index 在页面上找不到 → 该行不写，计入记录。
    """
    started = datetime.now()
    print("====== enemies 表 · 特殊页面重抓 ======")
    print("数据库  : %s" % args.db)
    print("数据源  : %s" % paths.DATA_SOURCE)
    src_ver = uc.read_source_version(paths.DATA_SOURCE)
    print("源版本  : 提交 %s | 游戏版本 %s | 数据时间 %s"
          % (src_ver["提交"], src_ver["游戏版本"], src_ver["数据时间"]))

    record = {
        "表": TABLE,
        "模式": "特殊页面重抓",
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {
            "仓库": "Kengxxiao/ArknightsGameData",
            "提交": src_ver["提交"],
            "提交标题": src_ver["提交标题"],
            "游戏版本": src_ver["游戏版本"],
            "数据时间": src_ver["数据时间"],
        },
        "脚本": {"文件": "update_enemies.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "映射": {"键数": len(PAGE_MAP), "命中行": 0, "未命中": []},
        "爬取": {"页面": 0, "成功": 0, "无页面": 0, "无详情结构": 0,
                 "重定向": [], "无页面明细": [], "无详情明细": []},
        "写入": {"改写": 0, "一致": 0, "index缺失": [], "明细": []},
        "补齐": {"待补行": 0, "明细": []},
        "结果": "未执行",
    }

    print("\n------ 目标 ------")
    ids = {eid for (eid, _lv) in PAGE_MAP}
    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        cur = conn.execute(
            "SELECT 敌人ID, 级别, 名称, 天赋文本, 技能文本, 能力列表, 关联敌人, 伤害类型"
            " FROM %s" % TABLE)
        have = {}
        hb_now = {}
        for eid, level, name, tianfu, jinneng, ab, link, dmg in cur.fetchall():
            if eid in ids:
                have[(eid, level)] = {"名称": name, "天赋文本": tianfu, "技能文本": jinneng}
            if eid in HB_FALLBACK_IDS:
                hb_now[(eid, level)] = {"能力列表": ab, "关联敌人": link, "伤害类型": dmg}

        todo = []
        pages = []
        seen = set()
        for key in sorted(have):
            hit = PAGE_MAP.get(key)
            if hit is None:
                record["映射"]["未命中"].append("%s|%s（库里有这行，映射里没有）" % key)
                continue
            page, index = hit
            todo.append((key, page, index))
            if page not in seen:
                seen.add(page)
                pages.append(page)
        record["映射"]["命中行"] = len(todo)
        for key in PAGE_MAP:
            if key not in have:
                record["映射"]["未命中"].append("%s|%s（映射里有，库里没有这行）" % key)

        print("  映射键: %d | 库内命中行: %d | 待抓页面: %d" % (len(PAGE_MAP), len(todo), len(pages)))
        for page in pages:
            print("      %s" % page)
        if record["映射"]["未命中"]:
            print("  ! 未命中 %d 条：" % len(record["映射"]["未命中"]))
            for s in record["映射"]["未命中"]:
                print("      %s" % s)

        # 三字段补齐：变体 ID 无图鉴条目 → 与本体条目对齐（见 HB_FALLBACK_IDS）
        want_rows = compute_rows() if hb_now else {}
        hb_writes = []
        for key, old in sorted(hb_now.items()):
            w = want_rows.get(key)
            if w is None:
                continue
            three = (w["能力列表"], w["关联敌人"], w["伤害类型"])
            if three == (old["能力列表"], old["关联敌人"], old["伤害类型"]):
                continue
            hb_writes.append((key[0], key[1]) + three)
        print("  三字段待补行: %d" % len(hb_writes))
        for eid, level, ab, link, dmg in hb_writes[:MAX_DETAIL]:
            print("      %s|%s  能力列表=%s 关联敌人=%s 伤害类型=%s"
                  % (eid, level, "有" if ab else "空", "有" if link else "空",
                     "有" if dmg else "空"))
        record["补齐"]["待补行"] = len(hb_writes)
        record["补齐"]["明细"] = ["%s|%s" % (w[0], w[1]) for w in hb_writes[:MAX_DETAIL]]

        if not pages and not hb_writes:
            print("\n没有可抓的页面、也没有要补的行，结束。")
            return 0

        if args.dry_run:
            print("\n(干跑：未抓取、未写库、未写记录)")
            record["结果"] = "干跑"
            return 0

        crawl_results, no_page, no_detail, redirects = {}, [], [], []
        if pages:
            print("\n------ 抓取 PRTS（%d 个页面）------" % len(pages))
            try:
                crawl_results, no_page, no_detail, redirects = crawl_new(pages, pages[0])
            except Exception as e:
                record["结果"] = "中止（%s）" % e
                path = uc.write_record(TABLE + "_recrawl", record)
                print("\n!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
        record["爬取"]["页面"] = len(pages)
        record["爬取"]["成功"] = len(crawl_results)
        record["爬取"]["无页面"] = len(no_page)
        record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
        record["爬取"]["无详情结构"] = len(no_detail)
        record["爬取"]["无详情明细"] = no_detail[:MAX_DETAIL]
        record["爬取"]["重定向"] = ["%s → %s" % (a, b) for a, b in redirects[:MAX_DETAIL]]

        print("\n------ 解析与对比 ------")
        writes = []
        for key, page, index in todo:
            eid, level = key
            levels = crawl_results.get(page)
            if not levels:
                continue
            block = pick_level(levels, index)
            if block is None:
                record["写入"]["index缺失"].append(
                    "%s|%s → %s index=%s" % (eid, level, page, index))
                continue
            tianfu, jinneng = text_from_level(block)
            old = have[key]
            if (tianfu, jinneng) == (old["天赋文本"], old["技能文本"]):
                record["写入"]["一致"] += 1
                continue
            writes.append((eid, level, tianfu, jinneng, page, index))
            record["写入"]["明细"].append({
                "键": "%s|%s" % (eid, level), "页面": page, "index": index,
                "天赋": ("%d 字" % len(tianfu)) if tianfu else "空",
                "技能": ("%d 字" % len(jinneng)) if jinneng else "空",
            })
        record["写入"]["改写"] = len(writes)
        print("  库里已一致: %d | 需改写: %d | index 缺失: %d"
              % (record["写入"]["一致"], len(writes), len(record["写入"]["index缺失"])))
        for eid, level, tianfu, jinneng, page, index in writes[:MAX_DETAIL]:
            print("      %s|%s ← %s index=%s  天赋=%s 技能=%s"
                  % (eid, level, page, index,
                     ("%d字" % len(tianfu)) if tianfu else "空",
                     ("%d字" % len(jinneng)) if jinneng else "空"))
        for s in record["写入"]["index缺失"][:MAX_DETAIL]:
            print("    ! index 缺失，跳过：%s" % s)

        if not writes and not hb_writes:
            print("\n没有需要改写的行，未写库。")
            record["结果"] = "成功（无改动）"
            path = uc.write_record(TABLE + "_recrawl", record)
            print("  更新记录: %s" % path)
            return 0

        print("\n------ 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            print("  备份: %s" % (uc.backup_db(args.db) or "（未生成）"))
        conn.execute("BEGIN IMMEDIATE")
        try:
            for eid, level, tianfu, jinneng, _page, _index in writes:
                conn.execute(
                    "UPDATE %s SET 天赋文本=?, 技能文本=? WHERE 敌人ID=? AND 级别=?" % TABLE,
                    (tianfu, jinneng, eid, level),
                )
            for eid, level, ab, link, dmg in hb_writes:
                conn.execute(
                    "UPDATE %s SET 能力列表=?, 关联敌人=?, 伤害类型=? WHERE 敌人ID=? AND 级别=?"
                    % TABLE,
                    (ab, link, dmg, eid, level),
                )
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            path = uc.write_record(TABLE + "_recrawl", record)
            print("\n!! 写库失败并已回滚：%s" % e)
            print("   记录：%s" % path)
            return 1
    finally:
        conn.close()

    record["结果"] = "成功"
    path = uc.write_record(TABLE + "_recrawl", record)
    elapsed = (datetime.now() - started).total_seconds()
    print("\n====== 完成（%.1fs）======" % elapsed)
    print("  改写: %d 行 | 已一致: %d | index 缺失: %d | 无页面: %d | 无详情: %d | 三字段补: %d"
          % (record["写入"]["改写"], record["写入"]["一致"],
             len(record["写入"]["index缺失"]), record["爬取"]["无页面"],
             record["爬取"]["无详情结构"], record["补齐"]["待补行"]))
    print("  更新记录: %s" % path)
    return 0


# ─── 主流程 ──────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description="enemies 表更新（校对 + 新增 + 记录）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不爬取、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--recrawl-special", action="store_true",
                    help="只重抓 PAGE_MAP 里的敌人：按映射重写其 天赋文本/技能文本，不做校对与新增")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    paths.ensure_gamedata()
    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    if args.recrawl_special:
        return recrawl_special(args)

    started = datetime.now()
    print("====== enemies 表更新 ======")
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
            "提交": src_ver["提交"],
            "提交标题": src_ver["提交标题"],
            "游戏版本": src_ver["游戏版本"],
            "数据时间": src_ver["数据时间"],
        },
        "脚本": {"文件": "update_enemies.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "校对": {"检查行": 0, "豁免行": 0, "差异行": 0, "差异列数": 0, "明细": [], "计数": {}},
        "新增": {"敌人": 0, "行": 0, "豁免爬取": 0, "明细": []},
        "爬取": {"目标敌人": 0, "成功": 0, "无页面": 0, "无详情结构": 0,
                 "重定向": [], "无页面明细": [], "无详情明细": []},
        "结果": "未执行",
    }

    print("\n------ 重算目标行 ------")
    rows = compute_rows()
    print("  目标行数: %d" % len(rows))

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None  # 手动管理事务
    try:
        db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        cur = conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), TABLE))
        existing = {}
        for r in cur.fetchall():
            d = dict(zip(db_cols, r))
            existing[(d["敌人ID"], d["级别"])] = d

        # ── 1. 校对 ──
        diffs = []
        per_col = {}
        excluded_rows = 0
        for n, (key, want) in enumerate(rows.items(), 1):
            if n % 500 == 0 or n == len(rows):
                print("  比对 %d/%d" % (n, len(rows)))
            have = existing.get(key)
            if have is None:
                continue
            if key[0].startswith(EXCLUDE_FROM_CHECK_AND_CRAWL):
                excluded_rows += 1
                continue
            for col in db_cols:
                if col in SKIP_COLS:
                    continue
                if not same(col, have[col], want[col]):
                    diffs.append((key, col, have[col], want[col]))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"]["检查行"] = len(existing)
        record["校对"]["豁免行"] = excluded_rows
        record["校对"]["差异行"] = len({(k[0], k[1]) for k, _, _, _ in diffs})
        record["校对"]["差异列数"] = len(diffs)
        record["校对"]["计数"] = per_col
        record["校对"]["明细"] = [
            dict({"键": "%s|%s" % (k[0], k[1]), "列": col},
                 **uc.diff_brief(old, new, as_json=col in JSON_COLS))
            for k, col, old, new in diffs[:MAX_DETAIL]
        ]

        print("\n------ 1. 校对结果 ------")
        print("  库内行数: %d | 豁免: %d | 差异行: %d | 差异列: %d"
              % (len(existing), excluded_rows, record["校对"]["差异行"], len(diffs)))
        for col, n in sorted(per_col.items(), key=lambda x: -x[1]):
            print("    %s: %d 处" % (col, n))

        # ── 2. 新增 ──
        new_keys = sorted(k for k in rows if k not in existing)
        new_ids = sorted({k[0] for k in new_keys})
        record["新增"]["行"] = len(new_keys)
        record["新增"]["敌人"] = len(new_ids)
        record["新增"]["明细"] = [
            {"键": "%s|%s" % (k[0], k[1]), "名称": rows[k]["名称"]} for k in new_keys[:MAX_DETAIL]
        ]
        print("\n------ 2. 新增 ------")
        print("  新增敌人: %d | 新增行: %d" % (len(new_ids), len(new_keys)))
        for k in new_keys[:20]:
            print("    %s|%s  %s" % (k[0], k[1], rows[k]["名称"] or ""))

        if args.dry_run:
            print("\n(干跑：未写库、未爬取、未写记录)")
            record["结果"] = "干跑"
            return 0

        # ── 3. 爬取（先爬后写；失败则整体中止）──
        crawl_results, no_page, no_detail, redirects = {}, [], [], []
        if new_ids:
            # 每个新敌人按 (页面名, index) 定位：默认页面名 = 名称、index = 级别，
            # PAGE_MAP 里列了的用映射写明的。按页面名去重（多个 ID 可能共用一页）。
            pages = []
            page_of = {}
            seen = set()
            skipped_excluded = 0
            for eid in new_ids:
                if eid.startswith(EXCLUDE_FROM_CHECK_AND_CRAWL):
                    skipped_excluded += 1
                    continue
                for key in new_keys:
                    if key[0] != eid:
                        continue
                    page, index = crawl_target(eid, key[1], rows[key]["名称"])
                    page_of[key] = (page, index)
                    if page and page not in seen:
                        seen.add(page)
                        pages.append(page)
            record["新增"]["豁免爬取"] = skipped_excluded
            record["爬取"]["目标敌人"] = len(pages)
            if skipped_excluded:
                print("  豁免爬取（%s 段）: %d 个敌人"
                      % (", ".join(EXCLUDE_FROM_CHECK_AND_CRAWL), skipped_excluded))
            print("\n------ 3. 爬取 PRTS（%d 个页面）------" % len(pages))
            sample = pages[0] if pages else None
            try:
                crawl_results, no_page, no_detail, redirects = crawl_new(pages, sample)
            except Exception as e:
                record["结果"] = "中止（%s）" % e
                path = uc.write_record(TABLE, record)
                print("\n!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
            record["爬取"]["成功"] = len(crawl_results)
            record["爬取"]["无页面"] = len(no_page)
            record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
            record["爬取"]["无详情结构"] = len(no_detail)
            record["爬取"]["无详情明细"] = no_detail[:MAX_DETAIL]
            record["爬取"]["重定向"] = [
                "%s → %s" % (a, b) for a, b in redirects[:MAX_DETAIL]
            ]
            if redirects:
                print("  重定向 %d 次（已记入更新记录）：" % len(redirects))
                for a, b in redirects[:5]:
                    print("      %s → %s" % (a, b))

        # ── 写库（单事务）──
        print("\n------ 4. 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = uc.backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        print("  写回校对差异: %d 列 | 插入新增: %d 行" % (len(diffs), len(new_keys)))
        conn.execute("BEGIN IMMEDIATE")
        try:
            for (eid, level), col, old, new in diffs:
                conn.execute(
                    "UPDATE %s SET %s=? WHERE 敌人ID=? AND 级别=?" % (TABLE, col),
                    (new, eid, level),
                )

            for (eid, level) in new_keys:
                want = dict(rows[(eid, level)])
                page, index = page_of.get((eid, level), (None, None))
                levels = crawl_results.get(page) if page else None
                if levels:
                    # 取映射指定的那一节；该 index 不在页面上时退回 index 0
                    block = pick_level(levels, index) or pick_level(levels, 0)
                    if block is not None:
                        want["天赋文本"], want["技能文本"] = text_from_level(block)

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
    print("  校对写回: %d 列 | 新增: %d 行（%d 个敌人）| 爬取成功: %d | 无页面: %d | 无详情结构: %d"
          % (record["校对"]["差异列数"], record["新增"]["行"], record["新增"]["敌人"],
             record["爬取"]["成功"], record["爬取"]["无页面"], record["爬取"]["无详情结构"]))
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
