# -*- coding: utf-8 -*-
"""ranges · 重建（攻击范围）

数据源（本地）
    excel/range_table.json · 每个 rangeID 的 grids（[row, col]，(0,0)=干员所在格）

输出
    src-tauri/resources/ranges.json · {rangeID: {"grids": [[r, c], ...], "alias": "…"}}

人工层
    `alias` 由人工补充，重建时从旧文件原样带回，不生成也不修改
    官方没有、旧文件里有的范围键同样保留

口径（沿用现有文件，否则重建一次整表都会 diff）
    坐标按 (row, col) 排序
    键按字符串排序（如 "3-10" 排在 "3-2" 前面）

用法
    python rebuild.py --dry-run    # 只报告，不写文件
    python rebuild.py
"""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import table_base as tb  # noqa: E402

SRC = paths.GAMEDATA / "excel" / "range_table.json"
OUT = paths.RESOURCES / "ranges.json"


def collect_official():
    """range_table.json → {rangeID: {"grids": [[row, col], ...]}}（保持官方顺序）。"""
    with open(SRC, encoding="utf-8") as f:
        data = json.load(f)
    out = {}
    for k, v in data.items():
        out[str(k)] = {"grids": sorted([g.get("row", 0), g.get("col", 0)]
                                       for g in (v.get("grids") or []))}
    return out


def build(old):
    official = collect_official()
    out = {}
    for k, v in official.items():                       # 官方顺序为准
        out[k] = {"grids": v["grids"], "alias": (old.get(k) or {}).get("alias", "")}
    for k, v in old.items():                            # 官方没有的（人工加的范围）保留
        if k not in out:
            out[k] = {"grids": v.get("grids", []), "alias": v.get("alias", "")}
    return {k: out[k] for k in sorted(out)}             # 键按字符串排序


SPEC = tb.RebuildSpec(name="ranges", output=OUT, build=build)

if __name__ == "__main__":
    sys.exit(tb.run_rebuild(SPEC))
