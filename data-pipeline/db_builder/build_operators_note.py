"""
PRTS 终端 — operators 特性备注入库
从 PRTS wiki 的 CharinfoV2 模板爬取 特性备注 字段，写入 operators.备注

用法：python build_operators_note.py
失败记录：operators_note_errors.log
"""

import re, sqlite3, os, time, urllib.request, urllib.error, urllib.parse, socket

socket.setdefaulttimeout(15)

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
ERROR_LOG  = os.path.join(OUTPUT_DIR, "operators_note_errors.log")


# ═══════════════════════════════════════════
# 工具函数
# ═══════════════════════════════════════════

def fetch_wikitext(name, depth=0):
    """获取 PRTS 页面原始 wikitext，自动跟随重定向"""
    if depth > 5:
        raise RuntimeError(f"重定向过深: {name}")
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req, timeout=10) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'#(?:redirect|重定向)\s*\[\[:?([^\]]+)\]\]', text, re.IGNORECASE)
    if m:
        target = m.group(1).replace(' ', '_')
        return fetch_wikitext(target, depth + 1)
    return text


def parse_template(text, tpl_name):
    """从 wikitext 中提取指定模板，返回参数列表"""
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
            if text[i:i+2] == '{{':
                depth += 1
                i += 2
                continue
            if text[i:i+2] == '}}':
                depth -= 1
                if depth == 0:
                    end = i + 2
                    break
                i += 2
                continue
            i += 1
        content = text[idx+len(start_tag):end-2]
        params = {}
        parts = re.split(r'\n\|', content)
        first = parts[0].strip()
        if first.startswith('|'):
            first = first[1:]
        if '=' in first:
            k, v = first.split('=', 1)
            params[k.strip()] = v.strip()
        for part in parts[1:]:
            part = part.strip()
            if '=' not in part:
                continue
            k, v = part.split('=', 1)
            params[k.strip()] = v.strip()
        results.append(params)
        pos = end
    return results


def clean_note(text):
    """清洗特性备注文本：解析 wiki 链接，去除格式噪声"""
    if not text:
        return text
    # [[链接|显示]] → 显示
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)
    # <br/> → 换行
    text = re.sub(r'<br\s*/?>', '\n', text)
    # 删除 HTML 注释
    text = re.sub(r'<!--.*?-->', '', text, flags=re.DOTALL)
    # {{popup|...}} → 删除（浮窗内容过于复杂）
    text = re.sub(r'\{\{popup\|[^}]*?\}\}', '', text)
    # 压缩空行
    text = re.sub(r'\n{3,}', '\n\n', text)
    return text.strip()


# ═══════════════════════════════════════════
# 主流程
# ═══════════════════════════════════════════

def main():
    print("=" * 70)
    print("  PRTS operators 特性备注入库")
    print("=" * 70)

    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    # 读取所有干员
    ops = conn.execute("SELECT 干员ID, 名称 FROM operators ORDER BY 干员ID").fetchall()
    print(f"  干员数: {len(ops)}")

    errors = []
    updated = 0
    has_note = 0
    no_note = 0
    start_time = time.time()

    for i, (oid, name) in enumerate(ops):
        if not name:
            continue

        # 进度
        if i % 50 == 0 or i == len(ops) - 1:
            elapsed = time.time() - start_time
            done = i + 1
            rate = done / elapsed if elapsed > 0 else 0
            pct = done / len(ops) * 100
            eta = (len(ops) - done) / rate if rate > 0 else 0
            print(f"  [{done:>4}/{len(ops)}] {pct:5.1f}% ETA {eta:.0f}s | {name}")

        try:
            text = fetch_wikitext(name)
        except urllib.error.HTTPError as e:
            errors.append((oid, name, "HTTP", str(e.code)))
            continue
        except Exception as e:
            errors.append((oid, name, "网络", str(e)))
            continue

        tpls = parse_template(text, "CharinfoV2")
        if not tpls:
            errors.append((oid, name, "模板", "未找到CharinfoV2"))
            continue

        raw = tpls[0].get("特性备注", "")
        if raw:
            note = clean_note(raw)
            if note:
                conn.execute("UPDATE operators SET 备注 = ? WHERE 干员ID = ?", (note, oid))
                updated += 1
                has_note += 1
            else:
                no_note += 1
        else:
            no_note += 1

        time.sleep(0.1)

    conn.commit()

    # 日志
    with open(ERROR_LOG, "w", encoding="utf-8") as f:
        f.write(f"# operators 特性备注爬取错误日志\n")
        f.write(f"# 时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        if errors:
            f.write(f"=== 错误 ({len(errors)} 个) ===\n")
            f.write(f"# 干员ID\t名称\t错误类型\t原因\n")
            for oid, name, etype, reason in errors:
                f.write(f"{oid}\t{name}\t{etype}\t{reason}\n")

    # 汇总
    elapsed = time.time() - start_time
    print(f"\n  {'─' * 45}")
    print(f"  有特性备注: {has_note}")
    print(f"  无特性备注: {no_note}")
    print(f"  错误:       {len(errors)}")
    print(f"  耗时:       {elapsed:.0f}s")
    print(f"  日志:       {ERROR_LOG}")

    conn.close()
    print(f"\n  完成")


if __name__ == "__main__":
    main()
