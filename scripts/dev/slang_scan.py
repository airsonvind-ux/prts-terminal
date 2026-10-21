#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""PRTS 终端 · slang 表素材扫描（一次性脚本）

从 prts.wiki 拉取全站重定向（社区别名的来源），对齐 arknights.db 分类、过滤，
产出可人工清洗的草稿。本脚本是一次性工具：之后 slang 表的维护直接人工修改
src-tauri/resources/slang.json，无需再跑它。

内置过滤（表里只收"库外"的社区信息）：
- 官方代号与名称变体：对照 operators / tokens / traps 的代号列，覆盖
  大小写、变音、缩写、词序省略、拼写近似（如 Surtr / Eyja / Mlynar 类）
- 日文名：假名（含片假名）、予備隊員 系
- 关卡类重定向：对照 stages 的编号与“编号+名称”组合，并覆盖活动代码、
  区域前缀、＃ 标题等结构模式（如 GT-1 日正当中 / EE-01 xx / 切尔诺伯格 13区废墟）
- 形如 “0-1” → “0-1 坍塌” 的编号规范化
- 引号/标点变体（如 三头犬 → “三头犬”）

用法
    python scripts/slang_scan.py                 # 全量（约 30 次只读请求，1 分钟内）
    python scripts/slang_scan.py --max-pages 1   # 冒烟：每段最多取 1 页

产出（scripts/ 目录）
    slang_draft.json            清洗主文件（别名 → 指向/类型；不含“未识别”目标）
    slang_draft_unmatched.json  未识别目标的条目（参考用，可整段忽略）

请求纪律：官方 API、只读、0.35s 间隔、maxlag=5、失败重试 3 次。
"""
import argparse
import json
import re
import sqlite3
import sys
import time
import unicodedata
import urllib.parse
import urllib.request
from collections import Counter
from difflib import SequenceMatcher
from pathlib import Path

API = 'https://prts.wiki/api.php'
UA = 'PRTS-terminal-slangescan/0.1 (one-time, read-only)'
SCRIPT_DIR = Path(__file__).resolve().parent
DB_PATH = SCRIPT_DIR.parent / 'src-tauri' / 'resources' / 'arknights.db'

try:  # 让中文进度在 Windows 控制台尽量可读
    sys.stdout.reconfigure(errors='replace')
except Exception:
    pass


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
    """官方代号 / 名称的变体判定：命中说明它是“库内信息”的变体，应剔除。"""
    fa, fn = fold(alias), fold(target)
    if fa == fn:
        return True                     # 官方名称的标点/大小写变体
    if _HAN.search(alias):
        return False
    if code:
        fc = fold(code)
        if fa == fc:
            return True                 # 官方代号本身
        if len(fa) >= 3 and (fa in fc or fc in fa):
            return True                 # 缩写 / 被缩写
        ta, tc = tokens_of(alias), tokens_of(code)
        if len(ta) >= 2 and all(t in tc for t in ta):
            return True                 # 词序省略（如 Reserve (Caster)）
        if len(fa) >= 4 and SequenceMatcher(None, fa, fc).ratio() >= 0.85:
            return True                 # 拼写近似
    if fn and len(fa) >= 4 and SequenceMatcher(None, fa, fn).ratio() >= 0.85:
        return True                     # 与名称的拼写近似
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
                return True             # 13区废墟 / A1的基石 类
            if re.match(r'^[A-Za-z]{1,4}-[A-Za-z]?\d', tok):
                return True             # EE-01 / AC-3 类
        if toks[0] in stage['types']:
            return True                 # 悖论模拟 xxx 类
    return '＃' in tt or '/' in tt


def is_act_id(alias):
    """Wiki 内部活动编号（Act1arkhub 01 之类），非社区用法。"""
    low = alias.lower()
    if low.startswith('level act'):
        return True
    return low.startswith('act') and len(low) > 3 and low[3].isdigit()


# ─────────────────────────── 数据拉取 ───────────────────────────

def api(params, retries=3):
    """带重试与 maxlag 的只读 API 调用。"""
    params = dict(params)
    params.setdefault('format', 'json')
    params.setdefault('maxlag', '5')
    url = API + '?' + urllib.parse.urlencode(params)
    last = None
    for i in range(retries):
        try:
            req = urllib.request.Request(url, headers={'User-Agent': UA})
            with urllib.request.urlopen(req, timeout=30) as r:
                data = json.loads(r.read().decode('utf-8'))
            if 'error' in data:
                if data['error'].get('code') == 'maxlag':
                    print('  (服务器提示 maxlag，稍候重试)', flush=True)
                    time.sleep(5)
                    continue
                raise RuntimeError(str(data['error']))
            return data
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(1.5 * (i + 1))
    raise RuntimeError(f'请求失败：{url}（{last}）')


def fetch_sources(max_pages):
    """allpages：全站重定向源页 → {pageid: 标题}。"""
    result, cont, page = {}, None, 0
    while True:
        p = {'action': 'query', 'list': 'allpages', 'apfilterredir': 'redirects',
             'aplimit': '500', 'apnamespace': '0'}
        if cont:
            p['apcontinue'] = cont
        d = api(p)
        items = d.get('query', {}).get('allpages', [])
        for it in items:
            result[it['pageid']] = it['title']
        page += 1
        print(f'  [源页] 第 {page} 页 +{len(items)}，累计 {len(result)}', flush=True)
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
        d = api(p)
        items = d.get('query', {}).get('allredirects', [])
        for it in items:
            pairs.append((it.get('fromid'), it.get('title', '')))
        page += 1
        print(f'  [映射] 第 {page} 页 +{len(items)}，累计 {len(pairs)}', flush=True)
        if max_pages and page >= max_pages:
            break
        c = d.get('continue')
        if not c:
            break
        cont = c.get('arcontinue')
        time.sleep(0.35)
    return pairs


# ─────────────────────────── 库内索引 ───────────────────────────

def load_db():
    """从 arknights.db 读入实体名称、代号、关卡索引。"""
    con = sqlite3.connect(str(DB_PATH))

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


ORDER = {'干员': 0, '敌人': 1, '其他实体': 2, '未识别': 3}


# ─────────────────────────── 主流程 ───────────────────────────

def main():
    ap = argparse.ArgumentParser(description='slang 素材扫描（一次性）')
    ap.add_argument('--max-pages', type=int, default=0, help='冒烟用：每段最多取 N 页')
    args = ap.parse_args()
    max_pages = args.max_pages or None

    t0 = time.time()
    print('① 拉取重定向源页（allpages）...', flush=True)
    sources = fetch_sources(max_pages)
    print('② 拉取重定向映射（allredirects）...', flush=True)
    mapping = fetch_map(max_pages)
    print(f'③ 联表、分类与过滤（源页 {len(sources)} / 映射 {len(mapping)}）...', flush=True)

    data = load_db()
    stats = Counter()
    draft, unmatched = {}, {}
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
        bucket = unmatched if t == '未识别' else draft
        bucket[alias] = {'指向': target, '类型': t}

    def ordered(d):
        return {k: d[k] for k in sorted(d, key=lambda a: (ORDER[d[a]['类型']], a))}

    draft, unmatched = ordered(draft), ordered(unmatched)
    out1 = SCRIPT_DIR / 'slang_draft.json'
    out2 = SCRIPT_DIR / 'slang_draft_unmatched.json'
    with open(out1, 'w', encoding='utf-8') as f:
        json.dump(draft, f, ensure_ascii=False, indent=2)
    with open(out2, 'w', encoding='utf-8') as f:
        json.dump(unmatched, f, ensure_ascii=False, indent=2)

    cnt = Counter(v['类型'] for v in draft.values())
    print('─' * 40)
    print(f'联表：映射 {len(mapping)} 条；缺源页 {stats["缺源页"]}；重复 {stats["重复"]}')
    print(f'过滤：关卡类 {stats["关卡类"]}；日文名 {stats["日文名"]}；'
          f'官方名或变体 {stats["官方名或变体"]}；自指 {stats["自指"]}；'
          f'编号规范化 {stats["编号规范化"]}')
    print('草稿：' + '，'.join(f'{k} {v}' for k, v in sorted(cnt.items(), key=lambda kv: ORDER[kv[0]]))
          + f'（共 {len(draft)}）')
    print(f'未识别：{len(unmatched)}（见 slang_draft_unmatched.json）')
    print(f'输出：{out1.name} / {out2.name}；耗时 {time.time() - t0:.1f}s')


if __name__ == '__main__':
    main()
