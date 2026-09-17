"""
PRTS 终端 - 数据管线 Demo
验证从 ArknightsGameData JSON 提取数据的关键逻辑，不写入 SQLite，仅打印结果。
"""

import json
import os

DATA = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"

# ─── 工具函数 ───────────────────────────────────────────

def load_json(relative_path):
    """加载 JSON 文件，返回 Python dict"""
    path = os.path.join(DATA, relative_path)
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def get_attr(flatbuffers_obj, default=None):
    """处理 FlatBuffers 的 m_defined / m_value 包装"""
    if flatbuffers_obj is None:
        return default
    if isinstance(flatbuffers_obj, dict) and "m_defined" in flatbuffers_obj:
        return flatbuffers_obj["m_value"] if flatbuffers_obj["m_defined"] else default
    return flatbuffers_obj


# ─── 阶段 1：敌人图鉴（handbook） ────────────────────────

print("=" * 60)
print("阶段 1：enemy_handbook_table.json → 敌人图鉴")
print("=" * 60)

handbook = load_json("excel/enemy_handbook_table.json")
handbook_data = handbook["enemyData"]
print(f"  总敌人数：{len(handbook_data)}")

# 取前 5 个作为样本
sample_ids = list(handbook_data.keys())[:5]

for eid in sample_ids:
    e = handbook_data[eid]
    ability_texts = [a["text"] for a in e["abilityList"]]
    print(f"\n  [{e['enemyId']}] {e['name']}")
    print(f"    等级：{e['enemyLevel']}")
    print(f"    伤害类型：{e['damageType']}")
    print(f"    描述：{e['description'][:50]}...")
    if ability_texts:
        print(f"    特性：{ability_texts}")


# ─── 阶段 2：敌人战斗数据（database） ───────────────────

print("\n" + "=" * 60)
print("阶段 2：enemy_database.json → 精确数值")
print("=" * 60)

database = load_json("levels/enemydata/enemy_database.json")
db_entries = database["enemies"]
print(f"  总敌人数：{len(db_entries)}")

# 转成 dict[id] = level-0 数据
db_dict = {}
for entry in db_entries:
    key = entry["Key"]
    if entry["Value"]:
        db_dict[key] = entry["Value"][0]["enemyData"]

# 展示与 handbook 同 id 的敌人
for eid in sample_ids:
    if eid not in db_dict:
        print(f"\n  [{eid}] 无战斗数据（可能仅在图鉴中）")
        continue
    d = db_dict[eid]
    attrs = d["attributes"]
    print(f"\n  [{eid}]")
    print(f"    HP:       {get_attr(attrs.get('maxHp'), '?')}")
    print(f"    ATK:      {get_attr(attrs.get('atk'), '?')}")
    print(f"    DEF:      {get_attr(attrs.get('def'), '?')}")
    print(f"    法抗:     {get_attr(attrs.get('magicResistance'), '?')}")
    print(f"    移速:     {get_attr(attrs.get('moveSpeed'), '?')}")
    print(f"    攻击间隔:     {get_attr(attrs.get('baseAttackTime'), '?')}")
    print(f"    攻击范围:  {get_attr(d.get('rangeRadius'), '?')}")
    print(f"    攻击方式:  {get_attr(d.get('applyWay'), '?')}")


# ─── 阶段 3：合并敌人数据 ───────────────────────────────

print("\n" + "=" * 60)
print("阶段 3：合并 handbook + database（交集 enemy_id）")
print("=" * 60)

common_ids = set(handbook_data.keys()) & set(db_dict.keys())
print(f"  两表共有敌人：{len(common_ids)}")
print(f"  仅图鉴有：{len(set(handbook_data.keys()) - set(db_dict.keys()))}")
print(f"  仅数据库有：{len(set(db_dict.keys()) - set(handbook_data.keys()))}")

# 展示 1 个合并后的完整敌人数据
demo_id = "enemy_1029_shdsbr"  # 幽灵组长（1-7 出场的敌人）
if demo_id in common_ids:
    h = handbook_data[demo_id]
    d = db_dict[demo_id]
    attrs = d["attributes"]
    print(f"\n  示例敌人：{h['name']}")
    print(f"    图鉴评级：{h['enemyLevel']}")
    print(f"    伤害类型：{','.join(h['damageType'])}")
    print(f"    描述：{h['description']}")
    ab_list = [a["text"] for a in h["abilityList"]]
    if ab_list:
        print(f"    特性：{ab_list}")
    print(f"    HP / ATK / DEF / RES：{get_attr(attrs.get('maxHp'))} / {get_attr(attrs.get('atk'))} / {get_attr(attrs.get('def'))} / {get_attr(attrs.get('magicResistance'))}")
    print(f"    攻击间隔 / 移速：{get_attr(attrs.get('baseAttackTime'))} / {get_attr(attrs.get('moveSpeed'))}")
    print(f"    攻击范围：{get_attr(d.get('rangeRadius'))}")


# ─── 阶段 4：关卡数据 ────────────────────────────────────

print("\n" + "=" * 60)
print("阶段 4：stage_table.json + 关卡文件 → 关卡×敌人")
print("=" * 60)

stages = load_json("excel/stage_table.json")["stages"]

# 以 1-7 为例
demo_stage_id = "main_04-10"
stage = stages[demo_stage_id]
print(f"\n  关卡：{stage['code']}「{stage['name']}」")
print(f"    AP 消耗：{stage['apCost']}")
print(f"    难度：{stage['difficulty']}")
print(f"    描述：{stage['description']}")
print(f"    levelId：{stage['levelId']}")

# 找对应的关卡文件
level_id = stage["levelId"]
file_path = os.path.join(DATA, "levels", level_id.lower() + ".json")
print(f"    关卡文件：{file_path}")
print(f"    文件存在：{os.path.exists(file_path)}")

if os.path.exists(file_path):
    level_data = load_json(os.path.join("levels", level_id.lower() + ".json"))

    # ── 4a：简易敌人清单（enemyDbRefs） ──
    enemy_db_refs = level_data.get("enemyDbRefs", [])
    enemy_ids = [ref["id"] for ref in enemy_db_refs]
    enemy_names = []
    for eid in enemy_ids:
        if eid in db_dict:
            enemy_names.append(db_dict[eid]["name"]["m_value"])
        elif eid in handbook_data:
            enemy_names.append(handbook_data[eid]["name"])
        else:
            enemy_names.append(eid)

    print(f"\n    📋 简易输出 - 本关敌人种类（{len(enemy_ids)}种）：")
    for name in enemy_names:
        print(f"      · {name}")

    # ── 4b：详细波次数据（按 fragment 分波，按红门展示出怪顺序） ──
    waves = level_data.get("waves", [])
    routes = level_data.get("routes", [])
    print(f"\n    📊 详细输出 - 按波次 × 红门（保持原顺序，不合并）：\n")

    # routeIndex → 红门编号（按 startPosition 坐标去重，跳过虚拟路线）
    pos_to_gate = {}
    route_to_gate = {}
    for ri, route in enumerate(routes):
        if route.get("motionMode") == "E_NUM":  # 非真实路线，跳过
            continue
        start = route["startPosition"]
        key = (start["row"], start["col"])
        if key not in pos_to_gate:
            pos_to_gate[key] = len(pos_to_gate) + 1
        route_to_gate[ri] = pos_to_gate[key]

    print(f"    实际红门数：{len(pos_to_gate)}（从 {len(routes)} 条路线识别）\n")

    wave_index = 0
    for wave in waves:
        for frag in wave.get("fragments", []):
            gate_map = {}  # { 红门编号: { enemy_id: count } }
            for action in frag.get("actions", []):
                if action.get("actionType") != "SPAWN":
                    continue
                rte = action.get("routeIndex", -1)
                gate = route_to_gate.get(rte, -1)
                eid = action["key"]
                cnt = action.get("count", 1)
                if gate not in gate_map:
                    gate_map[gate] = {}
                gate_map[gate][eid] = gate_map[gate].get(eid, 0) + cnt

            if not gate_map:
                continue

            wave_index += 1
            print(f"  第{wave_index}波：")
            for gate in sorted(gate_map.keys()):
                print(f"    红门{gate}：")
                for eid, cnt in sorted(gate_map[gate].items()):
                    if eid in db_dict:
                        name = db_dict[eid]["name"]["m_value"]
                    elif eid in handbook_data:
                        name = handbook_data[eid]["name"]
                    else:
                        name = eid
                    print(f"      {name} ×{cnt}")

print("\n" + "=" * 60)
print("Demo 完成。")
