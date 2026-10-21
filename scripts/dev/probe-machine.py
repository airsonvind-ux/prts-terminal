#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""PRTS 本机信息探测 · 验证脚本（只读，无任何写操作）

在真机上验证「本机信息」的分层探测逻辑：
  层2 注册表 Uninstall 枚举（主力）
  层3/4 盘根浅扫特征文件（兜底）
并全量列出匹配到的条目，供校准匹配规则。

运行：python probe-machine.py [--all]
  --all  额外打印全部 Uninstall 条目（默认只打印匹配项）
"""
import os
import re
import sys
import winreg
from pathlib import Path

# ── 匹配表（可扩展；小写比对）──
EMULATORS = [
    ("MuMu 12",      ["mumu"]),
    ("雷电 LDPlayer", ["雷电", "ldplayer", "dnplayer"]),
    ("BlueStacks",   ["bluestacks"]),
    ("夜神 Nox",     ["夜神", "nox"]),
    ("逍遥 XYAZ",    ["逍遥", "memu"]),
    ("腾讯 Androws", ["androws"]),
]
LAUNCHERS = ["鹰角", "hypergryph", "arknights", "明日方舟"]
MAA_KEYS = ["maa"]
EXTRA_KEYS = ["bilibili", "哔哩", "netease", "网易"]

EMU_MARKERS = ["MuMuManager.exe", "dnplayer.exe", "HD-Player.exe", "Nox.exe", "MEmu.exe"]
MAA_MARKER = "MaaCore.dll"
PC_MARKER = "Arknights.exe"

ROOTS = [
    (winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", "HKLM-64"),
    (winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall", "HKLM-32"),
    (winreg.HKEY_CURRENT_USER, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", "HKCU"),
]


def enum_uninstall():
    out = []
    for hive, sub, tag in ROOTS:
        try:
            root = winreg.OpenKey(hive, sub)
        except OSError:
            continue
        i = 0
        while True:
            try:
                name = winreg.EnumKey(root, i)
            except OSError:
                break
            i += 1
            try:
                k = winreg.OpenKey(root, name)
            except OSError:
                continue

            def val(n):
                try:
                    v = winreg.QueryValueEx(k, n)[0]
                    return str(v) if v is not None else ""
                except OSError:
                    return ""

            out.append(dict(hive=tag, key=name, display=val("DisplayName"),
                            publisher=val("Publisher"), location=val("InstallLocation"),
                            uninstall=val("UninstallString")))
            winreg.CloseKey(k)
        winreg.CloseKey(root)
    return out


def low(s):
    return (s or "").lower()


def has(hay, keys):
    return any(k in low(hay) for k in keys)


def dir_from_uninstall(s):
    s = (s or "").strip()
    if not s:
        return ""
    m = re.match(r'^"([^"]+)"', s)
    exe = m.group(1) if m else s.split(" ")[0]
    return str(Path(exe).parent) if exe else ""


def scan_roots(markers):
    hits = []
    for letter in "CDEFGH":
        root = letter + ":\\"
        if not os.path.isdir(root):
            continue
        try:
            entries = list(os.scandir(root))
        except OSError:
            continue
        for e in entries:
            if not e.is_dir():
                continue
            for m in markers:
                try:
                    if os.path.isfile(os.path.join(e.path, m)):
                        hits.append((e.path, m))
                except OSError:
                    pass
    return hits


def classify_server(g):
    b = (g / "BLPlatform64").is_dir()
    h = (g / "hgsdk.dll").is_file()
    if b and h:
        return "B服+官服?（异常，需人工看）"
    if b:
        return "B服"
    if h:
        return "官服"
    return "未知"


def pc_games(launcher):
    games = launcher / "games"
    if not games.is_dir():
        return []
    out = []
    for g in sorted(games.iterdir()):
        if g.is_dir() and (g / PC_MARKER).is_file():
            out.append((str(g), classify_server(g)))
    return out


def main():
    show_all = "--all" in sys.argv
    print("=" * 64)
    print("PRTS 本机信息探测 · 验证（只读）")
    print("=" * 64)

    entries = enum_uninstall()
    print("\n[1] 注册表 Uninstall：共 %d 条" % len(entries))

    all_keys = [k for _, ks in EMULATORS for k in ks] + LAUNCHERS + MAA_KEYS + EXTRA_KEYS
    hits = [e for e in entries
            if has(e["display"], all_keys) or has(e["publisher"], all_keys)]
    print("    相关条目：%d 条" % len(hits))
    for e in hits:
        print("\n    · [%s] %s" % (e["hive"], e["key"]))
        print("        DisplayName  : %s" % (e["display"] or "（空）"))
        print("        Publisher    : %s" % (e["publisher"] or "（空）"))
        print("        Location     : %s" % (e["location"] or "（空）"))
        print("        UninstallStr : %s" % (e["uninstall"] or "（空）"))

    if show_all:
        print("\n    —— 全部条目 ——")
        for e in entries:
            print("      [%s] %s" % (e["hive"], e["display"] or "（无名）"))

    print("\n[2] 分层探测结论（层2：注册表）")

    emus = set()
    for e in hits:
        for label, ks in EMULATORS:
            if has(e["display"], ks):
                path = e["location"] or dir_from_uninstall(e["uninstall"])
                emus.add((label, path))
    emu_list = sorted(emus)
    print("    模拟器        : %s" % (emu_list if emu_list else "未检测到"))

    maas = [e for e in hits if has(e["display"], MAA_KEYS)]
    print("    MAA（注册表）  : %s" % ([e["display"] for e in maas] if maas else "未登记（预期：便携版）"))

    launchers = [e for e in hits if has(e["display"], LAUNCHERS)]
    print("    PC 端启动器    : %d 个" % len(launchers))
    pc_found = {}
    for e in launchers:
        d = e["location"] or dir_from_uninstall(e["uninstall"])
        for gpath, server in pc_games(Path(d)):
            print("        %s  →  %s" % (gpath, server))
            pc_found[server] = gpath

    print("\n[3] 盘根浅扫（兜底，层 3/4）")
    emu_scan = scan_roots(EMU_MARKERS)
    maa_scan = scan_roots([MAA_MARKER])
    pc_scan = scan_roots([PC_MARKER])
    for path, marker in emu_scan:
        print("    模拟器特征: %s  (%s)" % (path, marker))
    for path, marker in maa_scan:
        print("    MAA 特征  : %s  (%s)" % (path, marker))
    for path, marker in pc_scan:
        print("    PC 端特征 : %s  (%s)" % (path, marker))
    if not (emu_scan or maa_scan or pc_scan):
        print("    （盘根未见特征文件）")

    print("\n[4] 值维度预览（给 PRTS 的）")
    print("    maa      : %s" % ("已安装" if (maas or maa_scan) else "未检测到"))
    print("    emulator : %s" % ("；".join(l for l, _ in emu_list) if emu_list else "未检测到"))
    print("    game_pc  : %s" % ("；".join(sorted(pc_found)) if pc_found else "未检测到"))
    print()


if __name__ == "__main__":
    main()
