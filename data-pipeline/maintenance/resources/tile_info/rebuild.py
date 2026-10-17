# -*- coding: utf-8 -*-
"""tile_info · 重建（地块信息）

数据源（本地）
    excel/stage_table.json 的 tileInfo 顶层字段（官方唯一来源）

输出
    src-tauri/resources/tile_info.json · {tileKey: {名称, 描述, 功能, 备注?}}

人工层
    `备注` 是手工写的说明，挂在官方地块上：重建时从旧文件读回，不生成、不改写。
    官方未收录的地块**不进表**——游戏不使用它们，版本更新后也会消失。

清洗
    名称 / 描述过 cleaning_game.clean_desc（游戏数据带 <@ba.kw> 这类富文本标签）；
    `<浆果虫>` 这类实体引用保留。洗完仍有残留标记则中止。

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
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import looks_dirty  # noqa: E402   # 残留标记表（含 <@ / <$）

STAGE_TABLE = paths.GAMEDATA / "excel" / "stage_table.json"
OUT = paths.RESOURCES / "tile_info.json"


def build(old):
    with open(STAGE_TABLE, encoding="utf-8") as f:
        tile_info = json.load(f).get("tileInfo") or {}

    out = {}
    for k, v in tile_info.items():
        out[k] = {
            "名称": clean_desc(v.get("name", "")),
            "描述": clean_desc(v.get("description", "")),
            "功能": bool(v.get("isFunctional", False)),
        }

    # 备注：人工列，从旧文件原样带回
    kept = 0
    for k, entry in out.items():
        note = (old.get(k) or {}).get("备注")
        if note:
            entry["备注"] = note
            kept += 1
    print("  官方 %d 种 | 备注保留 %d 条" % (len(out), kept))
    dropped = [k for k in old if k not in out]
    if dropped:
        print("  旧文件有、官方收录里没有（不再收录）: %d %s"
              % (len(dropped), dropped[:6]))

    # 残留检查：官方字段过 clean_desc 后不应再有富文本标记
    dirty = [k for k, e in out.items()
             if looks_dirty(e.get("名称", "")) or looks_dirty(e.get("描述", ""))]
    if dirty:
        raise RuntimeError("清洗后仍有残留标记，官方字段应全过 clean_desc：%s" % dirty[:8])
    return out


SPEC = tb.RebuildSpec(name="tile_info", output=OUT, build=build)

if __name__ == "__main__":
    sys.exit(tb.run_rebuild(SPEC))
