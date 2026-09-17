"""
PRTS 敌人技能/天赋爬取 — 测试脚本
"""

import re, urllib.request, urllib.parse

def fetch_wikitext(name):
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req) as resp:
        return resp.read().decode("utf-8")

def parse_template(text, tpl_name):
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1: break
        depth, i = 0, idx
        while i < len(text):
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

def clean_wiki(text):
    """
    清洗 PRTS wikitext 标记，白名单模式。
    只处理已知模板/语法，未知标记保留原文。

    规则概览：
    ① [[page]] 或 [[page|显示]]        → 取显示文字，无显示则取 page
    ② {{color|#xxx|text}}            → 取 text（去除颜色标记）
    ③ {{异常效果|name|flag}}          → name+flag（如 沉睡|免疫 → 沉睡免疫）
    ④ {{术语|key|display}}           → 取 display（丢弃引擎 key）
    ⑤ {{变动数值|value|arrow}}        → value + 方向（如 5%↓）
    ⑥ {{特殊机制|name}}              → 取 name
    ⑦ {{popup/range|code}}           → [攻击范围: code]
    ⑧ <br> / <br/>                   → 换行
    ⑨ '''粗体'''                      → 去标记
    ⑩ 多条空行                        → 压缩为一条
    """
    if not text: return text

    # ① [[链接|显示]] → 显示，或 [[链接]] → 链接
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)

    # ② {{color|#xxx|text}} → text
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)

    # ③ {{异常效果|name|flag}} → name+flag
    text = re.sub(r'\{\{异常效果\|([^|}]+)(?:\|([^|}]+))?\}\}',
                  lambda m: m.group(1) + (m.group(2) or ''), text)

    # ④ {{术语|key|display}} → display
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)

    # ⑤ {{变动数值|value|arrow}} → value + 方向标记
    def _arrow(m):
        val, arr = m.group(1), m.group(2)
        if arr == '-': return val + '↓'
        if arr == '+': return val + '↑'
        return val
    text = re.sub(r'\{\{变动数值\|([^|}]+?)\|([^}]+?)\}\}', _arrow, text)

    # ⑥ {{特殊机制|name}} → name
    text = re.sub(r'\{\{特殊机制\|([^}]+?)\}\}', r'\1', text)

    # ⑦ {{popup/range|code}} → [攻击范围: code]
    text = re.sub(r'\{\{popup/range\|([^}]+?)\}\}', r'[攻击范围: \1]', text)

    # ⑧ <br> → 换行
    text = re.sub(r'<br\s*/?>', '\n', text)

    # ⑨ '''粗体''' → 去标记
    text = re.sub(r"'''([^']*)'''", r'\1', text)

    # ⑩ 多余空行压缩
    text = re.sub(r'\n{3,}', '\n\n', text)

    return text.strip()

def scrape_enemy(name):
    print(f"====== {name} ======")
    wikitext = fetch_wikitext(name)
    levels = parse_template(wikitext, "敌人信息/levelcontent")
    if not levels:
        print("  ⚠ 未找到 levelcontent")
        return
    lv0 = levels[0]
    for lv in levels:
        idx = lv.get('index', '0')
        merged = dict(lv0); merged.update(lv)
        print(f"\n--- 技能 (level {idx}) ---")
        count = 0
        for key in sorted(merged.keys()):
            m = re.match(r'^技能(\d+)$', key)
            if not m: continue
            si = m.group(1)
            name     = clean_wiki(merged.get(key, ''))
            init_cd  = merged.get(f'技能{si}初始', '')
            cycle_cd = merged.get(f'技能{si}消耗', '')
            sp_cost  = merged.get(f'技能{si}技力', '/')
            effect   = clean_wiki(merged.get(f'技能{si}效果', ''))
            stype    = merged.get(f'技能{si}类型', '')
            print(f"\n  [{si}] {name}")
            if stype: print(f"  类型: {stype}")
            print(f"  初始冷却: {init_cd}  周期冷却: {cycle_cd}  技力消耗: {sp_cost}")
            if effect: print(f"  效果: {effect}")
            count += 1
        if count == 0: print("  (无技能)")
    for lv in levels:
        idx = lv.get('index', '0')
        merged = dict(lv0); merged.update(lv)
        talent = clean_wiki(merged.get('天赋', ''))
        print(f"\n--- 天赋&能力 (level {idx}) ---")
        print(talent if talent else "  (无)")

if __name__ == '__main__':
    for name in ['霜星', '源石虫', '“殉道者”，安多恩']:
        try: scrape_enemy(name); print("\n" + "="*60 + "\n")
        except Exception as e: print(f"  错误: {e}\n")
