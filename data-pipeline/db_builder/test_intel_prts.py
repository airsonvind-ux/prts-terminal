"""
情报提取 · 测试脚本 v2

每类型随机抽检，验证：
1. PRTS URL 映射（含 10章+ 磨难独立页面）
2. 情报字段存在性
3. 提取内容样例
"""

import re, sqlite3, os, random, urllib.request, urllib.parse, socket

socket.setdefaulttimeout(10)
DB = os.path.join(os.path.dirname(os.path.abspath(__file__)), "arknights.db")

URL_RULES = {
    "生息演算·沙洲遗闻": "{code}_{name}(沙洲遗闻)",
    "悖论模拟":             "悖论模拟_{name}",
}
DEFAULT_URL = "{code}_{name}"
SKIP_TYPES = {"全息作战矩阵"}
SAMPLE_SIZE = 2

TPL_NAMES = [
    "普通关卡信息", "突袭关卡信息", "剿灭关卡信息", "保全派驻关卡信息",
    "集成战略关卡信息", "教学关卡信息", "活动关卡信息", "主线关卡信息",
    "支线关卡信息", "区块化关卡信息", "磨难险地关卡信息",
]


def fetch(url):
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.read().decode("utf-8")
    except urllib.error.HTTPError as e:
        return f"HTTP {e.code}"


def parse_template(text, tpl_name):
    start = '{{' + tpl_name
    idx = text.find(start)
    if idx == -1:
        return None
    depth, i = 0, idx
    for _ in range(50000):
        if i >= len(text): break
        if text[i:i+2] == '{{': depth += 1; i += 2; continue
        if text[i:i+2] == '}}':
            depth -= 1
            if depth == 0: end = i + 2; break
            i += 2; continue
        i += 1
    content = text[idx+len(start):end-2]
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
    return params


def clean_intel(text):
    if not text: return text
    text = re.sub(r'<!--.*?-->', '', text, flags=re.DOTALL)
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]', lambda m: m.group(2) or m.group(1), text)
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{模糊\|([^}]+?)\}\}', r'[模糊: \1]', text)
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = text.replace('{{=}}', '=')
    text = re.sub(r'<span[^>]*>([^<]*)</span>', r'\1', text)
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    text = re.sub(r'<br\s*/?>', '\n', text)
    text = re.sub(r'\n{3,}', '\n\n', text)
    return text.strip()


# ═══ 抽样 ═══
conn = sqlite3.connect(DB)
type_samples = {}
for sid, code, name, stype in conn.execute("""
    SELECT 关卡ID, 关卡编号, 关卡名, 关卡类型 FROM stages
    WHERE 关卡编号 IS NOT NULL AND 关卡名 IS NOT NULL AND 关卡类型 NOT IN ('全息作战矩阵')
    ORDER BY 关卡类型, 关卡编号
""").fetchall():
    type_samples.setdefault(stype, [])
    if len(type_samples[stype]) < SAMPLE_SIZE * 4:
        type_samples[stype].append((sid, code, name))

# 洗牌
for stype in type_samples:
    random.shuffle(type_samples[stype])

# ═══ 运行 ═══
print(f"{'类型':<20s} {'URL':<50s} {'情报':<4s}  {'预览'}")
print("-" * 110)

ok, fail = 0, 0

for stype in sorted(type_samples.keys()):
    sampled = set()
    for sid, code, name in type_samples[stype]:
        if len(sampled) >= SAMPLE_SIZE:
            break

        # 10章后 SIX_STAR: URL 加 磨难 前缀
        url_code = code
        if sid.startswith("tough_"):
            url_code = "磨难" + code

        url_tpl = URL_RULES.get(stype, DEFAULT_URL)
        url_str = url_tpl.format(code=url_code, name=name)

        # 去重（同 code+name 不同难度）
        url_dedup = url_str + "|" + stype
        if url_dedup in sampled:
            continue
        sampled.add(url_dedup)

        full_url = f"https://prts.wiki/w/{urllib.parse.quote(url_str)}?action=raw"
        text = fetch(full_url)

        if text.startswith("HTTP"):
            print(f"{stype:<20s} {url_str:<50s} {'--':<4s}  ❌ {text}")
            fail += 1
            continue

        redir = re.match(r'#(?:redirect|重定向)\s*\[\[:?([^\]]+)\]\]', text, re.IGNORECASE)
        if redir:
            print(f"{stype:<20s} {url_str:<50s} {'--':<4s}  ↪ {redir.group(1)}")
            fail += 1
            continue

        # 提取情报（搜所有模板）
        intel = None
        tpl_used = None
        for tpl_name in TPL_NAMES:
            params = parse_template(text, tpl_name)
            if params:
                raw = params.get("情报")
                if raw:
                    intel = clean_intel(raw)
                    tpl_used = tpl_name
                    break
                elif tpl_used is None:
                    tpl_used = tpl_name

        if intel:
            preview = intel[:60].replace('\n', ' ') + ("..." if len(intel) > 60 else "")
            print(f"{stype:<20s} {url_str:<50s} {'✅':<4s}  [{tpl_used}] {preview}")
            ok += 1
        elif tpl_used:
            print(f"{stype:<20s} {url_str:<50s} {'⚠':<4s}  [{tpl_used}] 无情报字段")
            fail += 1
        else:
            print(f"{stype:<20s} {url_str:<50s} {'--':<4s}  ❓ 无匹配模板 ({len(text)} chars)")
            fail += 1

conn.close()
print(f"\n✅ {ok} 有情报  ❌ {fail} 无情报")
