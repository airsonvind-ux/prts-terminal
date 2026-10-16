# -*- coding: utf-8 -*-
"""PRTS 终端 — 术语表重建（维护用）

**重建制**：每次从 PRTS 页面全量重建 src-tauri/resources/term.json，与页面对齐，
不做保留合并（页面里没有的条目就不会出现在表里）。涉及页面少，全量爬取成本可接受。

数据源（写在前面的优先；同一名称多处出现时取更靠前的值）：
  ① 术语释义页「战斗术语」段    `{{术语释义|名|效果|id=…}}`
  ② 特殊机制页                 `{{特殊机制表格|名称|简介|举例}}`
  ③ 技能页「特殊属性」节        `* '''名'''` + 紧随的 `: 说明`

用法：
    python rebuild_term.py --dry-run   # 只报告差异
    python rebuild_term.py             # 重建（写前备份 + 写记录）

口径：
  - 术语释义页只取「战斗术语」段；「基建术语」段不取。
  - 特殊机制页的名称原样入库（含全角引号，如 `“叠叠乐”` 就是名称本体）。
  - 技能页条目名带括注时只取括注前（`可关闭（可随时主动关闭）` → `可关闭`）。
  - 文本：`<br>` 作段落分隔、※ 与 `<code>` 标记去掉，其余交给 cleaning_wiki。
  - 键序 = ① → ② → ③，各自沿用页面顺序。
"""

import argparse
import json
import os
import re
import sys
from collections import OrderedDict
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from paths import RESOURCES  # noqa: E402

# 抓取与解析口径都在 update_common（原 prts_wiki.py 已并入）；pw 是沿用旧调用名的别名
import update_common as pw  # noqa: E402
import update_common as uc  # noqa: E402

PAGE = "术语释义"
PAGE_SPECIAL = "特殊机制"
PAGE_SKILL = "技能"
DST = os.path.join(str(RESOURCES), "term.json")

# ─── 本表专有的页面模板规则（基础层不管这些）───────────────
# 每条写清页面渲染结果与处理依据：


def _star_replacer(m):
    """{{**|A|B}}：渲染为「图标 (%)✕ A B」——伤害倍率标注。
    取参依旧版 term 的值（“20%-80%”与“归零”）：B 带正负号时取 A+B，否则取 B。"""
    a, b = m.group(1).strip(), m.group(2).strip()
    if b[:1] in ("+", "-", "−"):
        return a + b
    return b or a


_RANGE_TABLE = None


def _load_ranges():
    global _RANGE_TABLE
    if _RANGE_TABLE is None:
        with open(os.path.join(str(RESOURCES), "ranges.json"), encoding="utf-8") as f:
            _RANGE_TABLE = {k: {tuple(g) for g in v["grids"]} for k, v in json.load(f).items()}
    return _RANGE_TABLE


def _range_replacer(m):
    """{{攻击范围/兼容版|行1=⋯}}：渲染为范围图。按格子集合在 ranges.json 反查 rangeID（Air 定）。"""
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
    # {{无效描述|X}}：渲染为划掉的灰字（<del>，title 写明“可能根本不存在”）——
    # 页面用它标注作废内容，所以删掉（只留文字不留线才是误导）。
    ("无效描述", re.compile(r"\{\{无效描述[^{}]*\}\}"), lambda m: ""),
    # {{dot}}：渲染为 &nbsp;<b>&middot;</b>，一个圆点。列表记号保留。
    ("dot", re.compile(r"\{\{dot\}\}"), lambda m: "·"),
    # {{=}}：渲染就是一个等号。
    ("等号", re.compile(r"\{\{=\}\}"), lambda m: "="),
    # {{**|A|B}}：见 _star_replacer。
    ("变动数值", re.compile(r"\{\{\*\*\|([^{}|]*)\|([^{}|]*)\}\}"), _star_replacer),
    # {{攻击范围/兼容版|⋯}}：见 _range_replacer。
    ("攻击范围兼容版", re.compile(r"\{\{攻击范围/兼容版[^{}]*\}\}"), _range_replacer),
]


def clean_value(raw):
    """本表的取值口径：update_common.clean_value + 本表 extra_rules。"""
    return pw.clean_value(raw, EXTRA_RULES)


def parse_terms():
    """术语释义页「战斗术语」段。"""
    text = pw.fetch_page(PAGE)
    body = ""
    for level, name, seg in pw.sections(text):
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
    text = pw.fetch_page(PAGE_SKILL)
    body = ""
    for level, name, seg in pw.sections(text):
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


def build():
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
    return out, dup


def main():
    ap = argparse.ArgumentParser(description="重建术语表 term.json")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    args = ap.parse_args()

    new, dup = build()
    old = OrderedDict()
    if os.path.exists(DST):
        with open(DST, encoding="utf-8") as f:
            old = json.load(f, object_pairs_hook=OrderedDict)

    added = [k for k in new if k not in old]
    removed = [k for k in old if k not in new]
    changed = [(k, old[k], new[k]) for k in new if k in old and old[k] != new[k]]

    print("====== 术语表重建 ======")
    print("页面合计   : %d 项" % len(new))
    print("原文件     : %d 项" % len(old))
    print("新增       : %d %s" % (len(added), added[:10]))
    print("页面已无（将删除）: %d %s" % (len(removed), removed[:10]))
    print("文本变化   : %d" % len(changed))
    for k, o, n in changed[:5]:
        print("  [%s]\n    旧 %s\n    新 %s" % (k, o[:80], n[:80]))
    if len(changed) > 5:
        print("  …（其余 %d 项%s）" % (len(changed) - 5,
                                    "见记录文件" if not args.dry_run else "干跑略过"))
    if dup:
        print("多来源重名（以后者/更靠前者为准）: %s" % dup)

    if args.dry_run:
        print("\n(干跑：未写文件)")
        return 0

    uc.backup_file(DST)                      # 写前备份（保留 3 份）
    with open(DST, "w", encoding="utf-8") as f:
        json.dump(new, f, ensure_ascii=False, indent=2)

    record = {
        "脚本": {"文件": "maintenance/rebuild_term.py", "版本": 1},
        "运行时间": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "模式": "重建",
        "数据源": {"页面": [PAGE, PAGE_SPECIAL, PAGE_SKILL],
                   "段": ["战斗术语", "特殊机制表格", "特殊属性"]},
        "表": "term.json",
        "计数": {"页面": len(new), "原文件": len(old)},
        "新增": added, "新增数": len(added),
        "页面已无": removed, "页面已无数": len(removed),
        "文本变化": [{"键": k, "旧": o, "新": n} for k, o, n in changed],
        "变化数": len(changed),
        "多来源重名": dup,
        "结果": "成功",
    }
    print("\n====== 完成：%s ======" % DST)
    print("记录：%s" % os.path.basename(uc.write_record("term", record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
