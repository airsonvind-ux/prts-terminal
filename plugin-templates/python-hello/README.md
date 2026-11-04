# Python 示例插件

最小可运行的插件：一次握手、两个工具（`hello`、`show_card`）、收到 `shutdown`
后退出。它跑起来只需要 Python 3.8+，没有第三方依赖。

## 跑一遍

```bash
python hello.py
```

单独跑不会打印任何东西——它在等宿主往 stdin 写帧。检查它能不能被宿主接上，
用校验器：

```bash
plugin_check .
```

八项全 PASS 就说明协议没问题。

## 改哪里

- `TOOLS`：工具面。宿主看这里的 `name` / `action` / `description` / `parameters`；
  `action` 是对话里那句「PRTS正在…」的动词短语（1–16 字、不含空白），可省略；
- `handle_tool`：实现。返回字符串会直接显示在对话里；返回
  `{"content": …, "view": …}` 时，字符串进模型上下文、视图渲染成一张卡片。
  `show_card` 是后者的示例。

视图的三种形态、大小上限与沙箱边界见随包手册 `10-视图`；想先看看长什么样：

```bash
plugin_check . --call show_card --render-view preview.html
```

工具名会加上 `<插件 id>_` 前缀，所以这里的 `hello` 在宿主里叫
`python_hello_hello`（`python-hello` 里的 `-` 换成 `_`）。

## 协议

`Content-Length: <字节数>\r\n\r\n<UTF-8 JSON>`，宿主发 `initialize` /
`tool.call` / `shutdown`，插件回 `result` 或 `error`。细节见随包手册
`05-protocol`。
