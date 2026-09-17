# -*- coding: utf-8 -*-
"""
PRTS 终端 — 地块信息表全量重建脚本（维护用）

数据源：ArknightsGameData stage_table.json 的 tileInfo 顶层字段
        + 现有 tile_info.json（保留人工补充的缺失地块描述）
输出：src-tauri/resources/tile_info.json

每次数据源更新后重跑一次即可：
    python rebuild_tile_info.py

采用「重建制」维护：官方 tileInfo 每次覆盖更新；tileInfo 未收录但
DB 中实际出现的地块（活动专属等）由人工补充，重跑时保留旧值不覆盖。
人工补充的「备注」不参与重建处理：旧文件里有的，重建时原样带回。

tile_info.json 结构：
{
  "tileKey": {
    "名称": "中文名（官方或补充）",
    "描述": "语义描述（官方或补充）",
    "功能": true | false,   # 是否为功能性地块
    "备注": "……（可选）"    # 人工补充；重建时原样保留，不做处理
  }
}
"""

import json
import os
import sqlite3

# ─── 路径配置 ─────────────────────────────

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
STAGE_TABLE = os.path.join(DATA_ROOT, "excel", "stage_table.json")
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.normpath(os.path.join(SCRIPT_DIR, "..", ".."))
DST = os.path.join(PROJECT_ROOT, "src-tauri", "resources", "tile_info.json")
DB = os.path.join(PROJECT_ROOT, "data-pipeline", "db_builder", "arknights.db")


# ─── 主流程 ───────────────────────────────

def collect_official():
    """从 stage_table.json tileInfo 提取官方条目。"""
    with open(STAGE_TABLE, encoding="utf-8") as f:
        data = json.load(f)
    tile_info = data.get("tileInfo") or {}
    out = {}
    for k, v in tile_info.items():
        out[k] = {
            "名称": v.get("name", ""),
            "描述": v.get("description", ""),
            "功能": bool(v.get("isFunctional", False)),
        }
    return out


def collect_db_keys():
    """从 DB 收集实际出现过的 tileKey（作为补充条目清单）。"""
    conn = sqlite3.connect(DB)
    rows = conn.execute("SELECT 地图数据 FROM levels WHERE 地图数据 IS NOT NULL").fetchall()
    conn.close()
    keys = set()
    for (raw,) in rows:
        try:
            data = json.loads(raw)
        except Exception:
            continue
        for t in data.get("tiles", []):
            if t:
                keys.add(t[0])
    return keys


def load_old_supplement():
    """读取旧 tile_info.json（用于保留补充条目、原样透传备注）。"""
    old = {}
    if os.path.exists(DST):
        try:
            with open(DST, encoding="utf-8") as f:
                old = json.load(f)
        except Exception as e:
            print(f"警告：读取旧 tile_info.json 失败，补充内容不保留: {e}")
    return old


def build():
    print("====== 地块信息表全量重建 ======")
    print(f"数据源: {STAGE_TABLE}")
    print(f"目标: {DST}")

    official = collect_official()
    db_keys = collect_db_keys()
    old = load_old_supplement()

    missing = sorted(db_keys - set(official.keys()))
    # 官方有但 DB 未出现的（如 tile_water）也保留，保证数据完整性
    extra_official = sorted(set(official.keys()) - db_keys)

    print(f"官方 tileInfo: {len(official)} 种")
    print(f"DB 实际出现:  {len(db_keys)} 种")
    print(f"需人工补充:   {len(missing)} 种（tileInfo 未收录）")
    print(f"官方但 DB 未出现: {len(extra_official)} 种（一并收录）")

    out = dict(official)
    for k in missing:
        old_entry = old.get(k) or {}
        out[k] = {
            "名称": old_entry.get("名称", ""),
            "描述": old_entry.get("描述", ""),
            "功能": bool(old_entry.get("功能", False)),
        }

    # 备注透传：人工补充层，不生成、不修改，旧文件里有的原样带回
    kept_notes = 0
    for k, entry in out.items():
        note = (old.get(k) or {}).get("备注")
        if note:
            entry["备注"] = note
            kept_notes += 1

    if missing:
        print("\n待补充描述的地块：")
        for k in missing:
            has_desc = bool(out[k]["描述"])
            marker = "（已补充）" if has_desc else "（待补充）"
            print(f"  {k:32s} {marker}")

    with open(DST, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=2)

    print(f"备注透传保留: {kept_notes} 条")
    print(f"\n====== 完成：共 {len(out)} 种地块 ======")


if __name__ == "__main__":
    build()
