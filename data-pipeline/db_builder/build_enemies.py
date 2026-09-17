"""
PRTS 终端 — enemies 表入库
第一版：直接来源字段（JSON 取值即入库）
"""

import json, sqlite3, os
from text_cleaner import clean_desc

# ─── 路径配置 ──────────────────────────────

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(OUTPUT_DIR, "arknights.db")

# ─── 枚举映射 ──────────────────────────────

STATUS_MAP  = {"NORMAL": "普通", "ELITE": "精英", "BOSS": "领袖"}
ATK_MAP     = {"MELEE": "近战", "RANGED": "远程", "NONE": "不攻击"}
MOTION_MAP  = {"WALK": "地面", "FLY": "飞行"}
DAMAGE_MAP  = {"PHYSIC": "物理", "MAGIC": "法术", "HEAL": "治疗", "NO_DAMAGE": "无"}

IMMUNE_MAP = {
    "stunImmune": "晕眩", "silenceImmune": "沉默",
    "sleepImmune": "沉睡", "frozenImmune": "冻结",
    "levitateImmune": "浮空", "disarmedCombatImmune": "战栗",
    "fearedImmune": "恐惧", "palsyImmune": "麻痹",
    "attractImmune": "诱导",
}

def load_json(path):
    with open(os.path.join(DATA_ROOT, path), "r", encoding="utf-8") as f:
        return json.load(f)

print("====== 读取 JSON ======")
database = load_json("levels/enemydata/enemy_database.json")
handbook = load_json("excel/enemy_handbook_table.json")

race_map = {k: v["raceName"] for k, v in handbook.get("raceData", {}).items()}
hb_data = handbook.get("enemyData", {})

# ─── 建库 ─────────────────────────────────

conn = sqlite3.connect(OUTPUT_DB)
conn.execute("PRAGMA journal_mode=OFF")
conn.execute("PRAGMA synchronous=OFF")
conn.execute("DROP TABLE IF EXISTS enemies")

conn.execute("""
CREATE TABLE enemies (
    敌人ID      TEXT NOT NULL,
    名称        TEXT,
    级别        INTEGER NOT NULL,
    描述        TEXT,
    地位        TEXT,
    能力列表    TEXT,
    种类        TEXT,
    关联敌人    TEXT,
    攻击方式    TEXT,
    伤害类型    TEXT,
    行动方式    TEXT,
    攻击半径    REAL,
    目标价值    INTEGER,
    不计入敌人总数 INTEGER,
    numOfExtraDrops INTEGER,
    viewRadius  REAL,
    最大生命值  INTEGER,
    攻击力      INTEGER,
    防御力      INTEGER,
    法术抗性    REAL,
    移动速度    REAL,
    攻击速度    REAL,
    攻击间隔    REAL,
    生命自回速度 REAL,
    重量        INTEGER,
    基础嘲讽等级 INTEGER,
    损伤抗性    REAL,
    元素抗性    REAL,
    异常抗性    TEXT,
    模式标记    TEXT,
    天赋文本    TEXT,
    技能文本    TEXT,
    PRIMARY KEY (敌人ID, 级别)
)
""")

# ─── 辅助函数 ──────────────────────────────

def mv(obj, default=None):
    """解包 m_defined/m_value，始终返回 m_value"""
    if obj is None:
        return default
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_value"]
    return obj

def is_defined(obj):
    """检查是否显式定义"""
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

def pick_hb(enemy_id, field, default=None):
    entry = hb_data.get(enemy_id.lower(), {})
    return entry.get(field, default)

# ─── 写入数据 ──────────────────────────────

print("====== 写入 enemies ======")
inserted = 0

for entry in database["enemies"]:
    enemy_id = entry["Key"]

    # 提取 level 0 的基准数据供继承
    lv0_ed = None
    for v0 in entry["Value"]:
        if v0["level"] == 0:
            lv0_ed = v0["enemyData"]
            break

    for variant in entry["Value"]:
        level = variant["level"]
        ed = variant["enemyData"]

        # 名称和描述（继承 level 0 后清洗标记，strip 空白）
        name = (clean_desc(val(ed, lv0_ed, "name")) or "").strip() or None
        desc = clean_desc(val(ed, lv0_ed, "description"))

        # 地位优先取实际游戏数据（enemy_database.levelType），仅无定义时回退 handbook
        raw_level = val(ed, lv0_ed, "levelType") or pick_hb(enemy_id, "enemyLevel")
        status = STATUS_MAP.get(raw_level, raw_level)
        # 能力列表：只取 text 文本并清洗标记
        raw_abilities = pick_hb(enemy_id, "abilityList", [])
        abilities = json.dumps(
            [clean_desc(a["text"]) for a in raw_abilities],
            ensure_ascii=False
        ) if raw_abilities else None
        dmg_types = pick_hb(enemy_id, "damageType", [])
        dmg_cn = json.dumps([DAMAGE_MAP.get(t, t) for t in dmg_types], ensure_ascii=False)

        # 种类
        tags = val(ed, lv0_ed, "enemyTags", [])
        tags_cn = json.dumps([race_map.get(t, t) for t in tags], ensure_ascii=False) if tags else None

        # 攻击/行动
        atk_raw = val(ed, lv0_ed, "applyWay")
        atk = ATK_MAP.get(atk_raw, atk_raw)
        mot_raw = val(ed, lv0_ed, "motion")
        motion = MOTION_MAP.get(mot_raw, mot_raw)

        # 属性（继承 level 0）
        hp      = aval(ed, lv0_ed, "maxHp")
        atk_val = aval(ed, lv0_ed, "atk")
        def_val = aval(ed, lv0_ed, "def")
        res     = aval(ed, lv0_ed, "magicResistance")
        ms      = aval(ed, lv0_ed, "moveSpeed")
        aspd    = aval(ed, lv0_ed, "attackSpeed")
        bat     = aval(ed, lv0_ed, "baseAttackTime")
        hpr     = aval(ed, lv0_ed, "hpRecoveryPerSec")
        mass    = aval(ed, lv0_ed, "massLevel")
        taunt   = aval(ed, lv0_ed, "tauntLevel")
        ep_dmg  = aval(ed, lv0_ed, "epDamageResistance")
        ep_res  = aval(ed, lv0_ed, "epResistance")

        # 免疫
        immunes = []
        for key, cn_name in IMMUNE_MAP.items():
            if aval(ed, lv0_ed, key) is True:
                immunes.append(cn_name)
        immune_json = json.dumps(immunes, ensure_ascii=False) if immunes else None

        # INSERT 按 CREATE TABLE 列顺序：敌人ID, 名称, 级别, 描述, ...
        conn.execute("""
            INSERT OR IGNORE INTO enemies VALUES (
                ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?, ?,
                ?, ?, ?
            )
        """, (
            enemy_id, name, level, desc, status, abilities, tags_cn, None, atk, dmg_cn,
            motion,
            val(ed, lv0_ed, "rangeRadius"),
            val(ed, lv0_ed, "lifePointReduce"),
            1 if val(ed, lv0_ed, "notCountInTotal") else 0,
            val(ed, lv0_ed, "numOfExtraDrops"),
            val(ed, lv0_ed, "viewRadius"),
            hp, atk_val, def_val, res, ms, aspd, bat, hpr,
            mass, taunt, ep_dmg, ep_res, immune_json,
            None, None, None,
        ))
        inserted += 1

conn.commit()

# ─── 验证 ────────────────────────────────

print(f"====== 完成 ======")
print(f"  写入: {inserted} 行")
print(f"  敌人种类: {len(database['enemies'])}")

conn.close()
print(f"  数据库: {OUTPUT_DB}")
