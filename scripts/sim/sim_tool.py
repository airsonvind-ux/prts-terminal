#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""sim_tool.py — 寻路模拟工具（测试版）

基于关卡 ID + 路线序号自动完成该路线的寻路模拟；支持自定义路线、
强制位移注入（推拉 / 阻挡偏移）、避障相位参数、时间轴参数。

用法：
  python sim_tool.py <levelId> <routeIndex> [选项]

选项：
  --k K              避障相位参数，默认 1（出生帧下一帧 = 避障第 0 帧）
  --speed S          理论移速（格/秒），默认 1.0（= 属性移速 × 关卡倍率 0.5）
  --start x,y        覆盖起点（可选）
  --checkpoints DSL  自定义检查点序列（不指定则用数据库路线）
  --inject "a;b"     强制位移注入（见 README / 文件头）
  --obstacle "x,y;x,y"  额外障碍物格（装置），默认无
  --t-global N       全局时间轴起点（相对帧，默认 0 = f0）
  --t-fragment N     段时间轴起点（相对帧，默认 0 = f0）
  --t-wave N         波时间轴起点（相对帧，默认 0 = f0）
  --log PATH         帧级 log 输出路径；不指定则输出到脚本旁 sim_logs/ 目录
  --no-log           不输出帧级 log
  --max-frames N     模拟帧上限，默认 12000

注入语法（分号分隔多个）：
  push@f:dir=(dx,dy),dist=D,dur=N     推拉：f 帧起匀速位移 D 格，历时 N 帧
  block@f:at=(gx,gy),dur=N             阻挡：f 帧按阻挡位几何计算偏移
                                     （d<0.5 径向推到 0.5；0.5~0.707 自然阻挡无偏移；
                                     >0.707 报错），随后 N 帧阻挡期
  block@f:pos=(x,y),dur=N             阻挡：f 帧坐标直接修正到 pos（高级模式，
                                     跳过计算；与 at 互斥），随后 N 帧阻挡期

检查点 DSL（分号分隔，参数内用逗号）：
  MOVE(x,y[,ox,oy])  WAIT(s)  WAIT_GLOBAL(s)  WAIT_FRAGMENT(s)
  WAIT_WAVE(s)  DISAPPEAR()  APPEAR(x,y)  END(x,y)

坐标约定：(x, y) = (列, 行)，y=0 为最底行。所有帧号为相对帧（f0 = 生成帧）。
"""
import argparse
import json
import math
import os
import re
import sqlite3
import sys
from collections import deque
from pathlib import Path

import numpy as np

F = np.float32
# 帧时长字面量对齐 sim_all.py 的 F(0.0333333)：与 1/30 差 2ulp，
# 在 WAIT 20s（600 帧）处跨越完成边界，必须与验证基准一致
# （否则 R8 停驻差 1 帧导致相位错开、入坑解消失）
DELTA = F(0.0333333)
FPS = 30
EPS = F(1e-6)
GOAL_R = F(0.05)
STEERING = F(8.0)
MAX_STEER = F(10.0)
HALF_BODY = F(0.2)
HAZARD_COST = 1000000

DEFAULT_DB = Path(__file__).resolve().parents[2] / "src-tauri" / "resources" / "arknights.db"

# ═══════════════════════════════════════════════════════
# 一、数据层：数据库读取与模型构建
# ═══════════════════════════════════════════════════════

def load_map(db_path, level_id):
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.cursor()
        cur.execute("SELECT 地图数据 FROM levels WHERE levelId = ?", (level_id,))
        row = cur.fetchone()
        if row is None:
            raise SystemExit(f"错误：未找到关卡 {level_id}")
        md = json.loads(row[0])
        width, height = md["width"], md["height"]
        grid, tiles = md["grid"], md["tiles"]
        passable = [[False] * width for _ in range(height)]
        tile_key = [[""] * width for _ in range(height)]
        for gi, row_vals in enumerate(grid):
            coord_row = height - 1 - gi          # grid[gi] = 坐标 row = height-1-gi
            for c in range(width):
                key, _ht, _bt, mask = tiles[row_vals[c]]
                tile_key[coord_row][c] = key
                passable[coord_row][c] = (mask == 3)
        return {"levelId": level_id, "width": width, "height": height,
                "passable": passable, "tile_key": tile_key, "tiles": tiles}
    finally:
        conn.close()


def load_route(db_path, level_id, route_index):
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.cursor()
        cur.execute("SELECT 路线 FROM levels WHERE levelId = ?", (level_id,))
        row = cur.fetchone()
    finally:
        conn.close()
    if row is None or not row[0]:
        raise SystemExit(f"错误：关卡 {level_id} 无路线数据")
    routes = json.loads(row[0])
    if not (0 <= route_index < len(routes)):
        raise SystemExit(f"错误：路线序号 {route_index} 越界（共 {len(routes)} 条）")
    return routes[route_index]


# ═══════════════════════════════════════════════════════
# 二、检查点模型：数据库路线 / DSL 统一归一
# ═══════════════════════════════════════════════════════

# 检查点类型映射（数据库整数 → 内部类型）
_CP_TYPE = {0: "MOVE", 1: "WAIT", 2: "WAIT_GLOBAL", 3: "WAIT_FRAGMENT",
            4: "WAIT_WAVE", 5: "DISAPPEAR", 6: "APPEAR", 7: "ALERT",
            8: "PATROL", 9: "BOSSRUSH", 10: "MAP_OFFSET"}
_SUPPORTED = {"MOVE", "WAIT", "WAIT_GLOBAL", "WAIT_FRAGMENT", "WAIT_WAVE",
              "DISAPPEAR", "APPEAR", "END"}


def _norm_checkpoint(cp):
    """内部检查点模型：{type, time, pos, offset}"""
    return {"type": cp["type"], "time": cp.get("time", 0.0),
            "pos": cp.get("pos"), "offset": cp.get("offset")}


def parse_db_route(route):
    """数据库路线数组 → (起点, 终点, 检查点列表, 元信息)"""
    motion = route[0]
    if motion == 1:
        raise SystemExit("错误：FLY 路线暂不支持（飞行移动未实现）")
    if motion == 2:
        raise SystemExit("错误：E_NUM 路线无意义")
    start_row, start_col = route[1][0], route[1][1]
    end_row, end_col = route[2][0], route[2][1]
    spawn_offset = (float(route[4][0]), float(route[4][1])) if len(route) > 4 else (0.0, 0.0)
    checkpoints = []
    for cp in route[5] if len(route) > 5 else []:
        cp_type = _CP_TYPE.get(cp[0])
        if cp_type is None:
            raise SystemExit(f"错误：未知检查点类型 {cp[0]}")
        if cp_type == "ALERT":
            continue                       # 纯演出，忽略
        if cp_type == "PATROL":
            cp_type = "MOVE"               # 同 MOVE 完成条件
        if cp_type not in _SUPPORTED:
            raise SystemExit(f"错误：检查点类型 {cp_type}（{cp[0]}）暂不支持")
        time = float(cp[1])
        row, col = cp[2][0], cp[2][1]
        offset = (float(cp[3][0]), float(cp[3][1]))
        if cp_type == "MOVE":
            checkpoints.append(_norm_checkpoint(
                {"type": "MOVE", "time": 0.0, "pos": (col, row), "offset": offset}))
        elif cp_type in ("WAIT", "WAIT_GLOBAL", "WAIT_FRAGMENT", "WAIT_WAVE"):
            checkpoints.append(_norm_checkpoint({"type": cp_type, "time": time}))
        elif cp_type == "DISAPPEAR":
            checkpoints.append(_norm_checkpoint({"type": "DISAPPEAR"}))
        elif cp_type == "APPEAR":
            checkpoints.append(_norm_checkpoint({"type": "APPEAR", "pos": (col, row)}))
    return (start_col, start_row), (end_col, end_row), spawn_offset, checkpoints


_DSL_RE = re.compile(r"^(\w+)\(([^)]*)\)$")


def parse_dsl(dsl):
    """自定义检查点 DSL → 检查点列表（检查点间以 ; 分隔，参数内用逗号）"""
    out = []
    for token in dsl.split(";"):
        token = token.strip()
        m = _DSL_RE.match(token)
        if not m:
            raise SystemExit(f"错误：DSL 语法错误：{token}")
        kind, args = m.group(1), m.group(2)
        parts = [p.strip() for p in args.split(",") if p.strip() != ""]
        if kind in ("MOVE", "PATROL"):
            if len(parts) not in (2, 4):
                raise SystemExit(f"错误：{kind} 需要 2 或 4 个参数：{token}")
            pos = (int(parts[0]), int(parts[1]))
            offset = (float(parts[2]), float(parts[3])) if len(parts) == 4 else (0.0, 0.0)
            out.append(_norm_checkpoint({"type": "MOVE", "pos": pos, "offset": offset}))
        elif kind in ("WAIT", "WAIT_GLOBAL", "WAIT_FRAGMENT", "WAIT_WAVE"):
            if len(parts) != 1:
                raise SystemExit(f"错误：{kind} 需要 1 个参数：{token}")
            out.append(_norm_checkpoint({"type": kind, "time": float(parts[0])}))
        elif kind == "DISAPPEAR":
            out.append(_norm_checkpoint({"type": "DISAPPEAR"}))
        elif kind == "APPEAR":
            if len(parts) != 2:
                raise SystemExit(f"错误：APPEAR 需要 2 个参数：{token}")
            out.append(_norm_checkpoint({"type": "APPEAR", "pos": (int(parts[0]), int(parts[1]))}))
        elif kind == "END":
            if len(parts) != 2:
                raise SystemExit(f"错误：END 需要 2 个参数：{token}")
            out.append(_norm_checkpoint({"type": "END", "pos": (int(parts[0]), int(parts[1]))}))
        else:
            raise SystemExit(f"错误：未知检查点类型：{kind}")
    return out


# ═══════════════════════════════════════════════════════
# 三、注入事件解析
# ═══════════════════════════════════════════════════════

_INJ_RE = re.compile(r"^(\w+)@(\d+):(.*)$")
_KV_RE = re.compile(r"(\w+)=\(([^)]*)\)|(\w+)=([^,]+)")


def _parse_kv(s):
    kv = {}
    for m in _KV_RE.finditer(s):
        if m.group(1):
            vals = [v.strip() for v in m.group(2).split(",")]
            kv[m.group(1)] = tuple(float(v) for v in vals) if len(vals) > 1 else float(vals[0])
        else:
            kv[m.group(3)] = float(m.group(4))
    return kv


def parse_injections(spec):
    """注入字符串 → 注入事件列表（按起始帧排序）"""
    events = []
    for token in spec.split(";"):
        token = token.strip()
        if not token:
            continue
        m = _INJ_RE.match(token)
        if not m:
            raise SystemExit(f"错误：注入语法错误：{token}")
        kind, f, body = m.group(1), int(m.group(2)), m.group(3)
        kv = _parse_kv(body)
        if kind == "push":
            if "dir" not in kv or "dist" not in kv or "dur" not in kv:
                raise SystemExit(f"错误：push 需要 dir/dist/dur：{token}")
            dx, dy = kv["dir"]
            l = np.sqrt(F(dx) * F(dx) + F(dy) * F(dy))
            if l < 1e-9:
                raise SystemExit(f"错误：push 方向为零向量：{token}")
            events.append({"kind": "push", "f": f, "dir": (dx / l, dy / l),
                           "dist": kv["dist"], "dur": int(kv["dur"])})
        elif kind == "block":
            if "dur" not in kv or ("at" not in kv and "pos" not in kv):
                raise SystemExit(f"错误：block 需要 dur 与 at（或 pos）：{token}")
            if "at" in kv and "pos" in kv:
                raise SystemExit(f"错误：block 的 at 与 pos 互斥：{token}")
            events.append({"kind": "block", "f": f,
                           "at": kv.get("at"),      # 阻挡位（部署格）→ 自动计算偏移
                           "pos": kv.get("pos"),    # 直接指定偏移后坐标（高级）
                           "dur": int(kv["dur"])})
        else:
            raise SystemExit(f"错误：未知注入类型：{kind}")
    events.sort(key=lambda e: e["f"])
    for i in range(len(events) - 1):
        if events[i]["f"] == events[i + 1]["f"]:
            raise SystemExit("错误：注入事件起始帧重叠")
    return events


# ═══════════════════════════════════════════════════════
# 四、寻路层：SPFA + 平整化 + nextNode（静默）
# ═══════════════════════════════════════════════════════

def spfa(goal, passable, obstacles, width, height):
    dist = {goal: 0}
    next_node = {}
    in_queue = {goal}
    q = deque([goal])
    while q:
        g = q.popleft()
        in_queue.discard(g)
        gx, gy = g
        for nb in ((gx, gy + 1), (gx + 1, gy), (gx, gy - 1), (gx - 1, gy)):
            nx, ny = nb
            if not (0 <= nx < width and 0 <= ny < height):
                continue
            if not passable[ny][nx]:
                continue
            cost = HAZARD_COST if nb in obstacles else 1
            nd = dist[g] + cost
            if nb not in dist or nd < dist[nb]:
                dist[nb] = nd
                next_node[nb] = g
                if nb not in in_queue:
                    in_queue.add(nb)
                    q.append(nb)
    return dist, next_node


def bresenham_cells(a, c):
    x0, y0 = a
    x1, y1 = c
    dx, dy = abs(x1 - x0), abs(y1 - y0)
    sx, sy = 1 if x1 > x0 else -1, 1 if y1 > y0 else -1
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
    cells = set(bresenham_cells(a, c))
    x0, y0 = a
    x1, y1 = c
    dx, dy = abs(x1 - x0), abs(y1 - y0)
    r0, r1 = sorted((y0, y1))
    c0, c1 = sorted((x0, x1))
    if dy == 1 or dx == 1:                 # 防擦边：行差或列差 1 → 整个矩形
        for r in range(r0, r1 + 1):
            for cc in range(c0, c1 + 1):
                cells.add((cc, r))
    path = bresenham_cells(a, c)
    for (px, py), (qx, qy) in zip(path, path[1:]):   # 防擦角：对角步进 → 斜角 2 格
        if px != qx and py != qy:
            cells.add((qx, py))
            cells.add((px, qy))
    return cells


def is_clear(a, c, passable, obstacles, width, height):
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
    cells = sorted((c for c in next_node if c != goal), key=lambda c: (c[1], c[0]))
    for a in cells:
        while True:
            b = next_node.get(a)
            c = next_node.get(b)
            if c is None:
                break
            if is_clear(a, c, passable, obstacles, width, height):
                next_node[a] = c
            else:
                break


def build_nav_map(model, goal, obstacles):
    """目标格 → nextNode 表（含平整化）。"""
    passable = model["passable"]
    w, h = model["width"], model["height"]
    _, next_node = spfa(goal, passable, obstacles, w, h)
    flatten(goal, passable, obstacles, w, h, next_node)
    return next_node


# ═══════════════════════════════════════════════════════
# 五、模拟层
# ═══════════════════════════════════════════════════════

def cell32(x, y):
    return (int(round(float(x))), int(round(float(y))))


def unit32(vx, vy):
    l = np.sqrt(vx * vx + vy * vy)
    if l < 1e-6:
        return None
    return (vx / l, vy / l)


def clamp32(vx, vy, maxlen):
    l = np.sqrt(vx * vx + vy * vy)
    if l <= maxlen or l < 1e-6:
        return (vx, vy)
    k = maxlen / l
    return (vx * k, vy * k)


def project32(a, given):
    if given is None:
        return a
    g = unit32(given[0], given[1])
    if g is None:
        return a
    proj = a[0] * g[0] + a[1] * g[1]
    return (a[0] - proj * g[0], a[1] - proj * g[1])


def avoid32(model, obstacles, cursor, foot, given):
    """避障力计算（含光标不可通行格 → 最近可通行格方向）。"""
    W, H = model["width"], model["height"]
    passable = model["passable"]
    cx = int(round(float(cursor[0])))
    cy = int(round(float(cursor[1])))
    if not (0 <= cx < W and 0 <= cy < H) or not passable[cy][cx]:
        best_dir = None
        best_d = None
        order = ((0, 1), (1, 1), (1, 0), (1, -1), (0, -1), (-1, -1), (-1, 0), (-1, 1))
        for dx, dy in order:
            ox, oy = cx + dx, cy + dy
            if 0 <= ox < W and 0 <= oy < H and passable[oy][ox] and (ox, oy) not in obstacles:
                d = np.sqrt(F(dx) * F(dx) + F(dy) * F(dy))
                if best_d is None or d < best_d:
                    best_d = d
                    best_dir = (F(dx), F(dy))
        if best_dir is None:
            return (F(0), F(0))
        l = np.sqrt(best_dir[0] * best_dir[0] + best_dir[1] * best_dir[1])
        return project32((best_dir[0] / l, best_dir[1] / l), given)
    push_x = F(0)
    push_y = F(0)
    for dx in (-1, 0, 1):
        for dy in (-1, 0, 1):
            if dx == 0 and dy == 0:
                continue
            ox, oy = cx + dx, cy + dy
            if not (0 <= ox < W and 0 <= oy < H):
                continue
            if passable[oy][ox] and (ox, oy) not in obstacles:
                continue
            if dx < 0:
                nx = foot[0] - HALF_BODY
            elif dx > 0:
                nx = foot[0] + HALF_BODY
            else:
                nx = foot[0]
            ny = foot[1]
            fdx, fdy = F(dx), F(dy)
            app_x = np.maximum((nx - F(cx)) * fdx, F(0))
            app_y = np.maximum((ny - F(cy)) * fdy, F(0))
            eff_x = (app_x - F(0.25)) * abs(fdx)
            eff_y = (app_y - F(0.25)) * abs(fdy)
            if dx != 0 and dy != 0:
                if eff_x > F(0) and eff_y > F(0):
                    avg = (eff_x + eff_y) / F(2)
                    push_x += -avg * fdx
                    push_y += -avg * fdy
            else:
                if eff_x > F(0):
                    push_x += -eff_x * fdx
                if eff_y > F(0):
                    push_y += -eff_y * fdy
    l = np.sqrt(push_x * push_x + push_y * push_y)
    if l < EPS:
        return (F(0), F(0))
    return project32((push_x / l, push_y / l), given)


class SimResult:
    def __init__(self):
        self.events = []          # [{f, text}]
        self.frames = []          # 帧记录（JSONL 用）
        self.cp_done = []         # 检查点完成帧
        self.end_frame = None     # 终止帧
        self.end_reason = None    # 终止原因


class Simulator:
    def __init__(self, model, obstacles, start, spawn_offset, checkpoints, end,
                 k=1, theo=1.0, t_global=0, t_fragment=0, t_wave=0,
                 max_frames=12000):
        self.model = model
        self.obstacles = obstacles
        self.checkpoints = checkpoints
        self.end = end
        self.k = k
        self.theo = F(theo)
        self.t_off = {"WAIT_GLOBAL": t_global, "WAIT_FRAGMENT": t_fragment,
                      "WAIT_WAVE": t_wave}
        self.max_frames = max_frames
        self.nav_maps = {}

        # 状态
        self.entity = [F(start[0] + spawn_offset[0]), F(start[1] + spawn_offset[1])]
        self.cursor = [F(start[0]), F(start[1])]
        self.spawn_offset = (F(spawn_offset[0]), F(spawn_offset[1]))
        self.inertia = (F(0), F(0))
        self.avoid = (F(0), F(0))
        self.nav = 0
        self.cp_idx = 0
        first = checkpoints[0] if checkpoints else None
        self.wait_left = F(first["time"]) if first and first["type"] == "WAIT" else F(0)
        self.disappeared = False
        self.last_cell = cell32(self.cursor[0], self.cursor[1])
        self.result = SimResult()

    # ── 辅助 ──

    def goal_point(self, pos, offset=(0.0, 0.0)):
        return (F(pos[0] + offset[0]), F(pos[1] + offset[1]))

    def nav_of(self, goal):
        if goal not in self.nav_maps:
            self.nav_maps[goal] = build_nav_map(self.model, goal, self.obstacles)
        return self.nav_maps[goal]

    def current_cp(self):
        if self.cp_idx < len(self.checkpoints):
            return self.checkpoints[self.cp_idx]
        return {"type": "END", "pos": self.end}

    def target_of(self, cell):
        """给定方向目标：nextNode 中心；nextNode 即目标格/无 → 目标点。"""
        cp = self.current_cp()
        if cp["type"] in ("MOVE", "END"):
            goal = cp["pos"] if cp["type"] == "MOVE" else self.end
            nn = self.nav_of(goal).get(cell)
            if nn is None or nn == goal:
                if cp["type"] == "MOVE":
                    return self.goal_point(goal, cp.get("offset") or (0.0, 0.0))
                return self.goal_point(goal)
            return (F(nn[0]), F(nn[1]))
        return None

    def given_direction(self):
        cp = self.current_cp()
        if cp["type"] in ("MOVE", "END"):
            t = self.target_of(cell32(self.cursor[0], self.cursor[1]))
            dx = t[0] - self.cursor[0]
            dy = t[1] - self.cursor[1]
            dl = np.sqrt(dx * dx + dy * dy)
            if dl > EPS:
                return (F(dx / dl), F(dy / dl))
        return None          # 停驻/消失等 → 方向 0（滑行或静止）

    # ── 检查点判定（每帧移动后执行）──

    def check_cp(self, f):
        cp = self.current_cp()
        t = cp["type"]
        if t == "MOVE":
            gp = self.goal_point(cp["pos"], cp.get("offset") or (0.0, 0.0))
            if np.sqrt((self.cursor[0] - gp[0]) ** 2 + (self.cursor[1] - gp[1]) ** 2) < GOAL_R:
                self.finish_cp(f, f"MOVE{cp['pos']}")
                return
            # 卡墙跳检查点：光标进入不可通行格
            cx, cy = cell32(self.cursor[0], self.cursor[1])
            if (not (0 <= cx < self.model["width"] and 0 <= cy < self.model["height"])
                    or not self.model["passable"][cy][cx]):
                self.finish_cp(f, "MOVE 卡墙跳")
                return
        elif t == "WAIT":
            self.wait_left -= DELTA
            if self.wait_left <= F(0):
                self.finish_cp(f, "WAIT")
        elif t in ("WAIT_GLOBAL", "WAIT_FRAGMENT", "WAIT_WAVE"):
            if f >= self.t_off[t] + int(cp["time"] * FPS):
                self.finish_cp(f, t)
        elif t == "END":
            if np.sqrt((self.cursor[0] - F(self.end[0])) ** 2
                           + (self.cursor[1] - F(self.end[1])) ** 2) < GOAL_R:
                self.result.end_reason = "抵达终点"
                self.result.end_frame = f
        elif t == "DISAPPEAR":
            self.disappeared = True
            self.finish_cp(f, "DISAPPEAR")
        elif t == "APPEAR":
            pos = cp["pos"]
            self.entity = [F(pos[0] + self.spawn_offset[0]), F(pos[1] + self.spawn_offset[1])]
            self.cursor = [F(pos[0]), F(pos[1])]
            self.disappeared = False
            self.finish_cp(f, f"APPEAR{pos}")

    def finish_cp(self, f, label):
        self.result.cp_done.append(f)
        self.result.events.append({"f": f, "text": f"检查点完成 {label}"})
        self.cp_idx += 1
        cp = self.current_cp()
        if cp["type"] == "WAIT":
            self.wait_left = F(cp["time"])
            self.result.events.append({"f": f, "text": f"停驻开始 {cp['time']}s"})

    # ── 入坑检测 ──

    def check_hole(self, f):
        hx, hy = cell32(self.entity[0], self.entity[1])
        if (0 <= hx < self.model["width"] and 0 <= hy < self.model["height"]
                and self.model["tile_key"][hy][hx] == "tile_hole"):
            self.result.end_reason = f"入坑：实体坐标进入地穴格 ({hx},{hy})"
            self.result.end_frame = f
            return True
        return False

    # ── 主循环 ──

    def run(self, injections):
        res = self.result
        res.events.append({"f": 0, "text": f"生成于 ({self.entity[0]:.3f},{self.entity[1]:.3f})"
                          f" [格{self.last_cell}]"})
        if self.spawn_offset != (F(0), F(0)):
            res.events.append({"f": 0, "text": f"生成偏移 {self.spawn_offset}"})

        inj_idx = 0
        active = None            # 激活的注入事件
        inj_rem = 0              # 注入剩余帧（push 持续 / block 冻结段）

        for f in range(0, self.max_frames):
            if f == 0:
                self.record_frame(f, None, None, None, "生成帧")
                continue

            # 注入激活：到达起始帧（按序）
            if active is None and inj_idx < len(injections) and f == injections[inj_idx]["f"]:
                active = injections[inj_idx]
                inj_idx += 1
                if active["kind"] == "push":
                    inj_rem = active["dur"]
                    res.events.append({"f": f, "text": f"推拉开始：方向{active['dir']} "
                                       f"距离{active['dist']} 持续{active['dur']}帧"})
                else:
                    inj_rem = active["dur"]

            # 注入处理
            if active is not None:
                if active["kind"] == "push":
                    self.frame_push(f, active, inj_rem)
                    inj_rem -= 1
                    if inj_rem <= 0:
                        res.events.append({"f": f, "text": "推拉结束"})
                        active = None
                    if res.end_reason:
                        break
                    continue
                else:  # block
                    if inj_rem == active["dur"]:
                        # ── 偏移帧：先执行正常移动判定（nav 照常、避障重算、
                        # 位移），得到该帧最终坐标后，再以该坐标做偏移修正 ──
                        self.nav += 1
                        given = self.given_direction()
                        if (self.nav - self.k) % 3 == 0:
                            foot = (self.entity[0], self.entity[1] - F(0.2))
                            self.avoid = avoid32(self.model, self.obstacles,
                                                 self.cursor, foot, given)
                        inert_len = np.sqrt(self.inertia[0] * self.inertia[0] + self.inertia[1] * self.inertia[1])
                        if self.avoid != (F(0), F(0)):
                            coef = np.maximum(inert_len / self.theo, F(0.5))
                            av = (self.avoid[0] * coef, self.avoid[1] * coef)
                        else:
                            av = (F(0), F(0))
                        if given is not None:
                            sb = (given[0] * self.theo - self.inertia[0],
                                  given[1] * self.theo - self.inertia[1])
                        else:
                            sb = (-self.inertia[0], -self.inertia[1])
                        steer = (sb[0] * STEERING + av[0], sb[1] * STEERING + av[1])
                        accel = clamp32(steer[0], steer[1], MAX_STEER)
                        vel = clamp32(accel[0] * DELTA + self.inertia[0],
                                      accel[1] * DELTA + self.inertia[1], self.theo)
                        disp = (vel[0] * DELTA, vel[1] * DELTA)
                        self.entity[0] += disp[0]
                        self.entity[1] += disp[1]
                        self.cursor[0] = self.entity[0] - self.spawn_offset[0]
                        self.cursor[1] = self.entity[1] - self.spawn_offset[1]
                        self.inertia = vel
                        cell = cell32(self.cursor[0], self.cursor[1])
                        if cell != self.last_cell:
                            self.last_cell = cell
                            res.events.append({"f": f, "text": f"进入格 {cell}，"
                                               f"位置 ({self.cursor[0]:.3f},{self.cursor[1]:.3f})"})
                        # 记录偏移前坐标（该帧正常移动后的状态）
                        self.record_frame(f, given, vel, self.avoid, "偏移前")
                        # ── 偏移修正：基于该帧最终坐标 ──
                        if active["pos"] is not None:
                            # 高级模式：直接指定偏移后坐标
                            pos = active["pos"]
                            desc = f"阻挡偏移：坐标修正到 {pos}"
                        else:
                            # 自动模式：按阻挡位几何计算
                            # d < 0.5 → 径向偏移推到 0.5；0.5~0.707 → 自然阻挡无偏移；
                            # > 0.707 → 不触发阻挡
                            bx, by = active["at"]
                            ex, ey = self.entity[0], self.entity[1]
                            dx, dy = ex - F(bx), ey - F(by)
                            d = np.sqrt(dx * dx + dy * dy)
                            if d < 0.5:
                                if d < 1e-5:   # 与阻挡位几乎重合：不偏移
                                    pos = (ex, ey)
                                    desc = f"阻挡（重合）：距阻挡位 {d:.5f}，无偏移"
                                else:
                                    k = F(0.5) / d
                                    pos = (bx + dx * k, by + dy * k)
                                    desc = (f"阻挡偏移（径向）：距阻挡位 {d:.4f} < 0.5，"
                                            f"坐标修正到 ({pos[0]:.4f},{pos[1]:.4f})")
                            elif d <= 0.707:
                                pos = (ex, ey)
                                desc = f"阻挡（自然）：距阻挡位 {d:.4f}，无偏移"
                            else:
                                raise SystemExit(
                                    f"错误：阻挡位 {active['at']} 距敌人 {d:.3f} > 0.707，不触发阻挡")
                        self.entity = [F(pos[0] + self.spawn_offset[0]),
                                       F(pos[1] + self.spawn_offset[1])]
                        self.cursor = [F(pos[0]), F(pos[1])]
                        res.events.append({"f": f, "text": desc})
                        if self.check_hole(f):
                            self.record_frame(f, None, self.inertia, self.avoid, "阻挡偏移后")
                            break
                        if inj_rem > 0:
                            res.events.append({"f": f, "text": f"阻挡开始（{inj_rem}帧）"})
                        self.record_frame(f, None, self.inertia, self.avoid, "阻挡偏移后")
                    else:
                        # 冻结段：位置不变、不计数、不判定
                        self.record_frame(f, None, self.inertia, self.avoid, "阻挡")
                    inj_rem -= 1
                    if inj_rem < 0:
                        res.events.append({"f": f, "text": "阻挡结束"})
                        active = None
                    continue

            # 消失状态：不移动、不寻路，仅检查点判定
            if self.disappeared:
                self.check_cp(f)
                self.record_frame(f, None, self.inertia, self.avoid, "消失")
                if res.end_reason:
                    break
                continue

            # 正常移动判定
            self.nav += 1
            given = self.given_direction()
            if (self.nav - self.k) % 3 == 0:
                foot = (self.entity[0], self.entity[1] - F(0.2))
                self.avoid = avoid32(self.model, self.obstacles, self.cursor, foot, given)
            inert_len = np.sqrt(self.inertia[0] * self.inertia[0] + self.inertia[1] * self.inertia[1])
            if self.avoid != (F(0), F(0)):
                coef = np.maximum(inert_len / self.theo, F(0.5))
                av = (self.avoid[0] * coef, self.avoid[1] * coef)
            else:
                av = (F(0), F(0))
            if given is not None:
                sb = (given[0] * self.theo - self.inertia[0],
                      given[1] * self.theo - self.inertia[1])
            else:
                sb = (-self.inertia[0], -self.inertia[1])
            steer = (sb[0] * STEERING + av[0], sb[1] * STEERING + av[1])
            accel = clamp32(steer[0], steer[1], MAX_STEER)
            vel = clamp32(accel[0] * DELTA + self.inertia[0],
                          accel[1] * DELTA + self.inertia[1], self.theo)
            disp = (vel[0] * DELTA, vel[1] * DELTA)
            self.entity[0] += disp[0]
            self.entity[1] += disp[1]
            self.cursor[0] = self.entity[0] - self.spawn_offset[0]
            self.cursor[1] = self.entity[1] - self.spawn_offset[1]
            self.inertia = vel

            # 进入新格事件
            cell = cell32(self.cursor[0], self.cursor[1])
            if cell != self.last_cell:
                self.last_cell = cell
                res.events.append({"f": f, "text": f"进入格 {cell}，"
                                   f"位置 ({self.cursor[0]:.3f},{self.cursor[1]:.3f})"})

            # 入坑检测（实体坐标）
            if self.check_hole(f):
                break

            # 检查点判定
            self.check_cp(f)
            self.record_frame(f, given, vel, self.avoid, None)
            if res.end_reason:
                break
        else:
            res.end_reason = f"超出帧上限（{self.max_frames}）"
            res.end_frame = self.max_frames - 1

        res.events.append({"f": res.end_frame, "text": res.end_reason})

    def frame_push(self, f, inj, rem):
        """推拉帧：匀速位移，期间不寻路、避障/惯性冻结，逐帧入坑检测。"""
        total = inj["dist"]
        done = total * (1 - rem / max(inj["dur"], 1))
        step = total / max(inj["dur"], 1)
        if rem == 1:
            step = total - done
        disp = (F(inj["dir"][0] * step), F(inj["dir"][1] * step))
        self.entity[0] += disp[0]
        self.entity[1] += disp[1]
        self.cursor[0] = self.entity[0] - self.spawn_offset[0]
        self.cursor[1] = self.entity[1] - self.spawn_offset[1]
        if self.check_hole(f):
            return
        cell = cell32(self.cursor[0], self.cursor[1])
        if cell != self.last_cell:
            self.last_cell = cell
            self.result.events.append({"f": f, "text": f"进入格 {cell}，"
                                       f"位置 ({self.cursor[0]:.3f},{self.cursor[1]:.3f})"})
        vel = (disp[0] * FPS, disp[1] * FPS)
        self.record_frame(f, None, vel, self.avoid, "推拉")

    def record_frame(self, f, given, vel, avoid, note):
        if avoid is None:
            avoid = self.avoid
        self.result.frames.append({
            "type": "frame", "f": f,
            "x": round(float(self.entity[0]), 6), "y": round(float(self.entity[1]), 6),
            "cx": round(float(self.cursor[0]), 6), "cy": round(float(self.cursor[1]), 6),
            "cell": list(cell32(self.cursor[0], self.cursor[1])),
            "given": [round(float(given[0]), 6), round(float(given[1]), 6)] if given else None,
            "vel": [round(float(vel[0]), 6), round(float(vel[1]), 6)] if vel else None,
            "avoid": [round(float(avoid[0]), 6), round(float(avoid[1]), 6)],
            "cp": self.cp_idx, "nav": self.nav, "phase": (self.nav - self.k) % 3,
            "note": note,
        })


# ═══════════════════════════════════════════════════════
# 六、输出层
# ═══════════════════════════════════════════════════════

def write_log(path, meta, frames, events):
    with open(path, "w", encoding="utf-8") as fp:
        fp.write(json.dumps(meta, ensure_ascii=False) + "\n")
        for fr in frames:
            fp.write(json.dumps(fr, ensure_ascii=False) + "\n")
        for ev in events:
            fp.write(json.dumps({"type": "event", "f": ev["f"], "text": ev["text"]},
                                ensure_ascii=False) + "\n")


def main():
    ap = argparse.ArgumentParser(description="寻路模拟工具（测试版）")
    ap.add_argument("levelId")
    ap.add_argument("routeIndex", type=int)
    ap.add_argument("--k", type=float, default=1.0)
    ap.add_argument("--speed", type=float, default=1.0)
    ap.add_argument("--start", default=None)
    ap.add_argument("--checkpoints", default=None)
    ap.add_argument("--inject", default=None)
    ap.add_argument("--obstacle", default=None)
    ap.add_argument("--t-global", type=int, default=0)
    ap.add_argument("--t-fragment", type=int, default=0)
    ap.add_argument("--t-wave", type=int, default=0)
    ap.add_argument("--log", default=None)
    ap.add_argument("--no-log", action="store_true")
    ap.add_argument("--max-frames", type=int, default=12000)
    ap.add_argument("--db", default=DEFAULT_DB)
    args = ap.parse_args()

    model = load_map(args.db, args.levelId)
    route = load_route(args.db, args.levelId, args.routeIndex)

    # 起点 / 检查点：数据库路线 or 自定义
    if args.checkpoints is not None:
        checkpoints = parse_dsl(args.checkpoints)
        if args.start:
            sx, sy = (int(v) for v in args.start.split(","))
            start = (sx, sy)
        else:
            start = (int(route[1][1]), int(route[1][0]))
        end = (int(route[2][1]), int(route[2][0]))
        spawn_offset = (0.0, 0.0)
        src_label = "自定义"
    else:
        start, end, spawn_offset, checkpoints = parse_db_route(route)
        src_label = "数据库"

    # 障碍物 = 地穴 + 额外指定
    obstacles = {(x, y) for y in range(model["height"]) for x in range(model["width"])
                 if model["passable"][y][x] and model["tile_key"][y][x] == "tile_hole"}
    if args.obstacle:
        for token in args.obstacle.split(";"):
            x, y = (int(v) for v in token.split(","))
            obstacles.add((x, y))

    injections = parse_injections(args.inject) if args.inject else []

    sim = Simulator(model, obstacles, start, spawn_offset, checkpoints, end,
                    k=args.k, theo=args.speed,
                    t_global=args.t_global, t_fragment=args.t_fragment,
                    t_wave=args.t_wave, max_frames=args.max_frames)
    sim.run(injections)
    res = sim.result

    # ── 主输出：事件流 ──
    print("══════ 寻路模拟 ══════")
    print(f"关卡: {args.levelId}  路线: {args.routeIndex}（{src_label}）  "
          f"K={args.k}  移速={args.speed}格/秒")
    print(f"起点: {start}  f0 生成  终点: {end}")
    if injections:
        print(f"注入: {args.inject}")
    print()
    for ev in res.events:
        print(f"f{ev['f']:<6}{ev['text']}")
    print()
    print(f"检查点完成帧: {res.cp_done}")

    # ── 帧级 log ──
    if not args.no_log:
        if args.log:
            log_path = args.log
        else:
            log_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sim_logs")
            os.makedirs(log_dir, exist_ok=True)
            log_path = os.path.join(log_dir, f"sim_{args.levelId}_r{args.routeIndex}.jsonl")
        meta = {"type": "meta", "levelId": args.levelId, "route": args.routeIndex,
                "k": args.k, "theo": args.speed, "start": list(start), "end": list(end),
                "spawn_offset": list(spawn_offset), "checkpoints": checkpoints,
                "injections": injections,
                "t_global": args.t_global, "t_fragment": args.t_fragment,
                "t_wave": args.t_wave, "width": model["width"], "height": model["height"],
                "end_frame": res.end_frame, "end_reason": res.end_reason}
        write_log(log_path, meta, res.frames, res.events)
        print(f"\nlog: {log_path}")


if __name__ == "__main__":
    main()
