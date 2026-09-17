"""
blackboard 占位符替换工具
将技能描述中的 {key:format} 替换为 blackboard 中的格式化数值

取整规则：四舍六入五成双（Python round() 默认行为）
{key} 无格式时：若值为整数（value == int(value)），显示整数形式

用法:
    from blackboard import replace
    result, unused = replace(desc_text, blackboard_dict)
"""

import re
from text_cleaner import clean_desc

# 匹配 {key:format} 或 {key}，可带负号前缀 {-key:format}
_RE_PLACEHOLDER = re.compile(r'\{(-?)([^}:]+)(?::([^}]+))?\}')


def format_value(value, fmt):
    """按格式说明符格式化单个数值。

    格式说明符规则：
        "0%"    → ×100, 四舍六入五成双取整, 追加 %
        "0.0%"  → ×100, 四舍六入五成双保留 1 位小数, 追加 %
        "0"     → 四舍六入五成双取整
        "0.0"   → 四舍六入五成双保留 1 位小数
        None    → 判断是否为整数（value == int(value)），是则 int 显示
    """
    if fmt is None:
        # 无格式：判断是否为整数 → 显示整数；否则原值
        if isinstance(value, float) and value == int(value):
            return str(int(value))
        return str(value)

    # 记录是否为百分比格式
    add_pct = False
    if fmt.endswith('%'):
        value = value * 100
        fmt = fmt[:-1]
        add_pct = True

    # 取整
    if fmt == '0':
        rounded = round(value)
        if rounded == int(rounded):
            result = str(int(rounded))
        else:
            result = str(rounded)

    # 保留 1 位小数
    elif fmt == '0.0':
        result = f'{round(value, 1):.1f}'

    # 未知格式：兜底
    else:
        result = str(value)

    if add_pct:
        result += '%'
    return result


def replace(text, blackboard):
    """替换描述文本中的占位符。

    Args:
        text: 原始描述文本（含 <@ba.xxx> 标签和 {key:fmt} 占位符）
        blackboard: dict，如 {"atk": 0.1, "cost": 6.0}

    Returns:
        (替换后纯文本, 未使用 blackboard key 列表)
    """
    if not text:
        return text, []

    # 第一步：剥离富文本标签
    text = clean_desc(text)

    # 第二步：替换占位符
    used_keys = set()

    def replacer(m):
        sign = m.group(1)   # '-' 或 ''（仅用于剥离前缀，不影响数值）
        key  = m.group(2)   # 实际 blackboard key
        fmt  = m.group(3)   # None if no ":"
        if key in blackboard:
            used_keys.add(key)
            return format_value(blackboard[key], fmt)
        return m.group(0)

    result = _RE_PLACEHOLDER.sub(replacer, text)

    # 收集 blackboard 中有但未被占位符引用的 key
    unused = [k for k in blackboard if k not in used_keys]

    return result, unused
