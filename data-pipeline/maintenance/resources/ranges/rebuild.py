# -*- coding: utf-8 -*-
"""PRTS 终端 — 攻击范围表重建脚本（维护用）

数据源：ArknightsGameData `excel/range_table.json`
        + 现有 ranges.json（保留人工 alias；官方没有的范围键也原样保留）
输出：src-tauri/resources/ranges.json

每次数据源更新后重跑一次即可：
    python rebuild_ranges.py --dry-run     # 先看会变什么
    python rebuild_ranges.py

采用「重建制」：官方 grids 每次覆盖更新；`alias` 是人工补充层，
重跑时原样带回，不生成也不修改。

ranges.json 结构：
{
  "0-1": {"grids": [[0, 0]], "alias": ""},   # grids 为 [row, col]，(0,0) = 干员所在格
  ...
}
"""

import argparse
import json
import os
import sys
import time

# ─── 路径配置（唯一来源见 data-pipeline/paths.py）─────────────

from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)
from paths import DATA_SOURCE, GAMEDATA as DATA_ROOT, RESOURCES  # noqa: E402

import update_common as uc  # noqa: E402

SRC = os.path.join(str(DATA_ROOT), "excel", "range_table.json")
DST = os.path.join(str(RESOURCES), "ranges.json")


def collect_official():
    """range_table.json → {rangeID: {"grids": [[row, col], ...]}}（保持官方顺序）"""
    with open(SRC, encoding="utf-8") as f:
        data = json.load(f)
    out = {}
    for k, v in data.items():
        # 坐标按 (row, col) 排序：沿用原 ranges.json 的口径，
        # 否则官方给的点序一变，整个文件的每一行都会 diff
        grids = sorted([g.get("row", 0), g.get("col", 0)] for g in (v.get("grids") or []))
        out[str(k)] = {"grids": grids}
    return out


def main():
    ap = argparse.ArgumentParser(description="重建攻击范围表 ranges.json")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    args = ap.parse_args()

    official = collect_official()
    old = {}
    if os.path.exists(DST):
        with open(DST, encoding="utf-8") as f:
            old = json.load(f)

    out = {}
    for k, v in official.items():                     # 官方顺序为准
        out[k] = {"grids": v["grids"], "alias": (old.get(k) or {}).get("alias", "")}
    for k, v in old.items():                          # 官方没有的（人工加的范围）保留
        if k not in out:
            out[k] = {"grids": v.get("grids", []), "alias": v.get("alias", "")}
    # 键按字符串排序：沿用原 ranges.json 的口径（如 "3-10" 排在 "3-2" 前），
    # 否则重建一次整表键序都会动
    out = {k: out[k] for k in sorted(out)}

    added = [k for k in official if k not in old]
    gone = [k for k in old if k not in official]
    changed = [k for k in official if k in old and old[k].get("grids") != out[k]["grids"]]
    aliased = [k for k, v in out.items() if v.get("alias")]

    print("====== 攻击范围表重建 ======")
    print("官方范围 : %d 项" % len(official))
    print("原文件   : %d 项（其中带 alias 的 %d 项）" % (len(old), len(aliased)))
    print("输出     : %d 项" % len(out))
    for label, keys in (("新增（官方有、原文件无）", added),
                        ("原文件有、官方无（保留）", gone),
                        ("grids 变化", changed)):
        print("%s: %d %s" % (label, len(keys), keys[:6] if keys else ""))
        for k in keys[:3]:
            print("    %-8s 原 %s" % (k, old.get(k, {}).get("grids")))
            print("    %-8s 新 %s" % ("", out[k]["grids"]))

    text = json.dumps(out, ensure_ascii=False, indent=2)
    identical = False
    if os.path.exists(DST):
        with open(DST, encoding="utf-8") as f:
            identical = f.read() == text
    print("与现有文件逐字节一致: %s" % identical)

    if args.dry_run:
        print("\n(干跑：未写文件)")
        return 0

    uc.backup_file(DST)                      # 写前备份（保留 3 份）
    with open(DST, "w", encoding="utf-8") as f:
        f.write(text)

    record = {
        "脚本": {"文件": "maintenance/rebuild_ranges.py", "版本": 1},
        "运行时间": time.strftime("%Y-%m-%d %H:%M:%S"),
        "模式": "重建",
        "数据源": dict({"文件": "ArknightsGameData/excel/range_table.json"},
                   **uc.read_source_version(DATA_SOURCE)),
        "表": "ranges.json",
        "计数": {"官方": len(official), "原文件": len(old), "输出": len(out)},
        "新增": added,
        "新增数": len(added),
        "原文件有官方无": gone,
        "grids 变化": changed,
        "结果": "成功",
    }
    print("\n====== 完成：写入 %s ======" % DST)
    print("记录：%s" % os.path.basename(uc.write_record("ranges", record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
