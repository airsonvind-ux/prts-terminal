"""
PRTS 终端 — 更新系统通用模块

只装「与具体表无关」的东西：数据源版本、库备份、更新记录、PRTS 抓取与结构哨兵、
列比较、输出环境。表脚本负责「算哪些行、比哪些列、抓什么页面、怎么解释页面」。

刻意不放进来的（各有各的理由）：
  - 清洗规则 → cleaning_game.py / cleaning_wiki.py（另一类通用模块，契约不同：那是一张
    顺序有语义的规则表，且仍随 PRTS 页面结构生长）；
  - 爬取循环 → 各表自己写。enemies 是「一个名字对一个页面」，stages 是「多个关卡共用
    一个页面（按 URL 分组）」，跳过的语义也不同，硬抽会在下一个用例上被推翻；
  - CLI 参数解析 → 各表自己定义（flag 组合会分叉）；
  - 表结构、枚举映射、列类型集合 → 各自的表脚本。

系统级约定（各表脚本共享，会写进更新记录）：
  - 更新记录：<maintenance>/updates/<表>/<时间戳>.json
  - 写法前备份：<maintenance>/backups/<文件名>.<时间戳>.bak，每个文件名保留最近
    KEEP_BACKUPS 份（库与资源 JSON 走同一条路径：backup_file，各表互不挤占）
  - 两个目录都不在本模块里推算，统一从 paths.BACKUPS / paths.UPDATES 取
    （本模块放在 lib/ 下，按 __file__ 推算会差一级）
"""

import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

# 路径唯一来源：paths.py 在 data-pipeline/（上一级或更上）。这里只补 sys.path，不自算目录
_HERE = Path(__file__).resolve().parent
_DP = next(p for p in _HERE.parents if (p / "paths.py").is_file())   # data-pipeline/
for _p in (str(_HERE), str(_DP)):
    if _p not in sys.path:
        sys.path.insert(0, _p)
import paths  # noqa: E402

KEEP_BACKUPS = 3            # 自动备份保留份数
SLEEP_BETWEEN_FETCH = 0.1   # 爬取间隔，礼貌抓取
FETCH_RETRIES = 2           # 单页网络异常重试次数（每次间隔 1 秒）
FETCH_TIMEOUT = 30          # 单页抓取超时（秒）


# ─── 输出环境 ─────────────────────────────────────────────────

def reconfigure_stdout():
    """关掉缓冲、统一 UTF-8：PowerShell 下否则进度憋到最后才吐、中文可能乱码。"""
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace", line_buffering=True)
    except Exception:
        pass


# ─── 数据源版本 ───────────────────────────────────────────────

def read_source_version(data_source):
    """从数据源仓库的最新提交信息里取 提交 / 提交标题 / 游戏版本 / 数据时间。

    提交信息形如：`[CN UPDATE] Client:2.7.81 Data:26-10-08-04-51-28-56071834`
    取不到不算失败（返回的字段为 None）。
    """
    info = {"提交": None, "提交标题": None, "游戏版本": None, "数据时间": None}
    try:
        out = subprocess.run(
            ["git", "-C", str(data_source), "log", "-1", "--format=%H%n%s"],
            capture_output=True, text=True, timeout=30,
        )
        if out.returncode == 0:
            lines = out.stdout.strip().splitlines()
            if lines:
                info["提交"] = lines[0][:7]
            if len(lines) > 1:
                info["提交标题"] = lines[1]
                m = re.search(r"Client:([0-9.]+)", lines[1])
                if m:
                    info["游戏版本"] = m.group(1)
                m = re.search(r"Data:([0-9A-Za-z\-]+)", lines[1])
                if m:
                    info["数据时间"] = m.group(1)
    except Exception:
        pass
    return info


# ─── 库备份 / 更新记录 ────────────────────────────────────────

def backup_file(path, keep=KEEP_BACKUPS, backup_dir=None):
    """把文件备份到 backup_dir（默认 <maintenance>/backups），只保留最近 keep 份。

    返回备份文件路径；源文件不存在时返回 None。数据库与资源 JSON 共用。
    """
    if not os.path.exists(path):
        return None
    if backup_dir is None:
        backup_dir = str(paths.BACKUPS)
    os.makedirs(backup_dir, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    dst = os.path.join(backup_dir, "%s.%s.bak" % (os.path.basename(path), stamp))
    with open(path, "rb") as src, open(dst, "wb") as out:
        shutil.copyfileobj(src, out, 1024 * 1024)
    # 按源文件名分别保留：不同表/不同库的备份互不挤占
    base = os.path.basename(path)
    backups = sorted(f for f in os.listdir(backup_dir)
                     if f.endswith(".bak") and f.startswith(base + "."))
    if len(backups) > keep:
        for stale in backups[:-keep]:
            try:
                os.remove(os.path.join(backup_dir, stale))
            except OSError:
                pass
    return dst


# 旧名保留：数据库类脚本仍按 backup_db 调用
def backup_db(db_path, keep=KEEP_BACKUPS, backup_dir=None):
    return backup_file(db_path, keep=keep, backup_dir=backup_dir)


def write_record(table, record, out_dir=None):
    """把更新记录写到 <maintenance>/updates/<时间戳>-<表>.json，返回路径。"""
    if out_dir is None:
        out_dir = str(paths.UPDATES)
    os.makedirs(out_dir, exist_ok=True)
    path = os.path.join(out_dir, time.strftime("%Y-%m-%d-%H%M%S") + "-%s.json" % table)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(record, f, ensure_ascii=False, indent=2)
    return path


def brief(value, limit=120):
    """把值截短到 limit 字符，用于记录里的明细。"""
    if value is None:
        return None
    s = str(value)
    return s[:limit] + ("…" if len(s) > limit else "")


# ─── 明细：把「旧→新」压成能看出差异的形态 ────────────────────
#
# 长列的旧值/新值各截一刀是没用的（两边前缀一模一样）。所以：
#   - 短值：照旧给 旧 / 新；
#   - JSON 列：按项/按键给「哪一项、哪个字段、旧→新」（敌人列表这类）；
#   - 长文本：算共同前缀，只给差异处及左右上下文。

_MISSING = object()


def _same_val(a, b):
    if isinstance(a, bool) or isinstance(b, bool):
        return a is b
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return float(a) == float(b)
    return (json.dumps(a, sort_keys=True, ensure_ascii=False) ==
            json.dumps(b, sort_keys=True, ensure_ascii=False))


values_equal = _same_val   # 供各表脚本复用（同一套“值是否相等”口径）


def _brief_json(v, limit=70):
    if v is None:
        return None
    s = json.dumps(v, ensure_ascii=False) if isinstance(v, (dict, list)) else str(v)
    return s[:limit] + ("…" if len(s) > limit else "")


def _json_items(value):
    """list → {标识: 项}；标识优先用 名称|级别，否则用下标。

    同名同级别可能出现多次（关卡里同一个敌人有两条 ref），所以加出现序号，
    否则后一条会覆盖前一条，差异比对会错位。
    """
    out = {}
    seen = {}
    for i, item in enumerate(value):
        if isinstance(item, dict) and "名称" in item:
            base = "%s|%s" % (item.get("名称"), item.get("级别", 0))
        else:
            base = str(i)
        seen[base] = seen.get(base, 0) + 1
        out[base if seen[base] == 1 else "%s#%d" % (base, seen[base])] = item
    return out


def _dict_changes(a, b):
    """{键: (旧, 新)}，含新增（旧为 None）与删除（新为 None）。"""
    out = {}
    for k in set(a) | set(b):
        va, vb = a.get(k, _MISSING), b.get(k, _MISSING)
        if va is _MISSING:
            out[k] = (None, vb)
        elif vb is _MISSING:
            out[k] = (va, None)
        elif not _same_val(va, vb):
            out[k] = (va, vb)
    return out


def _json_diff(a, b, limit):
    """a / b 为已解析的 JSON 对象；两者同为 list 或 dict 时给结构化差异，否则返回 None。"""
    out = []
    if isinstance(a, list) and isinstance(b, list):
        fa, fb = _json_items(a), _json_items(b)
        order = ([k for k in fa if k in fb] +
                 [k for k in fa if k not in fb] +
                 [k for k in fb if k not in fa])
        for k in order:
            if len(out) >= limit:
                break
            if k not in fa:
                out.append({"项": k, "变化": "新增", "新": _brief_json(fb[k])})
            elif k not in fb:
                out.append({"项": k, "变化": "删除", "旧": _brief_json(fa[k])})
            else:
                va, vb = fa[k], fb[k]
                if isinstance(va, dict) and isinstance(vb, dict):
                    for kk, (o, n) in _dict_changes(va, vb).items():
                        if len(out) >= limit:
                            break
                        out.append({"项": k, "字段": kk, "旧": _brief_json(o), "新": _brief_json(n)})
                elif not _same_val(va, vb):
                    out.append({"项": k, "旧": _brief_json(va), "新": _brief_json(vb)})
    elif isinstance(a, dict) and isinstance(b, dict):
        for kk, (o, n) in _dict_changes(a, b).items():
            if len(out) >= limit:
                break
            out.append({"字段": kk, "旧": _brief_json(o), "新": _brief_json(n)})
    else:
        return None
    return out or [{"说明": "结构相同，仅文本形式不同"}]


def _text_diff(a, b, ctx=20, span=60):
    """长文本：算共同前缀，只给差异处及左右上下文。"""
    n = min(len(a), len(b))
    p = 0
    while p < n and a[p] == b[p]:
        p += 1
    return {"共同前缀": p, "上文": a[max(0, p - ctx):p],
            "旧": brief(a[p:p + span]), "新": brief(b[p:p + span])}


def diff_brief(old, new, as_json=False, limit=4):
    """记录明细用：旧→新 的简短差异描述。

    JSON 列一律走结构化差异（不依赖文本长短）；文本列超长时只给差异处与上下文。
    """
    if old is None or new is None:
        return {"旧": brief(old, 120), "新": brief(new, 120)}
    a, b = str(old), str(new)
    if a == b:
        return {"旧": brief(a, 80), "新": brief(b, 80)}
    if as_json:
        try:
            va, vb = json.loads(a), json.loads(b)
        except (TypeError, ValueError):
            va = vb = None
        if ((isinstance(va, list) and isinstance(vb, list)) or
                (isinstance(va, dict) and isinstance(vb, dict))):
            d = _json_diff(va, vb, limit)
            if d is not None:
                return {"差异": d}
    if len(a) > 120 or len(b) > 120:
        return _text_diff(a, b)
    return {"旧": brief(a, 120), "新": brief(b, 120)}


# ─── 列比较（校对用）───────────────────────────────────────────

def _as_json(value):
    if value is None:
        return None
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return value


def same(col, a, b, json_cols=(), float_cols=(), int_cols=()):
    """按列类型比较库内值与重算值。列类型集合由各表传入（有的表一个浮点列都没有）。

    JSON 列按解析后的结构比，浮点列给小容差，整型列按 int 比，其余按去空白文本比。
    """
    if col in json_cols:
        return _as_json(a) == _as_json(b)
    if a is None or b is None:
        return a is None and b is None
    if col in float_cols:
        try:
            return abs(float(a) - float(b)) < 1e-9
        except (TypeError, ValueError):
            return str(a) == str(b)
    if col in int_cols:
        try:
            return int(a) == int(b)
        except (TypeError, ValueError):
            return str(a) == str(b)
    return str(a).strip() == str(b).strip()


# ─── PRTS 抓取 ───────────────────────────────────────────────

class NoPage(Exception):
    """目标页面不存在（HTTP 404）。调用方通常把它当「跳过并记录」，而不是失败。"""


def _fetch_once(name, redirects, timeout, depth):
    if depth > 5:
        raise RuntimeError("重定向过深：%s" % name)
    encoded = urllib.parse.quote(name)
    url = "https://prts.wiki/w/%s?action=raw" % encoded
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-Terminal/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            text = resp.read().decode("utf-8")
    except urllib.error.HTTPError as e:
        if e.code == 404:
            raise NoPage(name)
        raise
    m = re.match(r'#(?:REDIRECT|重定向)\s*\[\[([^\]]+)\]\]', text)
    if m:
        target = m.group(1)
        if redirects is not None:
            redirects.append((name, target))
        return _fetch_once(target.replace(" ", "_"), redirects, timeout, depth + 1)
    return text


def _fetch_via_api(name, timeout=FETCH_TIMEOUT):
    """备路：走 api.php 取 wikitext。

    PRTS 有反爬盾：部分 `?action=raw` 请求会被 302 到 sisyphus.prts.wiki（跟着跳会成环），
    而 api.php 不受影响。两条路径拿到的是同一份 wikitext。
    """
    params = {"action": "query", "prop": "revisions", "rvprop": "content",
              "rvslots": "main", "format": "json", "titles": name, "redirects": "1"}
    url = PRTS_API + "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": PRTS_UA})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    for pg in data.get("query", {}).get("pages", {}).values():
        if "revisions" not in pg:
            raise NoPage(name)
        return pg["revisions"][0]["slots"]["main"]["*"]
    raise RuntimeError("api.php 未返回页面：%s" % name)


def fetch_wikitext(name, redirects=None, retries=FETCH_RETRIES, timeout=FETCH_TIMEOUT):
    """取 PRTS 页面原始 wikitext，自动跟随重定向。

    网络类异常按 retries 重试（每次间隔 1 秒）；raw 路径全失败后改走 api.php；
    两者都失败才抛 RuntimeError（调用方据此中止）。404 抛 NoPage。
    redirects 传入列表时，每次重定向追加 (原名, 目标名)。
    """
    last = None
    for attempt in range(retries + 1):
        try:
            return _fetch_once(name, redirects, timeout, 0)
        except NoPage:
            raise
        except Exception as e:  # noqa: BLE001
            last = e
            if attempt < retries:
                time.sleep(1.0)
    try:
        return _fetch_via_api(name, timeout)
    except NoPage:
        raise
    except Exception as e:  # noqa: BLE001
        raise RuntimeError("PRTS 抓取失败（%s）：raw=%s；api=%s" % (name, last, e))


def parse_template(text, tpl_name):
    """从 wikitext 里提取指定模板的全部实例，每个实例返回一个参数字典。

    处理嵌套 {{}}，按行首 `|` 切分参数，形如 {{模板名 |键=值 |键=值}}。
    模板名与参数含义由调用方解释，本函数只管取。
    """
    results = []
    start_tag = "{{" + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break
        depth, i, end = 0, idx, None
        while i < len(text):
            if text[i:i + 2] == "{{":
                depth += 1
                i += 2
                continue
            if text[i:i + 2] == "}}":
                depth -= 1
                if depth == 0:
                    end = i + 2
                    break
                i += 2
                continue
            i += 1
        if end is None:
            break
        content = text[idx + len(start_tag):end - 2]
        params = {}
        parts = re.split(r'\n\|', content)
        first = parts[0].strip()
        if first.startswith("|"):
            first = first[1:]
        if "=" in first:
            k, v = first.split("=", 1)
            params[k.strip()] = v.strip()
        for part in parts[1:]:
            part = part.strip()
            if "=" not in part:
                continue
            k, v = part.split("=", 1)
            params[k.strip()] = v.strip()
        results.append(params)
        pos = end
    return results


def sentinel_ok(sample_name, marker, retries=FETCH_RETRIES):
    """结构哨兵：抓一个已知页面，确认关键标记仍在。返回 (ok, 说明)。

    marker 由各表给（enemies 是 {{敌人信息/levelcontent}}，stages 是关卡模板名）。
    """
    try:
        text = fetch_wikitext(sample_name, retries=retries)
    except NoPage:
        return False, "哨兵页面不存在：%s" % sample_name
    except Exception as e:
        return False, "哨兵页面抓取失败：%s" % e
    if marker not in text:
        return False, "哨兵页面 %r 中未见 %r，疑似 PRTS 结构变化" % (sample_name, marker)
    return True, "ok"


# ─── PRTS 页面解析与取值（「页面文本型」表共用：term / abnormal_effects 等）────
#
# 为什么需要这套：这些表的值是页面正文，模板参数**按位置**给
# （`{{术语释义|停顿|移动速度降低80%}}`），而本模块的 parse_template 按行首 `|`
# 切键值对，读不了单行位置参数；且参数里嵌着别的模板与链接，非贪婪正则会在内层 `}}`
# 处咬断。这里一律用深度计数配平。

PRTS_API = "https://prts.wiki/api.php"
PRTS_UA = "PRTS-terminal-maintenance/1.0 (read-only)"
_PAGE_CACHE = {}


def api(params, retries=3):
    """只读 API 调用（带重试与 maxlag），返回解析后的 JSON。

    供需要分页/多次查询的脚本使用（如 slang 的全站重定向扫描）；
    单页取文本用 fetch_page。
    """
    params = dict(params)
    params.setdefault("format", "json")
    params.setdefault("maxlag", "5")
    url = PRTS_API + "?" + urllib.parse.urlencode(params)
    last = None
    for i in range(retries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": PRTS_UA})
            with urllib.request.urlopen(req, timeout=FETCH_TIMEOUT) as r:
                data = json.loads(r.read().decode("utf-8"))
            if "error" in data:
                if data["error"].get("code") == "maxlag":
                    time.sleep(5)
                    continue
                raise RuntimeError(str(data["error"]))
            return data
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(1.5 * (i + 1))
    raise RuntimeError("API 请求失败：%s（%s）" % (url, last))


def fetch_page(title, retries=FETCH_RETRIES):
    """取页面 wikitext，一次运行内缓存。抓取口径沿用本模块 fetch_wikitext。"""
    if title not in _PAGE_CACHE:
        _PAGE_CACHE[title] = fetch_wikitext(title, retries=retries)
    return _PAGE_CACHE[title]


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
    """去掉媒体链接、链接与样式，粗略取显示文字（供匹配用）。"""
    # 媒体链接先删：[[文件:x.png|25px]] 抠不出名字，其“显示名”是 25px 不是条目名
    t = re.sub(r"\[\[(?:文件|图像|影音|File|Image):[^\]]*\]\]", "", text, flags=re.IGNORECASE)
    t = re.sub(r"\[\[(?:[^\]|]*\|)?([^\]|]*)\]\]", r"\1", t)
    t = re.sub(r"\{\{[^{}]*\|([^{}|]*)\}\}", r"\1", t)
    t = re.sub(r"\{\{[^{}]*\}\}", "", t)
    t = re.sub(r"<[^>]+>", "", t)
    t = t.replace("'''", "").replace("''", "")
    return t.replace("\u00a0", " ").strip()


def clean_value(raw, extra_rules=()):
    """页面参数 → 库内文本（页面文本型表共用）。

    只做两类处理：
      1. 去掉页面渲染不出来的东西：注释、媒体链接、`<code>` 里的内部 ID；
         以及被**删除线**标注为失效的内容（`<del>`/`<s>`/`<strike>`——页面用删除线
         表示“这段已作废”，只留文字不留线就是误导）。
      2. 其余交给 cleaning_wiki（`<br>` → 换行在那里做）。
    页面渲染成文字的字符（`※`、`·`、`=`、`*`）一律保留。
    """
    import os
    import sys
    here = os.path.dirname(os.path.abspath(__file__))
    if here not in sys.path:
        sys.path.insert(0, here)
    from cleaning_wiki import clean_wiki, looks_dirty  # noqa: E402

    t = re.sub(r"</?code(\s[^>]*)?>", "", raw)
    t = re.sub(r"<(?:del|s|strike)(?:\s[^>]*)?>.*?</(?:del|s|strike)>", "", t, flags=re.S)
    t = re.sub(r"</?(?:del|s|strike)(?:\s[^>]*)?>", "", t)      # 未闭合的残留标签
    t = clean_wiki(t, extra_rules)
    t = re.sub(r"[ \t]+\n", "\n", t)
    t = re.sub(r"\n{2,}", "\n", t)
    t = t.strip()
    # 硬门：还有未处理标记就不放行（页面一变就抦在这里，而不是把残渣写进库）
    if looks_dirty(t):
        raise RuntimeError("清洗后仍有未处理标记，请补规则：%r" % t[:120])
    return t
