# -*- coding: utf-8 -*-
"""levels · 重建（关卡详细数据）

数据源（本地，无 PRTS、无人工修正）
    levels/obt/**/*.json + levels/activities/**/*.json
    键 = 文件名（与 stages.levelId 对齐）；同文件名先到先得，撞车进记录
    源文件内部的 levelId 不可信（15 个生息演算关卡写的是模板名 [dev]test/level_training），
    只作记录、不作键

输出
    arknights.db 的 levels 表（8 列）
    写库：备份 → DROP TABLE → CREATE → 全量 INSERT（单事务，失败回滚）

列分类
    全部来自数据源：无爬取列、无人工列、无待办

细节处理（既有方案，原样保留）
    mapData      tiles 归一 + 值去重 + grid 重映射 → 4 位定长数组
    routes       dict 去重（含 visitEveryCheckPoint）→ 6/7 位数组；
                 waves / branches 的 routeIndex 同步重映射
    waves        [{preDelay, postDelay, fragments: [{preDelay, actions}]}]
    branches     {分支名: {phases: [{preDelay, actions}]}}
    predefines   [[tokenInsts 9位], [characterInsts 9位], [tokenCards 8位], [characterCards 7位]]
    runes        按 difficultyMask 分组 + 「可选」键 → JSON

写后复核
    行数 / 非空列计数 / 各列体积 / visitEveryCheckPoint=true 计数

跳过条件
    重建结果与库内逐行逐列完全一致 → 不写库、不写记录

用法
    python rebuild.py --dry-run    # 全量处理并报告，不写库、不写记录
    python rebuild.py
"""

import json
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import table_base as tb  # noqa: E402

DATA_ROOT = paths.GAMEDATA
PROGRESS_EVERY = 500
MAX_DETAIL = 200

# 本次重建的统计（进记录）
STATS = {}

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
    """收集关卡 json：先 obt 再 activities，文件名即候选键；同名先到先得。"""
    files = {}
    collisions = []

    def walk(root):
        for dirpath, dirnames, fnames in os.walk(root):
            dirnames.sort()
            for fname in sorted(fnames):
                if not fname.endswith(".json"):
                    continue
                lid = fname.removesuffix(".json")
                full = os.path.join(dirpath, fname)
                if lid in files:
                    collisions.append({"来源": "文件名相同", "键": lid,
                                       "保留": os.path.relpath(files[lid], DATA_ROOT),
                                       "忽略": os.path.relpath(full, DATA_ROOT)})
                else:
                    files[lid] = full

    walk(os.path.join(DATA_ROOT, "levels", "obt"))
    act_root = os.path.join(DATA_ROOT, "levels", "activities")
    if os.path.isdir(act_root):
        walk(act_root)
    return files, collisions


# ─── mapData ──────────────────────────────

def process_map(map_data):
    """tiles 归一化 + 值去重 + grid 重映射，直接输出 4 位定长数组。"""
    if not map_data:
        return None
    grid = map_data.get("map") or []
    height = len(grid)
    width = len(grid[0]) if height > 0 and grid[0] else 0

    seen, old_to_new, deduped = {}, {}, []
    for old_idx, t in enumerate(map_data.get("tiles") or []):
        ht, bt, pm = t.get("heightType"), t.get("buildableType"), t.get("passableMask")
        if isinstance(ht, str):
            ht = _HEIGHT_MAP.get(ht, ht)
        if isinstance(bt, str):
            bt = _BUILD_MAP.get(bt, bt)
        if isinstance(pm, str):
            pm = _PASSABLE_MAP.get(pm, pm)
        fp = json.dumps([t.get("tileKey"), ht, bt, pm], sort_keys=True, ensure_ascii=False)
        if fp in seen:
            old_to_new[old_idx] = seen[fp]
        else:
            new_idx = len(deduped)
            seen[fp] = new_idx
            deduped.append([t.get("tileKey"), ht, bt, pm])
            old_to_new[old_idx] = new_idx

    return {"width": width, "height": height,
            "grid": [[old_to_new.get(idx, idx) for idx in row] for row in grid],
            "tiles": deduped}


# ─── routes ───────────────────────────────

def process_one_route(rt):
    """route → dict。始终含 visitEveryCheckPoint，指纹据此区分几何相同但 visit 不同的路线。"""
    if rt is None:
        return None
    checkpoints = []
    for cp in (rt.get("checkpoints") or []):
        if cp is None:
            continue
        pos, off = cp.get("position"), cp.get("reachOffset")
        checkpoints.append([
            normalize_enum(cp.get("type"), _CHECKPOINT_TYPE_STR),
            cp.get("time"),
            [pos["row"], pos["col"]] if pos else None,
            [off["x"], off["y"]] if off else None,
        ])
    return {
        "motionMode": normalize_enum(rt.get("motionMode"), _MOTION_MODE_STR),
        "startPosition": [rt["startPosition"]["row"], rt["startPosition"]["col"]]
        if rt.get("startPosition") else None,
        "endPosition": [rt["endPosition"]["row"], rt["endPosition"]["col"]]
        if rt.get("endPosition") else None,
        "spawnRandomRange": [rt["spawnRandomRange"]["x"], rt["spawnRandomRange"]["y"]]
        if rt.get("spawnRandomRange") else None,
        "spawnOffset": [rt["spawnOffset"]["x"], rt["spawnOffset"]["y"]]
        if rt.get("spawnOffset") else None,
        "checkpoints": checkpoints,
        "visitEveryCheckPoint": rt.get("visitEveryCheckPoint", False),
    }


def route_to_arr(d):
    """dict → 6/7 位定长数组（visitEveryCheckPoint 仅在 true 时保留第 7 位）。"""
    arr = [d["motionMode"], d["startPosition"], d["endPosition"],
           d["spawnRandomRange"], d["spawnOffset"], d["checkpoints"]]
    if d.get("visitEveryCheckPoint"):
        arr.append(True)
    return arr


def dedup_routes(route_dicts):
    """值去重（JSON 指纹，含 visitEveryCheckPoint），返回去重后列表与索引映射。"""
    seen, old_to_new, deduped = {}, {}, []
    for old_idx, d in enumerate(route_dicts):
        if d is None:
            old_to_new[old_idx] = len(deduped)
            deduped.append(None)
        else:
            fp = json.dumps(d, sort_keys=True, ensure_ascii=False)
            if fp in seen:
                old_to_new[old_idx] = seen[fp]
            else:
                seen[fp] = len(deduped)
                old_to_new[old_idx] = len(deduped)
                deduped.append(d)
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
            fragments.append({"preDelay": frag.get("preDelay", 0.0),
                              "actions": process_actions(frag.get("actions"))})
        out.append({"preDelay": w.get("preDelay", 0.0),
                    "postDelay": w.get("postDelay", 0.0),
                    "fragments": fragments})
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
            phases.append({"preDelay": ph.get("preDelay", 0.0),
                           "actions": process_actions(ph.get("actions"))})
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
    """原始预置单位 → 扁平 dict（characterKey 等从 inst 解包）。"""
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
    """预置单位 → 4 元素数组。空子数组为 []，整列 NULL 表示无。"""
    if not predefines_raw:
        return None

    def xform_inst(item):
        d = _flatten_inst(item)
        if d is None:
            return None
        return [d["alias"], d["characterKey"], d["level"], d["phase"],
                d["skillIndex"], d["mainSkillLvl"], d["uniEquipIds"],
                [item["position"]["row"], item["position"]["col"]],
                normalize_enum(item["direction"], _DIRECTION_STR)]

    def xform_token_card(item):
        d = _flatten_inst(item)
        if d is None:
            return None
        return [d["alias"], d["characterKey"], d["level"], d["phase"],
                d["skillIndex"], d["mainSkillLvl"], d["uniEquipIds"], item["initialCnt"]]

    def xform_char_card(item):
        d = _flatten_inst(item)
        if d is None:
            return None
        return [d["alias"], d["characterKey"], d["level"], d["phase"],
                d["skillIndex"], d["mainSkillLvl"], d["uniEquipIds"]]

    def collect(cat, xform):
        return [x for i in (predefines_raw.get(cat) or []) if (x := xform(i)) is not None]

    return [collect("tokenInsts", xform_inst), collect("characterInsts", xform_inst),
            collect("tokenCards", xform_token_card), collect("characterCards", xform_char_card)]


# ─── 关卡符文 ─────────────────────────────

def _rune_entry(r):
    return {
        "key": r.get("key"),
        "professionMask": r.get("professionMask"),
        "buildableMask": r.get("buildableMask"),
        "blackboard": [{"key": b.get("key"), "value": b.get("value"),
                        "valueStr": b.get("valueStr")}
                       for b in (r.get("blackboard") or [])],
    }


def process_runes(level_data):
    """合并 runes 与 optionalRunes，按 difficultyMask 分组；optionalRunes 为 dict 时存「可选」。"""
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
        result.setdefault(str(dm), []).append(_rune_entry(r))

    if isinstance(opt, dict):
        result["可选"] = {k: [_rune_entry(r) for r in v] for k, v in opt.items()}

    return json.dumps(result, ensure_ascii=False)


# ─── 单关处理 ─────────────────────────────

def process_level(filepath, level_id):
    """处理单个关卡文件。入库键由调用方给定（文件名），源内部的 levelId 只作记录。"""
    try:
        with open(filepath, encoding="utf-8") as f:
            lv = json.load(f)
    except Exception as e:
        return None, str(e)

    map_data = process_map(lv.get("mapData"))
    routes_dicts = [process_one_route(rt) for rt in (lv.get("routes") or [])]
    extra_dicts = [process_one_route(rt) for rt in (lv.get("extraRoutes") or [])]
    routes_dedup, routes_remap = dedup_routes(routes_dicts)
    extra_dedup, extra_remap = dedup_routes(extra_dicts)

    waves_raw = process_waves(lv.get("waves"))
    for wave in waves_raw:
        for frag in wave.get("fragments", []):
            remap_action_route_index(frag.get("actions", []), routes_remap)

    branches_raw = process_branches(lv.get("branches"))
    remap_branch_route_indices(branches_raw, extra_remap)

    predefines_raw = process_predefines(lv.get("predefines"))

    return level_id, {
        "levelId": level_id,
        "内部levelId": lv.get("levelId"),
        "mapData": json.dumps(map_data, ensure_ascii=False) if map_data else None,
        "routes": json.dumps([route_to_arr(r) if r else None for r in routes_dedup],
                             ensure_ascii=False),
        "extraRoutes": json.dumps([route_to_arr(r) if r else None for r in extra_dedup],
                                  ensure_ascii=False) if extra_dedup else None,
        "waves": json.dumps(waves_raw, ensure_ascii=False),
        "branches": json.dumps(branches_raw, ensure_ascii=False) if branches_raw else None,
        "predefines": json.dumps(predefines_raw, ensure_ascii=False) if predefines_raw else None,
        "runes": process_runes(lv),
    }


# ─── 表结构 ───────────────────────────────

CREATE_SQL = """
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
"""

INSERT_COLS = [
    ("levelId", "levelId"),
    ("地图数据", "mapData"),
    ("路线", "routes"),
    ("额外路线", "extraRoutes"),
    ("波次", "waves"),
    ("分支", "branches"),
    ("预置单位", "predefines"),
    ("关卡符文", "runes"),
]
SIZE_COLS = ["地图数据", "路线", "额外路线", "波次", "分支", "预置单位", "关卡符文"]


# ─── 构建 ─────────────────────────────────

def build(old=None, db=None):
    """全部关卡文件 → {levelId: 行}。文件处理失败即抛（中止本次重建）。"""
    files, collisions = collect_json_files()
    if not files:
        raise RuntimeError("levels 下没有可用关卡文件，疑似数据源未就位")

    rows, errors = [], []
    total = len(files)
    start = time.time()
    for i, lid in enumerate(sorted(files), 1):
        if i % PROGRESS_EVERY == 0 or i == total:
            elapsed = time.time() - start
            rate = i / elapsed if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print("  [%d/%d] %.0fs | ETA %.0fs | %s" % (i, total, elapsed, eta, lid))
        rid, row = process_level(files[lid], lid)
        if rid is None:
            errors.append({"文件": os.path.relpath(files[lid], DATA_ROOT), "错误": str(row)})
        else:
            row["源文件"] = os.path.relpath(files[lid], DATA_ROOT)
            rows.append(row)

    if errors:
        raise RuntimeError("%d 个文件处理失败：%s" % (
            len(errors), "; ".join("%s: %s" % (e["文件"], e["错误"]) for e in errors[:5])))

    kept, level_collisions = {}, []
    for row in rows:
        k = row["levelId"]
        if k in kept:
            level_collisions.append({"来源": "levelId 相同", "键": k,
                                     "保留": kept[k]["源文件"], "忽略": row["源文件"]})
        else:
            kept[k] = row

    all_collisions = collisions + level_collisions
    mismatch = [{"键": r["levelId"], "内部levelId": r["内部levelId"], "源文件": r["源文件"]}
                for r in kept.values() if r.get("内部levelId") and r["内部levelId"] != r["levelId"]]
    STATS.update({
        "源文件": {"总数": len(files),
                 "obt": sum(1 for p in files.values() if p.startswith(os.path.join(DATA_ROOT, "levels", "obt"))),
                 "activities": sum(1 for p in files.values()
                                   if not p.startswith(os.path.join(DATA_ROOT, "levels", "obt")))},
        "撞车": len(all_collisions),
        "撞车明细": all_collisions[:MAX_DETAIL],
        "内部levelId 与文件名不一致": {"数量": len(mismatch), "明细": mismatch[:MAX_DETAIL]},
    })
    print("\n  处理成功: %d 行 | 撞车跳过: %d | 内部 levelId 不一致: %d"
          % (len(kept), len(all_collisions), len(mismatch)))
    for c in all_collisions[:10]:
        print("    [%s] %s：保留 %s，忽略 %s" % (c["来源"], c["键"], c["保留"], c["忽略"]))
    return kept


def verify(conn):
    """写后复核：行数 / 非空列计数 / 各列体积 / visitEveryCheckPoint=true 计数。"""
    total = conn.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
    flags = {
        "有额外路线": "额外路线 IS NOT NULL",
        "有分支": "分支 IS NOT NULL",
        "有预置单位": "预置单位 IS NOT NULL",
        "有关卡符文": "关卡符文 != '{}'",
    }
    counts = {k: conn.execute("SELECT COUNT(*) FROM levels WHERE %s" % w).fetchone()[0]
              for k, w in flags.items()}
    sizes = conn.execute("SELECT %s FROM levels"
                         % ", ".join("SUM(LENGTH(%s))" % c for c in SIZE_COLS)).fetchone()
    visit_true = 0
    for routes_raw, extra_raw in conn.execute("SELECT 路线, 额外路线 FROM levels"):
        for raw in (routes_raw, extra_raw):
            if not raw:
                continue
            for arr in json.loads(raw):
                if isinstance(arr, list) and len(arr) >= 7 and arr[6] is True:
                    visit_true += 1
    out = {"行数": total, "非空计数": counts,
           "体积KB": {c: round((s or 0) / 1024, 1) for c, s in zip(SIZE_COLS, sizes)},
           "visitEveryCheckPoint=true": visit_true}
    out.update(STATS)
    print("  复核: 行数 %d | 有额外路线 %d / 有分支 %d / 有预置单位 %d / 有关卡符文 %d"
          % (total, counts["有额外路线"], counts["有分支"], counts["有预置单位"], counts["有关卡符文"]))
    print("  visitEveryCheckPoint=true: %d | 体积 %s"
          % (visit_true, " | ".join("%s %.1fKB" % (c, out["体积KB"][c]) for c in SIZE_COLS)))
    return out


SPEC = tb.RebuildDbSpec(
    table="levels",
    create_sql=CREATE_SQL,
    insert_cols=INSERT_COLS,
    build=build,
    key="levelId",
    verify=verify,
)

if __name__ == "__main__":
    sys.exit(tb.run_rebuild_db(SPEC))
