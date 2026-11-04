# 消息流

聊天区的显示不是「一轮 = 一个气泡 + 一张卡」，而是一串有序列的块。模型在一次回答里可能先说话、再调工具、再说一句、再调工具，显示层按到达顺序把这些片段切开，用户看到的就是它们真实的先后。

实现分两半：`src/features/chat/blocks.ts` 的纯函数 `splitTurn(events)` 负责切块，`ProcessCard` / `MessageRow` / `MessageList` 负责画。

## 块

```ts
type Block =
  | { kind: "card"; id: string; rows: Row[]; summary: CardSummary; events: SessionEvent[] }
  | { kind: "text"; id: string; content: string; transition: boolean }
  | { kind: "view"; id: string; view: unknown }
  | { kind: "mood"; id: string; events: SessionEvent[] };
```

`view` 是工具结果里携带的前端卡片，形状由插件侧填；渲染层在形状定下来之前先跳过。`mood` 是情绪卡，规格正文只列了三种块，实现时按「情绪独立成块」另立一类——它有自己的样式和折叠态，塞进过程卡会改变它的可见性。

块 id 用「轮序号 + 轮内序号」（`t3-b0`），不用数组下标。下标做 key 时，中间插入一个视图块会让它后面的行全部重挂，用户正展开的卡会自己收起来。

## 两个切点

| 切点 | 触发 | 结果 |
|---|---|---|
| 文本 | 文本 chunk 累积之后出现非文本事件 | 当前卡闭合，文本成为独立块 |
| 视图 | `tool/result` 携带视图 | 结果行留在本卡，卡闭合，视图块紧随其后 |

视图切点在 `tool/result` 而不是 `tool/call`，两个原因：一是 `buildToolSlots` 按 `callId` 把 call 与 result 配成同一行，在 call 处闭合会让 result 落到下一张卡，配对与审批状态都得重写；二是视图本身是工具跑完才有的东西。

连续多次工具调用之间没有文本时合成一张卡，摘要写「调用 3 次」。

行分类没有复用 `isMetaEvent`——它把所有 `assistant/chunk`（含 text）都算 meta，拿来分行会把每个 text chunk 变成一行。

## 过程卡

- 收起态一行摘要：`思考 2 段 · 调用 3 次 · 1 项失败`，只列非零项；有失败必须露出来，不设阈值；
- 展开后按到达顺序混排思考行与工具行，文案与配色沿用既有那套（进行中 / 完成 / 失败 / 等待确认 / 已拒绝）；
- 每张卡只记自己的展开状态，互不影响，也不跨轮次；
- 等待审批时**该卡**强制展开，卡内只显示「请在输入框上方处理」——确认/拒绝按钮在输入框上方的 Action Dock，不在卡里；
- 情绪卡独立成块，不进过程卡。

## 文本

过程文本与最终文本走同一条 Markdown 渲染路径。过程文本（被工具调用闭合的那一段）用色阶 `--text-2` 弱化，不用 opacity——opacity 0.55 会把正文对比度压到 3:1 以下；Markdown 里 strong/code/标题自带颜色，需要一并拉回继承色，否则同一段里正文变灰、代码块还是原色。

归档后过程文本保留：只要本轮有 text chunk，文本段就从 chunk 重建，与流式中同构。

## 归档重建

1. **reset 吃掉**：只取最后一个 `assistant/reset` 之后的事件；
2. **无 chunk 的旧会话整轮回退**：本轮没有任何 chunk 就走 `assistant/message` 的聚合文本，等于旧外观，不白屏；
3. **思考也从 chunk 取**：现状归档的思考来自 `assistant/message.reasoningContent`，改走 chunk 之后归档与流式才真正同构，否则「思考 2 段」在回放里会塌成一整段。

## 不承诺的部分

展开视口上方的卡、内容变高把下面的行推下去，符合直觉，不补滚动锚定（`useMessageScroll` 的 `pinAfterToggle` 在上滑阅读时本来就是空操作）。

## 测试

- `blocks.test.ts`：切分器纯函数——文本切点、视图切点、连续工具合成一卡、reset 撤回、无 chunk 回退、稳定 id、5000 chunk 基准；
- `ProcessCard.test.tsx`：单卡摘要、失败项数、按到达顺序混排、审批强制展开、情绪独立；
- `messageFlow.test.tsx`：状态矩阵——流式中 / 完成 / 含失败 / 等待审批 / 中止 / 归档回放；
- `EventBadge.test.tsx`：既有 classic 形态，未受重构影响。
