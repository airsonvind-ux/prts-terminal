"""
PRTS 终端 — 生息演算·重启锚点 战斗数据爬取 · 测试脚本

从 PRTS wiki 的单关页面解析 部署上限/初始费用/费用上限/目标点耐久/敌方情报
单页测试，输出到终端
"""

import re, urllib.request, urllib.parse, sqlite3, os, json

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(OUTPUT_DIR, "arknights.db")


def fetch_wikitext(code, name):
    """获取 PRTS 页面原始 wikitext，两种 URL 模式"""
    keys = [code]
    if name:
        keys.append(f"{code}_{name}")
    last_err = None
    for key in keys:
        try:
            url = f"https://prts.wiki/w/{urllib.parse.quote(key)}?action=raw"
            req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.read().decode("utf-8")
        except urllib.error.HTTPError as e:
            last_err = f"HTTP {e.code}"
        except Exception as e:
            last_err = str(e)
    raise RuntimeError(last_err or "未知错误")


def parse_template(text, tpl_name):
    """从 wikitext 提取指定模板（处理嵌套 {{}}）"""
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break
        depth, i = 0, idx
        max_iter = 50000
        while i < len(text) and max_iter > 0:
            max_iter -= 1
            if text[i:i+2] == '{{': depth += 1; i += 2; continue
            if text[i:i+2] == '}}': depth -= 1
            if depth == 0: end = i + 2; break
            i += 2; continue
            i += 1
        content = text[idx+len(start_tag):end-2]
        params = {}
        parts = re.split(r'\n\|', content)
        first = parts[0].strip()
        if first.startswith('|'): first = first[1:]
        if '=' in first:
            k, v = first.split('=', 1)
            params[k.strip()] = v.strip()
        for part in parts[1:]:
            part = part.strip()
            if '=' not in part: continue
            k, v = part.split('=', 1)
            params[k.strip()] = v.strip()
        results.append(params)
        pos = end
    return results


def parse_enemy_intel(text):
    """解析 {{敌方情报}} 模板，提取敌人列表 [{名称, 级别}]"""
    enemies = []
    tpls = parse_template(text, "敌方情报")
    if not tpls:
        return enemies

    params = tpls[0]
    i = 1
    while True:
        name = params.get(f"敌人{i}")
        if name is None:
            break
        level = params.get(f"敌人{i}级别", "0")
        try:
            level = int(level)
        except ValueError:
            level = 0
        enemies.append({"名称": name, "级别": level})
        i += 1

    return enemies


# ═══════════════════════════════════════════

# 从数据库读取重启锚点关卡（有名称的）
conn = sqlite3.connect(DB_PATH)
stages = conn.execute("""
    SELECT 关卡ID, 关卡编号, 关卡名
    FROM stages
    WHERE 关卡类型 = '生息演算·重启锚点'
      AND 关卡名 IS NOT NULL
    ORDER BY 关卡ID
""").fetchall()
conn.close()

print(f"共 {len(stages)} 个关卡\n")

# 测试前 3 个
for sid, code, name in stages[:5]:
    print(f"======= {code} {name} =======")
    try:
        text = fetch_wikitext(code, name)
    except RuntimeError as e:
        print(f"  ❌ 获取失败: {e}")
        continue

    # 普通关卡信息
    info = parse_template(text, "普通关卡信息")
    if info:
        p = info[0]
        print(f"  部署上限: {p.get('部署上限', '?')}")
        print(f"  初始COST: {p.get('初始COST', '?')}")
        print(f"  COST上限: {p.get('COST上限', '?')}")
        print(f"  目标点耐久: {p.get('目标点耐久', '?')}")
        print(f"  敌人数量: {p.get('敌人数量', '?')}")

    # 敌方情报
    enemies = parse_enemy_intel(text)
    if enemies:
        print(f"  敌方情报 ({len(enemies)} 种):")
        for e in enemies:
            print(f"    {e['名称']} (级别{e['级别']})")
    else:
        print(f"  无敌方情报")

    print()

print("======= 测试完成 =======")
