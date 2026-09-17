"""
PRTS 终端 — tokens 备注入库
从 PRTS wiki 爬取召唤物的备注和技能备注文本

用法：python build_tokens_prts.py
失败记录：tokens_prts_errors.log
"""

import re, json, sqlite3, os, time, urllib.request, urllib.parse

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
ERROR_LOG  = os.path.join(OUTPUT_DIR, "tokens_prts_errors.log")

# 重名 token 的 PRTS 页面名（与干员同名冲突时使用）
PAGE_OVERRIDE = {
    'Mon3tr': 'Mon3tr(凯尔希的召唤物)',
}


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
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break
        depth, i = 0, idx
        while i < len(text):
            if text[i:i+2] == '{{':
                depth += 1; i += 2; continue
            if text[i:i+2] == '}}':
                depth -= 1
                if depth == 0:
                    content = text[idx+len(start_tag):i]
                    params = {}
                    for part in content.split('\n|'):
                        part = part.strip()
                        if '=' not in part:
                            continue
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
    if not text:
        return text
    # [[链接|显示]]
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)
    # {{color|#xxx|text}}
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    # {{术语|key|display}}
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    # {{异常效果|name|flag}}
    text = re.sub(r'\{\{异常效果\|([^|}]+)(?:\|([^|}]+))?\}\}',
                  lambda m: m.group(1) + (m.group(2) or ''), text)
    # '''粗体'''
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    # <br>
    text = re.sub(r'<br\s*/?>', '\n', text)
    # {{popup|内容=...}} → 保留内容
    text = re.sub(r'\{\{popup\|[^}]*?内容=([^}|]+?)[^}]*\}\}', r'\1', text)
    # <!--...--> → 删除
    text = re.sub(r'<!--[\s\S]*?-->', '', text)
    return text.strip()


# ─── 主流程 ─────────────────────────────────

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")

    rows = conn.execute(
        "SELECT rowid, tokenID, 名称, 技能列表 FROM tokens ORDER BY 名称"
    ).fetchall()

    total = len(rows)
    errors = []
    no_page = []
    note_updated = 0
    skill_updated = 0
    start_time = time.time()

    print(f"====== 开始爬取 ({total} 个召唤物) ======")

    for i, (rowid, tid, name, skill_json) in enumerate(rows):
        if i % 10 == 0:
            elapsed = time.time() - start_time
            rate = i / elapsed if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print(f"  [{i}/{total}] {elapsed:.0f}s | ETA {eta:.0f}s | {name}")

        try:
            page_name = PAGE_OVERRIDE.get(name, name)
            wikitext = fetch_wikitext(page_name)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                no_page.append(name)
            else:
                errors.append(f"{name}\tHTTP {e.code}")
            continue
        except Exception as e:
            errors.append(f"{name}\t网络: {e}")
            continue

        changed = False

        # ── token 自身备注 ──
        info_tpls = parse_template(wikitext, '召唤物信息')
        note = None
        if info_tpls:
            note_raw = info_tpls[0].get('备注', '')
            note = clean_wiki(note_raw) if note_raw else None
            current_note = conn.execute(
                "SELECT 备注 FROM tokens WHERE rowid=?", (rowid,)
            ).fetchone()[0]
            if current_note != note:
                conn.execute("UPDATE tokens SET 备注=? WHERE rowid=?", (note, rowid))
                note_updated += 1
                changed = True

        # ── 技能备注 ──
        skill_tpls = parse_template(wikitext, '技能2')
        skill_tpls += parse_template(wikitext, '技能\n')
        if skill_tpls and skill_json:
            skills = json.loads(skill_json)
            seen = {}
            for s in skill_tpls:
                sname = s.get('技能名', '')
                if not sname or sname in seen:
                    continue
                seen[sname] = s
            for sk in skills:
                sname = sk.get('技能名', '')
                if sname in seen:
                    note_raw = seen[sname].get('备注', '')
                    note = clean_wiki(note_raw) if note_raw else None
                    if sk.get('备注') != note:
                        sk['备注'] = note
                        changed = True
            if changed:
                conn.execute(
                    "UPDATE tokens SET 技能列表=? WHERE rowid=?",
                    (json.dumps(skills, ensure_ascii=False), rowid)
                )
                skill_updated += 1

        time.sleep(0.1)

    conn.commit()

    print(f"\n====== 完成 ======")
    print(f"  404: {len(no_page)}")
    print(f"  其他错误: {len(errors)}")
    print(f"  备注更新: {note_updated}")
    print(f"  技能备注更新: {skill_updated}")

    with open(ERROR_LOG, 'w', encoding='utf-8') as f:
        f.write(f"# tokens PRTS 爬取日志\n")
        f.write(f"# 时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        if no_page:
            f.write(f"=== 404 ({len(no_page)} 个) ===\n")
            for n in no_page:
                f.write(f"{n}\n")
            f.write("\n")
        if errors:
            f.write(f"=== 错误 ({len(errors)} 个) ===\n")
            for e in errors:
                f.write(f"{e}\n")

    print(f"日志: {ERROR_LOG}")
    conn.close()


if __name__ == '__main__':
    main()
