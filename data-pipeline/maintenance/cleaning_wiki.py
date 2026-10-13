# -*- coding: utf-8 -*-
"""PRTS wikitext 清洗：基础规则集 + 每表可追加的额外规则。

来源：合并 `db_builder/build_enemies_prts.py` 的 `clean_wiki` 与
      `db_builder/refine_enemies.py` 的 8 条规则，并按库内实际残留补全
      （db_builder/ 不进发布，此处自包含）。

设计：规则 = (名称, 编译后的正则, 替换函数) 三元组，按列表顺序应用。
      clean_wiki(text, extra_rules=[]) 先跑基础规则、再跑额外规则，最后压缩空行。
      各表的清洗差异用 extra_rules 表达，不改基础层。

**清洗目标**：让文本等于「PRTS 页面上读者真正看到的自然语言」——还原标记与模板，
保住战术信息，不引入外部知识、不做语义改写。四条判据：
  1. 读者看不到的 → 删（注释、纯装饰图标、媒体链接、样式标签）；
  2. 页面渲染成文字的 → 取那段文字（模板取内容、链接取显示名）；
  3. 页面渲染成别的形式的 → 换成等价写法（攻击范围图 → [攻击范围: x]，变动数值 + → ↑）；
  4. 有信息就留住，宁可换符号也不丢（这是第 3 条的意义）。

**顺序有语义**（不可随意重排）：
  - 注释最先删：`<!-- -->` 里可能装着任何东西，先清掉免得干扰后续规则；
  - 媒体链接（[[文件:…]]）先于普通链接：否则会被当成"显示名"取出图片参数；
  - 攻击范围（popup/range、#widget:Range）先于 popup：popup 内容里可能嵌着 widget；
  - 【{{x|y}}】先于模板类规则：否则 `{{x|y}}` 先被取走，留下孤零零的【y】。

**加括号的约定**：只有「补充说明」性质的模板加全角括号——`popup`（悬停提示）、
  `简易折叠`（折叠内容）。替代性质的模板（color/font/术语/异常效果…）渲染出来就是
  那个词本身，不加括号。

补全记录（2026-10-13，按库内实际残留统计）：通用注释、popup(347)、HTML 标签(22)、
简易折叠、异常效果多参数形态、变动数值无箭头形态、#widget:Range、媒体链接。
"""

import re

# ─── 基础规则 ─────────────────────────────────────────────────

BASE_RULES = [
    # 注释：一律删除（含 JSON 注释块与 spell_on / spell_end 之类标记）
    ("注释", re.compile(r'<!--[\s\S]*?-->'), lambda m: ''),

    # 媒体链接：[[文件:xxx.png|缩略图|<center>名字</center>]] → 删除
    # （页名是文件名，后面是图片参数，都不是文本内容）
    ("媒体链接", re.compile(r'\[\[(?:文件|图像|影音|File|Image):[^\]]*\]\]', re.IGNORECASE),
     lambda m: ''),

    # 链接：[[页名|显示名]] → 显示名；[[页名]] → 页名
    ("链接", re.compile(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]'),
     lambda m: m.group(2) or m.group(1)),

    # {{color|#xxx|text}} → text
    ("color", re.compile(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}'), lambda m: m.group(1)),

    # 攻击范围：{{popup/range|code}} / {{#widget:Range/code}} → [攻击范围: code]
    #   （页面渲染的是图形，必须把 code 保住）
    ("popup/range", re.compile(r'\{\{popup/range\|([^}]+?)\}\}'),
     lambda m: '[攻击范围: ' + m.group(1) + ']'),
    ("#widget:Range", re.compile(r'\{\{#widget:Range/([^}]+?)\}\}'),
     lambda m: '[攻击范围: ' + m.group(1) + ']'),

    # 补充说明类：取内容并加全角括号
    ("popup", re.compile(r'\{\{\s*[Pp]opup\s*\|[^{}]*?内容=([\s\S]*?)\}\}'),
     lambda m: '（' + m.group(1) + '）'),
    ("简易折叠", re.compile(r'\{\{简易折叠\|[^{}]*?内容=([\s\S]*?)\}\}'),
     lambda m: '（' + m.group(1) + '）'),

    # 异常效果：{{异常效果|…|显示名称=X}} → X；{{异常效果|name|flag|…}} → name+flag
    ("异常效果-显示名称", re.compile(r'\{\{异常效果\|[^|]*\|[^|]*\|显示名称=([^}]+?)\}\}'),
     lambda m: m.group(1)),
    ("异常效果", re.compile(r'\{\{异常效果\|([^|}]*)(?:\|([^|}]*))?[^}]*\}\}'),
     lambda m: (m.group(1) or '') + (m.group(2) or '')),

    # {{术语|key|display}} → display；{{术语/custom|key|显示}} → 显示
    ("术语", re.compile(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}'), lambda m: m.group(1)),
    ("术语/custom", re.compile(r'\{\{术语/custom\|[^|]+\|([^}]+)\}\}'), lambda m: m.group(1)),

    # {{变动数值|value|-}} → value↓ ；{{变动数值|value|+}} → value↑ ；无箭头形态取文本
    ("变动数值-箭头", re.compile(r'\{\{变动数值\|([^|}]+?)\|([+-])\}\}'),
     lambda m: m.group(1) + ('↓' if m.group(2) == '-' else '↑')),
    ("变动数值", re.compile(r'\{\{变动数值\|([^{}]+?)\}\}'), lambda m: m.group(1)),

    # {{特殊机制|name}} → name
    ("特殊机制", re.compile(r'\{\{特殊机制\|([^}]+?)\}\}'), lambda m: m.group(1)),

    # {{污染|显示文本}} → 显示文本
    ("污染", re.compile(r'\{\{污染\|([^}]+)\}\}'), lambda m: m.group(1)),

    # {{技能类型|显示文本}} → 显示文本
    ("技能类型", re.compile(r'\{\{技能类型\|([^}]+)\}\}'), lambda m: m.group(1)),

    # {{font|文字|color=...}} → 文字
    ("font", re.compile(r'\{\{font\|([^|]+)\|[^}]+\}\}'), lambda m: m.group(1)),

    # {{敌人头像|名称|px=...}} → 名称
    ("敌人头像", re.compile(r'\{\{敌人头像\|([^|]+)\|[^}]+\}\}'), lambda m: m.group(1)),

    # {{mdi|图标名}} → 删除（纯装饰）
    ("mdi", re.compile(r'\{\{mdi\|[^}]+\}\}'), lambda m: ''),

    # 【{{模板名|显示文本}}】 → 显示文本
    ("方括号模板", re.compile(r'【\{\{(.+?)\|(.+?)\}\}】'), lambda m: m.group(2)),

    # <br> → 换行
    ("br", re.compile(r'<br\s*/?>'), lambda m: '\n'),

    # '''粗体''' → 去标记
    ("粗体", re.compile(r"'''([^']*)'''"), lambda m: m.group(1)),

    # HTML 标签删除（白名单，避免误伤游戏文本里的 <@xx> / <$xx>）
    ("html标签", re.compile(
        r'</?(?:span|div|sup|sub|small|big|center|font|table|tr|td|th|ruby|rt|rb|p|li|ul|ol)\b[^>]*>',
        re.IGNORECASE), lambda m: ''),
]

# 用于定位「可能没洗干净」的文本（各脚本可用来做残留检查）
DIRTY_MARKERS = ['<!--', '{{', '【{{', '[[', "'''", '<div', '<span', '<center', '<@', '<$']

_RE_EXTRA_BLANK = re.compile(r'\n{3,}')


def clean_wiki(text, extra_rules=()):
    """按基础规则 + 额外规则清洗 wikitext，返回纯文本；输入 None 返回 None。"""
    if text is None:
        return None
    for _label, pattern, replacer in BASE_RULES:
        text = pattern.sub(replacer, text)
    for rule in extra_rules:
        _label, pattern, replacer = rule
        text = pattern.sub(replacer, text)
    text = _RE_EXTRA_BLANK.sub('\n\n', text)
    return text.strip()


def looks_dirty(text):
    """文本中是否残留已知未清洗标记。"""
    if not text:
        return False
    return any(marker in text for marker in DIRTY_MARKERS)
