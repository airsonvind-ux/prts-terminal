"""
PRTS 终端 — traps PRTS 数据入库
从 PRTS wiki 爬取装置的阻挡半径、装置机制文本和技能备注

用法：python build_traps_prts.py
失败记录：traps_prts_errors.log
"""

import re, json, sqlite3, os, time, urllib.request, urllib.parse

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
ERROR_LOG  = os.path.join(OUTPUT_DIR, "traps_prts_errors.log")


def fetch_wikitext(name):
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'(?i)#(?:redirect|重定向)\s*\[\[([^\]]+)\]\]', text)
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
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{异常效果\|([^|}]+)(?:\|([^|}]+))?\}\}',
                  lambda m: m.group(1) + (m.group(2) or ''), text)
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    text = re.sub(r'<br\s*/?>', '\n', text)
    text = re.sub(r'<nowiki\s*/>', '', text)
    text = re.sub(r'<!--[\s\S]*?-->', '', text)
    text = re.sub(r'\{\{popup\|[^}]*?内容=([^}|]+?)[^}]*\}\}', r'\1', text)
    text = re.sub(r'</?nowiki>', '', text)
    return text.strip()


# ═══════════════════════════════════════════════════════
# 页面类型检测
# ═══════════════════════════════════════════════════════

def is_disambiguation(wikitext):
    return wikitext.strip().startswith('{{消歧义页}}')


def is_operator_page(wikitext):
    return '{{干员信息}}' in wikitext or '{{CharinfoV2' in wikitext


# ═══════════════════════════════════════════════════════
# 主流程
# ═══════════════════════════════════════════════════════

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")

    rows = conn.execute(
        "SELECT rowid, 装置ID, 名称, 技能列表 FROM traps ORDER BY 名称"
    ).fetchall()

    total = len(rows)
    errors          = []
    no_page         = []
    disambig        = []
    operator_page   = []
    no_info_tpl     = []
    no_skill_tpl    = []
    radius_updated  = 0
    mech_updated    = 0
    skill_updated   = 0
    start_time = time.time()

    print(f"====== 开始爬取 ({total} 个装置) ======")

    for i, (rowid, tid, name, skill_json) in enumerate(rows):
        if i % 50 == 0:
            elapsed = time.time() - start_time
            rate = i / elapsed if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print(f"  [{i}/{total}] {elapsed:.0f}s | ETA {eta:.0f}s | "
                  f"半径={radius_updated} 机制={mech_updated} 备注={skill_updated} | {name}")

        # ── 获取页面 ──
        try:
            wikitext = fetch_wikitext(name)
            # 删除 HTML 注释，防止内部的 {{ }} 干扰模板深度解析
            wikitext = re.sub(r'<!--[\s\S]*?-->', '', wikitext)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                no_page.append(name)
            else:
                errors.append(f"{name}\tHTTP {e.code}")
            continue
        except Exception as e:
            errors.append(f"{name}\t网络: {e}")
            continue

        # ── 页面类型检测 ──
        if is_disambiguation(wikitext):
            disambig.append(name)
            continue

        if is_operator_page(wikitext):
            operator_page.append(name)
            continue

        # ── 装置信息（阻挡半径 + 装置机制）──
        info_tpls = parse_template(wikitext, '装置信息')
        if info_tpls:
            info = info_tpls[0]

            radius_raw = info.get('阻挡半径', '')
            if radius_raw:
                try:
                    radius = float(radius_raw)
                    cur = conn.execute(
                        "SELECT 阻挡半径 FROM traps WHERE rowid=?", (rowid,)
                    ).fetchone()[0]
                    if cur != radius:
                        conn.execute(
                            "UPDATE traps SET 阻挡半径=? WHERE rowid=?",
                            (radius, rowid)
                        )
                        radius_updated += 1
                except ValueError:
                    pass

            mech_raw = info.get('装置机制', '')
            mech = clean_wiki(mech_raw) if mech_raw else None
            if mech:
                cur = conn.execute(
                    "SELECT 装置机制 FROM traps WHERE rowid=?", (rowid,)
                ).fetchone()[0]
                if cur != mech:
                    conn.execute(
                        "UPDATE traps SET 装置机制=? WHERE rowid=?",
                        (mech, rowid)
                    )
                    mech_updated += 1
        else:
            no_info_tpl.append(name)

        # ── 技能备注（独立于装置信息）──
        if skill_json:
            skill_tpls = parse_template(wikitext, '装置技能')
            if skill_tpls:
                skills = json.loads(skill_json)
                seen = {}
                for s in skill_tpls:
                    sname = s.get('技能名', '')
                    if not sname or sname in seen:
                        continue
                    seen[sname] = s

                changed = False
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
                        "UPDATE traps SET 技能列表=? WHERE rowid=?",
                        (json.dumps(skills, ensure_ascii=False), rowid)
                    )
                    skill_updated += 1
            else:
                no_skill_tpl.append(name)

        time.sleep(0.1)

    conn.commit()

    print(f"\n====== 完成 ======")
    print(f"  阻挡半径更新: {radius_updated}")
    print(f"  装置机制更新: {mech_updated}")
    print(f"  技能备注更新: {skill_updated}")
    print(f"  无 {{装置信息}}: {len(no_info_tpl)}")
    print(f"  无 {{装置技能}}: {len(no_skill_tpl)}")
    print(f"  404: {len(no_page)}")
    print(f"  消歧义页: {len(disambig)}")
    print(f"  干员页: {len(operator_page)}")
    print(f"  其他错误: {len(errors)}")

    with open(ERROR_LOG, 'w', encoding='utf-8') as f:
        f.write(f"# traps PRTS 爬取日志\n")
        f.write(f"# 时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")

        if radius_updated or mech_updated or skill_updated:
            f.write(f"=== 更新 ===\n")
            f.write(f"阻挡半径: {radius_updated} / 装置机制: {mech_updated} / 技能备注: {skill_updated}\n\n")

        if no_skill_tpl:
            f.write(f"=== 无 {{装置技能}} 模板 ({len(no_skill_tpl)} 个) ===\n")
            for n in no_skill_tpl:
                f.write(f"{n}\n")
            f.write("\n")

        if no_info_tpl:
            f.write(f"=== 无 {{装置信息}} 模板 ({len(no_info_tpl)} 个) ===\n")
            for n in no_info_tpl:
                f.write(f"{n}\n")
            f.write("\n")

        if no_page:
            f.write(f"=== 404 ({len(no_page)} 个) ===\n")
            for n in no_page:
                f.write(f"{n}\n")
            f.write("\n")

        if disambig:
            f.write(f"=== 消歧义页 ({len(disambig)} 个) ===\n")
            for n in disambig:
                f.write(f"{n}\n")
            f.write("\n")

        if operator_page:
            f.write(f"=== 干员页 ({len(operator_page)} 个) ===\n")
            for n in operator_page:
                f.write(f"{n}\n")
            f.write("\n")

        if errors:
            f.write(f"=== 其他错误 ({len(errors)} 个) ===\n")
            for e in errors:
                f.write(f"{e}\n")

    print(f"日志: {ERROR_LOG}")
    conn.close()


if __name__ == '__main__':
    main()
