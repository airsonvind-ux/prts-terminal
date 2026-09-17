#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""plot_sim.py — level_rogue5_t-10 四车（R1/R2/R3/R8）轨迹交互图

用法：
  python plot_sim.py               # 按确认机制配置（K: R1:2 R2:2 R3:1 R8:1）绘制
  python plot_sim.py --save x.png  # 保存静态图

悬停显示：相对帧（出生后第几帧）、周期帧位（0=重算帧）、坐标。
"""
import argparse
import contextlib
import io

import numpy as np
import matplotlib
import matplotlib.pyplot as plt

matplotlib.rcParams["font.sans-serif"] = ["Microsoft YaHei", "SimHei", "Arial"]
matplotlib.rcParams["axes.unicode_minus"] = False

import sim_all

model = sim_all.model
W, H = model["width"], model["height"]

# ---------------- 配置 ----------------
K_MAP = sim_all.K_MECH   # 确认机制配置：R1/R2 K=2、R3/R8 K=1
DRAW_ROUTES = [1, 2, 3, 8]     # 默认四条全画
COLORS = {1: "#d62728", 2: "#2ca02c", 3: "#1f77b4", 8: "#ff7f0e"}
NAMES = {1: "路线1(左1)", 2: "路线2(左2)", 3: "路线3(左3)", 8: "路线8(右)"}


def simulate(route_no):
    """按机制配置模拟单条路线，返回帧列表（frame 为全局帧）。"""
    with contextlib.redirect_stdout(io.StringIO()):
        frames, _ = sim_all.simulate_car(route_no)
    return frames


def draw_map(ax):
    """画地图底图：墙 / 地穴 / 坑(4,2)高亮 / 格线。"""
    tile_key = model["tile_key"]
    passable = model["passable"]
    holes = [(x, y) for y in range(H) for x in range(W)
             if tile_key[y][x] == "tile_hole" and passable[y][x]]

    for y in range(H):
        for x in range(W):
            if not passable[y][x]:
                ax.add_patch(plt.Rectangle((x - 0.5, y - 0.5), 1, 1,
                                           facecolor="#3a3a3a", edgecolor="none", zorder=1))
    for (x, y) in holes:
        ax.add_patch(plt.Rectangle((x - 0.5, y - 0.5), 1, 1,
                                   facecolor="#2b3a4a", hatch="//",
                                   edgecolor="#4a6a8a", linewidth=0.8, zorder=2))
    # 入坑位 (4,2) 高亮
    ax.add_patch(plt.Rectangle((3.5, 1.5), 1, 1, fill=False,
                               edgecolor="#ff5555", linewidth=2.2, zorder=3))
    ax.text(4, 2, "坑(4,2)", color="#ff7777", fontsize=9, ha="center", va="center", zorder=4)

    ax.set_xlim(-0.5, W - 0.5)
    ax.set_ylim(-0.5, H - 0.5)
    ax.set_aspect("equal")
    ax.set_xticks(range(W))
    ax.set_yticks(range(H))
    ax.set_xticks(np.arange(W - 1) + 0.5, minor=True)
    ax.set_yticks(np.arange(H - 1) + 0.5, minor=True)
    ax.grid(which="minor", color="#666666", linewidth=0.5, alpha=0.45, zorder=5)
    ax.set_xlabel("x (格)")
    ax.set_ylabel("y (格)")
    ax.set_title(f"level_rogue5_t-10 四条路线轨迹（K={K_MAP}）")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--save", metavar="PNG", help="保存静态图并退出")
    parser.add_argument("--routes", default=None,
                        help="绘制路线列表，如 1,2,3（默认全部）")
    args = parser.parse_args()
    routes = ([int(s) for s in args.routes.split(",")]
              if args.routes else DRAW_ROUTES)

    series = {}
    for rn in routes:
        frames = simulate(rn)
        pts = np.array([[float(fr["entity"][0]), float(fr["entity"][1]),
                         fr["frame"]] for fr in frames])
        series[rn] = (frames, pts)

    fig, ax = plt.subplots(figsize=(12, 9))
    draw_map(ax)

    # 轨迹线 + 起点标记
    lines = {}
    for rn, (frames, pts) in series.items():
        (line,) = ax.plot(pts[:, 0], pts[:, 1], color=COLORS[rn], linewidth=1.0,
                          alpha=0.85, label=NAMES[rn], zorder=6)
        lines[rn] = line
        ax.plot(pts[0, 0], pts[0, 1], "o", color=COLORS[rn], markersize=7, zorder=7)
    ax.legend(loc="upper left", fontsize=9)

    # 悬停显示
    annot = ax.annotate("", xy=(0, 0), xytext=(12, 12), textcoords="offset points",
                        bbox=dict(boxstyle="round,pad=0.35", fc="#222222", ec="#888888",
                                   alpha=0.92),
                        arrowprops=dict(arrowstyle="->", color="#888888"),
                        color="#eeeeee", fontsize=9, zorder=10)
    annot.set_visible(False)
    (marker,) = ax.plot([], [], "o", mfc="none", mec="#ffdd00", ms=7, mew=1.6, zorder=8)
    HOVER_R = 0.06  # 命中半径（格）

    def on_move(event):
        if event.inaxes != ax or event.xdata is None:
            annot.set_visible(False)
            marker.set_data([], [])
            fig.canvas.draw_idle()
            return
        hits = []
        best_d = 1e9
        best_xy = None
        for rn, (frames, pts) in series.items():
            d = (pts[:, 0] - event.xdata) ** 2 + (pts[:, 1] - event.ydata) ** 2
            i = int(np.argmin(d))
            dd = float(np.sqrt(d[i]))
            if dd < HOVER_R:
                hits.append((rn, pts[i], dd))
            if dd < best_d:
                best_d = dd
                best_xy = (pts[i, 0], pts[i, 1])
        if not hits:
            annot.set_visible(False)
            marker.set_data([], [])
            fig.canvas.draw_idle()
            return
        hits.sort(key=lambda t: t[2])
        lines_txt = []
        for rn, p, dd in hits:
            f = int(p[2])
            birth = sim_all.CARS[rn]["birth"]
            k = K_MAP[rn]
            phase = (f - birth - k) % 3   # 该帧在三帧避障周期中的帧位（0=重算帧）
            lines_txt.append(f"{NAMES[rn]} 相对帧{f - birth} 周期第{phase}帧  ({p[0]:.6f}, {p[1]:.6f})")
        annot.xy = (best_xy[0], best_xy[1])
        annot.set_text("\n".join(lines_txt))
        annot.set_visible(True)
        marker.set_data([best_xy[0]], [best_xy[1]])
        fig.canvas.draw_idle()

    fig.canvas.mpl_connect("motion_notify_event", on_move)

    if args.save:
        fig.savefig(args.save, dpi=150, bbox_inches="tight")
        print(f"已保存 {args.save}")
    else:
        plt.show()


if __name__ == "__main__":
    main()
