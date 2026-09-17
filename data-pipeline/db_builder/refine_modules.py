"""
PRTS 终端 — 模组数据修正

修复问题：
1. 模组 数值更新 中的英文属性名 → 中文
   attack_speed→攻击速度, block_cnt→阻挡数, magic_resistance→法术抗性,
   respawn_time→再部署时间, cost→部署费用
2. 天赋更新文本 → 新增天赋
   当天赋更新文本提到的天赋名不在干员天赋列表中时，改为"新增天赋"标签

用法：python refine_modules.py
操作库：同目录下的 arknights.db（原地修改，幂等可重复跑）
"""

import json
import sqlite3
import os
import time

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(OUTPUT_DIR, "arknights.db")

# ─── 英文属性名映射 ────────────────────────

ATTR_MAP = {
    "attack_speed": "攻击速度",
    "block_cnt": "阻挡数",
    "magic_resistance": "法术抗性",
    "respawn_time": "再部署时间",
    "cost": "部署费用",
}


def fix_attr_names(mods: list) -> bool:
    """修复 数值更新 中的英文属性名。返回是否有修改。"""
    changed = False
    for m in mods:
        for lvl in m.get("各级效果", []):
            for nu in lvl.get("数值更新", []):
                attr = nu.get("属性", "")
                if attr in ATTR_MAP:
                    nu["属性"] = ATTR_MAP[attr]
                    changed = True
    return changed


def detect_new_talents(mods: list, talent_names: set) -> bool:
    """
    检测天赋更新文本中实际为新增天赋的条目。
    判定依据：天赋名不在干员天赋列表中 → 重命名 key 为"新增天赋"。
    返回是否有修改。
    """
    changed = False
    for m in mods:
        for lvl in m.get("各级效果", []):
            txt = lvl.get("天赋更新文本")
            if not txt:
                continue
            # 已经修正过的跳过
            if "新增天赋" in lvl:
                continue

            # 提取天赋名（格式："天赋名：描述..."）
            if "：" in txt or ":" in txt:
                sep = "：" if "：" in txt else ":"
                mentioned = txt.split(sep)[0].strip()
                if mentioned and mentioned not in talent_names:
                    # 重命名 key
                    lvl["新增天赋"] = txt
                    del lvl["天赋更新文本"]
                    changed = True
    return changed


# ─── 主流程 ────────────────────────────────

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    operators = conn.execute("""
        SELECT 干员ID, 名称, 模组列表, 天赋列表
        FROM operators
        WHERE 模组列表 IS NOT NULL AND 模组列表 != '[]'
        ORDER BY 名称
    """).fetchall()

    attr_fixed = 0
    talent_fixed = 0
    talent_details = []
    start = time.time()

    print(f"====== 模组数据修正 ({len(operators)} 名干员有模组) ======")

    for oid, name, mods_json, talents_json in operators:
        try:
            mods = json.loads(mods_json)
        except json.JSONDecodeError:
            print(f"  [SKIP] {name}: 模组列表 JSON 解析失败")
            continue

        modified = False

        # 修复 1：英文属性名
        if fix_attr_names(mods):
            attr_fixed += 1
            modified = True

        # 修复 2：新增天赋检测
        talent_names = set()
        if talents_json and talents_json != "null":
            try:
                raw_talents = json.loads(talents_json)
                for t in raw_talents:
                    tn = t.get("天赋名")
                    if tn:
                        talent_names.add(tn)
            except json.JSONDecodeError:
                pass

        if talent_names and detect_new_talents(mods, talent_names):
            talent_fixed += 1
            modified = True
            # 记录详情
            for m in mods:
                for lvl in m.get("各级效果", []):
                    if "新增天赋" in lvl:
                        talent_details.append(
                            f"{name} | {m.get('名称', '?')} | {lvl['新增天赋'][:60]}"
                        )

        if modified:
            conn.execute(
                "UPDATE operators SET 模组列表 = ? WHERE 干员ID = ?",
                (json.dumps(mods, ensure_ascii=False), oid),
            )

    conn.commit()
    conn.close()

    elapsed = time.time() - start

    print(f"\n====== 完成 ({elapsed:.1f}s) ======")
    print(f"  英文属性名修正: {attr_fixed} 名干员")
    print(f"  新增天赋纠正:   {talent_fixed} 名干员")

    if talent_details:
        print(f"\n  新增天赋详情:")
        for d in talent_details:
            print(f"    {d}")


if __name__ == "__main__":
    main()
