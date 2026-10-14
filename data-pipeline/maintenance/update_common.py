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
  - 更新记录：<maintenance>/updates/<时间戳>-<表>.json
  - 写库前备份：<maintenance>/backups/<库名>.<时间戳>.bak，保留最近 KEEP_BACKUPS 份
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

def backup_db(db_path, keep=KEEP_BACKUPS, backup_dir=None):
    """把库备份到 backup_dir（默认 <maintenance>/backups），只保留最近 keep 份。

    返回备份文件路径；库不存在时返回 None。
    """
    if not os.path.exists(db_path):
        return None
    if backup_dir is None:
        backup_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "backups")
    os.makedirs(backup_dir, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    dst = os.path.join(backup_dir, "%s.%s.bak" % (os.path.basename(db_path), stamp))
    with open(db_path, "rb") as src, open(dst, "wb") as out:
        shutil.copyfileobj(src, out, 1024 * 1024)
    backups = sorted(f for f in os.listdir(backup_dir) if f.endswith(".bak"))
    if len(backups) > keep:
        for stale in backups[:-keep]:
            try:
                os.remove(os.path.join(backup_dir, stale))
            except OSError:
                pass
    return dst


def write_record(table, record, out_dir=None):
    """把更新记录写到 <maintenance>/updates/<时间戳>-<表>.json，返回路径。"""
    if out_dir is None:
        out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "updates")
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


def fetch_wikitext(name, redirects=None, retries=FETCH_RETRIES, timeout=FETCH_TIMEOUT):
    """取 PRTS 页面原始 wikitext，自动跟随重定向。

    网络类异常按 retries 重试（每次间隔 1 秒），仍失败抛 RuntimeError（调用方据此中止）；
    404 抛 NoPage。redirects 传入列表时，每次重定向追加 (原名, 目标名)。
    """
    last = None
    for attempt in range(retries + 1):
        try:
            return _fetch_once(name, redirects, timeout, 0)
        except NoPage:
            raise
        except Exception as e:
            last = e
            if attempt < retries:
                time.sleep(1.0)
    raise RuntimeError("PRTS 抓取失败（%s，重试 %d 次）：%s" % (name, retries, last))


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
