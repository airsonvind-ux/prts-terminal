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

import argparse
import json
import os
import sqlite3
import sys
import time

# ─── 路径配置（唯一来源见 data-pipeline/paths.py）─────────────

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from paths import DATA_SOURCE, GAMEDATA as DATA_ROOT, DB_PATH, RESOURCES  # noqa: E402

import update_common as uc  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import looks_dirty  # noqa: E402   # 残留标记表（含 <@ / <$）

STAGE_TABLE = os.path.join(DATA_ROOT, "excel", "stage_table.json")
DST = os.path.join(RESOURCES, "tile_info.json")
DB = str(DB_PATH)


# ─── 主流程 ───────────────────────────────

def collect_official():
    """从 stage_table.json tileInfo 提取官方条目。

    名称/描述过 cleaning_game.clean_desc：游戏数据里带富文本标签
    （`<@ba.kw>能量束</>`、`<color=#…>…</color>`），数据库那批表就是这么洗的；
    `<浆果虫>` 这类实体引用由 clean_desc 保留。
    """
    with open(STAGE_TABLE, encoding="utf-8") as f:
        data = json.load(f)
    tile_info = data.get("tileInfo") or {}
    out = {}
    for k, v in tile_info.items():
        out[k] = {
            "名称": clean_desc(v.get("name", "")),
            "描述": clean_desc(v.get("description", "")),
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


def build(dry_run=False):
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
    # （补充条目的名称/描述也来自旧文件，即当年人工写的，同样不过清洗）
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

    # 残留检查：官方字段过 clean_desc 后不应再有富文本标记（拿现成的标记表验，不自列）
    dirty = [k for k, e in out.items()
             if looks_dirty(e.get("名称", "")) or looks_dirty(e.get("描述", ""))]
    if dirty:
        raise RuntimeError(
            "清洗后仍有残留标记，官方字段应全过 clean_desc；请检查口径：%s" % dirty[:8])

    # ── 重建前后对比 ───────────────────────────────
    added = [k for k in out if k not in old]
    gone = [k for k in old if k not in out]
    changed = [k for k in out if k in old and old[k] != out[k]]
    text = json.dumps(out, ensure_ascii=False, indent=2)
    identical = False
    if os.path.exists(DST):
        with open(DST, encoding="utf-8") as f:
            identical = f.read() == text
    print(f"\n新增 {len(added)} {added[:6]}；消失 {len(gone)} {gone[:6]}；变化 {len(changed)} {changed[:6]}")
    same_order = [k for k in old if k in out] == [k for k in out if k in old]
    print(f"键序一致（共同键）: {same_order}（现有 {len(old)} 键 / 重建 {len(out)} 键）")
    print(f"与现有文件逐字节一致: {identical}")
    print(f"备注透传保留: {kept_notes} 条")

    if dry_run:
        print("\n(干跑：未写文件)")
        return

    uc.backup_file(DST)                      # 写前备份（保留 3 份）
    with open(DST, "w", encoding="utf-8") as f:
        f.write(text)

    record = {
        "脚本": {"文件": "maintenance/rebuild_tile_info.py", "版本": 1},
        "运行时间": time.strftime("%Y-%m-%d %H:%M:%S"),
        "模式": "重建",
        "数据源": dict(
            {"文件": "ArknightsGameData/excel/stage_table.json",
             "库": "maintenance/arknights.db（levels.地图数据，用于补全清单）"},
            **uc.read_source_version(DATA_SOURCE)),
        "表": "tile_info.json",
        "计数": {"官方": len(official), "DB 出现": len(db_keys),
                 "需人工补充": len(missing), "输出": len(out),
                 "备注透传": kept_notes},
        "新增": added,
        "新增数": len(added),
        "消失": gone,
        "变化": changed,
        "结果": "成功",
    }
    print(f"\n====== 完成：共 {len(out)} 种地块 ======")
    print("记录：%s" % os.path.basename(uc.write_record("tile_info", record)))


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description="重建地块信息表 tile_info.json")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    build(dry_run=ap.parse_args().dry_run)
