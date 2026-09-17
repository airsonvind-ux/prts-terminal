"""
PRTS 终端 — operators 天赋/技能备注入库
从 PRTS wiki 爬取干员天赋和技能的备注文本

用法：python build_operators_prts.py
失败记录：operators_prts_errors.log
"""

import re, json, sqlite3, os, time, urllib.request, urllib.parse

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
ERROR_LOG  = os.path.join(OUTPUT_DIR, "operators_prts_errors.log")

# ─── 工具函数 ────────────────────────────

def fetch_wikitext(name):
    """获取 PRTS 页面原始 wikitext，自动跟随重定向"""
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'#(?:REDIRECT|重定向)\s*\[\[([^\]]+)\]\]', text, re.IGNORECASE)
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
    """清洗 PRTS wikitext 标记，白名单模式"""
    if not text:
        return text
    # [[链接|显示]] → 显示
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)
    # {{color|#xxx|text}} → text
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    # {{术语|key|display}} → display
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    # {{异常效果|name|flag}}
    text = re.sub(r'\{\{异常效果\|([^|}]+)(?:\|([^|}]+))?\}\}',
                  lambda m: m.group(1) + (m.group(2) or ''), text)
    # '''粗体'''
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    # <br> → 换行
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
        "SELECT rowid, 干员ID, 名称, 天赋列表, 技能列表 FROM operators ORDER BY 名称"
    ).fetchall()

    total = len(rows)
    errors = []
    no_page = []
    talent_updated = 0
    skill_updated = 0
    start_time = time.time()

    print(f"====== 开始爬取 ({total} 名干员) ======")

    for i, (rowid, cid, name, talent_json, skill_json) in enumerate(rows):
        if i % 20 == 0:
            elapsed = time.time() - start_time
            rate = i / elapsed if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print(f"  [{i}/{total}] {elapsed:.0f}s | ETA {eta:.0f}s | {name}")

        # ── 获取 wikitext ──
        try:
            wikitext = fetch_wikitext(name)
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

        # ── 天赋备注 ──
        talent_tpls = parse_template(wikitext, '天赋列表3')
        if talent_tpls and talent_json:
            talents = json.loads(talent_json)
            # 取数据库中有序号的非 "#" 天赋，按顺序与 PRTS 模板对应
            db_talents = [t for t in talents if t.get('序号') != '#']
            for ti, tpl in enumerate(talent_tpls):
                note_raw = tpl.get('备注', '')
                note = clean_wiki(note_raw) if note_raw else None
                if ti < len(db_talents):
                    if db_talents[ti].get('备注') != note:
                        db_talents[ti]['备注'] = note
                        changed = True

        # ── 技能备注 ──
        skill_tpls = parse_template(wikitext, '技能2')
        skill_tpls += parse_template(wikitext, '技能\n')
        if skill_tpls and skill_json:
            skills = json.loads(skill_json)
            # 按技能名去重（同一技能可能出现在两个模板变体中）
            seen = {}
            for s in skill_tpls:
                sname = s.get('技能名', '')
                if not sname or sname in seen:
                    continue
                seen[sname] = s
            # 匹配数据库中的技能
            for sk in skills:
                sname = sk.get('技能名', '')
                if sname in seen:
                    note_raw = seen[sname].get('备注', '')
                    note = clean_wiki(note_raw) if note_raw else None
                    if sk.get('备注') != note:
                        sk['备注'] = note
                        changed = True

        # ── 写回 ──
        if changed:
            if talent_json is not None:
                new_talent = json.dumps(talents, ensure_ascii=False) if talents else None
            else:
                new_talent = talent_json
            if skill_json is not None:
                new_skill = json.dumps(skills, ensure_ascii=False) if skills else None
            else:
                new_skill = skill_json

            conn.execute(
                "UPDATE operators SET 天赋列表=?, 技能列表=? WHERE rowid=?",
                (new_talent, new_skill, rowid)
            )
            talent_updated += 1
            skill_updated += 1

        time.sleep(0.1)

    conn.commit()

    # ── 统计 ──
    print(f"\n====== 完成 ======")
    print(f"  404: {len(no_page)}")
    print(f"  其他错误: {len(errors)}")
    print(f"  天赋备注更新: {talent_updated}")
    print(f"  技能备注更新: {skill_updated}")

    # 统计有备注的
    has_talent_note = conn.execute(
        "SELECT COUNT(*) FROM operators WHERE 天赋列表 IS NOT NULL"
    ).fetchone()[0]
    # 简单统计：天赋列表 JSON 中含 "备注" 不为 null 的
    has_skill_note = 0
    for (sk_json,) in conn.execute(
        "SELECT 技能列表 FROM operators WHERE 技能列表 IS NOT NULL"
    ).fetchall():
        skills = json.loads(sk_json)
        if any(sk.get('备注') for sk in skills):
            has_skill_note += 1
    print(f"  有天赋备注的干员: ~{talent_updated}")
    print(f"  有技能备注的干员: {has_skill_note}")

    # 写错误日志
    with open(ERROR_LOG, 'w', encoding='utf-8') as f:
        f.write(f"# PRTS 干员备注爬取错误日志\n")
        f.write(f"# 时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        if no_page:
            f.write(f"=== 404 无页面 ({len(no_page)} 个) ===\n")
            for n in no_page:
                f.write(f"{n}\n")
            f.write("\n")
        if errors:
            f.write(f"=== 其他错误 ({len(errors)} 个) ===\n")
            for e in errors:
                f.write(f"{e}\n")

    print(f"\n日志: {ERROR_LOG}")
    conn.close()


if __name__ == '__main__':
    main()
