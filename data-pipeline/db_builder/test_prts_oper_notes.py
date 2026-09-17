"""
PRTS oper 备注爬取 — 测试脚本
验证天赋和技能备注的解析逻辑
"""
import urllib.request, urllib.parse, re, time

def fetch_wikitext(name):
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'#(?:REDIRECT|重定向)\s*\[\[([^\]]+)\]\]', text)
    if m:
        return fetch_wikitext(m.group(1))
    return text

def parse_template(text, tpl_name):
    """从 wikitext 提取指定模板的所有实例"""
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
                if depth == 0:
                    content = text[idx+len(start_tag):i]
                    params = {}
                    for part in content.split('\n|'):
                        part = part.strip()
                        if '=' not in part: continue
                        k, v = part.split('=', 1)
                        params[k.lstrip('|').strip()] = v.strip()
                    results.append(params)
                    pos = i + 2
                    break
                i += 2; continue
            i += 1
        else:
            pos = idx + 2
    return results


def clean_wiki(text):
    """简易 wikitext 清洗"""
    if not text: return text
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]', lambda m: m.group(2) or m.group(1), text)
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{异常效果\|([^|}]+)(?:\|([^|}]+))?\}\}', lambda m: m.group(1)+(m.group(2) or ''), text)
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    text = re.sub(r'<br\s*/?>', '\n', text)
    return text.strip()


# ── 测试 ──
test_ops = ['阿米娅', '史尔特尔', '风笛']

for name in test_ops:
    print(f'====== {name} ======')
    try:
        wikitext = fetch_wikitext(name)
    except Exception as e:
        print(f'  获取失败: {e}')
        continue

    # 天赋备注
    talents = parse_template(wikitext, '天赋列表3')
    print(f'  天赋模板数: {len(talents)}')
    for t in talents:
        tag = t.get('天赋', '')
        note = t.get('备注', '(无)')
        print(f'    [{tag}] 备注: {clean_wiki(note)[:120]}')

    # 技能备注
    skills = parse_template(wikitext, '技能2')
    skills += parse_template(wikitext, '技能\n')
    print(f'  技能模板数: {len(skills)}')
    for s in skills:
        sname = s.get('技能名', '?')
        note = s.get('备注', '(无)')
        if note != '(无)':
            print(f'    [{sname}] 备注: {clean_wiki(note)[:120]}')
        else:
            print(f'    [{sname}] 备注: (无)')

    time.sleep(0.3)
    print()
