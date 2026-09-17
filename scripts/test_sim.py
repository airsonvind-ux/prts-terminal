#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""sim_path 交互测试脚本

用法：
    1. cases 模式：直接运行，按 cases 列表依次执行
       python test_sim.py
    2. 命令行模式：指定关卡与路线与参数
       python test_sim.py <levelId> <routeIndex> [--speed S] [--k K] [--inject "..."] [--route "..."]

speed 为属性移速（工具内部按关卡倍率换算理论移速），如沙滩车 1.3。
route 为自定义路线 DSL：START(x,y);MOVE(x,y);WAIT(s);END(x,y)（分号分隔）。
inject 注入语法：push@帧:dir=(dx,dy),dist=距离,dur=帧数 / block@帧:at=(gx,gy),dur=帧数。
"""
import os, subprocess, sys

proj = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri"


def run_case(level_id: str, route_index: str, speed: str = "1.0", k: str = "1",
             inject: str = "", route: str = ""):
    """运行一次 sim_path show_output 测试，返回 log 文件路径（无则 None）

    PRTS_SIM_WS 指向 src-tauri：帧级 log 默认落 src-tauri/sim_logs/（M1
    批 2 起 sim_path 默认落点为工作区 sim_logs/，show_output 的持久工作区
    由此 env 指定；log 留存供 plot_log.py 绘图与后续对拍）。
    """
    env = os.environ.copy()
    env["PRTS_SIM_LEVEL_ID"] = level_id
    env["PRTS_SIM_ROUTE_INDEX"] = route_index
    env["PRTS_SIM_SPEED"] = speed
    env["PRTS_SIM_K"] = k
    env["PRTS_SIM_WS"] = proj
    if inject:
        env["PRTS_SIM_INJECT"] = inject
    else:
        env.pop("PRTS_SIM_INJECT", None)
    if route:
        env["PRTS_SIM_ROUTE"] = route
    else:
        env.pop("PRTS_SIM_ROUTE", None)

    label = f"levelId={level_id}  route={route_index}  speed={speed}  k={k}"
    if inject:
        label += f"\n  inject: {inject}"
    if route:
        label += f"\n  route: {route}"

    print(f"{'='*60}")
    print(label)
    print(f"{'='*60}")

    result = subprocess.run(
        ["cargo", "test", "sim_path::tests::show_output", "--", "--nocapture"],
        cwd=proj, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        encoding="utf-8", errors="replace"
    )

    in_output = False
    log_path = None
    for line in result.stdout.splitlines():
        if "running " in line and "test" in line:
            in_output = True
            continue
        if in_output:
            if line.startswith("test ") or line.startswith("test result"):
                continue
            if "Running unittests" in line:
                continue
            print(line)
            if line.startswith("log: "):
                log_path = line[5:].strip()
    print()
    return log_path


def show_plot(log_path: str):
    """调用 plot_log.py 打开图像窗口（阻塞至窗口关闭）。"""
    if not log_path:
        print("（无 log 文件，跳过绘图）")
        return
    print(f"打开绘图窗口: {log_path}")
    subprocess.run([sys.executable, "plot_log.py", log_path])


# ═══════════════════════════════════════════════════════
# 测试用例列表（按需取消注释）
# ═══════════════════════════════════════════════════════

cases = [
    # ── 基础：路线移动情况（沙滩车属性移速 1.3 → 理论 0.65）──
    # {"level_id": "level_rogue5_t-10", "route_index": "8", "speed": "1.3"},
    # {"level_id": "level_rogue5_t-10", "route_index": "1", "speed": "1.3", "k": "1"},
    {"level_id": "level_rogue5_t-10", "route_index": "2", "speed": "1.3", "k": "2"},

    # ── 调避障帧（阻挡偏移）──
    # {"level_id": "level_rogue5_t-10", "route_index": "1", "speed": "1.3", "k": "1",
    #  "inject": "block@1941:at=(4,1),dur=6"},
    # {"level_id": "level_rogue5_t-10", "route_index": "3", "speed": "1.3", "k": "1",
    #  "inject": "block@3739:at=(2,1),dur=6"},

    # ── 推拉入坑 ──
    # {"level_id": "level_rogue5_t-10", "route_index": "5", "speed": "1.3",
    #  "inject": "push@10:dir=(-1,0),dist=3.0,dur=30"},

    # ── 自定义路线 ──
    # {"level_id": "level_rogue5_t-10", "route_index": "5", "speed": "1.3",
    #  "route": "START(10,2);MOVE(8,2);WAIT(10.0);END(2,7)"},
]

if len(sys.argv) > 1:
    # 命令行模式：python test_sim.py <levelId> <routeIndex> [选项]
    level_id = sys.argv[1]
    route_index = sys.argv[2]
    speed, k, inject, route, no_plot = "1.0", "1", "", "", False
    i = 3
    while i < len(sys.argv):
        arg = sys.argv[i]
        if arg == "--speed" and i + 1 < len(sys.argv):
            speed = sys.argv[i + 1]; i += 2
        elif arg == "--k" and i + 1 < len(sys.argv):
            k = sys.argv[i + 1]; i += 2
        elif arg == "--inject" and i + 1 < len(sys.argv):
            inject = sys.argv[i + 1]; i += 2
        elif arg == "--route" and i + 1 < len(sys.argv):
            route = sys.argv[i + 1]; i += 2
        elif arg == "--no-plot":
            no_plot = True; i += 1
        else:
            print(f"未知参数: {arg}")
            sys.exit(1)
    log_path = run_case(level_id, route_index, speed, k, inject, route)
    if not no_plot:
        show_plot(log_path)
else:
    # cases 模式：每个用例跑完后弹窗（--no-plot 跳过全部绘图）
    no_plot = "--no-plot" in sys.argv
    for c in cases:
        log_path = run_case(c["level_id"], c["route_index"], c.get("speed", "1.0"),
                            c.get("k", "1"), c.get("inject", ""), c.get("route", ""))
        if not no_plot:
            show_plot(log_path)
