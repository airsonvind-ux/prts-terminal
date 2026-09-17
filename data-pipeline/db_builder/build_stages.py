"""
PRTS 终端 — stages 表入库
来源：stage_table.json + levels/obt/**/*.json + levels/activities/**/*.json
"""

import json, sqlite3, os
from text_cleaner import clean_desc

# ─── 路径配置 ──────────────────────────────

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(OUTPUT_DIR, "arknights.db")

# ─── 枚举映射 ──────────────────────────────

STAGE_TYPE_MAP = {
    "MAIN":           "主线",
    "ACTIVITY":       "活动",
    "SUB":            "支线",
    "DAILY":          "日常",
    "CAMPAIGN":       "剿灭",
    "GUIDE":          "教学",
    "CLIMB_TOWER":    "保全派驻",
    "SPECIAL_STORY":  "特殊故事",
}

# overwrittenData 属性名 → 中文
ATTR_CN = {
    "maxHp":              "最大生命值",
    "atk":                "攻击力",
    "def":                "防御力",
    "magicResistance":    "法术抗性",
    "moveSpeed":          "移动速度",
    "attackSpeed":        "攻击速度",
    "baseAttackTime":     "攻击间隔",
    "hpRecoveryPerSec":   "生命自回速度",
    "spRecoveryPerSec":   "SP自回速度",
    "massLevel":          "重量",
    "tauntLevel":         "基础嘲讽等级",
    "epDamageResistance": "损伤抗性",
    "epResistance":       "元素抗性",
    "stunImmune":         "晕眩抗性",
    "silenceImmune":      "沉默抗性",
    "sleepImmune":        "沉睡抗性",
    "frozenImmune":       "冻结抗性",
    "levitateImmune":     "浮空抗性",
    "disarmedCombatImmune": "缴械抗性",
    "fearedImmune":       "恐惧抗性",
    "palsyImmune":        "麻痹抗性",
    "attractImmune":      "诱导抗性",
    "name":               "名称",
    "description":        "描述",
    "prefabKey":          "模型",
    "applyWay":           "攻击方式",
    "motion":             "行动方式",
    "enemyTags":          "种类标签",
    "lifePointReduce":    "目标价值",
    "levelType":          "地位",
    "rangeRadius":        "攻击半径",
    "numOfExtraDrops":    "额外掉落数",
    "notCountInTotal":    "不计入总数",
    "viewRadius":         "视野半径",
    "talentBlackboard":   "天赋覆写",
    "skills":             "技能覆写",
}

# ─── 辅助函数 ──────────────────────────────

def load_json(path):
    with open(os.path.join(DATA_ROOT, path), "r", encoding="utf-8") as f:
        return json.load(f)


def clean_level_id(raw):
    """Obt/Main/level_main_01-07 → level_main_01-07"""
    if raw is None:
        return None
    return raw.split("/")[-1]


def clean_unlock_condition(raw):
    """只保留 stageId + completeState"""
    if not raw:
        return "[]"
    return json.dumps([
        {"stageId": c.get("stageId", ""), "completeState": c.get("completeState", "")}
        for c in raw
    ], ensure_ascii=False)


def load_level_cache():
    """预加载 obt/ + activities/ 全部 level JSON"""
    cache = {}

    obt_root = os.path.join(DATA_ROOT, "levels", "obt")
    count_obt = 0
    for root, dirs, files in os.walk(obt_root):
        for fname in files:
            if fname.endswith(".json"):
                key = fname.replace(".json", "")
                with open(os.path.join(root, fname), "r", encoding="utf-8") as f:
                    cache[key] = json.load(f)
                    count_obt += 1

    act_root = os.path.join(DATA_ROOT, "levels", "activities")
    count_act = 0
    if os.path.isdir(act_root):
        for root, dirs, files in os.walk(act_root):
            for fname in files:
                if fname.endswith(".json"):
                    key = fname.replace(".json", "")
                    with open(os.path.join(root, fname), "r", encoding="utf-8") as f:
                        cache[key] = json.load(f)
                        count_act += 1

    print(f"  obt/: {count_obt}   activities/: {count_act}   合计: {count_obt + count_act}")
    return cache


def count_spawn_targets(waves):
    """waves[] → fragments[] → actions[], 统计 actionType=SPAWN 的 count"""
    total = 0
    if not waves:
        return total
    for wave in waves:
        for frag in wave.get("fragments", []):
            for act in frag.get("actions", []):
                if act.get("actionType") == "SPAWN":
                    total += act.get("count", 0)
    return total


def extract_overrides(od):
    """递归提取 overwrittenData 中 m_defined=true 的叶节点，键名转中文"""
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
            cn_key = ATTR_CN.get(key, key)
            if "__leaf__" in sub:
                result[cn_key] = sub["__leaf__"]
            else:
                for sk, sv in sub.items():
                    if sk != "__leaf__":
                        result[sk] = sv
        elif isinstance(val, list) and len(val) > 0:
            # 天赋/技能的 blackboard 覆写，直接保留原始结构
            cn_key = ATTR_CN.get(key, key)
            result[cn_key] = val
    return result


def build_enemy_list(enemy_db_refs, name_map):
    """enemyDbRefs → [{名称, 级别, 覆写属性?}]"""
    if not enemy_db_refs:
        return "[]"
    out = []
    for ref in enemy_db_refs:
        eid = ref["id"]
        item = {
            "名称": name_map.get(eid, eid),
            "级别": ref.get("level", 0),
        }
        overrides = extract_overrides(ref.get("overwrittenData"))
        if overrides:
            item["覆写属性"] = overrides
        out.append(item)
    return json.dumps(out, ensure_ascii=False)


# ─── 主流程 ─────────────────────────────────

print("====== 读取 stage_table.json ======")
stage_table = load_json("excel/stage_table.json")
stages = stage_table["stages"]
print(f"关卡条目: {len(stages)}")

print("====== 预加载 level JSON ======")
level_cache = load_level_cache()

print("====== 构建敌人名称映射 ======")
handbook = load_json("excel/enemy_handbook_table.json")
enemy_name_map = {eid: data.get("name", eid) for eid, data in handbook["enemyData"].items()}
print(f"映射条目: {len(enemy_name_map)}")

print("====== 建库 ======")
conn = sqlite3.connect(OUTPUT_DB)
conn.execute("PRAGMA journal_mode=OFF")
conn.execute("PRAGMA synchronous=OFF")
conn.execute("DROP TABLE IF EXISTS stages")

conn.execute("""
CREATE TABLE stages (
    关卡ID        TEXT NOT NULL,
    关卡名        TEXT,
    关卡描述      TEXT,
    关卡编号      TEXT,
    关卡类型      TEXT,
    解锁条件      TEXT,
    推荐等级      TEXT,
    理智消耗      INTEGER,
    levelId       TEXT,
    部署上限      INTEGER,
    初始费用      INTEGER,
    费用上限      INTEGER,
    目标点耐久    INTEGER,
    待处理目标数量 INTEGER,
    敌人列表      TEXT,
    情报          TEXT,
    PRIMARY KEY (关卡ID)
)
""")

print("====== 写入 stages ======")
inserted = 0
no_level = 0

for sid, s in stages.items():
    lid_clean = clean_level_id(s.get("levelId"))

    lv = level_cache.get(lid_clean) if lid_clean else None
    if lid_clean and lv is None:
        no_level += 1

    conn.execute("""
        INSERT INTO stages (
            关卡ID, 关卡名, 关卡描述, 关卡编号, 关卡类型,
            解锁条件, 推荐等级, 理智消耗, levelId,
            部署上限, 初始费用, 费用上限, 目标点耐久,
            待处理目标数量, 敌人列表, 情报
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    """, (
        sid,
        s.get("name"),
        clean_desc(s.get("description")),
        s.get("code"),
        STAGE_TYPE_MAP.get(s.get("stageType"), s.get("stageType")),
        clean_unlock_condition(s.get("unlockCondition")),
        s.get("dangerLevel"),
        s.get("apCost"),
        lid_clean,
        lv["options"].get("characterLimit") if lv else None,
        lv["options"].get("initialCost")    if lv else None,
        lv["options"].get("maxCost")         if lv else None,
        lv["options"].get("maxLifePoint")    if lv else None,
        count_spawn_targets(lv.get("waves")) if lv else None,
        build_enemy_list(lv.get("enemyDbRefs"), enemy_name_map) if lv else None,
        None,
    ))
    inserted += 1

conn.commit()

# ─── 统计 ───
rows = conn.execute("""
    SELECT 关卡类型,
           COUNT(*)                                AS 总数,
           SUM(CASE WHEN 部署上限 IS NOT NULL THEN 1 ELSE 0 END) AS 有数据,
           SUM(CASE WHEN levelId IS NULL   THEN 1 ELSE 0 END) AS 无levelId,
           SUM(CASE WHEN levelId IS NOT NULL AND 部署上限 IS NULL THEN 1 ELSE 0 END) AS JSON缺
    FROM stages
    GROUP BY 关卡类型
    ORDER BY 总数 DESC
""").fetchall()

print(f"\n====== 写入完成 ======")
print(f"总计: {inserted} 条,  level JSON 缺失: {no_level} 条")
print(f"\n{'类型':6s}  {'总数':>5s}  {'有数据':>5s}  {'无levelId':>7s}  {'JSON缺':>5s}")
for r in rows:
    print(f"  {r[0]:4s}  {r[1]:5d}  {r[2]:5d}  {r[3]:7d}  {r[4]:5d}")

conn.close()
print(f"\n数据库: {OUTPUT_DB}")
