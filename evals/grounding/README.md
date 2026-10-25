# 接地评测

这组文件用来回答一个很具体的问题：

> 一轮回答里，PRTS 到底有没有先查、引用是否存在、压缩后证据还在不在？

它不是模型排行榜，也不判断文风。它读的是一段已经录制好的 PRTS 会话 JSONL，检查工具调用、工具结果、最终回答和压缩摘要之间的对应关系。

默认不联网、不调用模型、不需要 API Key。

## 现在能做什么

- 用 JSONL 描述“问题、应调用的工具、应出现的证据词、回答应包含的词”。
- 用 `run.py` 对一份 PRTS 会话日志做离线检查。
- 用固定的 fixture 验证评测脚本本身，不依赖真实数据库或网络。
- 失败时给出具体原因，而不是只给一个分数。

## 目录

```text
evals/grounding/
├─ README.md
├─ cases.jsonl                 评测用例
├─ run.py                      离线评测器
└─ fixtures/
   └─ sample-session.jsonl     脚本自测用的会话样例
```

## 用例格式

`cases.jsonl` 每行一个 JSON 对象：

```json
{
  "id": "enemy-defense",
  "question": "源石虫的防御力是多少？",
  "required_tools": ["query_enemy"],
  "evidence_terms": ["防御力"],
  "answer_terms": ["防御力"],
  "compression_terms": [],
  "forbidden_answer_terms": [],
  "notes": "为什么需要这条用例"
}
```

字段含义：

| 字段 | 必填 | 作用 |
|---|---|---|
| `id` | 是 | 稳定标识，重复会报错 |
| `question` | 是 | 用来在会话里定位对应轮次 |
| `required_tools` | 否 | 这一轮必须出现的工具名 |
| `evidence_terms` | 否 | 必须出现在至少一条工具结果里的词 |
| `answer_terms` | 否 | 最终回答必须包含的词 |
| `compression_terms` | 否 | 压缩摘要必须保留的词或工具名 |
| `forbidden_answer_terms` | 否 | 最终回答不应出现的词 |
| `notes` | 否 | 用例意图，给人看 |

## 怎么跑

先检查用例格式：

```powershell
python evals/grounding/run.py --check
```

查看用例列表：

```powershell
python evals/grounding/run.py --list
```

对一份会话日志运行全部用例：

```powershell
python evals/grounding/run.py --session <会话文件.jsonl>
```

只跑某一条：

```powershell
python evals/grounding/run.py --session <会话文件.jsonl> --case enemy-defense
```

指定轮次：

```powershell
python evals/grounding/run.py --session <会话文件.jsonl> --turn 2
```

也可以使用 PowerShell 包装：

```powershell
.\scripts\grounding-eval.ps1 -Check
.\scripts\grounding-eval.ps1 -Session <会话文件.jsonl>
.\scripts\grounding-eval.ps1 -Session <会话文件.jsonl> -Case enemy-defense
```

## 会话文件从哪里来

PRTS 的会话日志在用户数据目录的 `sessions/` 下，文件名是 `{conversationId}.jsonl`。首行是 header，后续每行是一轮的 event 数组。

可以直接把一份会话文件交给脚本。不要把包含真实对话、文件路径或个人信息的生产会话直接提交进仓库。需要共享时，先脱敏，或者用 `fixtures/` 里的合成样例。

## 判定逻辑

一条用例通过，表示：

- `required_tools` 里的工具都出现过；
- 工具调用发生在最终回答之前；
- `evidence_terms` 都能在工具结果里找到；
- `answer_terms` 都能在最终回答里找到；
- `forbidden_answer_terms` 没有出现；
- `compression_terms` 在压缩摘要里找得到。

它不判断：

- 回答是否漂亮；
- 模型是否“理解”了机制；
- 工具结果是否在语义上回答了问题；
- 真实模型在未录制问题上的表现。

这些需要后续接入真实 provider 或更多人工标注，不应由这个脚本假装完成。

## 失败分类

脚本只报事实，但失败通常落在这几类：

| 现象 | 通常意味着 |
|---|---|
| `missing tools` | 模型没查，或工具选择错了 |
| `final answer appears before the required tool call` | 先答后查，或回答没有依据 |
| `evidence not found in tool results` | 工具结果不对，或用例锚点写错了 |
| `answer missing terms` | 回答没有引用查到的证据 |
| `compaction summary missing terms` | 压缩丢了关键锚点 |

## 和现有测试的关系

- `cargo test --lib` 保护代码契约。
- `scripts/data-contract.ps1` 保护真实数据库契约。
- 这里的评测保护“录制会话里的证据链”，不替代前两者。

它是一个开发协作框架，不是发布门禁。等用例和失败分类稳定后，再考虑接入 CI 或真实 provider。
