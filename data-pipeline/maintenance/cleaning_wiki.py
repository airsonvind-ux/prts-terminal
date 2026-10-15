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
  - 【{{x|y}}】先于模板类规则：否则 `{{x|y}}` 先被取走，留下孤零零的【y】；
  - ruby 先于 popup / 简易折叠：这些容器里可能嵌着 ruby，先解掉免得括号被提前吃掉。

**加括号的约定**：只有「补充说明」性质的模板加全角括号——`popup`（悬停提示）、
  `简易折叠`（折叠内容）。替代性质的模板（color/font/术语/异常效果…）渲染出来就是
  那个词本身，不加括号。

补全记录（2026-10-13，按库内实际残留统计）：通用注释、popup(347)、HTML 标签(22)、
简易折叠、异常效果多参数形态、变动数值无箭头形态、#widget:Range、媒体链接。

再次补全（2026-10-13，为 stages.情报 与 enemies 口径对齐）：模糊、资源概览、ruby、hr。
此前情报里有 104 行 <hr>、16 行资源概览、10 行模糊、5 行 ruby 无规则可洗。

修正（2026-10-14，Air 报告情报残留后查出的切割伤）：popup / 简易折叠 / 模糊 三条容器
规则原来用非贪婪匹配，内容里嵌着别的模板时会在内层第一个 `}}` 处切断外层，
在正文里留下 `{{异常效果…}}` 这类残骸（库里已有 5 个值这样坏掉，stages.情报还有
12 处同形态尚未洗）。改为容忍一层嵌套的匹配。**已经切坏的值清洗救不回，只能重爬。**

补规则（2026-10-15，Air 报告 装置机制 首行变 <nowiki/>）：合并时漏了 MediaWiki 的
`<nowiki/>` / `<nowiki>` / `</nowiki>` 标记——旧的私有 cleaner（build_traps_prts /
build_tokens_prts）有这条，新模块没有。装置页习惯把 `<nowiki/>` 放在多行参数
（如 `装置机制`）开头，于是被原样写进了库。现在补到基础规则里（紧跟注释之后）。
库里已有的那批由 `db_builder/repair_nowiki.py` 一次性清掉。

黑流树海（2026-10-15）开工前对照旧 `build_rogue_*.strip()` 后补的三处：
* `锚点`（{{锚点|名称}}）——不渲染内容，漏掉就会写进库；
* `修正`（非 lite）——旧规则只认 lite，乌托邦幸福论用的是四参数形态；
* `''斜体''` —— 页面上是排版强调（备注里的引语行），去标记即可；
* `术语` 的显示文本改为停在 `|` 前（四参数形态 `{{术语|key|物理|术语=…}}` 会多带一段）。

数值变化缩写形（2026-10-15，Air 定口径）：`{{*|<数值>|<原文>}}` / `{{+|<数值>|<原文>}}`
页面上默认显示第一个参数，但“移动速度50”“攻击速度50”读不通，所以**取第二个参数（原文）**，
只有单参数时才用数值。同时补 `{{popup|文本}}`（不带 内容= 的简写）→ `（文本）`。
"""

import re

# ─── 基础规则 ─────────────────────────────────────────────────

BASE_RULES = [
    # 注释：一律删除（含 JSON 注释块与 spell_on / spell_end 之类标记）
    ("注释", re.compile(r'<!--[\s\S]*?-->'), lambda m: ''),

    # <nowiki/> / <nowiki> / </nowiki>：MediaWiki 的「原样显示」标记，页面上不渲染任何东西。
    #   页面常把 <nowiki/> 放在多行参数（如 装置机制）的开头；漏掉就会当作正文写进库。
    ("nowiki", re.compile(r'</?nowiki\s*/?>', re.IGNORECASE), lambda m: ''),

    # {{锚点|名称}}：页面内的定位锚，不渲染任何内容，一律删除。
    #   （黑流树海的黑流数据库用它标每个乌托邦条目，漏掉会当正文写进库）
    ("锚点", re.compile(r'\{\{锚点\|[^}]*\}\}'), lambda m: ''),

    # 媒体链接：[[文件:xxx.png|缩略图|<center>名字</center>]] → 删除
    # （页名是文件名，后面是图片参数，都不是文本内容）
    ("媒体链接", re.compile(r'\[\[(?:文件|图像|影音|File|Image):[^\]]*\]\]', re.IGNORECASE),
     lambda m: ''),

    # 链接：[[页名|显示名]] → 显示名；[[页名]] → 页名
    ("链接", re.compile(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]'),
     lambda m: m.group(2) or m.group(1)),

    # {{color|#xxx|text}} → text
    ("color", re.compile(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}'), lambda m: m.group(1)),

    # {{ruby|汉字|注音}} → 汉字（注音）
    #   （页面上注音是小字，读者同时看到两者；先于 popup / 简易折叠，免得被容器规则提前吃掉）
    ("ruby", re.compile(r'\{\{ruby\|([^|}]+)\|([^|}]+?)\}\}'),
     lambda m: m.group(1) + '（' + m.group(2) + '）'),

    # 攻击范围：{{popup/range|code}} / {{#widget:Range/code}} → [攻击范围: code]
    #   （页面渲染的是图形，必须把 code 保住）
    ("popup/range", re.compile(r'\{\{popup/range\|([^}]+?)\}\}'),
     lambda m: '[攻击范围: ' + m.group(1) + ']'),
    ("#widget:Range", re.compile(r'\{\{#widget:Range/([^}]+?)\}\}'),
     lambda m: '[攻击范围: ' + m.group(1) + ']'),

    # 补充说明类：取内容并加全角括号。
    #   内容里可能嵌着别的模板（{{popup|…|内容=…{{特殊机制|…}}…}}）。非贪婪匹配会在
    #   内层的第一个 }} 处切断、把外层切开，在正文里留下残骸；下面容忍一层嵌套
    #   （内层模板自身不含花括号）。嵌套更像深的情形需再放宽，但尚未出现。
    ("popup", re.compile(r'\{\{\s*[Pp]opup\s*\|(?:[^{}]|\{\{[^{}]*?\}\})*?内容='
                          r'((?:[^{}]|\{\{[^{}]*?\}\})*?)\}\}'),
     lambda m: '（' + m.group(1) + '）'),
    ("简易折叠", re.compile(r'\{\{简易折叠\|(?:[^{}]|\{\{[^{}]*?\}\})*?内容='
                            r'((?:[^{}]|\{\{[^{}]*?\}\})*?)\}\}'),
     lambda m: '（' + m.group(1) + '）'),

    # {{模糊|内容}} → 内容（wiki 上的“糊住”文本，内容即信息，不加括号）；同样容忍一层嵌套
    ("模糊", re.compile(r'\{\{模糊\|((?:[^{}]|\{\{[^{}]*?\}\})*?)\}\}'), lambda m: m.group(1)),

    # 异常效果：{{异常效果|…|显示名称=X}} → X；{{异常效果|name|flag|…}} → name+flag
    ("异常效果-显示名称", re.compile(r'\{\{异常效果\|[^|]*\|[^|]*\|显示名称=([^}]+?)\}\}'),
     lambda m: m.group(1)),
    ("异常效果", re.compile(r'\{\{异常效果\|([^|}]*)(?:\|([^|}]*))?[^}]*\}\}'),
     lambda m: (m.group(1) or '') + (m.group(2) or '')),

    # {{术语|key|display}} → display；{{术语/custom|key|显示}} → 显示
    #   四参数形态 {{术语|key|物理|术语=物理脆弱}}：显示文本停在 | 前，尾部的 |… 一起吃掉
    ("术语", re.compile(r'\{\{术语\|[^|}]+?\|([^}|]+?)(?:\|[^}]*)?\}\}'), lambda m: m.group(1)),
    ("术语/custom", re.compile(r'\{\{术语/custom\|[^|]+\|([^}|]+)\}\}'), lambda m: m.group(1)),

    # {{变动数值|value|-}} → value↓ ；{{变动数值|value|+}} → value↑ ；无箭头形态取文本
    ("变动数值-箭头", re.compile(r'\{\{变动数值\|([^|}]+?)\|([+-])\}\}'),
     lambda m: m.group(1) + ('↓' if m.group(2) == '-' else '↑')),
    ("变动数值", re.compile(r'\{\{变动数值\|([^{}]+?)\}\}'), lambda m: m.group(1)),

    # lite 形态（页面上与普通形态渲染相同）：{{变动数值lite/2|文本}} / {{修正lite|文本|原因=…}} → 文本
    ("变动数值lite", re.compile(r'\{\{变动数值lite(?:/\d+)?\|((?:[^{}]|\{\{[^{}]*?\}\})*?)\}\}'),
     lambda m: m.group(1)),
    ("修正lite", re.compile(r'\{\{修正lite\|([^|}]+)[^}]*\}\}'), lambda m: m.group(1)),

    # {{修正|文本|原因=…|name=…}} → 文本（非 lite 形态；黑流树海的乌托邦幸福论在用它）
    #   注意：紧接 | 才算，所以不会误吃 修正lite；文本要用贪婪匹配，
    #   惰性 + [^}]* 尾巴只会捕到一个字。
    ("修正", re.compile(r'\{\{修正\|([^|}]+)[^}]*\}\}'), lambda m: m.group(1)),

    # {{*|<数值>|<原文>}} → 原文（直接乘算标记）
    #   页面上默认显示第一个参数，但“移动速度50”“攻击速度50”读不通，
    #   Air 统一取第二个参数（原文）；只有单参数时才用数值。
    ("乘算", re.compile(r'\{\{\*\|([^|}]+)\|([^|}]+)(?:\|[^}]*)?\}\}'), lambda m: m.group(2)),
    ("乘算-单参", re.compile(r'\{\{\*\|([^|}]+?)(?:\|[^}]*)?\}\}'), lambda m: m.group(1)),

    # {{+|<数值>|<原文>}} → 原文（直接加算标记，同上）
    ("加算", re.compile(r'\{\{\+\|([^|}]+)\|([^|}]+)(?:\|[^}]*)?\}\}'), lambda m: m.group(2)),
    ("加算-单参", re.compile(r'\{\{\+\|([^|}]+?)(?:\|[^}]*)?\}\}'), lambda m: m.group(1)),

    # {{特殊机制|name}} → name
    ("特殊机制", re.compile(r'\{\{特殊机制\|([^}]+?)\}\}'), lambda m: m.group(1)),

    # {{资源概览|名称|数量[|分类]}} → 名称×数量
    ("资源概览", re.compile(r'\{\{资源概览\|([^|}]+)\|([^|}]+)(?:\|[^}]*)?\}\}'),
     lambda m: m.group(1) + '×' + m.group(2)),

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

    # {{popup|文本}}（不带 内容= 的简写形态）→ (文本)
    #   必须排在上面的 popup 规则之后，免得把带 内容= / 颜色= 的形态吃掉
    ("popup-简写", re.compile(r'\{\{\s*[Pp]opup\s*\|([^}|=]+?)\s*\}\}'), lambda m: '（%s）' % m.group(1)),

    # <br> → 换行
    ("br", re.compile(r'<br\s*/?>'), lambda m: '\n'),

    # <hr> → 换行（页面上的分隔线，无信息）
    ("hr", re.compile(r'<hr\s*/?>'), lambda m: '\n'),

    # '''粗体''' → 去标记
    ("粗体", re.compile(r"'''([^']*)'''"), lambda m: m.group(1)),

    # ''斜体'' → 去标记（页面上用来排版强调，如乌托邦条目标题下那行引语）
    ("斜体", re.compile(r"''([^']*)''"), lambda m: m.group(1)),

    # HTML 标签删除（白名单，避免误伤游戏文本里的 <@xx> / <$xx>）
    # HTML 标签删除（白名单，避免误伤游戏文本里的 <@xx> / <$xx> / <替身> 这类尖括号中文）
    #   color 是游戏数据自身的富文本标签（<color=#20b4a1>…</color>），b/i 是残留的 HTML 强调
    ("html标签", re.compile(
        r'</?(?:span|div|sup|sub|small|big|center|font|table|tr|td|th|ruby|rt|rb|p|li|ul|ol'
        r'|color|b|i)\b[^>]*>',
        re.IGNORECASE), lambda m: ''),
]

# 用于定位「可能没洗干净」的文本（各脚本可用来做残留检查）
DIRTY_MARKERS = ['<!--', '{{', '【{{', '[[', "'''", '<div', '<span', '<center', '<@', '<$',
                '<nowiki']

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
