# -*- coding: utf-8 -*-
"""PRTS 终端 — 术语表更新（维护用）

数据源：PRTS「术语释义」页的 **战斗术语** 段（== 战斗术语 == 下属各小节）
        + PRTS「特殊机制」页的 **特殊机制表格** 条目
        + PRTS「技能」页的 **特殊属性** 节（* '''名''' / : 说明 逐条）
        + 现有 term.json（三个页面都没有的条目原样保留）
输出：src-tauri/resources/term.json

用法：
    python update_term.py --dry-run   # 只报告差异
    python update_term.py             # 写库（写前备份 + 写记录）

三段式：
  ① 校对：页面与库内同名条目的文本差异
  ② 新增：页面有、库内无
  ③ 保留：库内有、页面无（**原样不动**，只报告）

口径：
  - 术语释义页只取「战斗术语」段；「基建术语」段不取。
  - 特殊机制页取 `{{特殊机制表格|名称|简介|举例}}`（名称即条目名，取简介）；
    页面上的名称可能带全角引号（如 `“叠叠乐”`），去引号后入库。
  - 技能页取「特殊属性」节：`* '''名'''` 为条目名（带括注时只取括注前），
    紧随的 `: ` 行为说明。
  - 文本取页面全文，`<br>` 作段落分隔、※ 与 `<code>` 标记去掉，其余交给 cleaning_wiki。
  - 库内条目按原键序保留；新条目追加在末尾。
"""

import argparse
import json
import os
import shutil
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
from paths import RESOURCES  # noqa: E402

import prts_wiki as pw  # noqa: E402

PAGE = "术语释义"
PAGE_SPECIAL = "特殊机制"
PAGE_SKILL = "技能"
DST = os.path.join(str(RESOURCES), "term.json")
UPDATES = os.path.join(HERE, "updates")
BACKUPS = os.path.join(HERE, "backups")


def parse_skill_terms():
    """技能页「特殊属性」节：`* '''名'''` 起条，`:` 行为说明。"""
    text = pw.fetch_page(PAGE_SKILL)
    body = ""
    for level, name, seg in pw.sections(text):
        if name == "特殊属性":
            body = seg
            break
    if not body:
        raise RuntimeError("未找到「特殊属性」节：%s" % PAGE_SKILL)

    out, cur, buf = {}, None, []
    for line in body.splitlines():
        s = line.strip()
        if s.startswith("*") and "'''" in s:
            if cur:
                out[cur] = pw.clean_value("\n".join(buf))
            # 名称：去标记后取“（”前（如“可关闭（可随时主动关闭）”→“可关闭”）
            name = pw.plain(s.lstrip("*").strip()).split("（")[0].strip()
            cur, buf = name or None, []
        elif s.startswith(":") and cur:
            buf.append(s[1:].strip())
    if cur:
        out[cur] = pw.clean_value("\n".join(buf))
    return {k: v for k, v in out.items() if k and v}


def parse_page():
    """三个来源合计，**顺序即优先级**：特殊机制 → 技能 → 术语释义。

    术语释义页最后应用，冲突时以它为准（它是术语表主来源；
    例如「过载」在技能页与术语页都有且文案不同）。
    """
    out, dup = {}, []

    text = pw.fetch_page(PAGE)
    body = ""
    for level, name, seg in pw.sections(text):
        if level == 2 and name == "战斗术语":
            body = seg
            break
    if not body:
        raise RuntimeError("未找到「战斗术语」段：%s" % PAGE)

    text2 = pw.fetch_page(PAGE_SPECIAL)
    for frag in pw.templates(text2, "特殊机制表格"):
        ps = pw.split_params(frag)
        if len(ps) < 2:
            continue
        # 页面上的名称可能带全角引号（“叠叠乐”），入库时去掉
        name = pw.plain(ps[0]).strip().strip("“”\"'")
        value = pw.clean_value(ps[1])
        if not name:
            continue
        if name in out and out[name] != value:
            dup.append(name)
        out[name] = value

    for name, value in parse_skill_terms().items():
        if name in out and out[name] != value:
            dup.append(name)
        out[name] = value

    for frag in pw.templates(body, "术语释义"):     # 最后应用：术语页优先
        ps = pw.split_params(frag)
        if len(ps) < 2:
            continue
        name = ps[0].strip().replace("\u00a0", " ")
        value = pw.clean_value(ps[1])
        if name in out and out[name] != value:
            dup.append(name)
        out[name] = value
    return out, dup


def main():
    ap = argparse.ArgumentParser(description="更新术语表 term.json")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    args = ap.parse_args()

    page, dup = parse_page()
    old = {}
    if os.path.exists(DST):
        with open(DST, encoding="utf-8") as f:
            old = json.load(f)

    changed = [(k, old[k], page[k]) for k in page if k in old and old[k] != page[k]]
    added = [k for k in page if k not in old]
    kept = [k for k in old if k not in page]

    out = {}
    for k in old:                       # 原键序优先
        out[k] = page[k] if k in page else old[k]
    for k in page:                      # 新条目追加
        if k not in out:
            out[k] = page[k]

    print("====== 术语表更新 ======")
    print("页面条目          : %d 项" % len(page))
    print("库内              : %d 项" % len(old))
    print("输出              : %d 项" % len(out))
    print("文本变化          : %d" % len(changed))
    for k, o, n in changed[:5]:
        print("  [%s]\n    旧 %s\n    新 %s" % (k, o[:80], n[:80]))
    if len(changed) > 5:
        print("  …（其余 %d 项%s）" % (len(changed) - 5, "见记录文件" if not args.dry_run else "干跑略过"))
    print("新增              : %d %s" % (len(added), added[:10]))
    print("页面外保留（不动）: %d %s" % (len(kept), kept[:8]))
    if dup:
        print("页面重名不同值    : %s" % dup)

    if args.dry_run:
        print("\n(干跑：未写文件)")
        return 0

    os.makedirs(BACKUPS, exist_ok=True)
    os.makedirs(UPDATES, exist_ok=True)
    ts = datetime.now().strftime("%Y-%m-%d-%H%M%S")
    if os.path.exists(DST):
        shutil.copy2(DST, os.path.join(BACKUPS, "term.json.%s.bak" % ts.replace("-", "")[0:8]))
    with open(DST, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=2)

    record = {
        "脚本": {"文件": "maintenance/update_term.py", "版本": 1},
        "运行时间": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {"页面": [PAGE, PAGE_SPECIAL, PAGE_SKILL],
                   "段": ["战斗术语", "特殊机制表格", "特殊属性"]},
        "表": "term.json",
        "计数": {"页面": len(page), "库内": len(old), "输出": len(out)},
        "文本变化": [{"键": k, "旧": o, "新": n} for k, o, n in changed],
        "变化数": len(changed),
        "新增": added,
        "新增数": len(added),
        "页面外保留": kept,
        "结果": "成功",
        "写入": {"更新": len(changed), "新增": len(added)},
    }
    path = os.path.join(UPDATES, "%s-term.json" % ts)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(record, f, ensure_ascii=False, indent=2)
    print("\n====== 完成：%s ======" % DST)
    print("记录：%s" % os.path.basename(path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
