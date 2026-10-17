# -*- coding: utf-8 -*-
"""term · 重建（术语表）

数据源（PRTS 三页；写在前面的优先，同一名称多处出现时取更靠前的值）
    ① 术语释义页「战斗术语」段   {{术语释义|名|效果|id=…}}
    ② 特殊机制页                {{特殊机制表格|名称|简介|举例}}
    ③ 技能页「特殊属性」节       * '''名''' + 紧随的 : 说明

输出
    src-tauri/resources/term.json · {名: 文本}

口径
    术语释义页只取「战斗术语」段；特殊机制页名称原样（含全角引号）；
    技能页条目名带括注时只取括注前；键序 = ① → ② → ③，各自沿用页面顺序。
    与页面对齐，不做保留合并。

本表专有模板规则（基础层不管这些）
    {{无效描述|X}} → 删（页面用删除线标注作废内容）
    {{dot}} → ·      {{=}} → =
    {{**|A|B}} → 变动数值（B 带正负号取 A+B，否则取 B）
    {{攻击范围/兼容版|行1=…}} → 按格子集合反查 ranges.json → [攻击范围: x-2]

用法
    python rebuild.py --dry-run
    python rebuild.py
"""

import json
import re
import sys
from collections import OrderedDict
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import table_base as tb  # noqa: E402
import update_common as pw  # noqa: E402

PAGE = "术语释义"
PAGE_SPECIAL = "特殊机制"
PAGE_SKILL = "技能"
OUT = paths.RESOURCES / "term.json"


# ─── 本表专有的页面模板规则（基础层不管这些）───────────────

def _star_replacer(m):
    """{{**|A|B}}：渲染为伤害倍率标注。B 带正负号时取 A+B，否则取 B。"""
    a, b = m.group(1).strip(), m.group(2).strip()
    if b[:1] in ("+", "-", "−"):
        return a + b
    return b or a


_RANGE_TABLE = None


def _load_ranges():
    global _RANGE_TABLE
    if _RANGE_TABLE is None:
        with open(paths.RESOURCES / "ranges.json", encoding="utf-8") as f:
            _RANGE_TABLE = {k: {tuple(g) for g in v["grids"]} for k, v in json.load(f).items()}
    return _RANGE_TABLE


def _range_replacer(m):
    """{{攻击范围/兼容版|行1=⋯}}：按格子集合在 ranges.json 反查 rangeID。"""
    rows = re.findall(r"行\d+\s*=\s*([^|}]*)", m.group(0))
    cells, self_pos = set(), None
    for r, row in enumerate(rows):
        for c, ch in enumerate(row.strip()):
            if ch == "我":
                self_pos = (r, c)
            elif ch == "框":
                cells.add((r, c))
    if self_pos is None or not cells:
        return "[攻击范围: 未知]"
    want = {(r - self_pos[0], c - self_pos[1]) for r, c in cells} | {(0, 0)}
    for rid, grid in _load_ranges().items():
        if grid == want:
            return "[攻击范围: %s]" % rid
    return "[攻击范围: 未知]"


EXTRA_RULES = [
    ("无效描述", re.compile(r"\{\{无效描述[^{}]*\}\}"), lambda m: ""),
    ("dot", re.compile(r"\{\{dot\}\}"), lambda m: "·"),
    ("等号", re.compile(r"\{\{=\}\}"), lambda m: "="),
    ("变动数值", re.compile(r"\{\{\*\*\|([^{}|]*)\|([^{}|]*)\}\}"), _star_replacer),
    ("攻击范围兼容版", re.compile(r"\{\{攻击范围/兼容版[^{}]*\}\}"), _range_replacer),
]


def clean_value(raw):
    """本表取值口径：update_common.clean_value + 本表 extra_rules。"""
    return pw.clean_value(raw, EXTRA_RULES)


# ─── 三个来源 ────────────────────────────────────────────────

def parse_terms():
    """术语释义页「战斗术语」段。"""
    body = ""
    for level, name, seg in pw.sections(pw.fetch_page(PAGE)):
        if level == 2 and name == "战斗术语":
            body = seg
            break
    if not body:
        raise RuntimeError("未找到「战斗术语」段：%s" % PAGE)
    out = OrderedDict()
    for frag in pw.templates(body, "术语释义"):
        ps = pw.split_params(frag)
        if len(ps) < 2:
            continue
        out[ps[0].strip().replace("\u00a0", " ")] = clean_value(ps[1])
    return out


def parse_special():
    """特殊机制页的表格条目（名称原样，含引号）。"""
    out = OrderedDict()
    for frag in pw.templates(pw.fetch_page(PAGE_SPECIAL), "特殊机制表格"):
        ps = pw.split_params(frag)
        if len(ps) < 2:
            continue
        name = pw.plain(ps[0])
        if name:
            out[name] = clean_value(ps[1])
    return out


def parse_skill():
    """技能页「特殊属性」节：`* '''名'''` 起条，`:` 行为说明。"""
    body = ""
    for level, name, seg in pw.sections(pw.fetch_page(PAGE_SKILL)):
        if name == "特殊属性":
            body = seg
            break
    if not body:
        raise RuntimeError("未找到「特殊属性」节：%s" % PAGE_SKILL)

    out, cur, buf = OrderedDict(), None, []
    for line in body.splitlines():
        s = line.strip()
        if s.startswith("*") and "'''" in s:
            if cur:
                out[cur] = clean_value("\n".join(buf))
            name = pw.plain(s.lstrip("*").strip()).split("（")[0].strip()
            cur, buf = name or None, []
        elif s.startswith(":") and cur:
            buf.append(s[1:].strip())
    if cur:
        out[cur] = clean_value("\n".join(buf))
    return OrderedDict((k, v) for k, v in out.items() if k and v)


def build(old=None):
    """三来源合并：写在前面的优先（术语释义页是主来源）。"""
    sources = [("术语释义", parse_terms()), ("特殊机制", parse_special()),
               ("技能", parse_skill())]
    out = OrderedDict()
    dup = []
    for label, items in sources:
        for k, v in items.items():
            if k in out:
                if out[k] != v:
                    dup.append((k, label))
                continue
            out[k] = v
    print("  术语释义 %d | 特殊机制 %d | 技能 %d → 合并 %d"
          % (len(sources[0][1]), len(sources[1][1]), len(sources[2][1]), len(out)))
    if dup:
        print("  多来源重名（保留靠前的）: %s" % dup)
    return out


SPEC = tb.RebuildSpec(name="term", output=OUT, build=build)

if __name__ == "__main__":
    sys.exit(tb.run_rebuild(SPEC))
