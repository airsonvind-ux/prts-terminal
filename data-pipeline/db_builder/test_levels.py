"""
PRTS 终端 — levels 表 阶段三测试脚本
按阶段一字段清单 + 阶段二表结构设计，从原始 level JSON 抽取数据写入测试 SQLite。

输出：test_levels.db（与脚本同目录）
"""

import json
import sqlite3
import os
import sys

# ─── 路径配置 ─────────────────────────────

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(SCRIPT_DIR, "test_levels.db")

# ─── 辅助：收集所有 level JSON 文件 ────────

def collect_json_files():
    """递归收集所有 level JSON，返回 {levelId: filepath}"""
    files = {}

    def walk(root):
        for dirpath, _, fnames in os.walk(root):
            for fname in fnames:
                if fname.endswith(".json"):
                    lid = fname.replace(".json", "")
                    if lid not in files:
                        files[lid] = os.path.join(dirpath, fname)

    obt_root = os.path.join(DATA_ROOT, "levels", "obt")
    walk(obt_root)

    act_root = os.path.join(DATA_ROOT, "levels", "activities")
    if os.path.isdir(act_root):
        walk(act_root)

    return files


# ─── 枚举字段映射表（存储用整数，展示由 tool 层负责）──

# actionType: 字符串 → 整数
_ACTION_TYPE_STR = {
    "SPAWN": 0, "PREVIEW_CURSOR": 1, "STORY": 2, "TUTORIAL": 3,
    "PLAY_BGM": 4, "DISPLAY_ENEMY_INFO": 5, "ACTIVATE_PREDEFINED": 6,
    "PLAY_OPERA": 7, "TRIGGER_PREDEFINED": 8, "BATTLE_EVENTS": 9,
    "WITHDRAW_PREDEFINED": 10, "DIALOG": 11, "E_NUM": 12, "EMPTY": 13,
}

# checkpoint type: 字符串 → 整数
_CHECKPOINT_TYPE_STR = {
    "MOVE": 0, "WAIT_FOR_SECONDS": 1, "WAIT_FOR_PLAY_TIME": 2,
    "WAIT_CURRENT_FRAGMENT_TIME": 3, "WAIT_CURRENT_WAVE_TIME": 4,
    "DISAPPEAR": 5, "APPEAR_AT_POS": 6, "ALERT": 7,
    "PATROL_MOVE": 8, "WAIT_BOSSRUSH_WAVE": 9, "MAP_OFFSET_MOVE": 10,
}

# motionMode: 字符串 → 整数
_MOTION_MODE_STR = {
    "WALK": 0, "FLY": 1, "E_NUM": 2,
}

# direction: 字符串 → 整数
_DIRECTION_STR = {
    "UP": 0, "RIGHT": 1, "DOWN": 2, "LEFT": 3,
}

# phase: 字符串 → 整数
_PHASE_STR = {
    "PHASE_0": 0, "PHASE_1": 1, "PHASE_2": 2,
}


def normalize_enum(val, mapping):
    """整数字段不处理，字符串字段按映射表转整数。均不匹配则原样返回。"""
    if isinstance(val, int):
        return val
    if isinstance(val, str) and val in mapping:
        return mapping[val]
    return val


def normalize_action_type(val):
    return normalize_enum(val, _ACTION_TYPE_STR)

def normalize_checkpoint_type(val):
    return normalize_enum(val, _CHECKPOINT_TYPE_STR)

def normalize_motion_mode(val):
    return normalize_enum(val, _MOTION_MODE_STR)

def normalize_direction(val):
    return normalize_enum(val, _DIRECTION_STR)

def normalize_phase(val):
    return normalize_enum(val, _PHASE_STR)

# ─── 1. mapData 处理 ──────────────────────

def process_map(map_data):
    """mapData → grid（tile 索引）+ 去重 tiles
    tiles 值去重后，grid 中的索引同步重映射。
    """
    if not map_data:
        return None
    grid = map_data.get("map") or []
    height = len(grid)
    width = len(grid[0]) if height > 0 and grid[0] else 0

    tiles_raw = map_data.get("tiles") or []
    # 值去重
    seen = {}
    old_to_new = {}
    deduped = []
    for old_idx, t in enumerate(tiles_raw):
        fp = json.dumps({
            "tileKey": t.get("tileKey"),
            "heightType": t.get("heightType"),
            "buildableType": t.get("buildableType"),
            "passableMask": t.get("passableMask"),
        }, sort_keys=True, ensure_ascii=False)
        if fp in seen:
            old_to_new[old_idx] = seen[fp]
        else:
            new_idx = len(deduped)
            seen[fp] = new_idx
            deduped.append({
                "tileKey": t["tileKey"],
                "heightType": t["heightType"],
                "buildableType": t["buildableType"],
                "passableMask": t["passableMask"],
            })
            old_to_new[old_idx] = new_idx

    # grid 索引重映射
    remapped_grid = []
    for row in grid:
        remapped_grid.append([old_to_new.get(idx, idx) for idx in row])

    return {
        "width": width,
        "height": height,
        "grid": remapped_grid,
        "tiles": deduped,
    }


# ─── 2. routes / extraRoutes 处理 ──────────

def process_one_route(rt):
    """单条路线 → dict，枚举字段统一为整数"""
    if rt is None:
        return None
    checkpoints = []
    for cp in (rt.get("checkpoints") or []):
        if cp is None:
            continue
        pos = cp.get("position")
        off = cp.get("reachOffset")
        checkpoints.append([
            normalize_checkpoint_type(cp.get("type")),
            cp.get("time"),
            [pos["row"], pos["col"]] if pos else None,
            [off["x"], off["y"]] if off else None,
        ])
    return {
        "motionMode": normalize_motion_mode(rt.get("motionMode")),
        "startPosition": [rt["startPosition"]["row"], rt["startPosition"]["col"]] if rt.get("startPosition") else None,
        "endPosition": [rt["endPosition"]["row"], rt["endPosition"]["col"]] if rt.get("endPosition") else None,
        "spawnRandomRange": [rt["spawnRandomRange"]["x"], rt["spawnRandomRange"]["y"]] if rt.get("spawnRandomRange") else None,
        "spawnOffset": [rt["spawnOffset"]["x"], rt["spawnOffset"]["y"]] if rt.get("spawnOffset") else None,
        "checkpoints": checkpoints,
    }


def process_routes(routes_raw):
    """routes 数组 → 路线对象列表，null 保留占位"""
    if not routes_raw:
        return []
    return [process_one_route(rt) for rt in routes_raw]


# ─── 3. routes / extraRoutes 去重 + routeIndex 重映射 ──

def dedup_routes(routes):
    """
    值去重，null 保留占位。
    返回 (deduped_routes, old_to_new: dict)
    """
    if not routes:
        return [], {}

    seen = {}        # json_fingerprint → new_index
    old_to_new = {}  # old_index → new_index
    deduped = []     # 去重后的列表

    for old_idx, rt in enumerate(routes):
        if rt is None:
            # null 保留占位，每个 null 占一个索引
            new_idx = len(deduped)
            deduped.append(None)
            old_to_new[old_idx] = new_idx
        else:
            fp = json.dumps(rt, sort_keys=True, ensure_ascii=False)
            if fp in seen:
                old_to_new[old_idx] = seen[fp]
            else:
                new_idx = len(deduped)
                seen[fp] = new_idx
                deduped.append(rt)
                old_to_new[old_idx] = new_idx

    return deduped, old_to_new


# ─── 4. waves / branches 处理 ──────────────

# action 数组的 10 个位序
ACTION_FIELDS = [
    "actionType",              # 0
    "key",                     # 1
    "count",                   # 2
    "preDelay",                # 3
    "interval",                # 4
    "routeIndex",              # 5
    "randomSpawnGroupKey",     # 6
    "randomSpawnGroupPackKey", # 7
    "weight",                  # 8
    "hiddenGroup",             # 9
]


def process_actions(actions_raw):
    """actions 列表（dict）→ 定长数组列表，枚举字段统一为整数"""
    if not actions_raw:
        return []
    result = []
    for act in actions_raw:
        if act is None:
            continue
        arr = [act.get(field) for field in ACTION_FIELDS]
        arr[0] = normalize_action_type(arr[0])  # actionType 统一为字符串
        result.append(arr)
    return result


def remap_action_route_index(actions, old_to_new):
    """将 actions 中每个 action 的 routeIndex（位序 5）按映射表替换"""
    for act in actions:
        old_idx = act[5]
        if old_idx is not None and old_idx in old_to_new:
            act[5] = old_to_new[old_idx]


def process_waves(waves_raw):
    """waves 数组 → 简化结构，action 转为定长数组"""
    if not waves_raw:
        return []
    out = []
    for w in waves_raw:
        if w is None:
            continue
        fragments = []
        for frag in (w.get("fragments") or []):
            if frag is None:
                continue
            fragments.append({
                "preDelay": frag.get("preDelay", 0.0),
                "actions": process_actions(frag.get("actions")),
            })
        out.append({
            "preDelay": w.get("preDelay", 0.0),
            "postDelay": w.get("postDelay", 0.0),
            "fragments": fragments,
        })
    return out


def process_branches(branches_raw):
    """branches 字典 → 简化结构，phases 与 waves.fragments 同构"""
    if not branches_raw:
        return None
    out = {}
    for bname, bdata in branches_raw.items():
        phases = []
        for ph in (bdata.get("phases") or []):
            if ph is None:
                continue
            phases.append({
                "preDelay": ph.get("preDelay", 0.0),
                "actions": process_actions(ph.get("actions")),
            })
        out[bname] = {"phases": phases}
    return out


def remap_branch_route_indices(branches, old_to_new):
    """遍历 branches 所有 phase 的 action，重映射 routeIndex"""
    if not branches or not old_to_new:
        return
    for bdata in branches.values():
        for ph in (bdata.get("phases") or []):
            remap_action_route_index(ph.get("actions", []), old_to_new)


# ─── 5. predefines 处理 ────────────────────

def flatten_inst(item):
    """将嵌套 inst 扁平化到顶层，枚举字段统一为整数"""
    if item is None:
        return {}
    inst = item.get("inst") or {}
    return {
        "alias": item.get("alias"),
        "characterKey": inst.get("characterKey"),
        "level": inst.get("level"),
        "phase": normalize_phase(inst.get("phase")),
        "skillIndex": item.get("skillIndex"),
        "mainSkillLvl": item.get("mainSkillLvl"),
        "uniEquipIds": item.get("uniEquipIds"),
    }


def process_predefines(predefines_raw):
    """predefines → 四类各自扁平化，枚举字段统一为整数"""
    if not predefines_raw:
        return None

    result = {}

    for cat in ["tokenInsts", "characterInsts"]:
        raw = predefines_raw.get(cat)
        if raw:
            items = []
            for item in raw:
                if item is None:
                    continue
                entry = flatten_inst(item)
                entry["position"] = item.get("position")
                entry["direction"] = normalize_direction(item.get("direction"))
                items.append(entry)
            result[cat] = items
        else:
            result[cat] = []

    for cat in ["tokenCards", "characterCards"]:
        raw = predefines_raw.get(cat)
        if raw:
            items = []
            for item in raw:
                if item is None:
                    continue
                entry = flatten_inst(item)
                if cat == "tokenCards":
                    entry["initialCnt"] = item.get("initialCnt")
                items.append(entry)
            result[cat] = items
        else:
            result[cat] = []

    return result


# ─── 6. 单关处理 ───────────────────────────

def process_level(filepath):
    """处理单个 level JSON，返回 (levelId, row_dict) 或 (None, error_msg)"""
    try:
        with open(filepath, "r", encoding="utf-8") as f:
            lv = json.load(f)
    except Exception as e:
        return None, f"JSON 解析失败: {e}"

    level_id = lv.get("levelId") or os.path.basename(filepath).replace(".json", "")

    # mapData
    map_data = process_map(lv.get("mapData"))
    map_json = json.dumps(map_data, ensure_ascii=False) if map_data else None

    # routes（先处理，后去重）
    routes = process_routes(lv.get("routes", []))
    extra_routes = process_routes(lv.get("extraRoutes", []))

    # routes 去重 + 映射表
    routes_dedup, routes_remap = dedup_routes(routes)
    extra_dedup, extra_remap = dedup_routes(extra_routes)

    routes_json = json.dumps(routes_dedup, ensure_ascii=False)
    extra_json = json.dumps(extra_dedup, ensure_ascii=False) if extra_dedup else None

    # waves（处理 + routeIndex 重映射）
    waves_raw = process_waves(lv.get("waves"))
    # 重映射 waves 中的 routeIndex
    for wave in waves_raw:
        for frag in wave.get("fragments", []):
            remap_action_route_index(frag.get("actions", []), routes_remap)
    waves_json = json.dumps(waves_raw, ensure_ascii=False)

    # branches（处理 + routeIndex 重映射）
    branches_raw = process_branches(lv.get("branches"))
    remap_branch_route_indices(branches_raw, extra_remap)
    branches_json = json.dumps(branches_raw, ensure_ascii=False) if branches_raw else None

    # predefines
    predefines_raw = process_predefines(lv.get("predefines"))
    predefines_json = json.dumps(predefines_raw, ensure_ascii=False) if predefines_raw else None

    row = {
        "levelId": level_id,
        "mapData": map_json,
        "routes": routes_json,
        "extraRoutes": extra_json,
        "waves": waves_json,
        "branches": branches_json,
        "predefines": predefines_json,
    }
    return level_id, row


# ─── 7. 建库 + 入库 ────────────────────────

def build_test_db():
    print("====== 阶段三 · levels 测试建表 ======")
    print(f"数据源: {DATA_ROOT}")
    print(f"输出库: {OUTPUT_DB}")

    # 收集文件
    print("\n[1/4] 扫描 level JSON 文件...")
    all_files = collect_json_files()
    print(f"  文件总数: {len(all_files)}")

    # 按类型分类统计
    type_counts = {}
    for lid in all_files:
        parts = lid.split("_")
        # 提取类型标识
        if "rogue" in lid:
            t = "roguelike"
        elif "crisis" in lid:
            t = "crisis"
        elif "sandbox" in lid:
            t = "sandbox"
        elif "act" in lid:
            t = "activity"
        elif "guide" in lid:
            t = "guide"
        elif "memory" in lid:
            t = "memory"
        elif "training" in lid:
            t = "training"
        elif "weekly" in lid:
            t = "weekly"
        elif "hard" in lid:
            t = "hard"
        elif "main" in lid:
            t = "main"
        elif "campaign" in lid:
            t = "campaign"
        elif "promote" in lid:
            t = "promote"
        elif "recalrune" in lid:
            t = "recalrune"
        elif "rune" in lid:
            t = "rune"
        else:
            t = "other"
        type_counts[t] = type_counts.get(t, 0) + 1
    print("  关卡类型分布:")
    for t, c in sorted(type_counts.items()):
        print(f"    {t}: {c}")

    # 处理
    print("\n[2/4] 处理关卡数据...")
    rows = []
    errors = []

    for lid, filepath in sorted(all_files.items()):
        result_id, result = process_level(filepath)
        if result_id is None:
            errors.append((lid, result))  # result is error msg
        else:
            rows.append(result)

    if errors:
        print(f"  错误: {len(errors)} 个文件处理失败")
        for lid, msg in errors[:5]:
            print(f"    [{lid}] {msg}")
        if len(errors) > 5:
            print(f"    ... 还有 {len(errors) - 5} 个")
    print(f"  成功: {len(rows)} 行")

    # 建库
    print("\n[3/4] 建库...")
    if os.path.exists(OUTPUT_DB):
        os.remove(OUTPUT_DB)

    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    conn.execute("""
    CREATE TABLE levels (
        levelId      TEXT PRIMARY KEY,
        mapData      TEXT NOT NULL,
        routes       TEXT NOT NULL,
        extraRoutes  TEXT,
        waves        TEXT NOT NULL,
        branches     TEXT,
        predefines   TEXT
    )
    """)

    # 写入
    print("\n[4/4] 写入数据库...")
    conn.execute("BEGIN")
    dupes = 0
    for row in rows:
        try:
            conn.execute(
                "INSERT INTO levels (levelId, mapData, routes, extraRoutes, waves, branches, predefines) "
                "VALUES (?, ?, ?, ?, ?, ?, ?)",
                (row["levelId"], row["mapData"], row["routes"], row["extraRoutes"],
                 row["waves"], row["branches"], row["predefines"])
            )
        except sqlite3.IntegrityError:
            dupes += 1
    conn.commit()
    if dupes > 0:
        print(f"  跳过重复 levelId: {dupes} 行")

    # ─── 统计 ───
    total = conn.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
    with_map = conn.execute("SELECT COUNT(*) FROM levels WHERE mapData IS NOT NULL").fetchone()[0]
    with_extra = conn.execute("SELECT COUNT(*) FROM levels WHERE extraRoutes IS NOT NULL").fetchone()[0]
    with_branches = conn.execute("SELECT COUNT(*) FROM levels WHERE branches IS NOT NULL").fetchone()[0]
    with_predefines = conn.execute("SELECT COUNT(*) FROM levels WHERE predefines IS NOT NULL").fetchone()[0]

    print(f"\n====== 统计 ======")
    print(f"  总行数:     {total}")
    print(f"  有 mapData:  {with_map}")
    print(f"  有 extraRoutes: {with_extra}")
    print(f"  有 branches: {with_branches}")
    print(f"  有 predefines: {with_predefines}")

    # 各列大小估算
    sizes = conn.execute("""
        SELECT
            SUM(LENGTH(mapData)),
            SUM(LENGTH(routes)),
            SUM(LENGTH(COALESCE(extraRoutes, ''))),
            SUM(LENGTH(waves)),
            SUM(LENGTH(COALESCE(branches, ''))),
            SUM(LENGTH(COALESCE(predefines, '')))
        FROM levels
    """).fetchone()

    print(f"\n  各列总大小:")
    print(f"    mapData:      {sizes[0]/1024:,.1f} KB")
    print(f"    routes:       {sizes[1]/1024:,.1f} KB")
    print(f"    extraRoutes:  {sizes[2]/1024:,.1f} KB")
    print(f"    waves:        {sizes[3]/1024:,.1f} KB")
    print(f"    branches:     {sizes[4]/1024:,.1f} KB")
    print(f"    predefines:   {sizes[5]/1024:,.1f} KB")
    print(f"  DB 文件大小:     {os.path.getsize(OUTPUT_DB)/1024/1024:,.1f} MB")

    # ─── 抽样展示（每种类型各一行） ───
    print(f"\n====== 抽样展示 ======")
    type_samples = [
        ("主线", "levelId LIKE 'level_main_%' AND levelId NOT LIKE '%side%'"),
        ("活动", "levelId LIKE 'level_act%'"),
        ("肉鸽", "levelId LIKE 'level_rogue%'"),
        ("生息演算", "levelId LIKE 'level_sandbox%'"),
        ("剿灭", "levelId LIKE 'level_campaign%'"),
        ("危机合约", "levelId LIKE 'level_crisis%'"),
    ]

    for label, cond in type_samples:
        row = conn.execute(
            f"SELECT levelId, length(routes), length(waves), "
            f"  CASE WHEN extraRoutes IS NOT NULL THEN 'Y' ELSE '' END, "
            f"  CASE WHEN branches IS NOT NULL THEN 'Y' ELSE '' END, "
            f"  CASE WHEN predefines IS NOT NULL THEN 'Y' ELSE '' END "
            f"FROM levels WHERE {cond} LIMIT 1"
        ).fetchone()
        if row:
            lid, r_len, w_len, has_extra, has_branch, has_pre = row
            print(f"  [{label}] {lid}")
            print(f"    routes: {r_len}B, waves: {w_len}B, extraRoutes: {has_extra}, branches: {has_branch}, predefines: {has_pre}")

    conn.close()
    print(f"\n✓ 测试库已写入: {OUTPUT_DB}")


if __name__ == "__main__":
    build_test_db()
