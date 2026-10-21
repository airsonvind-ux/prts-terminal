#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""plot_log.py — 从 sim_tool log 文件绘制轨迹交互图

用法：
  python plot_log.py <log文件>                # 交互窗口（悬停查看帧信息）
  python plot_log.py <log文件> --save x.png   # 保存静态图

悬停显示：帧号、坐标、所在格、检查点索引、避障相位、given/vel/avoid、帧注记。
地图底图：深色 = 不可通行，斜纹 = 地穴。
标记：绿点 = 起点，红叉 = 终点（入坑/抵达/截断位置），事件点（注入/检查点）标注于轨迹上。
"""
import argparse
import json
import sys

import numpy as np
import matplotlib
import matplotlib.pyplot as plt

matplotlib.rcParams["font.sans-serif"] = ["Microsoft YaHei", "SimHei", "Arial"]
matplotlib.rcParams["axes.unicode_minus"] = False

import sim_tool


def load_log(path):
    meta = None
    frames = []
    events = []
    with open(path, encoding="utf-8") as fp:
        for line in fp:
            line = line.strip()
            if not line:
                continue
            d = json.loads(line)
            if d.get("type") == "meta":
                meta = d
            elif d.get("type") == "frame":
                frames.append(d)
            elif d.get("type") == "event":
                events.append(d)
    return meta, frames, events


def draw_map(ax, model):
    W, H = model["width"], model["height"]
    holes = []
    for y in range(H):
        for x in range(W):
            if not model["passable"][y][x]:
                ax.add_patch(plt.Rectangle((x - 0.5, y - 0.5), 1, 1,
                                           facecolor="#3a3a3a", edgecolor="none", zorder=1))
            elif model["tile_key"][y][x] == "tile_hole":
                holes.append((x, y))
    for (x, y) in holes:
        ax.add_patch(plt.Rectangle((x - 0.5, y - 0.5), 1, 1,
                                   facecolor="#2b3a4a", hatch="//",
                                   edgecolor="#4a6a8a", linewidth=0.8, zorder=2))
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
    ax.set_title(f"{model['levelId']} 路线{meta['route']} 轨迹（K={meta['k']}，移速={meta['theo']}）")


def setup_window(fig):
    """窗口定位：figsize 控制默认尺寸（10×7.2 英寸），窗口显示并完成布局后
    再延迟定位到屏幕上方居中（不遮挡任务栏）。不手动设置窗口尺寸，
    避免 canvas 与窗口尺寸不匹配导致的内容错位。"""
    fig.set_size_inches(10, 7.2)
    win = getattr(fig.canvas.manager, "window", None)
    if win is None:
        return

    def place():
        try:
            sw = win.winfo_screenwidth()
            sh = win.winfo_screenheight()
            w = win.winfo_width()
            h = win.winfo_height()
            x = max(0, (sw - w) // 2)
            y = max(0, int(sh * 0.05))
            win.geometry(f"+{x}+{y}")
        except Exception:
            pass

    try:
        win.after(150, place)   # 窗口映射后布局完成再定位
    except Exception:
        place()


def main():
    ap = argparse.ArgumentParser(description="从 sim_tool log 绘制轨迹交互图")
    ap.add_argument("log", help="sim_tool 输出的 log 文件路径")
    ap.add_argument("--save", metavar="PNG", help="保存静态图并退出")
    ap.add_argument("--db", default=sim_tool.DEFAULT_DB, help="数据库路径")
    args = ap.parse_args()

    global meta
    meta, frames, events = load_log(args.log)
    if meta is None:
        sys.exit("错误：log 文件缺少 meta 行")
    if not frames:
        sys.exit("错误：log 文件无帧数据")

    model = sim_tool.load_map(args.db, meta["levelId"])
    fig, ax = plt.subplots(figsize=(12, 9))
    setup_window(fig)
    draw_map(ax, model)

    # 轨迹（实体坐标）
    pts = np.array([[fr["x"], fr["y"]] for fr in frames])
    ax.plot(pts[:, 0], pts[:, 1], color="#1f77b4", linewidth=1.0, alpha=0.85, zorder=6)
    ax.plot(pts[0, 0], pts[0, 1], "o", color="#2ca02c", markersize=7, zorder=7)
    ax.plot(pts[-1, 0], pts[-1, 1], "x", color="#d62728", markersize=9, zorder=7)

    # 事件点标注：入坑 / 注入 / 终点类事件
    by_f = {fr["f"]: fr for fr in frames}
    for ev in events:
        fr = by_f.get(ev["f"])
        if fr is None:
            continue
        text = ev["text"]
        if any(k in text for k in ("入坑", "推拉开始", "阻挡偏移", "抵达终点", "超出帧上限")):
            color = {"入坑": "#ff5555", "推拉开始": "#ffaa00", "阻挡偏移": "#cc66ff",
                     "抵达终点": "#2ca02c"}.get(
                next((k for k in ("入坑", "推拉开始", "阻挡偏移", "抵达终点") if k in text), "#888888"))
            ax.plot(fr["x"], fr["y"], "o", mfc="none", mec=color, ms=10, mew=1.5, zorder=7)

    # 悬停
    annot = ax.annotate("", xy=(0, 0), xytext=(12, 12), textcoords="offset points",
                        bbox=dict(boxstyle="round,pad=0.35", fc="#222222", ec="#888888", alpha=0.92),
                        arrowprops=dict(arrowstyle="->", color="#888888"),
                        color="#eeeeee", fontsize=9, zorder=10)
    annot.set_visible(False)
    (marker,) = ax.plot([], [], "o", mfc="none", mec="#ffdd00", ms=7, mew=1.6, zorder=8)
    HOVER_R = 0.06

    def on_move(event):
        if event.inaxes != ax or event.xdata is None:
            annot.set_visible(False)
            marker.set_data([], [])
            fig.canvas.draw_idle()
            return
        d = (pts[:, 0] - event.xdata) ** 2 + (pts[:, 1] - event.ydata) ** 2
        i = int(np.argmin(d))
        if float(np.sqrt(d[i])) > HOVER_R:
            annot.set_visible(False)
            marker.set_data([], [])
            fig.canvas.draw_idle()
            return
        fr = frames[i]
        txt = f"f{fr['f']}  ({fr['x']:.6f},{fr['y']:.6f})  格{fr['cell']}\n" \
              f"cp={fr['cp']}  phase={fr['phase']}  nav={fr['nav']}"
        if fr.get("given"):
            txt += f"\ngiven=({fr['given'][0]:.6f},{fr['given'][1]:.6f})"
        if fr.get("vel"):
            txt += f"\nvel=({fr['vel'][0]:.6f},{fr['vel'][1]:.6f})"
        if fr.get("avoid") and (fr["avoid"][0] or fr["avoid"][1]):
            txt += f"\navoid=({fr['avoid'][0]:.6f},{fr['avoid'][1]:.6f})"
        if fr.get("note"):
            txt += f"\n[{fr['note']}]"
        annot.xy = (pts[i, 0], pts[i, 1])
        annot.set_text(txt)
        annot.set_visible(True)
        marker.set_data([pts[i, 0]], [pts[i, 1]])
        fig.canvas.draw_idle()

    fig.canvas.mpl_connect("motion_notify_event", on_move)

    if args.save:
        fig.savefig(args.save, dpi=150, bbox_inches="tight")
        print(f"已保存 {args.save}")
    else:
        plt.show()


if __name__ == "__main__":
    main()
