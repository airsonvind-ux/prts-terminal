"""验证 arknights.db 内容"""
import sqlite3, json

DB = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db"
conn = sqlite3.connect(DB)

# 基础统计
for table in ("enemies", "levels"):
    count = conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
    print(f"[{table}] {count} 条")

# 敌人样本
print("\n── enemies 样本（前 5 条）──")
for row in conn.execute(
    "SELECT enemy_id, name, hp_base, atk_base, def_base, res_base, attack_interval, attack_range, damage_type "
    "FROM enemies LIMIT 5"
):
    print(f"  {row[0]} | {row[1]} | HP:{row[2]} ATK:{row[3]} DEF:{row[4]} RES:{row[5]} | 间隔:{row[6]} 射程:{row[7]} | {row[8]}")

# 特性不为空的敌人
with_ability = conn.execute("SELECT COUNT(*) FROM enemies WHERE ability IS NOT NULL").fetchone()[0]
print(f"\n  有特性的敌人：{with_ability}")

# 关卡样本
print("\n── levels 样本 ──")
for row in conn.execute(
    "SELECT level_id, code, name, zone_id, ap_cost, "
    "enemy_list IS NOT NULL as has_enemies, waves_detail IS NOT NULL as has_waves "
    "FROM levels LIMIT 5"
):
    print(f"  {row[0]} | {row[1]}「{row[2]}」| zone:{row[3]} AP:{row[4]} | 敌人:{'✓' if row[5] else '✗'} 波次:{'✓' if row[6] else '✗'}")

# 查一条有 wave 的关卡详细数据
row = conn.execute(
    "SELECT code, name, enemy_list, waves_detail FROM levels "
    "WHERE waves_detail IS NOT NULL AND code = '1-7' LIMIT 1"
).fetchone()
if row:
    print(f"\n── {row[0]}「{row[1]}」详细数据 ──")
    enemies = json.loads(row[2])
    print(f"  敌人种类：{len(enemies)}，ID 列表：{enemies[:10]}{'...' if len(enemies)>10 else ''}")
    waves = json.loads(row[3])
    print(f"  波次总数：{len(waves)}")
    for w in waves[:3]:  # 只展示前 3 波
        print(f"    第{w['wave']}波：")
        for gate, actions in w['gates'].items():
            for a in actions:
                print(f"      红门{gate}: {a['enemy']} x{a['count']} (preDelay:{a['preDelay']}s interval:{a['interval']}s)")

# 检查远程敌人
ranged = conn.execute(
    "SELECT COUNT(*) FROM enemies WHERE attack_range > 0"
).fetchone()[0]
melee = conn.execute(
    "SELECT COUNT(*) FROM enemies WHERE attack_range <= 0"
).fetchone()[0]
print(f"\n  近战/无攻击敌人：{melee}")
print(f"  远程敌人（攻击范围>0）：{ranged}")

conn.close()
