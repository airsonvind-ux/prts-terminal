#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
path_finder.py — 单段寻路工具（工具二）
========================================
在真实关卡地图（level_map 模型）上计算单段寻路：SPFA 建表 → 平整化 → 导航推演。

寻路两层判定：
  1. 可通行性：passable[row][col] == True（passableMask == 3）的地块地面可通行，
     其余不可通行，排除出搜索空间（来自地图通行视图，无需手动输入）
  2. 障碍物代价：地穴（tile_hole，可通行但代价高）自动识别，SPFA 代价 +1000000，
     敌人一般不主动走入；魔改 Bresenham 连通判定中地穴同样阻断直线
  - 终点允许位于不可通行格/地穴（作为 SPFA 种子格，敌人最后一跳走入即完成）

用法：
  python path_finder.py <levelId> <起点列> <起点行> <终点列> <终点行>
  例：python path_finder.py level_a001_01 7 3 3 3

坐标约定：输入输出均用 (x, y) = (列, 行)，y=0 为最底行（与 level_map 模型一致）。
输出：SPFA 初始链、导航路径段（含换向点）、路径相关格 nextNode 表。
"""

import argparse
import math
import sys
from collections import deque

import level_map

DEFAULT_DB = level_map.DEFAULT_DB
HAZARD_COST = 1000000   # 地穴寻路代价（笔记 2.3）
EPS = 1e-9
CORNER_TOL = 1e-3


def spfa(goal, passable, obstacles, width, height, obstacle_cost):
    """地图感知 SPFA 建表（笔记 2.3）。

    仅在 地图内 且 地面可通行 的格上扩展；障碍物格代价 +obstacle_cost；
    不可通行格不进搜索空间；距离相等不覆盖（先记录保留）。
    返回 (dist, next_node)。
    """
    gx, gy = goal
    if not (0 <= gx < width and 0 <= gy < height):
        raise SystemExit(f"错误：终点 ({gx},{gy}) 在地图外（地图 {width} 列 x {height} 行）")

    dist = {goal: 0}
    next_node = {}
    in_queue = {goal}
    q = deque([goal])
    while q:
        g = q.popleft()
        in_queue.discard(g)
        gx, gy = g
        # 上右下左
        for nb in ((gx, gy + 1), (gx + 1, gy), (gx, gy - 1), (gx - 1, gy)):
            nx, ny = nb
            if not (0 <= nx < width and 0 <= ny < height):
                continue
            if not passable[ny][nx]:
                continue
            cost = obstacle_cost if nb in obstacles else 1
            nd = dist[g] + cost
            if nb not in dist or nd < dist[nb]:
                dist[nb] = nd
                next_node[nb] = g
                if nb not in in_queue:
                    in_queue.add(nb)
                    q.append(nb)
    return dist, next_node


def bresenham_cells(a, c):
    """标准 Bresenham 填涂主路径格（含起终点）。"""
    x0, y0 = a
    x1, y1 = c
    dx = abs(x1 - x0)
    dy = abs(y1 - y0)
    sx = 1 if x1 > x0 else -1
    sy = 1 if y1 > y0 else -1
    err = dx - dy
    x, y = x0, y0
    cells = []
    while True:
        cells.append((x, y))
        if x == x1 and y == y1:
            break
        e2 = 2 * err
        if e2 > -dy:
            err -= dy
            x += sx
        if e2 < dx:
            err += dx
            y += sy
    return cells


def check_cells(a, c):
    """魔改 Bresenham 判定格集合（主路径 + 规则 1 矩形 + 规则 2 斜角格）。"""
    cells = set(bresenham_cells(a, c))
    x0, y0 = a
    x1, y1 = c
    dx = abs(x1 - x0)
    dy = abs(y1 - y0)
    r0, r1 = sorted((y0, y1))
    c0, c1 = sorted((x0, x1))
    if dy == 1:                      # 规则 1 防擦边：行差 1 → 整个矩形
        for r in range(r0, r1 + 1):
            for cc in range(c0, c1 + 1):
                cells.add((cc, r))
    if dx == 1:                      # 规则 1：列差 1 → 整个矩形
        for r in range(r0, r1 + 1):
            for cc in range(c0, c1 + 1):
                cells.add((cc, r))
    path = bresenham_cells(a, c)
    for (px, py), (qx, qy) in zip(path, path[1:]):   # 规则 2 防擦角：对角步进 → 斜角 2 格
        if px != qx and py != qy:
            cells.add((qx, py))
            cells.add((px, qy))
    return cells


def is_clear(a, c, passable, obstacles, width, height):
    """魔改 Bresenham 直线连通判定：判定格集合中任一格 越界 / 不可通行 / 障碍 → 不连通。"""
    for cell in check_cells(a, c):
        x, y = cell
        if not (0 <= x < width and 0 <= y < height):
            return False
        if not passable[y][x]:
            return False
        if cell in obstacles:
            return False
    return True


def flatten(goal, passable, obstacles, width, height, next_node):
    """平整化（笔记 2.4）：行小优先、同行列小优先，逐格拉远。"""
    cells = sorted((c for c in next_node if c != goal), key=lambda c: (c[1], c[0]))
    for a in cells:
        while True:
            b = next_node.get(a)
            c = next_node.get(b)
            if c is None:      # b 已是目标格，a 已直接指向目标
                break
            if is_clear(a, c, passable, obstacles, width, height):
                next_node[a] = c
            else:
                break


def chain_from(start, goal, next_node):
    """回溯 nextNode 链（起点 → 目标）。"""
    path = [start]
    c = start
    while c != goal and c in next_node:
        c = next_node[c]
        path.append(c)
    return path


def describe_dir(d):
    """方向描述：水平/垂直/斜线（斜率 + 与垂直方向夹角）。"""
    dx, dy = d
    if abs(dy) < EPS:
        return '水平' + ('向右' if dx > 0 else '向左')
    if abs(dx) < EPS:
        return '垂直' + ('向上' if dy > 0 else '向下')
    slope = dy / dx
    ang = math.degrees(math.atan2(abs(dx), abs(dy)))
    return '斜线 斜率%.3f 与垂直夹角%.1f°' % (slope, ang)


def navigate(start, goal, next_node, reach_offset=(0, 0)):
    """导航推演（笔记 2.6/2.7）：沿 nextNode 方向的连续几何路径。

    返回 (segments, visited)；segments = [(起点, 终点, 方向描述, 原因)]。
    """
    target_point = (float(goal[0]) + reach_offset[0], float(goal[1]) + reach_offset[1])

    def target_of(cell):
        nn = next_node.get(cell)
        if nn is None or nn == goal:
            return target_point
        return (float(nn[0]), float(nn[1]))

    pos = [float(start[0]), float(start[1])]
    current = start
    seg_start = (pos[0], pos[1])
    seg_desc = None
    segments = []
    visited = {start}

    for _ in range(200):
        target = target_of(current)
        dx = target[0] - pos[0]
        dy = target[1] - pos[1]
        dist_t = math.hypot(dx, dy)
        if dist_t < 0.05:      # 光标进入目标点 0.05 半径（检查点完成）
            segments.append((seg_start, (pos[0], pos[1]), seg_desc, '到达目标点 %s' % fmt_t(target_point)))
            break
        d = (dx / dist_t, dy / dist_t)
        if seg_desc is None:
            seg_desc = describe_dir(d)

        col = int(round(pos[0]))
        row = int(round(pos[1]))
        if d[0] > 0:
            tx = (col + 0.5 - pos[0]) / d[0]
            xsign = 1
        elif d[0] < 0:
            tx = (col - 0.5 - pos[0]) / d[0]
            xsign = -1
        else:
            tx = float('inf')
            xsign = 0
        if d[1] > 0:
            ty = (row + 0.5 - pos[1]) / d[1]
            ysign = 1
        elif d[1] < 0:
            ty = (row - 0.5 - pos[1]) / d[1]
            ysign = -1
        else:
            ty = float('inf')
            ysign = 0

        t = min(tx, ty)
        if math.isinf(t):
            break
        if dist_t <= t:
            q = (pos[0] + d[0] * dist_t, pos[1] + d[1] * dist_t)
            segments.append((seg_start, q, seg_desc, '到达目标点 %s' % fmt_t(target_point)))
            break
        q = (pos[0] + d[0] * t, pos[1] + d[1] * t)

        def near_half(v):
            hv = round(v * 2) / 2
            return abs(v - hv) < CORNER_TOL and (round(v * 2) % 2 == 1)

        if near_half(q[0]) and near_half(q[1]):
            qx, qy = round(q[0] * 2) / 2, round(q[1] * 2) / 2
            new_cell = (col + xsign, row + ysign)
        elif tx <= ty:
            qx, qy = round(q[0] * 2) / 2, q[1]
            new_cell = (col + xsign, row)
        else:
            qx, qy = q[0], round(q[1] * 2) / 2
            new_cell = (col, row + ysign)

        new_target = target_of(new_cell)
        visited.add(new_cell)
        if math.hypot(new_target[0] - target[0], new_target[1] - target[1]) > EPS:
            segments.append((seg_start, (qx, qy), seg_desc,
                             '进入 %s 换向：方向目标 %s → %s' % (fmt(new_cell), fmt_t(target), fmt_t(new_target))))
            pos = [qx + d[0] * 1e-6, qy + d[1] * 1e-6]
            current = new_cell
            seg_start = (pos[0], pos[1])
            seg_desc = None
        else:
            pos = [qx + d[0] * 1e-6, qy + d[1] * 1e-6]
            current = new_cell

    return segments, visited


def fmt(cell):
    return '(%d,%d)' % cell


def fmt_t(p):
    def g(v):
        if abs(v - round(v)) < 1e-9:
            return '%d' % round(v)
        r2 = round(v * 2) / 2
        if abs(v - r2) < 1e-9:
            return '%.1f' % v
        return '%.3f' % v
    return '(%s,%s)' % (g(p[0]), g(p[1]))


def run_path(model, start, goal, reach_offset=(0, 0)):
    """完整寻路流程：SPFA → 平整化 → 导航。返回 (segments, visited, next_node)。"""
    w, h = model["width"], model["height"]
    passable = model["passable"]
    tile_key = model["tile_key"]
    # 障碍物 = 地穴（可通行但代价高）：按 tileKey 自动识别，无需手动输入
    obstacles = {(x, y) for y in range(h) for x in range(w)
                 if passable[y][x] and tile_key[y][x] == 'tile_hole'}
    sx, sy = start
    if not (0 <= sx < w and 0 <= sy < h):
        raise SystemExit(f"错误：起点 ({sx},{sy}) 在地图外")
    if not passable[sy][sx]:
        print(f"注意：起点 ({sx},{sy}) 为不可通行格（光标在不可通行格 → 方向 0，无自主移动）")

    dist, next_node = spfa(goal, passable, obstacles, w, h, HAZARD_COST)
    if start != goal and start not in next_node:
        raise SystemExit(f"错误：起点 ({sx},{sy}) 无法到达终点 ({goal[0]},{goal[1]})（起点不可达或位于孤岛）")

    print(f"地图 {model['levelId']}（{w} 列 x {h} 行），起点 {fmt(start)} → 终点 {fmt(goal)}"
          f"{'，地穴 ' + ', '.join(fmt(o) for o in sorted(obstacles)) if obstacles else ''}")

    print('\n[SPFA 初始链]')
    print(' -> '.join(fmt(c) for c in chain_from(start, goal, next_node)))

    flatten(goal, passable, obstacles, w, h, next_node)

    print('\n[导航推演]')
    segments, visited = navigate(start, goal, next_node, reach_offset)
    for i, (s, e, desc, why) in enumerate(segments, 1):
        print('段%d：%s → %s' % (i, fmt_t(s), fmt_t(e)))
        print('    %s；%s' % (desc if desc else '方向待定', why))

    print('\n[路径相关格 nextNode]（行小优先、同行列小优先）')
    for c in sorted(visited, key=lambda c: (c[1], c[0])):
        if c == goal:
            continue
        print('  %s -> %s' % (fmt(c), fmt(next_node.get(c))))
    print()
    return segments, visited, next_node


def main():
    parser = argparse.ArgumentParser(description="单段寻路工具（地图感知）")
    parser.add_argument("level_id", help="关卡标识")
    parser.add_argument("start", nargs=2, type=int, metavar=("起点列", "起点行"))
    parser.add_argument("goal", nargs=2, type=int, metavar=("终点列", "终点行"))
    parser.add_argument("--db", default=DEFAULT_DB, help="数据库路径")
    args = parser.parse_args()

    start = (args.start[0], args.start[1])
    goal = (args.goal[0], args.goal[1])

    md = level_map.load_level_map(args.db, args.level_id)
    model = level_map.build_model(md, args.level_id)
    run_path(model, start, goal)


if __name__ == '__main__':
    main()
