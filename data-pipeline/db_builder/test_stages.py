"""
PRTS 终端 — stages 表入库 · 测试脚本 v2
目标：全部 3390 条关卡入测试库（arknights_test.db），供用户验证数据读取效果。

修正：
- level JSON 从 obt/ 全部 9 个子目录加载
- 描述文本用 text_cleaner.clean_desc() 清洗富文本标记
- 敌人列表处理为 [{名称, 级别, 覆写属性}] 格式
"""

import json, sqlite3, os
from text_cleaner import clean_desc

# ─── 路径配置 ──────────────────────────────

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
TEST_DB = os.path.join(OUTPUT_DIR, "arknights_test.db")

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

# overwrittenData 属性名 → 中文（与 enemies 表列名一致）
ATTR_CN = {
    # attributes 子字段
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
    # 顶层字段
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
    out = []
    for cond in raw:
        out.append({
            "stageId": cond.get("stageId", ""),
            "completeState": cond.get("completeState", ""),
        })
    return json.dumps(out, ensure_ascii=False)


def load_level_cache():
    """预加载所有 level JSON 到内存 dict

    来源：
    - obt/ 递归全部子目录（main, hard, memory, legion, roguelike, sandbox 等）
    - activities/ 递归
    """
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

    print(f"  obt/ 加载: {count_obt} 个, activities/ 加载: {count_act} 个")
    return cache


def count_spawn_targets(waves):
    """统计所有 actionType=SPAWN 的 count 之和
    JSON 结构: waves[] → fragments[] → actions[]
    术语对应:    波       → 段         → 组
    """
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
    """递归提取 overwrittenData 中 m_defined=true 的值，键名转中文

    树形结构: attributes → maxHp → {m_defined: true, m_value: 1360}
    扁平字段: name → {m_defined: true, m_value: "赞助无人机"}

    返回扁平化 dict: {"最大生命值": 1360, "名称": "赞助无人机"}
    """
    result = {}
    if not isinstance(od, dict):
        return result

    # 叶子节点：{m_defined, m_value}
    if "m_defined" in od:
        if od["m_defined"]:
            # key 已在 recursion 中通过参数传递了——这里需要另一种方式
            return {"__leaf__": od.get("m_value")}
        return {}

    # 分支节点：递归
    for key, val in od.items():
        if isinstance(val, dict):
            sub = extract_overrides(val)
            cn_key = ATTR_CN.get(key, key)
            if "__leaf__" in sub:
                # 单值叶子
                result[cn_key] = sub["__leaf__"]
            else:
                # 嵌套结果（如 attributes 下的多个字段）
                for sk, sv in sub.items():
                    if sk != "__leaf__":
                        result[sk] = sv
    return result


def build_enemy_list(enemy_db_refs, name_map):
    """将 enemyDbRefs 转为 [{名称, 级别, 覆写属性}] 列表

    enemyDbRefs 结构: [{useDb, id, level, overwrittenData}]
    overwrittenData 树形结构，只保留 m_defined=true 的叶节点
    """
    if not enemy_db_refs:
        return "[]"
    out = []
    for ref in enemy_db_refs:
        eid = ref["id"]
        name = name_map.get(eid, eid)

        overwritten = extract_overrides(ref.get("overwrittenData"))

        item = {"名称": name, "级别": ref.get("level", 0)}
        if overwritten:
            item["覆写属性"] = overwritten
        out.append(item)

    return json.dumps(out, ensure_ascii=False)


def build_name_map():
    """构建 enemyId → 中文名称 映射，从 handbook 读取"""
    handbook = load_json("excel/enemy_handbook_table.json")
    name_map = {}
    for eid, data in handbook.get("enemyData", {}).items():
        name_map[eid] = data.get("name", eid)
    print(f"  敌人名称映射: {len(name_map)} 条")
    return name_map


# ─── 主流程 ─────────────────────────────────

print("====== 读取 JSON ======")
stage_table = load_json("excel/stage_table.json")
stages = stage_table.get("stages", {})
print(f"stage_table 条目: {len(stages)}")

print("====== 预加载 level JSON ======")
level_cache = load_level_cache()
print(f"  level JSON 总计缓存: {len(level_cache)} 个文件")

print("====== 构建敌人名称映射 ======")
enemy_name_map = build_name_map()

print("====== 建测试库 ======")
conn = sqlite3.connect(TEST_DB)
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

print("====== 写入数据 ======")
inserted = 0
no_level_json = 0

for sid, s in stages.items():
    lid_raw = s.get("levelId")
    lid_clean = clean_level_id(lid_raw)

    # ── 读取 level JSON ──
    lv = level_cache.get(lid_clean) if lid_clean else None
    if lid_clean and lv is None:
        no_level_json += 1

    char_limit     = lv["options"].get("characterLimit") if lv else None
    init_cost      = lv["options"].get("initialCost")    if lv else None
    max_cost       = lv["options"].get("maxCost")         if lv else None
    max_life       = lv["options"].get("maxLifePoint")    if lv else None
    spawn_count    = count_spawn_targets(lv.get("waves")) if lv else None
    enemy_list     = build_enemy_list(lv.get("enemyDbRefs"), enemy_name_map) if lv else None

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
        char_limit,
        init_cost,
        max_cost,
        max_life,
        spawn_count,
        enemy_list,
        None,  # 情报 — 暂空，后续 PRTS 爬取
    ))
    inserted += 1

conn.commit()
print(f"写入完成: {inserted} 条")
print(f"level JSON 缺失: {no_level_json} 条 (对应列留空)")

# ─── 验证查询 ───
print(f"\n====== 验证查询 ======")

# 1. 各类型统计
print("\n--- 按关卡类型 × 有无 level JSON ---")
rows = conn.execute("""
    SELECT 关卡类型,
           COUNT(*) AS 总数,
           SUM(CASE WHEN levelId IS NULL THEN 1 ELSE 0 END) AS levelId为空,
           SUM(CASE WHEN levelId IS NOT NULL AND 部署上限 IS NULL THEN 1 ELSE 0 END) AS JSON缺失,
           SUM(CASE WHEN 部署上限 IS NOT NULL THEN 1 ELSE 0 END) AS 有战斗数据
    FROM stages
    GROUP BY 关卡类型
    ORDER BY 总数 DESC
""").fetchall()
for r in rows:
    print(f"  {r[0]:6s}  总数{r[1]:4d}  levelId空:{r[2]:3d}  JSON缺:{r[3]:3d}  有数据:{r[4]:4d}")

# 2. 主线 1-7
print("\n--- 主线 1-7 ---")
for r in conn.execute("SELECT * FROM stages WHERE 关卡ID = 'main_01-07'"):
    cols = [d[0] for d in conn.execute("PRAGMA table_info(stages)")]
    for c, v in zip(cols, r):
        val = str(v)
        if len(val) > 120:
            val = val[:120] + "..."
        print(f"  {c}: {val}")

# 3. 1-7 突袭
print("\n--- 主线 1-7 突袭 (#f#) ---")
for r in conn.execute("SELECT * FROM stages WHERE 关卡ID = 'main_01-07#f#'"):
    cols = [d[0] for d in conn.execute("PRAGMA table_info(stages)")]
    for c, v in zip(cols, r):
        val = str(v)
        if len(val) > 120:
            val = val[:120] + "..."
        print(f"  {c}: {val}")

# 4. 剿灭样例（之前全部缺失，现在应有数据）
print("\n--- 剿灭样例 ---")
for r in conn.execute("SELECT 关卡ID, 关卡名, 部署上限, 初始费用, 待处理目标数量 FROM stages WHERE 关卡类型='剿灭' LIMIT 3"):
    print(f"  {r}")

# 5. 日常样例
print("\n--- 日常样例 ---")
for r in conn.execute("SELECT 关卡ID, 关卡名, 部署上限, 待处理目标数量 FROM stages WHERE 关卡类型='日常' LIMIT 3"):
    print(f"  {r}")

# 6. 保全派驻样例
print("\n--- 保全派驻样例 ---")
for r in conn.execute("SELECT 关卡ID, 关卡名, 部署上限, 待处理目标数量 FROM stages WHERE 关卡类型='保全派驻' LIMIT 3"):
    print(f"  {r}")

# 7. description 清洗效果对比
print("\n--- 描述清洗效果（1-7 突袭）---")
raw_desc = stages["main_01-07#f#"].get("description", "")
cleaned = clean_desc(raw_desc)
print(f"  原始: {raw_desc}")
print(f"  清洗: {cleaned}")

print(f"\n====== 测试库: {TEST_DB} ======")
conn.close()
