# -*- coding: utf-8 -*-
"""游戏内文本清洗：去除明日方舟游戏文本中的富文本标记，保留纯文本。

来源：搬运自 `db_builder/text_cleaner.py`。
（db_builder/ 不进发布，维护脚本必须自包含，所以这里保有一份。）

标记规则：
- <@xx>text</> 或 <@xx>text<>  → 保留 text
- <$xx>text</> 或 <$xx>text<>  → 保留 text
- <color=#xxx>text</color>     → 保留 text
- <@xx> 或 <$xx>              → 删除
- 孤立 </>                     → 删除
- <汉字>                        → 保留（游戏实体引用，如 <浆果虫>）

用法：
    from cleaning_game import clean_desc
    clean_desc("<color=#FF4200>危险等级</color>提高了")  # → "危险等级提高了"

注意：规则顺序有语义（先取成对内容、再清独立标记），不要重排。
"""

import re

# 成对标记：<@xx>text</>  和  <$xx>text</>
# (.*?) 允许内容中含 < 号（如 <@lv.item><震撼装置></> → 震撼装置）
_RE_PAIR_SLASH = re.compile(r'<[@$][^>]*>(.*?)</>')

# 成对标记：<@xx>text<>  和  <$xx>text<>
_RE_PAIR_EMPTY = re.compile(r'<[@$][^>]*>(.*?)<>')

# 独立标记：<@xx>  和  <$xx>
_RE_STANDALONE = re.compile(r'<[@$][^>]*>')

# HTML 风格 color 标记：<color=#FF4200>text</color>（内容可能跨行）
_RE_COLOR = re.compile(r'<color=[^>]*>(.*?)</color>', re.DOTALL)

# 成对标记提取后可能残留的孤立 </> 结尾
_RE_ORPHAN_CLOSE = re.compile(r'</>')


def clean_desc(text):
    """清洗文本中的游戏富文本标记，返回纯文本。

    保留不含 @/$ 前缀的尖括号内容（如 <浆果虫>）。
    输入 None 返回 None。
    """
    if text is None:
        return None

    text = _RE_PAIR_SLASH.sub(r'\1', text)    # <@xx>text</> → text
    text = _RE_PAIR_EMPTY.sub(r'\1', text)    # <@xx>text<>  → text
    text = _RE_STANDALONE.sub('', text)       # <@xx> / <$xx> → 删除
    text = _RE_ORPHAN_CLOSE.sub('', text)     # 孤立 </> → 删除
    text = _RE_COLOR.sub(r'\1', text)         # <color=#xxx>text</color> → text

    return text
