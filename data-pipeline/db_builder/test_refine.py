"""
PRTS 终端 — refine 测试脚本
1. 敌人名称解析：enemy_xxx → enemies 表中文名
2. 待处理目标数量：从 PRTS 页面爬取，验证不同类型关卡的页面映射
"""

import sqlite3, json, re, os, urllib.request, urllib.parse, socket

socket.setdefaulttimeout(15)
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
DB = os.path.join(OUTPUT_DIR, "arknights.db")


def fetch_wikitext(code, name, depth=0):
    """获取 PRTS 页面 raw wikitext，自动跟随重定向"""
    if depth > 5:
        return None
    keys = [code]
    if name:
        keys.append(f"{code}_{name}")
    for key in keys:
        try:
            url = f"https://prts.wiki/w/{urllib.parse.quote(key)}?action=raw"
            req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
            with urllib.request.urlopen(req, timeout=10) as resp:
                text = resp.read().decode("utf-8")
            # 跟随重定向
            m = re.match(r'#(?:redirect|重定向)\s*\[\[:?([^\]]+)\]\]', text, re.IGNORECASE)
            if m:
                target = m.group(1).replace(' ', '_')
                return fetch_wikitext(target, None, depth + 1)
            return text
        except urllib.error.HTTPError:
            continue
    return None


def parse_enemy_count(text):
    """从 PRTS wikitext 提取敌人数量"""
    m = re.search(r'敌人数量\s*=\s*(\d+)', text)
    if m:
        return int(m.group(1))
    # 也可能是 N/A 或空
    if re.search(r'敌人数量\s*=\s*[Nn]/[Aa]', text):
        return None
    return None


# ═══════════════════════════════════════════

conn = sqlite3.connect(DB)

# ─── 1. 敌人名称解析测试 ───

print("====== 1. 敌人名称解析 =====")

# 构建 enemies 索引：敌人ID → 名称
enemy_map = {}
for r in conn.execute("SELECT 敌人ID, 名称 FROM enemies"):
    enemy_map[r[0]] = r[1]
print(f"enemies 表索引: {len(enemy_map)} 条")

# 找几条有未解析英文名的关卡
unresolved_stages = conn.execute("""
    SELECT 关卡ID, 关卡类型, 敌人列表
    FROM stages
    WHERE 敌人列表 LIKE '%enemy_%'
    LIMIT 5
""").fetchall()

for sid, stype, el_raw in unresolved_stages:
    el = json.loads(el_raw)
    to_fix = [e for e in el if e['名称'].startswith('enemy_')]
    if to_fix:
        print(f"\n{sid} ({stype}): {len(to_fix)}/{len(el)} 未解析")
        for e in to_fix[:3]:
            old = e['名称']
            new = enemy_map.get(old)
            print(f"  {old} → {new}")

# 可以成功解析的统计
resolvable = sum(1 for eid in enemy_map if eid.startswith('enemy_') and enemy_map[eid] != eid)
print(f"\nenemies 表中可解析的 enemy_ 前缀ID: {resolvable}")

# ─── 2. PRTS 敌人数量爬取测试 ───

print("\n====== 2. PRTS 敌人数量爬取 =====")

# 不同类型关卡各取几条，测试页面映射和敌人数量解析
test_stages = [
    ("主线", "main_01-07", "1-7", "暴君"),
    ("主线", "main_00-01", "0-1", "坍塌"),
    ("主线", "main_00-01#f#", "0-1", "坍塌"),  # 突袭，同一页面
    ("活动", "act35side_01", None, None),  # 活动关卡，编编号可能不同
    ("集成战略·萨卡兹的无终奇语", "ro4_n_1_1", "ISW-NO", "坏邻居"),
    ("悖论模拟", "mem_12fce_1", "mem_12fce_1", "生还者"),
]

# 先看数据库中各类型的关卡编号和名称格式
print("\n--- 各类型关卡编号/名称 样例 ---")
for stype in conn.execute("""
    SELECT DISTINCT 关卡类型 FROM stages
    WHERE 关卡编号 IS NOT NULL AND 关卡名 IS NOT NULL
    ORDER BY 关卡类型
""").fetchall():
    stype = stype[0]
    samples = conn.execute("""
        SELECT 关卡ID, 关卡编号, 关卡名 FROM stages
        WHERE 关卡类型 = ? AND 关卡编号 IS NOT NULL AND 关卡名 IS NOT NULL
        LIMIT 2
    """, (stype,)).fetchall()
    for sid, code, name in samples:
        # 测试页面是否存在
        text = fetch_wikitext(code, name)
        status = "✅" if text else "❌"
        count = parse_enemy_count(text) if text else None
        print(f"  {status} {stype}: {code}_{name} → 敌人数量={count}")

conn.close()

print("\n====== 测试完成 =====")
