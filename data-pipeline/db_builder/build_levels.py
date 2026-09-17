"""
PRTS 终端 — levels 表入库
来源：levels/obt/**/*.json + levels/activities/**/*.json
每个 level JSON 一行，地图/路线/波次/分支/预置单位为 JSON 列。
枚举字段存储为整数，展示由 tool 层负责。
"""

import json
import sqlite3
import os

# ─── 路径配置 ─────────────────────────────

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(SCRIPT_DIR, "arknights.db")

# ─── 枚举映射表 ───────────────────────────

_ACTION_TYPE_STR = {
    "SPAWN": 0, "PREVIEW_CURSOR": 1, "STORY": 2, "TUTORIAL": 3,
    "PLAY_BGM": 4, "DISPLAY_ENEMY_INFO": 5, "ACTIVATE_PREDEFINED": 6,
    "PLAY_OPERA": 7, "TRIGGER_PREDEFINED": 8, "BATTLE_EVENTS": 9,
    "WITHDRAW_PREDEFINED": 10, "DIALOG": 11, "E_NUM": 12, "EMPTY": 13,
}

_CHECKPOINT_TYPE_STR = {
    "MOVE": 0, "WAIT_FOR_SECONDS": 1, "WAIT_FOR_PLAY_TIME": 2,
    "WAIT_CURRENT_FRAGMENT_TIME": 3, "WAIT_CURRENT_WAVE_TIME": 4,
    "DISAPPEAR": 5, "APPEAR_AT_POS": 6, "ALERT": 7,
    "PATROL_MOVE": 8, "WAIT_BOSSRUSH_WAVE": 9, "MAP_OFFSET_MOVE": 10,
}

_MOTION_MODE_STR = {"WALK": 0, "FLY": 1, "E_NUM": 2}
_DIRECTION_STR = {"UP": 0, "RIGHT": 1, "DOWN": 2, "LEFT": 3}
_PHASE_STR = {"PHASE_0": 0, "PHASE_1": 1, "PHASE_2": 2}


def normalize_enum(val, mapping):
    if isinstance(val, int):
        return val
    if isinstance(val, str) and val in mapping:
        return mapping[val]
    return val


# ─── 文件收集 ─────────────────────────────

def collect_json_files():
    files = {}
    def walk(root):
        for dirpath, _, fnames in os.walk(root):
            for fname in fnames:
                if fname.endswith(".json"):
                    lid = fname.replace(".json", "")
                    if lid not in files:
                        files[lid] = os.path.join(dirpath, fname)
    walk(os.path.join(DATA_ROOT, "levels", "obt"))
    act_root = os.path.join(DATA_ROOT, "levels", "activities")
    if os.path.isdir(act_root):
        walk(act_root)
    return files


# ─── mapData ──────────────────────────────

def process_map(map_data):
    if not map_data:
        return None
    grid = map_data.get("map") or []
    height = len(grid)
    width = len(grid[0]) if height > 0 and grid[0] else 0

    tiles_raw = map_data.get("tiles") or []
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

    return {
        "width": width,
        "height": height,
        "grid": [[old_to_new.get(idx, idx) for idx in row] for row in grid],
        "tiles": deduped,
    }


# ─── routes ───────────────────────────────

def process_one_route(rt):
    if rt is None:
        return None
    checkpoints = []
    for cp in (rt.get("checkpoints") or []):
        if cp is None:
            continue
        pos = cp.get("position")
        off = cp.get("reachOffset")
        checkpoints.append([
            normalize_enum(cp.get("type"), _CHECKPOINT_TYPE_STR),
            cp.get("time"),
            [pos["row"], pos["col"]] if pos else None,
            [off["x"], off["y"]] if off else None,
        ])
    return {
        "motionMode": normalize_enum(rt.get("motionMode"), _MOTION_MODE_STR),
        "startPosition": [rt["startPosition"]["row"], rt["startPosition"]["col"]] if rt.get("startPosition") else None,
        "endPosition": [rt["endPosition"]["row"], rt["endPosition"]["col"]] if rt.get("endPosition") else None,
        "spawnRandomRange": [rt["spawnRandomRange"]["x"], rt["spawnRandomRange"]["y"]] if rt.get("spawnRandomRange") else None,
        "spawnOffset": [rt["spawnOffset"]["x"], rt["spawnOffset"]["y"]] if rt.get("spawnOffset") else None,
        "checkpoints": checkpoints,
    }


def process_routes(routes_raw):
    if not routes_raw:
        return []
    return [process_one_route(rt) for rt in routes_raw]


def dedup_routes(routes):
    if not routes:
        return [], {}
    seen = {}
    old_to_new = {}
    deduped = []
    for old_idx, rt in enumerate(routes):
        if rt is None:
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


# ─── waves / branches ─────────────────────

ACTION_FIELDS = [
    "actionType", "key", "count", "preDelay", "interval", "routeIndex",
    "randomSpawnGroupKey", "randomSpawnGroupPackKey", "weight", "hiddenGroup",
]


def process_actions(actions_raw):
    if not actions_raw:
        return []
    result = []
    for act in actions_raw:
        if act is None:
            continue
        arr = [act.get(f) for f in ACTION_FIELDS]
        arr[0] = normalize_enum(arr[0], _ACTION_TYPE_STR)
        result.append(arr)
    return result


def remap_action_route_index(actions, old_to_new):
    for act in actions:
        old_idx = act[5]
        if old_idx is not None and old_idx in old_to_new:
            act[5] = old_to_new[old_idx]


def process_waves(waves_raw):
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
    if not branches or not old_to_new:
        return
    for bdata in branches.values():
        for ph in (bdata.get("phases") or []):
            remap_action_route_index(ph.get("actions", []), old_to_new)


# ─── predefines ───────────────────────────

def flatten_inst(item):
    if item is None:
        return {}
    inst = item.get("inst") or {}
    return {
        "alias": item.get("alias"),
        "characterKey": inst.get("characterKey"),
        "level": inst.get("level"),
        "phase": normalize_enum(inst.get("phase"), _PHASE_STR),
        "skillIndex": item.get("skillIndex"),
        "mainSkillLvl": item.get("mainSkillLvl"),
        "uniEquipIds": item.get("uniEquipIds"),
    }


def process_predefines(predefines_raw):
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
                entry["direction"] = normalize_enum(item.get("direction"), _DIRECTION_STR)
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


# ─── 单关处理 ─────────────────────────────

def process_level(filepath):
    try:
        with open(filepath, "r", encoding="utf-8") as f:
            lv = json.load(f)
    except Exception as e:
        return None, str(e)

    level_id = lv.get("levelId") or os.path.basename(filepath).replace(".json", "")

    map_data = process_map(lv.get("mapData"))
    map_json = json.dumps(map_data, ensure_ascii=False) if map_data else None

    routes = process_routes(lv.get("routes", []))
    extra_routes = process_routes(lv.get("extraRoutes", []))
    routes_dedup, routes_remap = dedup_routes(routes)
    extra_dedup, extra_remap = dedup_routes(extra_routes)

    routes_json = json.dumps(routes_dedup, ensure_ascii=False)
    extra_json = json.dumps(extra_dedup, ensure_ascii=False) if extra_dedup else None

    waves_raw = process_waves(lv.get("waves"))
    for wave in waves_raw:
        for frag in wave.get("fragments", []):
            remap_action_route_index(frag.get("actions", []), routes_remap)
    waves_json = json.dumps(waves_raw, ensure_ascii=False)

    branches_raw = process_branches(lv.get("branches"))
    remap_branch_route_indices(branches_raw, extra_remap)
    branches_json = json.dumps(branches_raw, ensure_ascii=False) if branches_raw else None

    predefines_raw = process_predefines(lv.get("predefines"))
    predefines_json = json.dumps(predefines_raw, ensure_ascii=False) if predefines_raw else None

    return level_id, {
        "levelId": level_id, "mapData": map_json, "routes": routes_json,
        "extraRoutes": extra_json, "waves": waves_json,
        "branches": branches_json, "predefines": predefines_json,
    }


# ─── 主流程 ───────────────────────────────

def build_levels():
    print("====== levels 表入库 ======")
    print(f"数据源: {DATA_ROOT}")

    all_files = collect_json_files()
    print(f"文件总数: {len(all_files)}")

    print("处理关卡数据...")
    rows = []
    errors = []
    for lid, filepath in sorted(all_files.items()):
        rid, row = process_level(filepath)
        if rid is None:
            errors.append((lid, row))
        else:
            rows.append(row)

    if errors:
        print(f"错误: {len(errors)} 个文件处理失败")
        for lid, msg in errors[:5]:
            print(f"  [{lid}] {msg}")
    print(f"成功: {len(rows)} 行")

    print("建表 + 写入...")
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")
    conn.execute("DROP TABLE IF EXISTS levels")
    conn.execute("""
    CREATE TABLE levels (
        levelId     TEXT PRIMARY KEY,
        地图数据     TEXT NOT NULL,
        路线        TEXT NOT NULL,
        额外路线     TEXT,
        波次        TEXT NOT NULL,
        分支        TEXT,
        预置单位     TEXT
    )
    """)

    conn.execute("BEGIN")
    dupes = 0
    for row in rows:
        try:
            conn.execute(
                "INSERT INTO levels (levelId, 地图数据, 路线, 额外路线, 波次, 分支, 预置单位) "
                "VALUES (?, ?, ?, ?, ?, ?, ?)",
                (row["levelId"], row["mapData"], row["routes"], row["extraRoutes"],
                 row["waves"], row["branches"], row["predefines"])
            )
        except sqlite3.IntegrityError:
            dupes += 1
    conn.commit()

    total = conn.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
    with_extra = conn.execute("SELECT COUNT(*) FROM levels WHERE 额外路线 IS NOT NULL").fetchone()[0]
    with_branches = conn.execute("SELECT COUNT(*) FROM levels WHERE 分支 IS NOT NULL").fetchone()[0]
    with_predefines = conn.execute("SELECT COUNT(*) FROM levels WHERE 预置单位 IS NOT NULL").fetchone()[0]

    sizes = conn.execute("""
        SELECT SUM(LENGTH(地图数据)), SUM(LENGTH(路线)),
               SUM(LENGTH(COALESCE(额外路线,''))), SUM(LENGTH(波次)),
               SUM(LENGTH(COALESCE(分支,''))), SUM(LENGTH(COALESCE(预置单位,'')))
        FROM levels
    """).fetchone()

    conn.close()

    print(f"\n====== 完成 ======")
    print(f"  总行数:            {total}")
    if dupes:
        print(f"  跳过重复 levelId:   {dupes}")
    print(f"  有 extraRoutes:    {with_extra}")
    print(f"  有 branches:       {with_branches}")
    print(f"  有 predefines:     {with_predefines}")
    print(f"  mapData:           {sizes[0]/1024:,.1f} KB")
    print(f"  routes:            {sizes[1]/1024:,.1f} KB")
    print(f"  extraRoutes:       {sizes[2]/1024:,.1f} KB")
    print(f"  waves:             {sizes[3]/1024:,.1f} KB")
    print(f"  branches:          {sizes[4]/1024:,.1f} KB")
    print(f"  predefines:        {sizes[5]/1024:,.1f} KB")


if __name__ == "__main__":
    build_levels()
