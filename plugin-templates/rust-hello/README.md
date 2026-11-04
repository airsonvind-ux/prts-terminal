# Rust 示例插件

最小可运行的插件：一次握手、两个工具（`hello`、`show_card`）、收到 `shutdown`
后退出。只有一个依赖 `serde_json`——分帧与 JSON 都不手写。

## 构建

```bash
cargo build --release
```

产物在 `target/release/hello.exe`（Windows）或 `target/release/hello`。
把它拷到本目录，`plugin.json` 里的 `entry.exe` 是 `hello.exe`：

```bash
cp target/release/hello.exe .
```

非 Windows 平台把 `plugin.json` 的 `entry.exe` 改成 `hello`。

## 跑一遍

```bash
plugin_check .
```

八项全 PASS 就说明协议没问题。

## 改哪里

- `tools()`：工具面。宿主看这里的 `name` / `action` / `description` / `parameters`；
  `action` 是对话里那句「PRTS正在…」的动词短语（1–16 字、不含空白），可省略；
- `handle_tool`：实现。返回 `Value::String` 直接显示在对话里；返回
  `{content, view}` 时字符串进模型上下文、视图渲染成卡片，`show_card` 是示例。

视图细节见随包手册 `10-视图`；预览：

```bash
plugin_check . --call show_card --render-view preview.html
```

工具名会加上 `<插件 id>_` 前缀，所以这里的 `hello` 在宿主里叫
`rust_hello_hello`（`rust-hello` 里的 `-` 换成 `_`）。

## 协议

`Content-Length: <字节数>\r\n\r\n<UTF-8 JSON>`，宿主发 `initialize` /
`tool.call` / `shutdown`，插件回 `result` 或 `error`。细节见随包手册
`05-protocol`。
