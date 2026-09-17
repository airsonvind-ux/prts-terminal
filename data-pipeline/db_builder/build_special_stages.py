"""
PRTS 终端 — stages 表扩充：特殊关卡类型入库

将六种不在 stage_table.json 中的关卡类型写入 arknights.db 的 stages 表：
  1. 集成战略（肉鸽）   roguelike_topic_table.json
  2. 生息演算0/1/2     sandbox_table.json / sandbox_perm_table.json
  3. 全息作战矩阵        crisis_v2_table.json
  4. 悖论模拟           handbook_info_table.json
  5. 训练场             story_review_meta_table.json

暂不处理：危机合约 V2 作战赛季 / 尖灭测试作战（名称/描述出处未定）
"""

import json, sqlite3, os
from text_cleaner import clean_desc

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(OUTPUT_DIR, "arknights.db")

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
}


def load_json(path):
    with open(os.path.join(DATA_ROOT, path), "r", encoding="utf-8") as f:
        return json.load(f)


def clean_level_id(raw):
    if raw is None:
        return None
    return raw.split("/")[-1]


def load_level_cache():
    cache = {}
    for base in ["levels/obt", "levels/activities"]:
        root = os.path.join(DATA_ROOT, base)
        if not os.path.isdir(root):
            continue
        for dirpath, dirs, files in os.walk(root):
            for fname in files:
                if fname.endswith(".json"):
                    key = fname.replace(".json", "")
                    with open(os.path.join(dirpath, fname), "r", encoding="utf-8") as f:
                        cache[key] = json.load(f)
    return cache


def count_spawn_targets(waves):
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
    return result


def build_enemy_list(enemy_db_refs, name_map):
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


def build_enemy_name_map():
    handbook = load_json("excel/enemy_handbook_table.json")
    return {eid: data.get("name", eid) for eid, data in handbook.get("enemyData", {}).items()}


# ═══════════════════════════════════════════

def extract_roguelike_stages():
    print("====== 1. 集成战略（肉鸽）======")
    data = load_json("excel/roguelike_topic_table.json")
    topics = data["topics"]
    details = data["details"]

    stages = []
    for topic_id in topics:
        topic_name = topics[topic_id].get("name", topic_id)
        topic_stages = details.get(topic_id, {}).get("stages", {})
        for sid, s in topic_stages.items():
            main_lid = clean_level_id(s.get("levelId"))
            lids = [main_lid]
            for repl in s.get("levelReplaceIds", []):
                lids.append(clean_level_id(repl))

            name = s.get("name", "").strip()
            desc = s.get("description", "").strip()
            code = s.get("code", "").strip()

            stages.append({
                "关卡ID":    s["id"],
                "关卡名":    name or None,
                "关卡描述":  clean_desc(desc) if desc else None,
                "关卡编号":  code or None,
                "关卡类型":  f"集成战略·{topic_name}",
                "levelId":   ",".join(lids),
                "_main_lid": main_lid,
            })
        print(f"  {topic_name}: {len(topic_stages)} 关")
    return stages


def extract_sandbox_stages():
    print("\n====== 2. 生息演算 =====")
    stages = []

    sb0 = load_json("excel/sandbox_table.json")
    act_stages = sb0["sandboxActTables"]["act1sandbox"]["stageDatas"]
    for sid, s in act_stages.items():
        stages.append({
            "关卡ID":    s["stageId"],
            "关卡名":    s["name"],
            "关卡描述":  clean_desc(s.get("description")),
            "关卡编号":  s.get("code"),
            "关卡类型":  "生息演算·第0期",
            "levelId":   clean_level_id(s.get("levelId")),
        })
    print(f"  第0期: {len(act_stages)} 关")

    sbp = load_json("excel/sandbox_perm_table.json")

    sb1_info = sbp["basicInfo"]["sandbox_1"]
    sb1_name = sb1_info.get("topicName", "沙洲遗闻")
    sb1_stages = sbp["detail"]["SANDBOX_V2"]["sandbox_1"]["stageData"]
    for sid, s in sb1_stages.items():
        stages.append({
            "关卡ID":    s["stageId"],
            "关卡名":    s["name"],
            "关卡描述":  clean_desc(s.get("description")),
            "关卡编号":  s.get("code"),
            "关卡类型":  f"生息演算·{sb1_name}",
            "levelId":   clean_level_id(s.get("levelId")),
        })
    print(f"  {sb1_name}: {len(sb1_stages)} 关")

    sb2_info = sbp["basicInfo"]["sandbox_2"]
    sb2_name = sb2_info.get("topicName", "重启锚点")
    sb2_stages = sbp["detail"]["SANDBOX_V3"]["sandbox_2"]["stageData"]
    sb2_story = sbp["detail"]["SANDBOX_V3"]["sandbox_2"]["storyStageData"]
    sb2_sub   = sbp["detail"]["SANDBOX_V3"]["sandbox_2"]["subStageData"]

    for sid, s in sb2_stages.items():
        story = sb2_story.get(sid, {})
        sub_ids = [si for si in story.get("subStageIdList", []) if si]
        lids = []
        main_lid = None
        for si in sub_ids:
            sub_entry = sb2_sub.get(si, {})
            lid = clean_level_id(sub_entry.get("levelId"))
            if lid:
                if main_lid is None:
                    main_lid = lid
                lids.append(lid)

        stages.append({
            "关卡ID":    s["stageId"],
            "关卡名":    s["name"],
            "关卡描述":  clean_desc(s.get("description")),
            "关卡编号":  s.get("code"),
            "关卡类型":  f"生息演算·{sb2_name}",
            "levelId":   ",".join(lids) if lids else None,
            "_main_lid": main_lid,
        })
    print(f"  {sb2_name}: {len(sb2_stages)} 关")
    return stages


def extract_recalrune_stages():
    print("\n====== 3. 全息作战矩阵 =====")
    data = load_json("excel/crisis_v2_table.json")
    seasons = data["recalRuneData"]["seasons"]

    stages = []
    for season_id, season in seasons.items():
        season_code = season.get("seasonCode", "?")
        season_stages = season.get("stages", {})
        for sid, s in season_stages.items():
            stages.append({
                "关卡ID":    s["stageId"],
                "关卡名":    sid,
                "关卡描述":  None,
                "关卡编号":  f"全息#{season_code}",
                "关卡类型":  "全息作战矩阵",
                "levelId":   clean_level_id(s.get("levelId")),
            })
        print(f"  season_{season_code}: {len(season_stages)} 关")
    return stages


def extract_memory_stages():
    print("\n====== 4. 悖论模拟 =====")
    data = load_json("excel/handbook_info_table.json")
    stage_data = data["handbookStageData"]

    stages = []
    for char_id, s in stage_data.items():
        stages.append({
            "关卡ID":    s["stageId"],
            "关卡名":    s["name"],
            "关卡描述":  clean_desc(s.get("description")),
            "关卡编号":  s.get("code"),
            "关卡类型":  "悖论模拟",
            "levelId":   clean_level_id(s.get("levelId")),
        })
    print(f"  合计: {len(stages)} 关")
    return stages


def extract_training_stages():
    print("\n====== 5. 训练场 =====")
    data = load_json("excel/story_review_meta_table.json")
    stage_data = data["trainingCampData"]["stageData"]

    stages = []
    for sid, s in stage_data.items():
        stages.append({
            "关卡ID":    s["stageId"],
            "关卡名":    s["name"],
            "关卡描述":  clean_desc(s.get("description")),
            "关卡编号":  s.get("code"),
            "关卡类型":  "训练场",
            "levelId":   clean_level_id(s.get("levelId")),
        })
    print(f"  合计: {len(stages)} 关")
    return stages


# ═══════════════════════════════════════════

print("====== 预加载数据 ======")
level_cache = load_level_cache()
enemy_name_map = build_enemy_name_map()
print(f"  level JSON: {len(level_cache)} 个")
print(f"  敌人名称: {len(enemy_name_map)} 条\n")

all_stages = []
all_stages.extend(extract_roguelike_stages())
all_stages.extend(extract_sandbox_stages())
all_stages.extend(extract_recalrune_stages())
all_stages.extend(extract_memory_stages())
all_stages.extend(extract_training_stages())

print(f"\n====== 总计: {len(all_stages)} 条 ======")

# ─── 检查主键冲突 ───

print("\n====== 检查主键冲突 ======")
conn = sqlite3.connect(OUTPUT_DB)
existing = set()
for r in conn.execute("SELECT 关卡ID FROM stages").fetchall():
    existing.add(r[0])

conflicts = [s for s in all_stages if s["关卡ID"] in existing]
new_count = len(all_stages) - len(conflicts)
if conflicts:
    print(f"⚠ 冲突: {len(conflicts)} 条已存在于 stages 表中")
    for s in conflicts[:5]:
        print(f"  {s['关卡ID']} ({s['关卡类型']})")
print(f"  新增: {new_count} 条")

# ─── 写入 ───

print("\n====== 写入 stages 表 ======")
inserted = 0
skipped = 0
no_level = 0
single_count = 0
multi_count = 0

for s in all_stages:
    if s["关卡ID"] in existing:
        skipped += 1
        continue

    lid_raw = s.get("levelId")
    main_lid = s.get("_main_lid", lid_raw)

    if not lid_raw:
        combat = (None, None, None, None, None, None)
    else:
        is_multi = "," in lid_raw
        is_roguelike = "集成战略" in s["关卡类型"]

        if is_multi and not is_roguelike:
            no_level += 1
            combat = (None, None, None, None, None, None)
        else:
            lv = level_cache.get(main_lid) if main_lid else None
            if lv is None:
                no_level += 1
                combat = (None, None, None, None, None, None)
            else:
                if is_multi:
                    multi_count += 1
                else:
                    single_count += 1
                combat = (
                    lv["options"].get("characterLimit"),
                    lv["options"].get("initialCost"),
                    lv["options"].get("maxCost"),
                    lv["options"].get("maxLifePoint"),
                    count_spawn_targets(lv.get("waves")),
                    build_enemy_list(lv.get("enemyDbRefs"), enemy_name_map),
                )

    conn.execute("""
        INSERT INTO stages (
            关卡ID, 关卡名, 关卡描述, 关卡编号, 关卡类型,
            解锁条件, 推荐等级, 理智消耗, levelId,
            部署上限, 初始费用, 费用上限, 目标点耐久,
            待处理目标数量, 敌人列表, 情报
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    """, (
        s["关卡ID"],
        s["关卡名"],
        s.get("关卡描述"),
        s.get("关卡编号"),
        s["关卡类型"],
        None, None, None,
        lid_raw,
        combat[0], combat[1], combat[2], combat[3], combat[4], combat[5],
        None,
    ))
    inserted += 1

conn.commit()

# ─── 统计 ───

print(f"  写入: {inserted} 条")
print(f"  跳过（已存在）: {skipped} 条")
print(f"  单 levelId 有数据: {single_count}")
print(f"  多 levelId 有数据（肉鸽）: {multi_count}")
print(f"  level JSON 缺失: {no_level}")

print("\n====== stages 表各类型总计 =====")
rows = conn.execute("""
    SELECT 关卡类型,
           COUNT(*) AS 总数,
           SUM(CASE WHEN 部署上限 IS NOT NULL THEN 1 ELSE 0 END) AS 有战斗数据
    FROM stages
    GROUP BY 关卡类型
    ORDER BY 总数 DESC
""").fetchall()

for r in rows:
    print(f"  {r[0]:24s}  总数{r[1]:5d}  有数据{r[2]:5d}")

conn.close()
print(f"\n====== 数据库: {OUTPUT_DB} ======")
