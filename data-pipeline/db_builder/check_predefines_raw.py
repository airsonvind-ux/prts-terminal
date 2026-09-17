"""查看 level_act6d5_04 的预置单位原始值"""
import sqlite3

conn = sqlite3.connect(r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db")
row = conn.execute(
    "SELECT levelId, 预置单位 FROM levels WHERE levelId='level_act6d5_04'"
).fetchone()
print("levelId:", row[0])
val = row[1]
print("预置单位类型:", type(val).__name__)
print("预置单位长度:", len(val) if val else 0)
print("repr:", repr(val[:200]) if val else "NULL")

# 同时看看其他关卡是否正常
print("\n=== 抽查其他关卡 ===")
for lid in ["level_a001_01", "level_act12mini_04"]:
    r = conn.execute("SELECT 预置单位 FROM levels WHERE levelId=?", (lid,)).fetchone()
    v = r[0] if r else None
    print(f"{lid}: len={len(v) if v else 0} 前30字符={repr(v[:30]) if v else 'NULL'}")
