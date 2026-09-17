"""
修正敌人表中"地位"列，从 handbook 切换为实际游戏数据（enemy_database.json）

背景：handbook.enemyLevel 与 enemy_database.levelType 存在 170 处不一致。
       enemy_database 是游戏实际使用的分类，应为权威来源。
       本脚本仅更新地位列，不触及 build_enemies.py 的其他逻辑。

用法：python fix_enemy_status.py
"""

import json, sqlite3, os

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(SCRIPT_DIR, "arknights.db")
DATA_DIR = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\levels\enemydata"

STATUS_MAP = {
    "NORMAL": "普通",
    "ELITE":  "精英",
    "BOSS":   "领袖",
}


def mv(obj, default=None):
    """解包 m_defined/m_value"""
    if obj is None:
        return default
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_value"]
    return obj


def is_defined(obj):
    if obj is None:
        return False
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_defined"]
    return True


def get_level_type(enemy_entry, level):
    """从 enemy_database 条目中取指定 level 的 levelType，带回退到 level 0"""
    lv0_ed = None
    for v in enemy_entry.get("Value", []):
        if v["level"] == 0:
            lv0_ed = v.get("enemyData", {})
            break

    for v in enemy_entry.get("Value", []):
        if v["level"] == level:
            ed = v.get("enemyData", {})
            obj = ed.get("levelType")
            if not is_defined(obj) and lv0_ed:
                obj = lv0_ed.get("levelType")
            return mv(obj)
    return None


def main():
    print("=" * 70)
    print("  敌人地位修正")
    print("=" * 70)

    # 加载 enemy_database.json
    db_path = os.path.join(DATA_DIR, "enemy_database.json")
    with open(db_path, "r", encoding="utf-8") as f:
        game_db = json.load(f)

    # 建立索引: enemy_id → entry
    db_index = {}
    for e in game_db["enemies"]:
        db_index[e["Key"]] = e

    # 连接 SQLite
    conn = sqlite3.connect(DB_PATH)
    cur = conn.cursor()

    # 读取需要检查的敌人
    rows = cur.execute("SELECT 敌人ID, 级别, 地位 FROM enemies ORDER BY 敌人ID, 级别").fetchall()

    updated = 0
    no_data = 0
    unchanged = 0
    samples = []

    for enemy_id, level, current_status in rows:
        entry = db_index.get(enemy_id)
        if not entry:
            no_data += 1
            continue

        raw = get_level_type(entry, level)
        if raw is None:
            no_data += 1
            continue

        new_status = STATUS_MAP.get(raw, raw)

        if new_status != current_status:
            cur.execute("UPDATE enemies SET 地位 = ? WHERE 敌人ID = ? AND 级别 = ?",
                        (new_status, enemy_id, level))
            updated += 1
            if len(samples) < 20:
                samples.append((enemy_id, level, current_status, new_status))
        else:
            unchanged += 1

    conn.commit()

    # 报告
    print(f"\n  检查条目: {len(rows)}")
    print(f"  已修正:   {updated}")
    print(f"  无变化:   {unchanged}")
    print(f"  无数据源: {no_data}")

    if samples:
        print(f"\n  修正样本:")
        for sid, lv, old, new in samples:
            name_row = cur.execute("SELECT 名称 FROM enemies WHERE 敌人ID = ? AND 级别 = 0", (sid,)).fetchone()
            name = name_row[0] if name_row else "?"
            print(f"    {sid:<35s} Lv{lv} {name:<15s} {old} → {new}")

    conn.close()
    print(f"\n  完成")


if __name__ == "__main__":
    main()
