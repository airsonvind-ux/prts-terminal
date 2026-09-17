"""
PRTS 终端 — 生息演算·重启锚点 战斗数据 & 敌方情报 入库

从 PRTS wiki 的 {{普通关卡信息}} 提取战斗四维，
从 {{敌方情报}} 提取敌人列表（含可能的覆写属性），
写入 arknights.db 的 stages 表。

用法：python build_sandbox2_prts.py
"""

import re, json, sqlite3, os, time, urllib.request, urllib.error, socket

socket.setdefaulttimeout(15)

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")


def fetch_wikitext(code, name):
    """获取 PRTS 页面 raw wikitext，先试 code，再试 code_name"""
    keys = [code]
    if name:
        keys.append(f"{code}_{name}")
    for key in keys:
        try:
            url = f"https://prts.wiki/w/{urllib.parse.quote(key)}?action=raw"
            req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.read().decode("utf-8")
        except urllib.error.HTTPError:
            continue
    return None


def parse_template(text, tpl_name):
    """从 wikitext 提取指定模板参数（处理嵌套 {{}}）"""
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
            if text[i:i+2] == '}}':
                depth -= 1
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


# PRTS 属性名 → 数据库 enemies 表列名 / 中文
PRTS_ATTR_MAP = {
    "最大生命值":  "最大生命值",
    "攻击力":     "攻击力",
    "防御力":     "防御力",
    "法术抗性":   "法术抗性",
    "移动速度":   "移动速度",
    "攻击速度":   "攻击速度",
    "攻击间隔":   "攻击间隔",
    "生命恢复速度": "生命自回速度",
    "重量":       "重量",
    "眩晕抗性":   "晕眩抗性",
    "沉默抗性":   "沉默抗性",
    "沉睡抗性":   "沉睡抗性",
    "冻结抗性":   "冻结抗性",
    "浮空抗性":   "浮空抗性",
    "战栗抗性":   "恐惧抗性",
    "元素抗性":   "元素抗性",
    "损伤抗性":   "损伤抗性",
}


def parse_enemy_intel(text):
    """解析 {{敌方情报}}，返回 [{名称, 级别, 覆写属性?}]

    覆写属性格式：敌人N防御力=30 → {"防御力": 30}
    数值自动转为 int/float
    """
    tpls = parse_template(text, "敌方情报")
    if not tpls:
        return "[]"

    params = tpls[0]
    enemies = []
    i = 1
    while True:
        name_key = f"敌人{i}"
        name = params.get(name_key)
        if name is None:
            break

        level_str = params.get(f"敌人{i}级别", "0")
        try:
            level = int(level_str)
        except ValueError:
            level = 0

        item = {"名称": name, "级别": level}

        # 覆写属性：正则匹配 敌人{i}(.+) 且非 数量/数量下限/级别
        overrides = {}
        for k, v in params.items():
            m = re.match(rf'敌人{i}(.+)', k)
            if not m:
                continue
            attr_raw = m.group(1)
            if attr_raw in ('数量', '数量下限', '级别'):
                continue
            cn_name = PRTS_ATTR_MAP.get(attr_raw, attr_raw)
            # 尝试转数值
            try:
                if '.' in v:
                    val = float(v)
                else:
                    val = int(v)
            except ValueError:
                val = v
            overrides[cn_name] = val

        if overrides:
            item["覆写属性"] = overrides

        enemies.append(item)
        i += 1

    return json.dumps(enemies, ensure_ascii=False)


# ═══════════════════════════════════════════

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    stages = conn.execute("""
        SELECT 关卡ID, 关卡编号, 关卡名
        FROM stages
        WHERE 关卡类型 = '生息演算·重启锚点'
          AND 关卡名 IS NOT NULL
        ORDER BY 关卡ID
    """).fetchall()

    updated = 0
    errors = 0
    start_time = time.time()

    print(f"====== 共 {len(stages)} 关 ======")

    for sid, code, name in stages:
        text = fetch_wikitext(code, name)
        if text is None:
            print(f"❌ {code} {name}: 页面不存在")
            errors += 1
            continue

        # 战斗数据（兼容两种模板）
        info = parse_template(text, "普通关卡信息")
        if not info:
            info = parse_template(text, "区块化关卡信息")
        if info:
            p = info[0]
            char_limit = int(p.get("部署上限", 0)) if p.get("部署上限") else None
            init_cost  = int(p.get("初始COST", 0)) if p.get("初始COST") else None
            max_cost   = int(p.get("COST上限", 0)) if p.get("COST上限") else None
            max_life   = int(p.get("目标点耐久", 0)) if p.get("目标点耐久") else None
        else:
            char_limit = init_cost = max_cost = max_life = None

        # 敌方情报
        enemy_list = parse_enemy_intel(text)

        conn.execute("""
            UPDATE stages
            SET 部署上限 = ?, 初始费用 = ?, 费用上限 = ?, 目标点耐久 = ?,
                敌人列表 = ?
            WHERE 关卡ID = ?
        """, (char_limit, init_cost, max_cost, max_life, enemy_list, sid))
        updated += 1

        if updated <= 3:
            enemies = json.loads(enemy_list)
            print(f"✅ {code} {name}: 部署{char_limit} 初始{init_cost} 上限{max_cost} 耐久{max_life} 敌人{len(enemies)}")

        time.sleep(0.1)

    conn.commit()

    # 验证
    stats = conn.execute("""
        SELECT 关卡ID, 关卡名, 部署上限, 初始费用, 敌人列表
        FROM stages
        WHERE 关卡类型 = '生息演算·重启锚点'
        ORDER BY 关卡ID
    """).fetchall()

    has_data = sum(1 for r in stats if r[2] is not None)
    print(f"\n====== 完成 ======")
    print(f"  更新: {updated}, 错误: {errors}")
    print(f"  有战斗数据: {has_data}/{len(stats)}")

    conn.close()


if __name__ == '__main__':
    main()
