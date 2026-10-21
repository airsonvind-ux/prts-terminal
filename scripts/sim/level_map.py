#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
level_map.py — 关卡地图解读工具（工具一）
=========================================
读取数据库 levels 表 → 输出两部分：

  1. 地图通行视图（地面视角）：'.' 可通行 / '#' 不可通行
     passableMask == 3（全部）的地块地面可通行；== 0（不可）/2（仅飞行）地面不可通行。
     飞行敌人"全图可飞"，本工具只处理地面移动模式。
  2. 敌人路径表：每条路线的起点 → 检查点序列 → 终点
     格式与 query_level 工具一致（motionMode/偏移/检查点类型语义）。

--json 模式输出结构化地图模型（供 path_finder / frame_sim 作为输入）。

用法：
  python level_map.py <levelId>                 # 通行视图 + 敌人路径表
  python level_map.py <levelId> --json          # 输出模型 JSON（stdout）
  python level_map.py <levelId> --json -o map.json  # 保存模型到文件
  python level_map.py <levelId> --db <路径>     # 指定数据库

坐标约定：模型内部与数据库一致 [row][col]，row=0 在最底一行；
视图按行从上到下打印（顶部 y=height-1 在上），每格标注 (x=col, y=row)。
"""

import argparse
import json
import sqlite3
from pathlib import Path

DEFAULT_DB = Path(__file__).resolve().parents[2] / "src-tauri" / "resources" / "arknights.db"

MOTION_MODE_NAMES = {0: "WALK", 1: "FLY", 2: "E_NUM"}
CHECKPOINT_TYPE_NAMES = {
    0: "移动", 1: "停驻·计时", 2: "停驻·全局计时", 3: "停驻·段计时",
    4: "停驻·波计时", 5: "进入传送", 6: "离开传送", 7: "警报",
    8: "巡逻", 9: "引航者试炼休整", 10: "地图偏移移动",
}
OMIT_COORD_TYPES = {1, 2, 3, 4, 5, 7, 9}   # 坐标无意义的检查点类型（填充值不输出）


def load_level_map(db_path, level_id):
    """从数据库 levels 表读取关卡地图数据（JSON 字符串 → dict）。"""
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.cursor()
        cur.execute("SELECT 地图数据 FROM levels WHERE levelId = ?", (level_id,))
        row = cur.fetchone()
    finally:
        conn.close()
    if row is None:
        raise SystemExit(f"错误：未找到关卡 {level_id}")
    return json.loads(row[0])


def load_routes(db_path, level_id):
    """从数据库 levels 表读取路线列表（JSON 数组）。"""
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.cursor()
        cur.execute("SELECT 路线 FROM levels WHERE levelId = ?", (level_id,))
        row = cur.fetchone()
    finally:
        conn.close()
    if row is None or not row[0]:
        return []
    return json.loads(row[0])


def build_model(md, level_id):
    """地图数据 JSON → 地面视角地图模型。

    模型字段：
      levelId   关卡标识
      width     列数
      height    行数
      passable  [row][col] 二维 bool，地面是否可通行（passableMask == 3）
      tile_key  [row][col] 二维 str，每格地块名
      tiles     原始 tile 定义表（供障碍物分类等后续用途）
    """
    width = md["width"]
    height = md["height"]
    grid = md["grid"]      # [row][col] → tiles 索引，grid 按顶部→底部存储（grid[0] = 最上方）
    tiles = md["tiles"]    # [tileKey, heightType, buildableType, passableMask]

    passable = [[False] * width for _ in range(height)]
    tile_key = [[""] * width for _ in range(height)]
    for gi, row_vals in enumerate(grid):
        coord_row = height - 1 - gi          # grid[gi] = 坐标 row = height-1-gi
        for c in range(width):
            idx = row_vals[c]
            key, _ht, _bt, mask = tiles[idx]
            tile_key[coord_row][c] = key
            passable[coord_row][c] = (mask == 3)   # 地面视角：仅 mask==3（全部）可通行

    return {
        "levelId": level_id,
        "width": width,
        "height": height,
        "passable": passable,
        "tile_key": tile_key,
        "tiles": tiles,
    }


def print_view(model):
    """打印地图通行视图（地面视角）：'.' 可通行 / '#' 不可通行 / 'H' 地穴（可通行但代价高）。"""
    w, h = model["width"], model["height"]
    passable = model["passable"]
    tile_key = model["tile_key"]
    holes = [(x, y) for y in range(h) for x in range(w)
             if tile_key[y][x] == 'tile_hole' and passable[y][x]]

    print(f"地图 {model['levelId']}（{w} 列 x {h} 行）通行视图：'.' 可通行 / '#' 不可通行 / 'H' 地穴")
    print("     " + " ".join(str(x) for x in range(w)) + "   (列 x)")
    for y in range(h - 1, -1, -1):          # 顶部（坐标 y=height-1）在上
        cells = []
        for x in range(w):
            if not passable[y][x]:
                cells.append('#')
            elif tile_key[y][x] == 'tile_hole':
                cells.append('H')
            else:
                cells.append('.')
        print(f"  {y:2d}  " + " ".join(cells) + f"   (行 y={y})")

    if holes:
        print(f"\n地穴（tile_hole，可通行但寻路代价高，敌人一般不主动走入）："
              + ", ".join(f"({x},{y})" for x, y in holes))


def _fmt_float(v):
    return f"{v:.1f}"


def _is_zero_offset(off):
    return abs(off[0]) < 1e-9 and abs(off[1]) < 1e-9


def _pos_from_json(v):
    """[row, col] → (row, col)"""
    if isinstance(v, list) and len(v) >= 2:
        return int(v[0]), int(v[1])
    return 0, 0


def _offset_from_json(v):
    """[x, y] → (x, y)"""
    if isinstance(v, list) and len(v) >= 2:
        return float(v[0]), float(v[1])
    return 0.0, 0.0


def fmt_route(idx, route):
    """单条路线格式化，格式与 query_level 一致。

    route = [motionMode, startPosition, endPosition, spawnRandomRange,
             spawnOffset, checkpoints, (visitEveryCheckPoint)]
    checkpoint = [type, time, [row, col], [x, y]]
    """
    if route is None:
        return f"{idx}(null)"
    if not isinstance(route, list):
        return f"{idx}(?)"

    motion_mode = route[0] if route and isinstance(route[0], int) else 0
    if motion_mode == 2:                     # E_NUM：无意义路线
        return f"{idx}(E_NUM)"
    mode_str = MOTION_MODE_NAMES.get(motion_mode, f"mode{motion_mode}")

    start_row, start_col = _pos_from_json(route[1]) if len(route) > 1 else (0, 0)
    end_row, end_col = _pos_from_json(route[2]) if len(route) > 2 else (0, 0)
    spawn_random = _offset_from_json(route[3]) if len(route) > 3 else (0.0, 0.0)
    spawn_offset = _offset_from_json(route[4]) if len(route) > 4 else (0.0, 0.0)
    checkpoints = route[5] if len(route) > 5 and isinstance(route[5], list) else []

    parts = []
    start_extras = []
    if not _is_zero_offset(spawn_offset):
        start_extras.append(f"spawnOffset=({_fmt_float(spawn_offset[0])},{_fmt_float(spawn_offset[1])})")
    if not _is_zero_offset(spawn_random):
        start_extras.append(f"spawnRandomRange=({_fmt_float(spawn_random[0])},{_fmt_float(spawn_random[1])})")
    if start_extras:
        parts.append(f"[({start_col},{start_row}),{','.join(start_extras)}]")
    else:
        parts.append(f"[({start_col},{start_row})]")

    for cp in checkpoints:
        if not isinstance(cp, list) or len(cp) < 4:
            continue
        cp_type = cp[0] if isinstance(cp[0], int) else 0
        cp_time = float(cp[1]) if isinstance(cp[1], (int, float)) else 0.0
        cp_row, cp_col = _pos_from_json(cp[2])
        cp_offset = _offset_from_json(cp[3])

        is_default = cp_type == 0 and abs(cp_time) < 1e-9 and _is_zero_offset(cp_offset)
        if is_default:
            parts.append(f"({cp_col},{cp_row})")
        else:
            extras = []
            if cp_type != 0:
                extras.append(CHECKPOINT_TYPE_NAMES.get(cp_type, f"type{cp_type}"))
            if abs(cp_time) > 1e-9:
                extras.append(f"{_fmt_float(cp_time)}s")
            if not _is_zero_offset(cp_offset):
                extras.append(f"offset=({_fmt_float(cp_offset[0])},{_fmt_float(cp_offset[1])})")
            if cp_type in OMIT_COORD_TYPES:
                cp_str = f"[{','.join(extras)}]"
            elif not extras:
                cp_str = f"[({cp_col},{cp_row})]"
            else:
                cp_str = f"[({cp_col},{cp_row}),{','.join(extras)}]"
            parts.append(cp_str)

    parts.append(f"[({end_col},{end_row})]")
    line = f"{idx}({mode_str}):{'→'.join(parts)}"
    if len(route) > 6 and route[6] is True:
        line += " [visitEveryCheckPoint]"
    return line


def print_routes(routes):
    """打印敌人路径表。"""
    print("\n══════ 敌人路径表 ══════")
    if not routes:
        print("（无路线）")
        return
    for idx, route in enumerate(routes):
        print(fmt_route(idx, route))


def main():
    parser = argparse.ArgumentParser(description="关卡地图解读工具")
    parser.add_argument("level_id", help="关卡标识，如 level_a001_01")
    parser.add_argument("--db", default=DEFAULT_DB, help="数据库路径")
    parser.add_argument("--json", action="store_true", help="输出地图模型 JSON")
    parser.add_argument("-o", "--output", help="JSON 输出到文件（配合 --json）")
    args = parser.parse_args()

    md = load_level_map(args.db, args.level_id)
    model = build_model(md, args.level_id)

    if args.json:
        text = json.dumps(model, ensure_ascii=False)
        if args.output:
            with open(args.output, "w", encoding="utf-8") as f:
                f.write(text)
            print(f"已写入 {args.output}")
        else:
            print(text)
    else:
        print_view(model)
        print_routes(load_routes(args.db, args.level_id))


if __name__ == "__main__":
    main()
