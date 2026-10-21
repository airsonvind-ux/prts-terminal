"""
PRTS 终端 - 数据管线：JSON → SQLite
将 ArknightsGameData 原始 JSON 转换为 arknights.db
"""

import json
import os
import sqlite3
import sys

import paths

# ─── 路径配置 ───────────────────────────────────────────

DATA_ROOT = str(paths.GAMEDATA)
OUTPUT_DB = str(paths.RESOURCES / "arknights.db")

# ─── 工具函数 ───────────────────────────────────────────

def load_json(relative_path):
    path = os.path.join(DATA_ROOT, relative_path)
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def get_attr(fb_obj, default=None):
    """处理 FlatBuffers m_defined/m_value 包装"""
    if fb_obj is None:
        return default
    if isinstance(fb_obj, dict) and "m_defined" in fb_obj:
        return fb_obj["m_value"] if fb_obj["m_defined"] else default
    return fb_obj


def progress(msg):
    print(f"  {msg}")

# ─── 初始化 SQLite ──────────────────────────────────────

os.makedirs(os.path.dirname(OUTPUT_DB), exist_ok=True)
if os.path.exists(OUTPUT_DB):
    os.remove(OUTPUT_DB)
    progress("已删除旧数据库")

conn = sqlite3.connect(OUTPUT_DB)
conn.execute("PRAGMA journal_mode=OFF")    # 加速批量写入
conn.execute("PRAGMA synchronous=OFF")
conn.execute("PRAGMA foreign_keys=OFF")

conn.execute("""
CREATE TABLE enemies (
    enemy_id      TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    description   TEXT,
    hp_base       REAL,
    atk_base      REAL,
    def_base      REAL,
    res_base      REAL,
    attack_interval  REAL,
    attack_range  REAL,
    move_speed    REAL,
    damage_type   TEXT,
    ability       TEXT,
    aliases       TEXT,
    hp_scaling    TEXT,
    atk_scaling   TEXT,
    def_scaling   TEXT,
    res_scaling   TEXT
)
""")

conn.execute("""
CREATE TABLE levels (
    level_id      TEXT PRIMARY KEY,
    code          TEXT NOT NULL,
    name          TEXT NOT NULL,
    zone_id       TEXT,
    ap_cost       INTEGER,
    description   TEXT,
    difficulty    TEXT,
    enemy_list    TEXT,
    waves_detail  TEXT,
    special_rules TEXT,
    scaling_rule  TEXT,
    tips          TEXT
)
""")

# ─── 阶段 1：加载 handbook ─────────────────────────────

print("=" * 50)
print("阶段 1：加载 enemy_handbook_table.json")
print("=" * 50)

handbook_data = load_json("excel/enemy_handbook_table.json")["enemyData"]
progress(f"图鉴敌人总数：{len(handbook_data)}")

handbook = {}  # enemy_id → {name, description, damageType, abilityList}
for eid, entry in handbook_data.items():
    handbook[eid] = {
        "name": entry["name"],
        "description": entry.get("description"),
        "damage_type": ",".join(entry.get("damageType") or []),
        "ability": json.dumps(
            [a["text"] for a in entry.get("abilityList", [])],
            ensure_ascii=False
        ) if entry.get("abilityList") else None,
    }

# ─── 阶段 2：加载 database ─────────────────────────────

print("=" * 50)
print("阶段 2：加载 enemy_database.json")
print("=" * 50)

db_entries = load_json("levels/enemydata/enemy_database.json")["enemies"]
progress(f"数据库敌人总数：{len(db_entries)}")

database = {}  # enemy_id → {hp_base, atk_base, def_base, ...}
for entry in db_entries:
    key = entry["Key"]
    if not entry["Value"]:
        continue
    ed = entry["Value"][0]["enemyData"]  # level 0
    attrs = ed["attributes"]
    database[key] = {
        "hp_base":        get_attr(attrs.get("maxHp")),
        "atk_base":       get_attr(attrs.get("atk")),
        "def_base":       get_attr(attrs.get("def")),
        "res_base":       get_attr(attrs.get("magicResistance")),
        "attack_interval":   get_attr(attrs.get("baseAttackTime")),
        "move_speed":     get_attr(attrs.get("moveSpeed")),
        "attack_range":   get_attr(ed.get("rangeRadius")),
    }

# ─── 阶段 3：合并写入 enemies 表 ──────────────────────

print("=" * 50)
print("阶段 3：合并敌人数据 → enemies 表")
print("=" * 50)

common = set(handbook.keys()) & set(database.keys())
progress(f"两表交集：{len(common)} 个敌人")

rows = []
for eid in common:
    h = handbook[eid]
    d = database[eid]
    rows.append((
        eid, h["name"], h["description"],
        d["hp_base"], d["atk_base"], d["def_base"], d["res_base"],
        d["attack_interval"], d["attack_range"], d["move_speed"],
        h["damage_type"], h["ability"],
        None,  # aliases
        None, None, None, None,  # scaling columns
    ))

conn.executemany(
    "INSERT INTO enemies VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    rows
)
conn.commit()
progress(f"已写入 {len(rows)} 条敌人记录")

# ─── 阶段 4：加载 stage_table + 关卡文件 ──────────────

print("=" * 50)
print("阶段 4：加载 stage_table.json + 关卡文件 → levels 表")
print("=" * 50)

all_stages = load_json("excel/stage_table.json")["stages"]

# 筛选主线普通关卡：stageId 以 "main_" 开头，不含变体标记
def is_main_normal(stage_id):
    if not stage_id.startswith("main_"):
        return False
    for suffix in ("#f#", "#y#", "#p#"):
        if suffix in stage_id:
            return False
    return True

stages = {sid: s for sid, s in all_stages.items() if is_main_normal(sid)}
progress(f"主线普通关卡：{len(stages)} 个（从 {len(all_stages)} 个总关卡中筛选）")

# 缓存：路径 → 关卡数据，避免重复读同一个文件
level_cache = {}

def get_level_data(level_id):
    """根据 levelId 加载对应的关卡 JSON"""
    if level_id in level_cache:
        return level_cache[level_id]
    file_path = os.path.join(DATA_ROOT, "levels", level_id.lower() + ".json")
    if os.path.exists(file_path):
        data = load_json(os.path.join("levels", level_id.lower() + ".json"))
        level_cache[level_id] = data
        return data
    level_cache[level_id] = None
    return None


def parse_routes(level_data):
    """从 routes 数组提取 routeIndex → 红门编号（按 startPosition 去重）"""
    routes = level_data.get("routes", [])
    pos_to_gate = {}
    route_to_gate = {}
    for ri, route in enumerate(routes):
        if route.get("motionMode") == "E_NUM":
            continue
        start = route["startPosition"]
        key = (start["row"], start["col"])
        if key not in pos_to_gate:
            pos_to_gate[key] = len(pos_to_gate) + 1
        route_to_gate[ri] = pos_to_gate[key]
    return route_to_gate


def extract_enemy_list(level_data):
    """从 enemyDbRefs 提取去重敌人 ID 列表"""
    refs = level_data.get("enemyDbRefs", [])
    return [ref["id"] for ref in refs]


def extract_waves(level_data, route_to_gate):
    """从 waves 提取波次×红门×敌人数据，保留每条 SPAWN 动作的时序信息"""
    waves = level_data.get("waves", [])
    result = []

    wave_index = 0
    for wave in waves:
        for frag in wave.get("fragments", []):
            gate_map = {}
            for action in frag.get("actions", []):
                if action.get("actionType") != "SPAWN":
                    continue
                rte = action.get("routeIndex", -1)
                gate = route_to_gate.get(rte, -1)
                gate_str = str(gate)
                if gate_str not in gate_map:
                    gate_map[gate_str] = []
                gate_map[gate_str].append({
                    "enemy": action["key"],
                    "count": action.get("count", 1),
                    "preDelay": action.get("preDelay", 0),
                    "interval": action.get("interval", 1),
                })

            if not gate_map:
                continue

            wave_index += 1
            result.append({
                "wave": wave_index,
                "gates": gate_map
            })

    return result


level_rows = []
skip_no_file = 0
skip_no_enemies = 0

for stage_id, stage in stages.items():
    level_id = stage["levelId"]
    data = get_level_data(level_id)

    if data is None:
        skip_no_file += 1
        continue

    enemy_refs = extract_enemy_list(data)
    if not enemy_refs:
        skip_no_enemies += 1
        # 没有敌人列表，仍写入关卡基本信息
        enemy_list_json = None
        waves_json = None
    else:
        enemy_list_json = json.dumps(enemy_refs, ensure_ascii=False)
        route_to_gate = parse_routes(data)
        waves_data = extract_waves(data, route_to_gate)
        waves_json = json.dumps(waves_data, ensure_ascii=False) if waves_data else None

    level_rows.append((
        stage_id,
        stage.get("code"),
        stage.get("name"),
        stage.get("zoneId"),
        stage.get("apCost"),
        stage.get("description"),
        stage.get("difficulty"),
        enemy_list_json,
        waves_json,
        None,  # special_rules
        None,  # scaling_rule
        None,  # tips
    ))

conn.executemany(
    "INSERT INTO levels VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
    level_rows
)
conn.commit()

progress(f"已写入 {len(level_rows)} 条关卡记录")
progress(f"  其中 {skip_no_file} 个关卡无对应 level 文件（跳过）")
progress(f"  其中 {skip_no_enemies} 个关卡无敌人列表（仅存入基本信息）")

# ─── 收尾 ───────────────────────────────────────────────

print("=" * 50)
print("完成")
print("=" * 50)

conn.close()
print(f"输出：{OUTPUT_DB}")
print(f"enemies 表：{len(rows)} 条")
print(f"levels 表：{len(level_rows)} 条")
