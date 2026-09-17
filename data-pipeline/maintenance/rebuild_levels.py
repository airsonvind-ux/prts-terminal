"""
PRTS 终端 — levels 表全量重建脚本（维护用）

数据源：levels/obt/**/*.json + levels/activities/**/*.json
每次数据源更新后，对本脚本执行一次即可完成 levels 表重建：
    python rebuild_levels.py

本脚本合并了旧 build_levels.py + build_remove_keys_levels.py + build_levels_runes.py
三者的职责，一次产出最终入库格式（去键化 + 归一化 + 符文），不再分 build/refine 两阶段。

levels 表采用「重建制」维护（数据源为本地 JSON，重建快且稳定）；
其他含 Wiki 爬取信息的表采用「更新制」，不要用重建方式维护。
"""

import json
import os
import shutil
import sqlite3
import datetime

# ─── 路径配置 ─────────────────────────────

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))          # .../data-pipeline/maintenance
DB_BUILDER_DIR = os.path.join(os.path.dirname(SCRIPT_DIR), "db_builder")
OUTPUT_DB = os.path.join(DB_BUILDER_DIR, "arknights.db")

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

# 地块归一化（原 refine 阶段职责，现并入 build）
_HEIGHT_MAP = {"LOWLAND": 0, "HIGHLAND": 1}
_BUILD_MAP = {"NONE": 0, "MELEE": 1, "RANGED": 2, "ALL": 3}
_PASSABLE_MAP = {"NONE": 0, "FLY_ONLY": 2, "ALL": 3}


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
    """tiles 归一化 + 值去重 + grid 重映射，直接输出 4 位定长数组。"""
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
        tk = t.get("tileKey")
        ht = t.get("heightType")
        bt = t.get("buildableType")
        pm = t.get("passableMask")
        if isinstance(ht, str):
            ht = _HEIGHT_MAP.get(ht, ht)
        if isinstance(bt, str):
            bt = _BUILD_MAP.get(bt, bt)
        if isinstance(pm, str):
            pm = _PASSABLE_MAP.get(pm, pm)
        fp = json.dumps([tk, ht, bt, pm], sort_keys=True, ensure_ascii=False)
        if fp in seen:
            old_to_new[old_idx] = seen[fp]
        else:
            new_idx = len(deduped)
            seen[fp] = new_idx
            deduped.append([tk, ht, bt, pm])
            old_to_new[old_idx] = new_idx

    return {
        "width": width,
        "height": height,
        "grid": [[old_to_new.get(idx, idx) for idx in row] for row in grid],
        "tiles": deduped,
    }


# ─── routes ───────────────────────────────

def process_one_route(rt):
    """route → dict。dict 始终含 visitEveryCheckPoint（True/False），
    去重指纹据此区分「几何相同但 visit 不同」的两条路线，不再合并。"""
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
        "visitEveryCheckPoint": rt.get("visitEveryCheckPoint", False),
    }


def route_to_arr(d):
    """dict → 6/7 位定长数组。visitEveryCheckPoint 仅在 true 时保留第 7 位。"""
    arr = [
        d["motionMode"],
        d["startPosition"],
        d["endPosition"],
        d["spawnRandomRange"],
        d["spawnOffset"],
        d["checkpoints"],
    ]
    if d.get("visitEveryCheckPoint"):
        arr.append(True)
    return arr


def dedup_routes(route_dicts):
    """值去重（JSON 序列化指纹，含 visitEveryCheckPoint），返回去重后 dict 列表与索引映射。"""
    seen = {}
    old_to_new = {}
    deduped = []
    for old_idx, d in enumerate(route_dicts):
        if d is None:
            new_idx = len(deduped)
            deduped.append(None)
            old_to_new[old_idx] = new_idx
        else:
            fp = json.dumps(d, sort_keys=True, ensure_ascii=False)
            if fp in seen:
                old_to_new[old_idx] = seen[fp]
            else:
                new_idx = len(deduped)
                seen[fp] = new_idx
                deduped.append(d)
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

def _flatten_inst(item):
    """原始预置单位 item → 扁平 dict（characterKey 等从 inst 解包）。"""
    if item is None:
        return None
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
    """预置单位 → 4 元素数组：[[tokenInsts 9位], [characterInsts 9位],
    [tokenCards 8位], [characterCards 7位]]。空子数组为 []，整列 NULL 表示无。"""
    if not predefines_raw:
        return None

    def xform_inst(item):
        d = _flatten_inst(item)
        if d is None:
            return None
        return [
            d["alias"], d["characterKey"], d["level"], d["phase"],
            d["skillIndex"], d["mainSkillLvl"], d["uniEquipIds"],
            [item["position"]["row"], item["position"]["col"]],
            normalize_enum(item["direction"], _DIRECTION_STR),
        ]

    def xform_token_card(item):
        d = _flatten_inst(item)
        if d is None:
            return None
        return [
            d["alias"], d["characterKey"], d["level"], d["phase"],
            d["skillIndex"], d["mainSkillLvl"], d["uniEquipIds"],
            item["initialCnt"],
        ]

    def xform_char_card(item):
        d = _flatten_inst(item)
        if d is None:
            return None
        return [
            d["alias"], d["characterKey"], d["level"], d["phase"],
            d["skillIndex"], d["mainSkillLvl"], d["uniEquipIds"],
        ]

    def collect(cat, xform):
        return [x for i in (predefines_raw.get(cat) or []) if (x := xform(i)) is not None]

    return [
        collect("tokenInsts", xform_inst),
        collect("characterInsts", xform_inst),
        collect("tokenCards", xform_token_card),
        collect("characterCards", xform_char_card),
    ]


# ─── 关卡符文 ─────────────────────────────

def process_runes(level_data):
    """合并 runes 和 optionalRunes，按 difficultyMask 分组。
    optionalRunes 为 dict 时存入顶层「可选」键。"""
    runes = level_data.get("runes") or []
    opt = level_data.get("optionalRunes")

    if isinstance(opt, list):
        runes = runes + opt
        opt = None

    if not runes and not isinstance(opt, dict):
        return "{}"

    result = {}
    for r in runes:
        dm = r.get("difficultyMask")
        if dm is None:
            continue
        dm = str(dm)
        entry = {
            "key": r.get("key"),
            "professionMask": r.get("professionMask"),
            "buildableMask": r.get("buildableMask"),
            "blackboard": [
                {
                    "key": b.get("key"),
                    "value": b.get("value"),
                    "valueStr": b.get("valueStr"),
                }
                for b in (r.get("blackboard") or [])
            ],
        }
        result.setdefault(dm, []).append(entry)

    if isinstance(opt, dict):
        opt_clean = {}
        for group_name, group_runes in opt.items():
            opt_clean[group_name] = []
            for r in group_runes:
                opt_clean[group_name].append({
                    "key": r.get("key"),
                    "professionMask": r.get("professionMask"),
                    "buildableMask": r.get("buildableMask"),
                    "blackboard": [
                        {
                            "key": b.get("key"),
                            "value": b.get("value"),
                            "valueStr": b.get("valueStr"),
                        }
                        for b in (r.get("blackboard") or [])
                    ],
                })
        result["可选"] = opt_clean

    return json.dumps(result, ensure_ascii=False)


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

    # 路线：dict（含 visit）→ 去重 → 6/7 位数组
    routes_dicts = [process_one_route(rt) for rt in (lv.get("routes") or [])]
    extra_dicts = [process_one_route(rt) for rt in (lv.get("extraRoutes") or [])]
    routes_dedup, routes_remap = dedup_routes(routes_dicts)
    extra_dedup, extra_remap = dedup_routes(extra_dicts)

    routes_json = json.dumps([route_to_arr(r) if r else None for r in routes_dedup], ensure_ascii=False)
    extra_json = json.dumps([route_to_arr(r) if r else None for r in extra_dedup], ensure_ascii=False) if extra_dedup else None

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

    runes_json = process_runes(lv)

    return level_id, {
        "levelId": level_id,
        "mapData": map_json,
        "routes": routes_json,
        "extraRoutes": extra_json,
        "waves": waves_json,
        "branches": branches_json,
        "predefines": predefines_json,
        "runes": runes_json,
    }


# ─── 主流程 ───────────────────────────────

def backup_db():
    """重建前备份现有库（若存在）。"""
    if not os.path.exists(OUTPUT_DB):
        return None
    ts = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    bak = os.path.join(DB_BUILDER_DIR, f"arknights_backup_{ts}.db")
    shutil.copy2(OUTPUT_DB, bak)
    return bak


def build():
    print("====== levels 表全量重建 ======")
    print(f"数据源: {DATA_ROOT}")
    print(f"目标 DB: {OUTPUT_DB}")

    bak = backup_db()
    if bak:
        print(f"已备份: {os.path.basename(bak)}")

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
        预置单位     TEXT,
        关卡符文     TEXT NOT NULL DEFAULT '{}'
    )
    """)

    conn.execute("BEGIN")
    dupes = 0
    for row in rows:
        try:
            conn.execute(
                "INSERT INTO levels (levelId, 地图数据, 路线, 额外路线, 波次, 分支, 预置单位, 关卡符文) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                (row["levelId"], row["mapData"], row["routes"], row["extraRoutes"],
                 row["waves"], row["branches"], row["predefines"], row["runes"])
            )
        except sqlite3.IntegrityError:
            dupes += 1
    conn.commit()

    total = conn.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
    with_extra = conn.execute("SELECT COUNT(*) FROM levels WHERE 额外路线 IS NOT NULL").fetchone()[0]
    with_branches = conn.execute("SELECT COUNT(*) FROM levels WHERE 分支 IS NOT NULL").fetchone()[0]
    with_predefines = conn.execute("SELECT COUNT(*) FROM levels WHERE 预置单位 IS NOT NULL").fetchone()[0]
    with_runes = conn.execute("SELECT COUNT(*) FROM levels WHERE 关卡符文 != '{}'").fetchone()[0]

    # visitEveryCheckPoint 统计（从路线 JSON 中扫描第 7 位）
    visit_true_routes = 0
    visit_true_extra = 0
    route_len7 = 0
    for lid, routes_raw, extra_raw in conn.execute("SELECT levelId, 路线, 额外路线 FROM levels"):
        for arr in json.loads(routes_raw):
            if arr is None:
                continue
            if len(arr) >= 7 and arr[6] is True:
                visit_true_routes += 1
                route_len7 += 1
        if extra_raw:
            for arr in json.loads(extra_raw):
                if arr is None:
                    continue
                if len(arr) >= 7 and arr[6] is True:
                    visit_true_extra += 1
                    route_len7 += 1

    sizes = conn.execute("""
        SELECT SUM(LENGTH(地图数据)), SUM(LENGTH(路线)),
               SUM(LENGTH(COALESCE(额外路线,''))), SUM(LENGTH(波次)),
               SUM(LENGTH(COALESCE(分支,''))), SUM(LENGTH(COALESCE(预置单位,''))),
               SUM(LENGTH(关卡符文))
        FROM levels
    """).fetchone()

    conn.close()

    print(f"\n====== 完成 ======")
    print(f"  总行数:              {total}")
    if dupes:
        print(f"  跳过重复 levelId:     {dupes}")
    print(f"  有 extraRoutes:      {with_extra}")
    print(f"  有 branches:         {with_branches}")
    print(f"  有 predefines:       {with_predefines}")
    print(f"  有 runes:            {with_runes}")
    print(f"  visitEveryCheckPoint=true:")
    print(f"    路线:              {visit_true_routes}")
    print(f"    额外路线:           {visit_true_extra}")
    print(f"    合计:              {visit_true_routes + visit_true_extra}")
    print(f"  mapData:             {sizes[0]/1024:,.1f} KB")
    print(f"  routes:              {sizes[1]/1024:,.1f} KB")
    print(f"  extraRoutes:         {sizes[2]/1024:,.1f} KB")
    print(f"  waves:               {sizes[3]/1024:,.1f} KB")
    print(f"  branches:            {sizes[4]/1024:,.1f} KB")
    print(f"  predefines:          {sizes[5]/1024:,.1f} KB")
    print(f"  runes:               {sizes[6]/1024:,.1f} KB")


if __name__ == "__main__":
    build()
