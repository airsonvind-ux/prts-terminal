"""
干员技能 PRTS 描述爬取 · 测试脚本
验证从 PRTS 页面解析技能逐级描述的策略

模板: {{技能2|...|技能N描述=xxx|...}}  和  {{技能|...|技能N描述=xxx|...}}
描述字段: 技能1~7 → Lv1~7, 技能专精1~3 → 专精1~3
"""

import re, urllib.request, urllib.parse, time

def fetch_wikitext(name):
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req, timeout=10) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'#(?:redirect|重定向)\s*\[\[:?([^\]]+)\]\]', text, re.IGNORECASE)
    if m:
        return fetch_wikitext(m.group(1).replace(' ', '_'))
    return text


def parse_template(text, tpl_name):
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1: break
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


def clean_desc(text):
    if not text: return text
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{修正\|([^|}]+?)\|[^}]*?\}\}', r'\1', text)
    text = re.sub(r'\{\{\+\|([^|}]+?)\|([^}]+?)\}\}', r'\2', text)
    text = re.sub(r'\{\{\*\|([^|}]+?)\|([^}]+?)\}\}', r'\2', text)
    text = re.sub(r'<br\s*/?>', '\n', text)
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    return text.strip()


def extract_skill_descs(text):
    skills = []
    for tpl_name in ['技能2', '技能|']:
        tpls = parse_template(text, tpl_name)
        for tpl in tpls:
            name = tpl.get('技能名', '?')
            descs = []
            for i in range(1, 8):
                descs.append(clean_desc(tpl.get(f'技能{i}描述', '')))
            for i in range(1, 4):
                descs.append(clean_desc(tpl.get(f'技能专精{i}描述', '')))
            if any(descs):
                skills.append({'技能名': name, '描述': descs})
    return skills


tests = [
    ('阿米娅',   '3技能含专精'),
    ('斑点',     'blackboard未解析'),
    ('史尔特尔', '强力技能'),
    ('安赛尔',   '范围强化'),
]

for name, desc in tests:
    print(f"====== {name}（{desc}）======")
    try:
        text = fetch_wikitext(name)
        for sk in extract_skill_descs(text):
            print(f"  【{sk['技能名']}】")
            for j, d in enumerate(sk['描述']):
                if d:
                    label = f"Lv{j+1}" if j < 7 else f"专精{j-6}"
                    print(f"    {label}: {d[:120]}{'...' if len(d)>120 else ''}")
    except Exception as e:
        print(f"  ERROR: {e}")
    print()
    time.sleep(0.2)

print("====== 完成 ======")
