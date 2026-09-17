#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""sim_all.py — level_rogue5_t-10 四车（R1/R2/R3/R8）轨迹模拟器

机制：寻路判定从出生帧下一帧开始；避障三帧周期按寻路帧计数（nav）判定，
周期第 0 帧 = 出生后第 K 个寻路帧，K 为每车机制参数（确认配置：
R1/R2 微小足影型 K=2，R3/R8 完整足影型 K=1）；
出生帧的全局位置只决定路径的全局偏移，不影响周期相位；
停驻 = MOVE 状态（方向 0、周期照常）；冻结段不计数，不影响后续路径。
"""
import contextlib
import io

import numpy as np
import level_map
import path_finder

F = np.float32
DB = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db"
LEVEL = "level_rogue5_t-10"

with contextlib.redirect_stdout(io.StringIO()):
    md = level_map.load_level_map(DB, LEVEL)
    model = level_map.build_model(md, LEVEL)
W, H = model["width"], model["height"]
obstacles = {(x, y) for y in range(H) for x in range(W)
             if model["passable"][y][x] and model["tile_key"][y][x] == "tile_hole"}

DELTA = F(0.0333333)
STEERING = F(8.0)
MAX_STEER = F(10.0)
HALF_BODY = F(0.2)
EPS = F(1e-6)
GOAL_R = F(0.05)
THEO = F(0.65)

# 每车：出生帧（全局）、出生位置、检查点序列 (type, goal, wait)
#   WAIT 为停驻·计时；MOVE 为目标格；终点 END = (2,7)
CARS = {
    1: {"birth": 2,  "pos": (1.0, 1.0), "cps": [("WAIT", None, 60.0), ("MOVE", (9, 5), 0.0),
                                                ("MOVE", (9, 2), 0.0), ("MOVE", (8, 2), 0.0),
                                                ("MOVE", (8, 5), 0.0)]},
    2: {"birth": 0,  "pos": (1.0, 2.0), "cps": [("WAIT", None, 120.0), ("MOVE", (1, 1), 0.0),
                                                ("MOVE", (9, 5), 0.0), ("MOVE", (9, 2), 0.0),
                                                ("MOVE", (8, 2), 0.0), ("MOVE", (8, 5), 0.0)]},
    3: {"birth": 1,  "pos": (1.0, 3.0), "cps": [("WAIT", None, 120.0), ("MOVE", (1, 1), 0.0),
                                                ("MOVE", (9, 5), 0.0), ("MOVE", (9, 2), 0.0),
                                                ("MOVE", (8, 2), 0.0), ("MOVE", (8, 5), 0.0)]},
    8: {"birth": 4,  "pos": (8.0, 7.0), "cps": [("MOVE", (8, 6), 0.0), ("WAIT", None, 20.0),
                                                ("MOVE", (8, 5), 0.0), ("MOVE", (9, 5), 0.0),
                                                ("MOVE", (9, 2), 0.0), ("MOVE", (8, 2), 0.0),
                                                ("MOVE", (8, 5), 0.0)]},
}
END = (2, 7)

# 机制参数：周期第 0 帧（重算帧）= 出生后第 K 个寻路帧
# （相对计数，出生帧全局位置不影响周期相位，只决定全局偏移）
# 确认配置：R1/R2 微小足影型 K=2，R3/R8 完整足影型 K=1
K_MECH = {1: 2, 2: 2, 3: 1, 8: 1}


def clamp32(vx, vy, maxlen):
    l = np.sqrt(vx * vx + vy * vy)
    if l <= maxlen or l < EPS:
        return vx, vy
    k = maxlen / l
    return vx * k, vy * k


def cell32(x, y):
    return (int(round(float(x))), int(round(float(y))))


def unit32(vx, vy):
    l = np.sqrt(vx * vx + vy * vy)
    if l < EPS:
        return None
    return vx / l, vy / l


def project32(a, given):
    if given is None:
        return a
    g = unit32(given[0], given[1])
    if g is None:
        return a
    proj = a[0] * g[0] + a[1] * g[1]
    return (a[0] - proj * g[0], a[1] - proj * g[1])


def avoid32(cursor, foot, given):
    cx = int(round(float(cursor[0])))
    cy = int(round(float(cursor[1])))
    if not (0 <= cx < W and 0 <= cy < H) or not model["passable"][cy][cx]:
        best_dir = None
        best_d = None
        order = ((0, 1), (1, 1), (1, 0), (1, -1), (0, -1), (-1, -1), (-1, 0), (-1, 1))
        for dx, dy in order:
            ox, oy = cx + dx, cy + dy
            if 0 <= ox < W and 0 <= oy < H and model["passable"][oy][ox] and (ox, oy) not in obstacles:
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
            if model["passable"][oy][ox] and (ox, oy) not in obstacles:
                continue
            if dx < 0:
                nx = foot[0] - HALF_BODY
            elif dx > 0:
                nx = foot[0] + HALF_BODY
            else:
                nx = foot[0]
            ny = foot[1]
            fdx = F(dx)
            fdy = F(dy)
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


def simulate_car(no, max_frames=12000, reroute=None):
    """reroute: {frame, pos, freeze_until} 阻挡注入——
    frame 帧位置修正到 pos（阻挡偏移/调轴），该帧 nav 照常计数
    （避障帧 = 该帧无阻挡时的相位，即冻结值；解除后从冻结值继续）；
    frame < f < freeze_until 为冻结段（位置不变、不计数）；freeze_until 起恢复。
    自然阻挡（进入圈帧已正常移动）用 freeze_until 后一帧作为 frame。
    避障周期按寻路帧计数（nav）判定：周期第 0 帧 = 出生后第 K 个寻路帧。
    冻结不改变寻路帧计数，因此冻结完全不影响后续路径（仅时间平移）。"""
    cfg = CARS[no]
    cyc_start = K_MECH[no]
    nav_maps = {}
    entity = [F(cfg["pos"][0]), F(cfg["pos"][1])]
    cursor = [F(cfg["pos"][0]), F(cfg["pos"][1])]
    inertia = (F(0), F(0))
    avoid = (F(0), F(0))
    cp_idx = 0
    wait_left = F(cfg["cps"][0][2]) if cfg["cps"][0][0] == "WAIT" else F(0)
    frames = []
    cp_done = []
    nav = 0

    def goal_point_of(goal):
        return (F(goal[0]), F(goal[1]))

    def nav_of(goal):
        if goal not in nav_maps:
            sc = cell32(cursor[0], cursor[1])
            with contextlib.redirect_stdout(io.StringIO()):
                _, _, nn = path_finder.run_path(model, sc, goal)
            nav_maps[goal] = nn
        return nav_maps[goal]

    for frame in range(0, max_frames):
        if frame < cfg["birth"]:
            continue                    # 出生前的帧不属于敌人生命周期，不入轨迹
        if reroute and reroute.get("freeze_until") and reroute["frame"] < frame < reroute["freeze_until"]:
            # 自然阻挡冻结段：位置不变、不寻路不计数
            frames.append({"frame": frame, "entity": (entity[0], entity[1]),
                           "cell": cell32(cursor[0], cursor[1]), "given": None,
                           "vel": (inertia[0], inertia[1]), "avoid": (F(0), F(0)),
                           "cp": cp_idx})
            continue
        if reroute and frame == reroute["frame"]:
            # 阻挡偏移帧：位置被修正（部署延迟 1 帧后生效），nav 照常计数
            # （避障帧 = 该帧无阻挡时的相位，即冻结值；下一帧解除后 +1）
            entity = [F(reroute["pos"][0]), F(reroute["pos"][1])]
            cursor = list(entity)
            nav += 1
            frames.append({"frame": frame, "entity": (entity[0], entity[1]),
                           "cell": cell32(cursor[0], cursor[1]), "given": None,
                           "vel": (inertia[0], inertia[1]), "avoid": (F(0), F(0)),
                           "cp": cp_idx})
            continue
        if frame < cfg["birth"] + 1:
            frames.append({"frame": frame, "entity": (entity[0], entity[1]),
                           "cell": cell32(cursor[0], cursor[1]), "given": None,
                           "vel": (F(0), F(0)), "avoid": (F(0), F(0)), "cp": cp_idx})
            continue
        nav += 1                     # 本帧为寻路判定帧（冻结段不计入）
        cp_type, cp_goal, _ = (cfg["cps"][cp_idx] if cp_idx < len(cfg["cps"])
                               else ("END", END, 0.0))
        given = None
        if cp_type in ("MOVE", "END"):
            nn = nav_of(cp_goal)
            cell = cell32(cursor[0], cursor[1])
            tgt = nn.get(cell)
            if tgt is None or tgt == cp_goal:
                tgt = cp_goal
            t = goal_point_of(tgt)
            dx = t[0] - cursor[0]
            dy = t[1] - cursor[1]
            dl = np.sqrt(dx * dx + dy * dy)
            given = (dx / dl, dy / dl) if dl > EPS else None
        if (nav - cyc_start) % 3 == 0:
            # 周期按寻路帧计数：第 0 帧 = 出生后第 K 个寻路帧（冻结不计入，不影响相位）
            foot = (entity[0], entity[1] - F(0.2))
            avoid = avoid32(cursor, foot, given)
        inert_len = np.sqrt(inertia[0] * inertia[0] + inertia[1] * inertia[1])
        if avoid != (F(0), F(0)):
            coef = np.maximum(inert_len / THEO, F(0.5))
            av = (avoid[0] * coef, avoid[1] * coef)
        else:
            av = (F(0), F(0))
        if given is not None:
            sb = (given[0] * THEO - inertia[0], given[1] * THEO - inertia[1])
        else:
            sb = (-inertia[0], -inertia[1])
        steer = (sb[0] * STEERING + av[0], sb[1] * STEERING + av[1])
        accel = clamp32(steer[0], steer[1], MAX_STEER)
        vel = clamp32(accel[0] * DELTA + inertia[0], accel[1] * DELTA + inertia[1], THEO)
        disp = (vel[0] * DELTA, vel[1] * DELTA)
        entity[0] += disp[0]
        entity[1] += disp[1]
        cursor[0] = entity[0]
        cursor[1] = entity[1]
        inertia = vel
        if cp_type == "MOVE":
            gp = goal_point_of(cp_goal)
            if np.sqrt((cursor[0] - gp[0]) ** 2 + (cursor[1] - gp[1]) ** 2) < GOAL_R:
                cp_done.append(frame)
                cp_idx += 1
                if cp_idx < len(cfg["cps"]):
                    _, _, t = cfg["cps"][cp_idx]
                    wait_left = F(t)
        elif cp_type == "WAIT":
            wait_left -= DELTA
            if wait_left <= F(0):
                cp_done.append(frame)
                cp_idx += 1
                if cp_idx < len(cfg["cps"]):
                    _, _, t = cfg["cps"][cp_idx]
                    wait_left = F(t)
        elif cp_type == "END":
            gp = goal_point_of(END)
            if np.sqrt((cursor[0] - gp[0]) ** 2 + (cursor[1] - gp[1]) ** 2) < GOAL_R:
                cp_done.append(frame)
                break
        frames.append({"frame": frame, "entity": (entity[0], entity[1]),
                       "cell": cell32(cursor[0], cursor[1]), "given": given,
                       "vel": vel, "avoid": avoid, "cp": cp_idx})
    return frames, cp_done


def report(no, frames, cp_done):
    cfg = CARS[no]
    # 回程起点 = 倒数第二个 MOVE 检查点完成后
    move_cps = [i for i, c in enumerate(cfg["cps"]) if c[0] == "MOVE"]
    back_start = cp_done[move_cps[-1]] + 1
    trig = next((fr for fr in frames if fr["frame"] >= back_start and fr["avoid"] != (0, 0)), None)
    cands = [fr for fr in frames
             if fr["frame"] >= back_start and 3.5 < float(fr["entity"][0]) < 4.5
             and 0.5 < float(fr["entity"][1]) < 1.5]
    cands.sort(key=lambda fr: abs(float(fr["entity"][0]) - 4.0))
    b = cands[0] if cands else None
    e = trig["entity"] if trig else None
    x4 = b["entity"] if b else None
    rel = (trig["frame"] - back_start + 1) if trig else None
    print(f"路线{no} 出生{cfg['birth']}: 段长{[cp_done[0]-cfg['birth']] + [cp_done[i]-cp_done[i-1] for i in range(1,len(cp_done))]}"
          f" | 触发帧{trig['frame']}≡{trig['frame']%3} 相对{rel}"
          f" ({float(e[0]):.6f},{float(e[1]):.6f})" if trig else "")
    if b:
        print(f"          x4 帧{b['frame']} 秒内{b['frame']%30}≡{b['frame']%3}"
              f" ({float(x4[0]):.6f},{float(x4[1]):.6f}) 余数{abs(float(x4[0])-4.0):.6f}")


if __name__ == "__main__":
    for no in (1, 2, 3, 8):
        frames, cp_done = simulate_car(no)
        report(no, frames, cp_done)
