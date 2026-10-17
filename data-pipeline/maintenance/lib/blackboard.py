# -*- coding: utf-8 -*-
"""占位符替换：把技能/模组描述里的 {key:格式} 换成 blackboard 里的实际数值。

来源：合并 `db_builder/blackboard.py` 与 `db_builder/refine_operators.py` 的
`replace_enhanced`（db_builder/ 不进发布，维护脚本必须自包含）。

与旧 `blackboard.replace` 的三处差别，都是 refine 阶段修出来的：
  1. **负号取反**：`{-key:0%}` 要取负值。旧版把 `-` 当噪声剥掉、不取值，
     等于把「-30%」写成「30%」，refine_operators 才补上；
  2. **key 大小写不敏感**（源里出现过 `{ATK}` 这种写法）；
  3. **`{duration}` 兜底**：blackboard 里没有 duration 时，用本级的时长数据填。

取整规则沿用旧版：四舍六入五成双（Python `round`）。

用法：
    from blackboard import replace, has_residue
    text, unused = replace("{atk:0%} 的攻击力", {"atk": 0.3})
"""

import re

from cleaning_game import clean_desc

# {key} / {key:格式} / {-key:格式}
_RE_PLACEHOLDER = re.compile(r'\{(-?)([^}:]+)(?::([^}]+))?\}')

# 没解析掉的占位符、或残留的孤立标签，用于「洗干净了吗」的检查
_RE_RESIDUE = re.compile(r'\{[^{}]*\}|</>')


def format_value(value, fmt):
    """按格式说明符格式化单个数值。

    "0%"   → ×100 后四舍六入五成双取整，追加 %
    "0.0%" → ×100 后保留 1 位小数，追加 %
    "0"    → 四舍六入五成双取整
    "0.0"  → 四舍六入五成双保留 1 位小数
    None   → 整数就显示整数，否则原值
    """
    if fmt is None:
        if isinstance(value, float) and value == int(value):
            return str(int(value))
        return str(value)

    add_pct = False
    if fmt.endswith('%'):
        value = value * 100
        fmt = fmt[:-1]
        add_pct = True

    if fmt == '0':
        rounded = round(value)
        result = str(int(rounded)) if rounded == int(rounded) else str(rounded)
    elif fmt == '0.0':
        result = f'{round(value, 1):.1f}'
    else:
        result = str(value)

    return result + '%' if add_pct else result


def replace(text, blackboard, duration=None):
    """替换描述文本里的占位符。

    返回 (替换后文本, 未被引用的 blackboard 键)。解析不到的占位符原样留下，
    调用方可用 has_residue() 判断是否还有残留。

    blackboard 用「小写键」做匹配，但返回值里保留原始键名，便于统计未使用键。
    """
    if not text:
        return text, []

    text = clean_desc(text)
    bb_lower = {}
    for k, v in (blackboard or {}).items():
        bb_lower.setdefault(k.lower(), (k, v))

    used = set()

    def repl(m):
        sign, key, fmt = m.group(1), m.group(2), m.group(3)
        kk = key.lower()
        if kk in bb_lower:
            orig, val = bb_lower[kk]
            used.add(orig)
        elif kk == 'duration' and duration is not None:
            val = duration
        else:
            return m.group(0)
        if sign == '-':
            val = -val
        return format_value(val, fmt)

    out = _RE_PLACEHOLDER.sub(repl, text)
    unused = [k for k in (blackboard or {}) if k not in used]
    return out, unused


def has_residue(text):
    """文本里是否还有未解析的占位符或孤立标签。"""
    return bool(text) and bool(_RE_RESIDUE.search(text))
