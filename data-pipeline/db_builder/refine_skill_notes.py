"""
PRTS 终端 — 技能备注 refine 脚本
从 PRTS wiki 同步/补充干员技能备注，支持两种模式：

模式 missing（默认）：仅爬取"数据库中存在技能备注为空"的干员页面，
    页面有备注则补充，无备注则跳过。绝不修改已有备注。
模式 full：全量同步。爬取所有干员页面，以页面备注为准更新，
    库中文本与页面不同则更新（含库中有而页面无的备注，会被清空）。

用法：
    python refine_skill_notes.py                # 默认 missing
    python refine_skill_notes.py --mode full

输出：
    refine_skill_notes_changes.log  变更明细
    refine_skill_notes_errors.log   错误/警告
"""

import argparse, re, json, sqlite3, os, sys, time
import urllib.request, urllib.parse, urllib.error

BASE = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(BASE, 'arknights.db')
CHANGE_LOG = os.path.join(BASE, 'refine_skill_notes_changes.log')
ERROR_LOG = os.path.join(BASE, 'refine_skill_notes_errors.log')

UA = {'User-Agent': 'PRTS-Terminal/1.0'}


# ─── 爬取与解析 ──────────────────────────

def fetch_wikitext(name, depth=0):
    """获取 PRTS 页面原始 wikitext，自动跟随重定向"""
    if depth > 5:
        raise RuntimeError(f'重定向过深: {name}')
    encoded = urllib.parse.quote(name)
    url = f'https://prts.wiki/w/{encoded}?action=raw'
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=15) as resp:
        text = resp.read().decode('utf-8')
    m = re.match(r'#(?:REDIRECT|重定向)\s*\[\[([^\]]+)\]\]', text, re.IGNORECASE)
    if m:
        return fetch_wikitext(m.group(1), depth + 1)
    return text


def parse_template(text, tpl_name):
    """从 wikitext 提取指定模板的所有实例，返回参数 dict 列表"""
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break
        depth, i = 0, idx
        while i < len(text):
            if text[i:i + 2] == '{{':
                depth += 1
                i += 2
                continue
            if text[i:i + 2] == '}}':
                depth -= 1
                if depth == 0:
                    content = text[idx + len(start_tag):i]
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
                i += 2
                continue
            i += 1
        else:
            # 模板未闭合，跳过
            pos = idx + 2
    return results


def clean_wiki(text):
    """清洗 PRTS wikitext 标记，白名单模式"""
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
    text = re.sub(r'\{\{popup\|[^}]*?内容=([^}|]+?)[^}]*\}\}', r'\1', text)
    text = re.sub(r'<!--[\s\S]*?-->', '', text)
    return text.strip()


def skill_templates(wikitext):
    """解析页面所有技能模板，按技能名去重（同名时优先保留含备注的）"""
    tpls = parse_template(wikitext, '技能2') + parse_template(wikitext, '技能\n')
    seen = {}
    for t in tpls:
        sname = t.get('技能名', '')
        if not sname:
            continue
        if sname not in seen or (t.get('备注') and not seen[sname].get('备注')):
            seen[sname] = t
    return seen


# ─── 主流程 ──────────────────────────────

def main():
    ap = argparse.ArgumentParser(description='技能备注 refine')
    ap.add_argument('--mode', choices=['missing', 'full'], default='missing',
                    help='missing=仅补缺失（默认）；full=全量以页面为准同步')
    args = ap.parse_args()
    mode = args.mode

    print(f'====== 技能备注 refine [{mode}] ======')
    conn = sqlite3.connect(DB_PATH)
    conn.execute('PRAGMA journal_mode=OFF')
    conn.execute('PRAGMA synchronous=OFF')

    rows = conn.execute(
        'SELECT rowid, 名称, 技能列表 FROM operators WHERE 技能列表 IS NOT NULL ORDER BY 名称'
    ).fetchall()

    targets = []   # (rowid, name, skills, need_skills 或 None)
    for rowid, name, sk_json in rows:
        try:
            skills = json.loads(sk_json)
        except Exception as e:
            print(f'  [警告] {name} 技能列表 JSON 解析失败: {e}')
            continue
        if mode == 'missing':
            need = {sk.get('技能名') for sk in skills if not sk.get('备注')}
            if not need:
                continue
            targets.append((rowid, name, skills, need))
        else:
            targets.append((rowid, name, skills, None))

    print(f'  待处理干员: {len(targets)}')

    changes = []
    errors = []
    updated_rows = 0
    start = time.time()

    for i, (rowid, name, skills, need) in enumerate(targets):
        if i % 20 == 0:
            elapsed = time.time() - start
            rate = i / elapsed if elapsed > 0 else 0
            eta = (len(targets) - i) / rate if rate > 0 else 0
            print(f'  [{i}/{len(targets)}] {elapsed:.0f}s ETA {eta:.0f}s | {name}')

        # 爬取
        try:
            wikitext = fetch_wikitext(name)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                errors.append(f'{name}\t404 无页面')
            else:
                errors.append(f'{name}\tHTTP {e.code}')
            continue
        except Exception as e:
            errors.append(f'{name}\t网络: {e}')
            continue

        seen = skill_templates(wikitext)
        if not seen:
            # 页面无技能模板（预备干员等），信息性跳过
            changes.append(f'跳过 | {name} | 页面无技能模板')
            continue

        changed = False
        for sk in skills:
            sname = sk.get('技能名', '')
            if not sname:
                continue
            tpl = seen.get(sname)
            if mode == 'missing':
                if sk.get('备注'):
                    continue
                if not tpl or not tpl.get('备注'):
                    continue
                note = clean_wiki(tpl['备注'])
                if not note:
                    continue
                changes.append(f'补 | {name} | {sname} | {note}')
                sk['备注'] = note
                changed = True
            else:  # full
                if not tpl:
                    continue
                note = clean_wiki(tpl.get('备注', '')) or None
                if sk.get('备注') != note:
                    old = sk.get('备注') or '(空)'
                    new = note or '(空)'
                    changes.append(f'改 | {name} | {sname} | 旧: {old} | 新: {new}')
                    sk['备注'] = note
                    changed = True

        if changed:
            new_json = json.dumps(skills, ensure_ascii=False)
            conn.execute('UPDATE operators SET 技能列表 = ? WHERE rowid = ?', (new_json, rowid))
            updated_rows += 1

        time.sleep(0.1)

    conn.commit()

    # ── 汇总 ──
    elapsed = time.time() - start
    print(f'\n====== 完成 ({elapsed:.0f}s) ======')
    print(f'  处理干员: {len(targets)}，更新行: {updated_rows}，变更条目: {len(changes)}')
    print(f'  错误: {len(errors)}')

    with open(CHANGE_LOG, 'w', encoding='utf-8') as f:
        f.write(f'# 技能备注变更 {time.strftime("%Y-%m-%d %H:%M:%S")} [{mode}]\n')
        f.write(f'# 类型 | 干员 | 技能 | 内容\n\n')
        for c in changes:
            f.write(c + '\n')
    with open(ERROR_LOG, 'w', encoding='utf-8') as f:
        f.write(f'# 技能备注错误/警告 {time.strftime("%Y-%m-%d %H:%M:%S")} [{mode}]\n\n')
        for e in errors:
            f.write(e + '\n')

    print(f'  变更日志: {CHANGE_LOG}')
    print(f'  错误日志: {ERROR_LOG}')
    conn.close()


if __name__ == '__main__':
    main()
