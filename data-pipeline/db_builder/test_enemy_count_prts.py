"""
敌人数 PRTS 爬取 · 测试脚本 v4

每类型从 stages 表随机抽 3 关，验证 PRTS URL 映射和敌人数量提取。
"""

import re, sqlite3, os, random, urllib.request, urllib.parse, socket

socket.setdefaulttimeout(10)
DB = os.path.join(os.path.dirname(os.path.abspath(__file__)), "arknights.db")

URL_RULES = {
    "生息演算·沙洲遗闻": "{code}_{name}(沙洲遗闻)",
    "悖论模拟":             "悖论模拟_{name}",
}
DEFAULT_URL = "{code}_{name}"
SAMPLE_SIZE = 3
SKIP_TYPES = {"全息作战矩阵"}


def fetch(url):
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.read().decode("utf-8")
    except urllib.error.HTTPError as e:
        return f"HTTP {e.code}"


TPL_NAMES = [
    "普通关卡信息", "突袭关卡信息", "剿灭关卡信息", "保全派驻关卡信息",
    "集成战略关卡信息", "区块化关卡信息", "教学关卡信息",
    "活动关卡信息", "主线关卡信息", "支线关卡信息",
]


def extract_count(text):
    for tpl_name in TPL_NAMES:
        start = '{{' + tpl_name
        idx = text.find(start)
        if idx == -1:
            continue
        depth, i = 0, idx
        for _ in range(50000):
            if i >= len(text): break
            if text[i:i+2] == '{{': depth += 1; i += 2; continue
            if text[i:i+2] == '}}':
                depth -= 1
                if depth == 0: end = i + 2; break
                i += 2; continue
            i += 1
        content = text[idx:end]
        m = re.search(r'敌人数量\s*=\s*(\S+)', content)
        if m:
            return m.group(1).strip(), tpl_name
        return None, tpl_name
    return None, None


# ═══ 从 DB 随机抽样 ═══
conn = sqlite3.connect(DB)

type_rows = {}
for sid, code, name, stype in conn.execute("""
    SELECT 关卡ID, 关卡编号, 关卡名, 关卡类型 FROM stages
    WHERE 关卡编号 IS NOT NULL AND 关卡名 IS NOT NULL
    ORDER BY 关卡类型, 关卡编号
""").fetchall():
    if stype in SKIP_TYPES:
        continue
    type_rows.setdefault(stype, []).append((code, name))

conn.close()

for stype in type_rows:
    random.shuffle(type_rows[stype])


# ═══ 运行 ═══
print(f"{'类型':<20s} {'URL':<50s} {'人数':>6s}  {'结果'}")
print("-" * 95)

ok = 0; fail = 0; no_field_ok = 0

for stype in sorted(type_rows.keys()):
    samples = type_rows[stype][:SAMPLE_SIZE]
    for code, name in samples:
        url_tpl = URL_RULES.get(stype, DEFAULT_URL)
        url_str = url_tpl.format(code=code, name=name)
        full_url = f"https://prts.wiki/w/{urllib.parse.quote(url_str)}?action=raw"
        text = fetch(full_url)

        if text.startswith("HTTP"):
            print(f"{stype:<20s} {url_str:<50s} {'--':>6s}  ❌ {text}")
            fail += 1
            continue

        redir = re.match(r'#(?:redirect|重定向)\s*\[\[:?([^\]]+)\]\]', text, re.IGNORECASE)
        if redir:
            print(f"{stype:<20s} {url_str:<50s} {'--':>6s}  ↪ {redir.group(1)}")
            fail += 1
            continue

        if '{{消歧义页}}' in text:
            print(f"{stype:<20s} {url_str:<50s} {'--':>6s}  ⚠ 消歧义页")
            fail += 1
            continue

        count_val, tpl_name = extract_count(text)
        if count_val is not None:
            print(f"{stype:<20s} {url_str:<50s} {count_val:>6s}  ✅ {tpl_name}")
            ok += 1
        elif tpl_name is not None:
            battle = re.search(r'战斗关卡\s*=\s*(\S+)', text)
            if battle and battle.group(1).lower() == 'false':
                print(f"{stype:<20s} {url_str:<50s} {'(剧情)':>6s}  ⚪ 纯剧情")
                no_field_ok += 1
            else:
                print(f"{stype:<20s} {url_str:<50s} {'--':>6s}  ⚠ {tpl_name} 缺敌人数量")
                fail += 1
        else:
            print(f"{stype:<20s} {url_str:<50s} {'--':>6s}  ❓ 无模板 ({len(text)} chars)")
            fail += 1

total = ok + fail + no_field_ok
print(f"\n✅ {ok}/{total} 有数据  ⚪ {no_field_ok} 纯剧情  ❌ {fail} 失败")
