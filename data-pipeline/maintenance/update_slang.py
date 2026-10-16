# -*- coding: utf-8 -*-
"""PRTS 终端 — 黑话/别名表增量更新（维护用）

**增量制**：全量扫描 PRTS 全站重定向，过滤 + 归类后，**只把库里还没有的新条目
加入 slang.json**；现有条目一律不动（含人工补充与手改）。

数据源：
  - PRTS 全站重定向（`list=allpages&apfilterredir=redirects` + `list=allredirects`）
  - arknights.db（权威副本）：operators / enemies / tokens / traps / stages，
    用于判定别名指向能否归类

过滤（沿用 scripts/slang_scan.py 的口径，只收"库外"的社区信息）：
  官方代号与名称变体、日文名、关卡类重定向、Wiki 内部活动编号、编号规范化。

用法：
    python update_slang.py --dry-run   # 只报告，不写文件
    python update_slang.py             # 写入（写前备份 + 写记录）

输出：src-tauri/resources/slang.json（键序 = 类型序（干员/敌人/其他实体）再按名称）
"""

import argparse
import json
import os
import re
import sqlite3
import sys
import time
import unicodedata
from collections import Counter
from datetime import datetime
from difflib import SequenceMatcher

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from paths import DB_PATH, RESOURCES  # noqa: E402

# 抓取与解析口径都在 update_common（原 prts_wiki.py 已并入）；pw 是沿用旧调用名的别名
import update_common as pw  # noqa: E402
import update_common as uc  # noqa: E402

API = pw.PRTS_API
DB = str(DB_PATH)
DST = os.path.join(str(RESOURCES), "slang.json")

ORDER = {"干员": 0, "敌人": 1, "其他实体": 2, "未识别": 3}


# ─────────────────────── 文本归一化与变体判定 ───────────────────────

_CYR = {'а': 'a', 'б': 'b', 'в': 'v', 'г': 'g', 'д': 'd', 'е': 'e', 'ё': 'e',
        'ж': 'zh', 'з': 'z', 'и': 'i', 'й': 'i', 'к': 'k', 'л': 'l', 'м': 'm',
        'н': 'n', 'о': 'o', 'п': 'p', 'р': 'r', 'с': 's', 'т': 't', 'у': 'u',
        'ф': 'f', 'х': 'kh', 'ц': 'ts', 'ч': 'ch', 'ш': 'sh', 'щ': 'shch',
        'ъ': '', 'ы': 'y', 'ь': '', 'э': 'e', 'ю': 'yu', 'я': 'ya'}
_EXTRA = {'ł': 'l', 'ø': 'o', 'đ': 'd', 'ß': 'ss', 'æ': 'ae', 'œ': 'oe',
          'ð': 'd', 'þ': 'th', 'ı': 'i'}


def _build_translit():
    table = {}

    def put(ch, value):
        table[ord(ch)] = value
        up = ch.upper()
        if len(up) == 1:
            table[ord(up)] = value.capitalize()

    for ch, value in _CYR.items():
        put(ch, value)
    for ch, value in _EXTRA.items():
        put(ch, value)
    return table


_TRANSLIT = _build_translit()
_KANA = re.compile('[\u3041-\u3096\u30a1-\u30fa\u30fc-\u30ff]')
_HAN = re.compile('[\u4e00-\u9fff]')


def fold(s):
    """归一化：NFKD 去变音 → 西里尔转写 → 小写 → 仅保留字母数字。"""
    s = unicodedata.normalize('NFKD', s)
    s = ''.join(ch for ch in s if not unicodedata.combining(ch))
    s = s.translate(_TRANSLIT).casefold()
    return ''.join(ch for ch in s if ch.isalnum())


def tokens_of(s):
    s = unicodedata.normalize('NFKD', s)
    s = ''.join(ch for ch in s if not unicodedata.combining(ch))
    s = s.translate(_TRANSLIT).casefold()
    return re.findall(r'[0-9a-z\u0400-\u04ff]+', s)


def is_official_variant(alias, target, code):
    """官方代号 / 名称的变体判定：命中说明它是"库内信息"的变体，应剔除。"""
    fa, fn = fold(alias), fold(target)
    if fa == fn:
        return True
    if _HAN.search(alias):
        return False
    if code:
        fc = fold(code)
        if fa == fc:
            return True
        if len(fa) >= 3 and (fa in fc or fc in fa):
            return True
        ta, tc = tokens_of(alias), tokens_of(code)
        if len(ta) >= 2 and all(t in tc for t in ta):
            return True
        if len(fa) >= 4 and SequenceMatcher(None, fa, fc).ratio() >= 0.85:
            return True
    if fn and len(fa) >= 4 and SequenceMatcher(None, fa, fn).ratio() >= 0.85:
        return True
    return False


def is_stage_like(target, stage):
    """关卡类重定向判定：命中即剔除（表不收录关卡检索用的映射）。"""
    tt = target.strip()
    if tt in stage['codes'] or tt in stage['combos']:
        return True
    toks = [x for x in tt.split(' ') if x]
    if len(toks) >= 2:
        for tok in toks:
            if tok in stage['codes']:
                return True
            if re.search(r'\d[\u4e00-\u9fff]', tok):
                return True
            if re.match(r'^[A-Za-z]{1,4}-[A-Za-z]?\d', tok):
                return True
        if toks[0] in stage['types']:
            return True
    return '＃' in tt or '/' in tt


def is_act_id(alias):
    """Wiki 内部活动编号（Act1arkhub 01 之类），非社区用法。"""
    low = alias.lower()
    if low.startswith('level act'):
        return True
    return low.startswith('act') and len(low) > 3 and low[3].isdigit()


# ─────────────────────────── 数据拉取 ───────────────────────────

def fetch_sources(max_pages):
    """allpages：全站重定向源页 → {pageid: 标题}。"""
    result, cont, page = {}, None, 0
    while True:
        p = {'action': 'query', 'list': 'allpages', 'apfilterredir': 'redirects',
             'aplimit': '500', 'apnamespace': '0'}
        if cont:
            p['apcontinue'] = cont
        d = pw.api(p)
        items = d.get('query', {}).get('allpages', [])
        for it in items:
            result[it['pageid']] = it['title']
        page += 1
        print('  [源页] 第 %d 页 +%d，累计 %d' % (page, len(items), len(result)), flush=True)
        if max_pages and page >= max_pages:
            break
        c = d.get('continue')
        if not c:
            break
        cont = c.get('apcontinue')
        time.sleep(0.35)
    return result


def fetch_map(max_pages):
    """allredirects：源页 pageid → 目标标题。"""
    pairs, cont, page = [], None, 0
    while True:
        p = {'action': 'query', 'list': 'allredirects', 'arlimit': '500',
             'arprop': 'ids|title'}
        if cont:
            p['arcontinue'] = cont
        d = pw.api(p)
        items = d.get('query', {}).get('allredirects', [])
        for it in items:
            pairs.append((it.get('fromid'), it.get('title', '')))
        page += 1
        print('  [映射] 第 %d 页 +%d，累计 %d' % (page, len(items), len(pairs)), flush=True)
        if max_pages and page >= max_pages:
            break
        c = d.get('continue')
        if not c:
            break
        cont = c.get('arcontinue')
        time.sleep(0.35)
    return pairs


def load_db():
    """从权威库读入实体名称、代号、关卡索引。"""
    con = sqlite3.connect(DB)

    def col(table, column):
        return {row[0] for row in con.execute(f'SELECT {column} FROM {table}') if row[0]}

    data = {
        '干员': col('operators', '名称'),
        '敌人': col('enemies', '名称'),
        '其他实体': col('tokens', '名称') | col('traps', '名称'),
        'code': {},
        'stage': {'codes': set(), 'combos': set(), 'types': set()},
    }
    for table in ('operators', 'tokens', 'traps'):
        for name, code in con.execute(f'SELECT 名称, 代号 FROM {table}'):
            if name and code:
                data['code'].setdefault(name, code)
    for no, nm, ty in con.execute('SELECT 关卡编号, 关卡名, 关卡类型 FROM stages'):
        if no:
            data['stage']['codes'].add(no)
        if no and nm:
            data['stage']['combos'].add(f'{no} {nm.strip()}')
        if ty:
            data['stage']['types'].add(ty)
    con.close()
    return data


def classify(target, data):
    if target in data['干员']:
        return '干员'
    if target in data['敌人']:
        return '敌人'
    if target in data['其他实体']:
        return '其他实体'
    return '未识别'


# ─────────────────────────── 主流程 ───────────────────────────

def scan(max_pages=None):
    """扫描重定向并归类，返回 (可入库条目, 未识别条目, 统计)。"""
    print('① 拉取重定向源页（allpages）...', flush=True)
    sources = fetch_sources(max_pages)
    print('② 拉取重定向映射（allredirects）...', flush=True)
    mapping = fetch_map(max_pages)
    print('③ 联表、分类与过滤（源页 %d / 映射 %d）...' % (len(sources), len(mapping)), flush=True)

    data = load_db()
    stats = Counter()
    found, unmatched = {}, {}
    seen = set()
    for fromid, target in mapping:
        if fromid in seen:
            stats['重复'] += 1
            continue
        seen.add(fromid)
        alias = sources.get(fromid)
        if not alias:
            stats['缺源页'] += 1
            continue
        if alias == target:
            stats['自指'] += 1
            continue
        if is_act_id(alias) or is_stage_like(target, data['stage']):
            stats['关卡类'] += 1
            continue
        if _KANA.search(alias) or alias.startswith('予備隊員'):
            stats['日文名'] += 1
            continue
        if is_official_variant(alias, target, data['code'].get(target)):
            stats['官方名或变体'] += 1
            continue
        if (alias.isascii() and len(target) > len(alias)
                and target.startswith(alias) and target[len(alias)] in ' \u3000（('):
            stats['编号规范化'] += 1
            continue
        t = classify(target, data)
        bucket = unmatched if t == '未识别' else found
        bucket[alias] = {'指向': target, '类型': t}
    return found, unmatched, stats


def sorted_items(d):
    return {k: d[k] for k in sorted(d, key=lambda a: (ORDER[d[a]['类型']], a))}


def main():
    ap = argparse.ArgumentParser(description='黑话/别名表增量更新（slang.json）')
    ap.add_argument('--dry-run', action='store_true', help='只报告，不写文件')
    ap.add_argument('--max-pages', type=int, default=0, help='冒烟用：每段最多取 N 页')
    args = ap.parse_args()

    t0 = time.time()
    found, unmatched, stats = scan(args.max_pages or None)
    found = sorted_items(found)
    unmatched = sorted_items(unmatched)

    old = {}
    if os.path.exists(DST):
        with open(DST, encoding='utf-8') as f:
            old = json.load(f)

    new = {k: v for k, v in found.items() if k not in old}
    gone = [k for k in old if k not in found]

    out = dict(old)
    out.update(new)
    out = sorted_items(out)

    moved = [o for o, n in zip([k for k in old if k in out], [k for k in out if k in old]) if o != n]
    order_same = not moved

    print('─' * 46)
    print('过滤：关卡类 %d；日文名 %d；官方名或变体 %d；自指 %d；编号规范化 %d'
          % (stats['关卡类'], stats['日文名'], stats['官方名或变体'],
             stats['自指'], stats['编号规范化']))
    print('可归类 %d；未识别 %d（不入库）' % (len(found), len(unmatched)))
    print('库内 %d；新增 %d' % (len(old), len(new)))
    for k, v in list(new.items())[:20]:
        print('   + %-14s → %s（%s）' % (k, v['指向'], v['类型']))
    if len(new) > 20:
        print('   …（其余 %d 条见记录文件）' % (len(new) - 20))
    print('扫描中已无、但库内保留（不删）: %d' % len(gone))
    print('库内原有条目相对顺序: %s；受影响 %d %s' % (order_same, len(moved), moved[:6]))

    if args.dry_run:
        print('\n(干跑：未写文件，耗时 %.1fs)' % (time.time() - t0))
        return 0

    uc.backup_file(DST)                      # 写前备份（保留 3 份）
    with open(DST, 'w', encoding='utf-8') as f:
        json.dump(out, f, ensure_ascii=False, indent=2)

    record = {
        '脚本': {'文件': 'maintenance/update_slang.py', '版本': 1},
        '运行时间': datetime.now().strftime('%Y-%m-%d %H:%M:%S'),
        '模式': '增量',
        '数据源': {'页面': 'prts.wiki 全站重定向', '库': 'maintenance/arknights.db'},
        '表': 'slang.json',
        '计数': {'库内': len(old), '可归类': len(found), '未识别': len(unmatched), '输出': len(out)},
        '新增': new, '新增数': len(new),
        '扫描中已无（保留不删）': gone,
        '未识别': unmatched,
        '过滤统计': dict(stats),
        '结果': '成功',
    }
    print('\n====== 完成：%s ======' % DST)
    print('记录：%s' % os.path.basename(uc.write_record('slang', record)))
    return 0


if __name__ == '__main__':
    sys.exit(main())
