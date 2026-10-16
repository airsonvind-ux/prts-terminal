# -*- coding: utf-8 -*-
"""PRTS Wiki 抓取与 wikitext 解析（维护用）

term / abnormal_effects 等「页面文本型」表共用：
  - fetch_page：按标题取 wikitext（跟随重定向，只读）
  - templates：找出 `{{名|…}}`，括号配平，容忍嵌套模板
  - split_params：按顶层 `|` 切分参数（跳过嵌套 {{}} 与 [[]]）
  - sections：切出 `== 标题 ==` 小节
  - block_cells：从 `{|…|}` 表格块里取单元格文本

解析为什么不直接用正则：一个参数里可能嵌着别的模板（`{{**|20%|-80%}}`）
或链接，非贪婪正则会提前咬断；这里一律用深度计数配平。
"""

import json
import re
import time
import urllib.parse
import urllib.request

API = "https://prts.wiki/api.php"
UA = "PRTS-terminal-maintenance/1.0 (read-only, one-shot per table)"

# 一次运行内的页面缓存，避免重复请求
_CACHE = {}


def fetch_page(title, retries=3):
    """取页面 wikitext；跟随重定向。失败重试后抛异常（更新脚本据此中止）。"""
    if title in _CACHE:
        return _CACHE[title]
    params = {"action": "query", "prop": "revisions", "rvprop": "content",
              "rvslots": "main", "format": "json", "titles": title, "redirects": "1"}
    url = API + "?" + urllib.parse.urlencode(params)
    last = None
    for i in range(retries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=30) as r:
                d = json.loads(r.read().decode("utf-8"))
            for pg in d.get("query", {}).get("pages", {}).values():
                if "revisions" not in pg:
                    raise RuntimeError("页面无内容：%s（%s）" % (title, pg.get("title")))
                text = pg["revisions"][0]["slots"]["main"]["*"]
                _CACHE[title] = text
                return text
            raise RuntimeError("未取到页面：%s" % title)
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(1.5 * (i + 1))
    raise RuntimeError("拉取失败：%s（%s）" % (title, last))


def strip_comments(text):
    return re.sub(r"<!--.*?-->", "", text, flags=re.S)


def templates(text, name):
    """返回所有 {{name|…}} 的原始片段（括号配平）。"""
    out, i, tag = [], 0, "{{" + name + "|"
    while True:
        i = text.find(tag, i)
        if i < 0:
            return out
        depth, j = 0, i
        while j < len(text):
            if text.startswith("{{", j):
                depth += 1
                j += 2
            elif text.startswith("}}", j):
                depth -= 1
                j += 2
                if depth == 0:
                    break
            else:
                j += 1
        if j >= len(text):          # 未闭合，丢弃
            return out
        out.append(text[i:j])
        i = j


def split_params(frag):
    """切模板参数（不含模板名）。顶层 `|` 才切，嵌套模板/链接整体保留。"""
    body = frag[2:-2]
    parts, cur, d1, d2, k = [], [], 0, 0, 0
    while k < len(body):
        if body.startswith("{{", k):
            d1 += 1
        elif body.startswith("}}", k):
            d1 -= 1
        elif body.startswith("[[", k):
            d2 += 1
        elif body.startswith("]]", k):
            d2 -= 1
        if body[k] == "|" and d1 == 0 and d2 == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(body[k])
        k += 1
    parts.append("".join(cur))
    return parts[1:]


def sections(text):
    """返回 [(层级, 标题, 正文)]；正文到同层或更浅的下一个标题为止。"""
    marks = list(re.finditer(r"^(=+)\s*([^=\n]+?)\s*\1\s*$", text, re.M))
    out = []
    for i, m in enumerate(marks):
        level = len(m.group(1))
        end = len(text)
        for nxt in marks[i + 1:]:
            if len(nxt.group(1)) <= level:
                end = nxt.start()
                break
        out.append((level, m.group(2).strip(), text[m.end():end]))
    return out


def block_cells(body):
    """从 {|…|} 表格块里取单元格文本（按 || 切，跳过表头与样式行）。"""
    cells = []
    for block in re.findall(r"\{\|.*?\|\}", body, re.S):
        for line in block.splitlines():
            s = line.strip()
            if not s.startswith("|") or s.startswith(("|-", "|}", "|+")):
                continue
            s = s[1:]
            if re.match(r"^(class|style|colspan|rowspan|width)\s*=", s):
                continue
            for c in s.split("||"):
                c = c.strip()
                if c and c != "}":
                    cells.append(c)
    return cells


def plain(text):
    """去掉单元格/文本里的媒体链接、链接与样式，粗略取显示文字（供匹配用）。"""
    # 媒体链接先删：[[文件:x.png|25px]] 抠不出名字，其“显示名”是 25px 不是条目名
    t = re.sub(r"\[\[(?:文件|图像|影音|File|Image):[^\]]*\]\]", "", text, flags=re.IGNORECASE)
    t = re.sub(r"\[\[(?:[^\]|]*\|)?([^\]|]*)\]\]", r"\1", t)
    t = re.sub(r"\{\{[^{}]*\|([^{}|]*)\}\}", r"\1", t)
    t = re.sub(r"\{\{[^{}]*\}\}", "", t)
    t = re.sub(r"<[^>]+>", "", t)
    t = t.replace("'''", "").replace("''", "")
    return t.replace("\u00a0", " ").strip()


def clean_value(raw):
    """页面参数 → 库内文本：`<br>`/`<br/>` 作段落分隔（丢弃段首 ※），再过共享清洗。"""
    import os
    import sys
    here = os.path.dirname(os.path.abspath(__file__))
    if here not in sys.path:
        sys.path.insert(0, here)
    from cleaning_wiki import clean_wiki  # noqa: E402

    t = re.sub(r"<br\s*/?>\s*", "。", raw)
    # 页面用 <code> 包内部 ID（如 <code>STUN</code>），渲染出来只是那个词本身
    t = re.sub(r"</?code(\s[^>]*)?>", "", t)
    t = clean_wiki(t)
    t = re.sub(r"※\s*", "", t)             # ※ 是页面标注记号，渲染时不存在
    t = re.sub(r"[；，]+\s*。", "。", t)     # “；。” → “。”
    t = re.sub(r"。\s*[；，]", "。", t)     # “。；” → “。”
    t = re.sub(r"。{2,}", "。", t)
    t = re.sub(r"^[。；，]+\s*", "", t)
    t = re.sub(r"\s*[。；，]+\s*$", "", t)
    t = re.sub(r"\s*\n\s*", "", t)
    return t.strip()
