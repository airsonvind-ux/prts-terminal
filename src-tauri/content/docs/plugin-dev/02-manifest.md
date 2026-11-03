# 02 plugin.json

一个插件目录里必须有 `plugin.json`。缺字段会被拒绝加载，多出来的字段会被
忽略（向前兼容：新版本加的字段，旧版本不报错）。

最小可用：

```json
{
  "manifestVersion": 1,
  "id": "my-plugin",
  "name": "我的插件",
  "version": "0.1.0",
  "kind": "process",
  "entry": { "exe": "python", "args": ["hello.py"], "transport": "stdio" },
  "requires": { "protocol": 1 }
}
```

## 字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `manifestVersion` | 否 | 清单格式版本，现在只有 `1` |
| `id` | 是 | 插件身份，装上后不能改。规则见下 |
| `name` | 是 | 显示名，对话与设置页里用它 |
| `version` | 是 | 插件自己的版本，语义化版本（`0.1.0`） |
| `description` | 否 | 一句话说明，装之前给人看 |
| `author` | 否 | 作者名 |
| `kind` | 是 | 外部插件写 `process` |
| `entry` | process 必填 | 入口，见「两种入口写法」 |
| `requires.protocol` | process 必填 | 协议版本，现在是 `1` |
| `requires.app` | 否 | 要求的应用版本，如 `>=0.1.1`；不满足会拒绝加载 |
| `capabilities` | 否 | 要用到的能力，见 [04-能力](04-能力.md) |
| `activation` | 否 | 现在只接受 `["onStartup"]` |
| `contributes.skills` | 否 | 附带技能的名字，见 [07-技能与提示词](07-技能与提示词.md) |
| `network` | 否 | 保留字段，当前不可用 |

## id 规则

`^[a-z][a-z0-9-]{1,63}$`：小写字母开头，只能有小写字母、数字、连字符，
长度 2–64。

工具名会加前缀，前缀由 id 派生：`my-plugin` → `my_plugin_<工具名>`。所以
id 里用连字符没问题，最终工具名会换成下划线。

id 还决定两件事：`plugins.json` 里开关的键、插件数据目录
（`plugin-data/<id>/`）。**装上之后不要改 id**——改了等于换一个插件。

目录名与 id 不一致只会有一条提示，不影响加载。

## 两种入口写法

`entry.exe` 有两种形态，判定顺序是「先看目录、再看 PATH」：

| 你写的 | 解析成 | 适合 |
|---|---|---|
| `hello.exe`（目录里有这个文件） | 插件目录里的 `hello.exe` | 编译型语言 |
| `python`（目录里没有同名文件） | PATH 里的 `python` | 解释型语言 |

`entry.args` 是启动参数数组，默认空：元素不得为空串，最多 32 项。参数**不经
shell**，不会做变量展开、通配符、管道——`"args": ["a.py", "--flag"]` 就是两个
参数原样传过去。

`entry.transport` 现在只支持 `stdio`，省略也行。

### 一条安全提醒

裸名解析的代价是：插件目录里如果放了一个和解释器同名的文件（比如
`python.exe`），`"exe": "python"` 会先命中它。这是你自己目录里的东西，危害
等同于插件本身能跑代码；但如果你是从别处拷来的插件目录，先看一眼里面有没有
多余的可执行文件。

## 十二条校验

加载与安装都会跑同一套检查，按顺序执行，第一条失败即停：

| # | 检查 | 失败时你会看到 |
|---|---|---|
| 1 | id 格式 | `id` 必须匹配 `^[a-z][a-z0-9-]{1,63}$` |
| 2 | 同一位置不能有两个同 id 插件 | 外部那份被跳过，内置优先 |
| 3 | version 是合法语义化版本 | `version` 不是 `1.0` 这种半截写法 |
| 4 | `builtin` 不能带 `entry` | 只对内置插件有意义 |
| 5 | `process` 必须有 `entry`，exe 是相对路径 | `entry.exe` 不能是绝对路径、不能含 `..` |
| 6 | `process` 必须声明 `protocol` 且等于 1 | 协议版本不匹配 |
| 7 | `requires.app` 要满足 | 说清要求什么版本、当前是什么版本 |
| 8 | 每个 capability 都是已知能力 | 指出是哪一条不认识 |
| 9 | 声明了 `network` 能力就要写 `allowedHosts` | 当前网络能力未开放 |
| 10 | `settingsForm` 只能是 `schema` 或 `builtin-page` | 外部插件不用这个字段 |
| 11 | `entry.args` 元素非空 | `entry.args[2] 为空` |
| 12 | `entry.args` 不超过 32 项 | `entry.args 超过 32 项` |

## 常见错误

### `id` 不合法

**你会看到**

```text
[2] FAIL  manifest 十二条校验
          id 不合法：My_Plugin
```

**说明什么**

id 里有大写字母和下划线。工具名前缀、数据目录都用它派生，只能用小写与连字符。

**怎么改**

改成 `my-plugin`；想分词就用连字符。

### `entry.exe` 含 `..`

**你会看到**

```text
[3] FAIL  entry.exe 解析
          入口不存在：/plugins/demo/../evil.exe
```

**说明什么**

入口必须落在插件目录内，`..` 一律拒绝。

**怎么改**

把文件放进插件目录，写相对路径；要调用外部程序就写裸名（走 PATH）。

### `requires.app` 不满足

**你会看到**

```text
[2] FAIL  manifest 十二条校验
          需要应用版本 >=9.0.0，当前 0.1.1
```

**说明什么**

你在 `requires.app` 里写了一个比当前应用更高的版本。

**怎么改**

要么把要求降下来，要么先在应用里更新到那个版本。
