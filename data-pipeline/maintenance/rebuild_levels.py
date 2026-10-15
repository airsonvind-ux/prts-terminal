"""
PRTS 终端 — levels 表全量重建脚本（维护用）

levels 表采用「重建制」维护：数据源为本地 JSON、没有人工修正、无 PRTS 爬取，
每次数据源更新后整表重建即可。其他含 Wiki 爬取信息的表用「更新制」，不要用重建方式维护。

数据源：levels/obt/**/*.json + levels/activities/**/*.json
（同一 levelId 先到先得，撞车记入更新记录）
重建前备份、失败即中止、单事务写库、结束写 updates/<时间戳>-levels.json。

本脚本合并了旧 build_levels.py + build_remove_keys_levels.py + build_levels_runes.py
三者的职责，一次产出最终入库格式（去键化 + 归一化 + 符文），不再分 build/refine 两阶段。

用法：
    python rebuild_levels.py --dry-run   # 全量处理并报告，不写库、不写记录
    python rebuild_levels.py             # 正式执行
"""

import argparse
import datetime
import json
import os
import sqlite3
import sys
import time

# ─── 路径配置（唯一来源见 data-pipeline/paths.py）─────────────

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import paths  # noqa: E402
import update_common as uc  # noqa: E402
from paths import GAMEDATA as DATA_ROOT, DB_PATH as OUTPUT_DB  # noqa: E402

TABLE = "levels"
SCRIPT_VERSION = 1
MAX_DETAIL = 200      # 记录里明细条数上限
PROGRESS_EVERY = 500  # 处理进度打印间隔
# journal_mode=OFF 会连事务回滚一起关掉（写库失败时无法撤回）；WAL 会写进数据库文件头，
# 这份库随包分发当只读资源用，届时需要目录写权限才能打开，故不用。
JOURNAL_MODE = "MEMORY"

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
    """收集关卡 json：先 levels/obt 再 levels/activities，文件名即候选键。

    同一「文件名」出现多次时保留先者，撞车记入明细（返回 (files, 撞车明细)）。
    遍历顺序显式排序，避免依赖文件系统顺序导致每次重建结果不同。
    """
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

INSERT_COLS = ["levelId", "地图数据", "路线", "额外路线", "波次", "分支", "预置单位", "关卡符文"]
SIZE_COLS = ["地图数据", "路线", "额外路线", "波次", "分支", "预置单位", "关卡符文"]


def count_rows(db_path):
    """库里现有的 levels 行数；库或表不存在返回 None（首次重建）。"""
    if not os.path.exists(db_path):
        return None
    try:
        conn = sqlite3.connect(str(db_path))
        try:
            return conn.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
        finally:
            conn.close()
    except sqlite3.Error:
        return None


def main():
    ap = argparse.ArgumentParser(description="levels 表全量重建")
    ap.add_argument("--dry-run", action="store_true", help="全量处理并报告，不写库、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    args = ap.parse_args()

    paths.ensure_gamedata()
    uc.reconfigure_stdout()
    started = datetime.datetime.now()

    print("====== levels 表全量重建 ======")
    print("数据库  : %s" % OUTPUT_DB)
    print("数据源  : %s" % paths.DATA_SOURCE)
    src_ver = uc.read_source_version(paths.DATA_SOURCE)
    print("源版本  : 提交 %s | 游戏版本 %s | 数据时间 %s"
          % (src_ver["提交"], src_ver["游戏版本"], src_ver["数据时间"]))

    record = {
        "表": TABLE,
        "模式": "全量重建",
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {
            "提交": src_ver["提交"], "提交标题": src_ver["提交标题"],
            "游戏版本": src_ver["游戏版本"], "数据时间": src_ver["数据时间"],
        },
        "脚本": {"文件": os.path.basename(__file__), "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(str(OUTPUT_DB)),
        "源文件": {},
        "撞车": 0,
        "撞车明细": [],
        "行数": {"重建前": count_rows(OUTPUT_DB), "重建后": 0},
        "列统计": {},
        "体积KB": {},
        "备份": None,
        "结果": "未执行",
    }

    # ── 1. 收集源文件 ──
    print("\n------ 1. 收集源文件 ------")
    obt_root = os.path.join(DATA_ROOT, "levels", "obt")
    if not os.path.isdir(obt_root):
        raise SystemExit("数据源缺少 levels/obt，疑似未就位：%s" % obt_root)
    files, collisions = collect_json_files()
    if not files:
        raise SystemExit("levels/obt 下没有可用关卡文件，疑似数据源未就位：%s" % obt_root)
    record["源文件"] = {
        "总数": len(files),
        "obt": sum(1 for p in files.values() if p.startswith(obt_root)),
        "activities": sum(1 for p in files.values() if not p.startswith(obt_root)),
    }
    print("  源文件: 共 %d（obt %d / activities %d）"
          % (record["源文件"]["总数"], record["源文件"]["obt"], record["源文件"]["activities"]))

    # ── 2. 处理 ──
    print("\n------ 2. 处理关卡数据 ------")
    rows, errors = [], []
    total_src = len(files)
    start = time.time()
    for i, lid in enumerate(sorted(files), 1):
        if i % PROGRESS_EVERY == 0 or i == total_src:
            el = time.time() - start
            rate = i / el if el > 0 else 0
            eta = (total_src - i) / rate if rate > 0 else 0
            print("  [%d/%d] %.0fs | ETA %.0fs | %s" % (i, total_src, el, eta, lid))
        rid, row = process_level(files[lid])
        if rid is None:
            errors.append({"文件": os.path.relpath(files[lid], DATA_ROOT), "错误": str(row)})
        else:
            row["源文件"] = os.path.relpath(files[lid], DATA_ROOT)
            rows.append(row)

    if errors:
        record["失败文件"] = errors[:MAX_DETAIL]
        record["结果"] = "中止（%d 个文件处理失败）" % len(errors)
        path = uc.write_record(TABLE, record)
        print("\n!! %d 个文件处理失败，未对数据库做任何修改：" % len(errors))
        for e in errors[:MAX_DETAIL]:
            print("   [%s] %s" % (e["文件"], e["错误"]))
        print("   记录: %s" % path)
        return 1

    # 同一 levelId 被多个文件产出时保留先者（文件名排序在前）
    kept = {}
    level_collisions = []
    for row in rows:
        k = row["levelId"]
        if k in kept:
            level_collisions.append({"来源": "levelId 相同", "键": k,
                                     "保留": kept[k]["源文件"], "忽略": row["源文件"]})
        else:
            kept[k] = row
    rows = list(kept.values())
    all_collisions = collisions + level_collisions
    record["撞车"] = len(all_collisions)
    record["撞车明细"] = all_collisions[:MAX_DETAIL]
    record["行数"]["重建后"] = len(rows)
    print("\n  处理成功: %d 行 | 撞车跳过: %d" % (len(rows), len(all_collisions)))
    for c in all_collisions[:10]:
        print("    [%s] %s：保留 %s，忽略 %s" % (c["来源"], c["键"], c["保留"], c["忽略"]))

    if args.dry_run:
        print("\n(干跑：未写库、未写记录)")
        print("  重建前 %s 行 → 本次会写出 %d 行"
              % (record["行数"]["重建前"], record["行数"]["重建后"]))
        return 0

    # ── 3. 写库（单事务）──
    print("\n------ 3. 写库（单事务）------")
    if args.no_backup:
        print("  (已跳过备份)")
    else:
        bak = uc.backup_db(str(OUTPUT_DB))
        record["备份"] = os.path.basename(bak) if bak else None
        print("  备份: %s" % (record["备份"] or "（未生成，库不存在）"))

    conn = sqlite3.connect(str(OUTPUT_DB), isolation_level=None)
    try:
        conn.execute("PRAGMA journal_mode=%s" % JOURNAL_MODE)
        conn.execute("PRAGMA synchronous=OFF")
        conn.execute("BEGIN IMMEDIATE")
        try:
            conn.execute("DROP TABLE IF EXISTS levels")
            conn.execute(CREATE_SQL)
            conn.executemany(
                "INSERT INTO levels (%s) VALUES (%s)"
                % (", ".join(INSERT_COLS), ", ".join("?" * len(INSERT_COLS))),
                [tuple(row[c] for c in INSERT_COLS) for row in rows],
            )
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            path = uc.write_record(TABLE, record)
            print("\n!! 写库失败并已回滚: %s" % e)
            print("   记录: %s" % path)
            return 1

        # ── 4. 复核 ──
        total = conn.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
        flags = {
            "有额外路线": "额外路线 IS NOT NULL",
            "有分支": "分支 IS NOT NULL",
            "有预置单位": "预置单位 IS NOT NULL",
            "有关卡符文": "关卡符文 != '{}'",
        }
        counts = {k: conn.execute("SELECT COUNT(*) FROM levels WHERE %s" % w).fetchone()[0]
                  for k, w in flags.items()}
        sizes = conn.execute(
            "SELECT %s FROM levels" % ", ".join("SUM(LENGTH(%s))" % c for c in SIZE_COLS)
        ).fetchone()

        # visitEveryCheckPoint 统计（从路线 JSON 中扫描第 7 位）
        visit_true = 0
        for routes_raw, extra_raw in conn.execute("SELECT 路线, 额外路线 FROM levels"):
            for raw in (routes_raw, extra_raw):
                if not raw:
                    continue
                for arr in json.loads(raw):
                    if isinstance(arr, list) and len(arr) >= 7 and arr[6] is True:
                        visit_true += 1
    finally:
        conn.close()

    record["行数"]["重建后"] = total
    record["行数"]["visitEveryCheckPoint=true"] = visit_true
    record["列统计"] = counts
    record["体积KB"] = {c: round((s or 0) / 1024, 1) for c, s in zip(SIZE_COLS, sizes)}
    record["结果"] = "成功"
    path = uc.write_record(TABLE, record)

    elapsed = (datetime.datetime.now() - started).total_seconds()
    print("\n====== 完成（%.1fs）======" % elapsed)
    print("  行数: %s → %d | 撞车跳过: %d" % (record["行数"]["重建前"], total, record["撞车"]))
    print("  有额外路线 %d / 有分支 %d / 有预置单位 %d / 有关卡符文 %d"
          % (counts["有额外路线"], counts["有分支"], counts["有预置单位"], counts["有关卡符文"]))
    print("  visitEveryCheckPoint=true: %d" % visit_true)
    print("  体积: %s" % " | ".join("%s %.1fKB" % (c, record["体积KB"][c]) for c in SIZE_COLS))
    print("  更新记录: %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
