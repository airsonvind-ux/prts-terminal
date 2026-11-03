# 插件系统

插件是**功能切片**：它把自己的工具、数据和能力声明装在一起，由宿主装配。目前有两种
插件——`builtin`（随包编译进宿主，例如 MAA）与 `process`（独立子进程，经 stdio 通信）。
M3 起可以在「设置 → 扩展」里管理它们：安装、启停、重试、卸载。

## 一、目录

| 位置 | 用途 |
|---|---|
| `content/plugins/<id>/plugin.json`（开发）/ `resources/plugins/<id>/plugin.json`（发布） | 随包插件 |
| `<data_dir>/plugins/<id>/plugin.json` | 用户插件（后扫描，与随包同名时随包优先） |
| `<插件目录>/skills/` | 插件自带的技能（停用即不进索引，见第七节） |
| `<data_dir>/plugin-data/<id>/` | 插件的私有数据（`fs.plugin`，卸载时按勾选删） |
| `<data_dir>/plugin-temp/<id>/` | 插件的临时文件（`fs.temp`，应用退出时整个 `plugin-temp` 清空） |
| `<data_dir>/plugin-tmp/` | 安装暂存区（预览时拷进来的目录；启动时整个清空） |
| `<data_dir>/plugins.json` | 启停状态（由管理页改写） |

`data_dir` 在开发构建是 `~/.prts-dev`、发布构建是 `~/.prts`。

## 二、manifest

`plugin.json` 是一个 JSON 对象，字段用 camelCase，未知字段忽略（向前兼容）。

```json
{
  "manifestVersion": 1,
  "id": "maa",
  "name": "MAA 自动化",
  "version": "0.1.1",
  "description": "把游戏操作交给本机 MAA",
  "kind": "builtin",
  "requires": { "app": ">=0.1.1" },
  "capabilities": ["maa.bridge"],
  "activation": ["onStartup"],
  "contributes": { "skills": ["MAA 任务"], "settingsForm": "builtin-page", "assets": [] }
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | `^[a-z][a-z0-9-]{1,63}$`；与目录名不一致只告警 |
| `name` / `version` | 是 | `version` 必须是合法 semver |
| `kind` | 是 | `builtin` 或 `process` |
| `entry` | `process` 必填 | `{ "exe": "相对插件目录的路径", "transport": "stdio" }`；不许绝对路径、不许 `..` |
| `requires.protocol` | `process` 必填 | 目前只接受 `1` |
| `requires.app` | 否 | 宿主版本要求（semver 范围），不满足则整插件失败 |
| `capabilities` | 否 | 要用的服务键，见下表；未登记的键会让插件加载失败 |
| `activation` | 否 | 预留（目前只支持 `onStartup`） |
| `contributes` | 否 | `settingsForm` 取 `schema` / `builtin-page`；目前只实现后者（内置插件用宿主原生页）。`skills` 只作声明，实际以插件目录下的 `skills/` 为准 |

命名约定：内置插件沿用原有工具名，`process` 插件的工具一律带 `<id>_` 前缀。

## 三、工具名

`prefix = id.replace('-', "_")`，最终名字是 `prefix + "_" + <插件自己声明的名字>`，整体必须
匹配 `^[a-z][a-z0-9_]{0,63}$`。不合法的名字会让**整个插件**加载失败——不做静默改名。

## 四、能力表

能力名就是服务键，一对一。插件必须在 `capabilities` 里声明，宿主按
「声明 ∩ 已登记」裁剪；没声明的调用直接 `-32000`，不会走到实现。

| 键 | 作用 | 参数 | 返回 |
|---|---|---|---|
| `fs.plugin` | 插件私有数据目录 | `{op, path?, data?, confirm?}` | `read` → `{data, bytes}`；`list` → `{entries}`；`stat` → `{exists, kind, bytes}` |
| `fs.temp` | 临时目录 | `{op, path?, data?}` | 同上；`clear` 清空本插件临时目录 |
| `query.call` | 只读查询宿主领域工具 | `{tool, args}` | 该工具的规范化输出 |
| `maa.bridge` | MAA 桥进程托管 | `{op}` | `{state, port, error}` |

细节：

- `fs.plugin` / `fs.temp` 的 `path` 必须是相对路径；绝对路径、`..`、符号链接一律拒绝。
  `fs.plugin` 的 `delete` 只删文件与空目录，`purge` 要带 `"confirm": true`。
- `query.call` 只放行 13 个只读工具（`search_entities`、`sim_path`、`query_*`、`time`、
  `read_knowledge`、`list_knowledge`），且**不能调插件自己的工具**（防插件间递归）。
- `maa.bridge` 的 `ensure` 只起进程并立刻返回端口，**不连接**：桥只 accept 一次，探活会把
  插件唯一的连接机会吃掉。`state` 是 `starting` / `running` / `stopped` / `unavailable`。

`HostServiceInfo.timeout` 目前**不执行**（规格 §5）：超时由调用方自己定。

## 五、process 插件协议

### 分帧

一帧 = `Content-Length: <字节数>\r\n\r\n` + UTF-8 JSON。单帧上限 8 MiB。stdout 只能有帧；
往 stdout 打日志会把插件判成协议错误并杀掉，日志请走 `log`（或者干脆写 stderr）。

### 宿主 → 插件

| 方法 | 参数 | 期望应答 |
|---|---|---|
| `initialize` | `{protocol, hostVersion, pluginId}` | `{protocol, tools, capabilities}` |
| `tool.call` | `{name, args, deadlineMs}` | 任意 JSON |
| `shutdown` | `{}` | 任意 JSON（之后进程应退出） |
| `cancel` | `{id}`（通知，无应答） | — |

`tools` 的每一项是 `{name, description, parameters, timeoutMs?}`，`timeoutMs` 缺省 60000。
`initialize` 是工具面的**唯一来源**；它回报的 `capabilities` 只作诊断，不参与授权。

### 插件 → 宿主

| 方法 | 参数 | 应答 |
|---|---|---|
| `host.call` | `{key, args}` | `{id, result}` 或 `{id, error}` |
| `log` | `{level, text}` | 无 |

读线程每读到一帧就立刻分派，`host.call` 交给工作线程执行，所以插件的 `host.call` 与宿主的
`tool.call` 可以同时在路上——**不要在插件里假设请求串行**。

### 错误码

| 码 | 含义 |
|---|---|
| `-32600` | 请求格式不对 |
| `-32601` | 方法/服务不存在 |
| `-32602` | 参数不合法 |
| `-32000` | 能力未授权 |
| `-32001` | 超时 |
| `-32010` | 插件故障（进程死了、通道断了） |

### 超时与生命周期

- 握手 10 秒；工具调用按 `timeoutMs`（宿主下发的 `deadlineMs` 为准）。
- 调用超时先发 `cancel`，2 秒内还不回话就杀进程并记 `Faulted`。
- 崩溃**不自动重启**（避免崩溃循环）：下一次调用尝试拉起一次，仍失败就标记故障，
  直到用户重启 PRTS。
- 状态：`Starting` → `Loaded` / `Faulted(原因)` / `Disabled` / `Failed(原因)`。

## 六、启停、安装与卸载

`<data_dir>/plugins.json` 形如 `{"enabled": {"<id>": true}}`：文件缺失按全启用；写坏会备份成
`plugins.json.bad` 后按全启用；文件里不认识的 id 原样保留。管理页的每一次改动都经同一个
`PluginsConfig` 读改写并立即落盘，不会因为并发点击丢更新。

- **停用**：摘掉工具并写墓碑，正在跑的对话里再调用会得到
  `插件 <名字> 已停用`（而不是「工具不存在」）；process 插件按 id 收进程，内置 MAA 同时关桥。
  该插件的技能不再进索引（新对话生效）。提示语是「停用后新对话生效」。
  **对话处理中不能停用 / 卸载 / 重试**：收权会撕掉正在执行的任务（停用 MAA 会连桥一起收），
  后端在 `TurnRegistry::active_count() > 0` 时直接拒绝，界面同时置灰——闸在后端，不只是 UI。
  启用与安装不设闸：它们不打断任何东西，只是多一个当前轮用不到的选项。
- **启用**：按记录里的原目录重新安装一次（目录名可以和 id 不同），工具回来，状态回 `Loaded`。
- **安装**（M3 只支持文件夹，zip 请先解压）：选目录或拖到安装区 → 复制到 `plugin-tmp/<token>/`
  → 读 `plugin.json` 走完整校验 → 预览卡片 → 确认后 `rename` 原子落位
  `plugins/<id>/` → 尝试加载，失败则删目录回滚。守卫：拒符号链接、拒绝对路径与 `..`、
  单插件 100 MB / 10000 个文件、`kind: "builtin"` 在预览阶段就拒。
- **卸载**：杀进程 → 删插件目录与它的 `plugin-temp` → 从 `plugins.json` 移除；
  「同时删除它的数据」勾选后才删 `plugin-data/<id>/`（默认保留）。内置插件不能卸载。
- **未登记目录**：`plugins/` 下没有可读 `plugin.json` 的目录会单列出来，不会静默消失。

管理页的状态口径：`Starting` 显示为「启动中」，仅 `Loaded` 显示工具数；`Faulted` 给「重试」
（只对进程退出的插件有效），`Failed` 只显示原因。

日志都走 stderr：宿主侧带 `[plugins]` 前缀，插件转写带 `[plugin <id>]` 前缀。目前还没有日志器
（D28），所以这些行只出现在控制台。

## 七、技能与提示词归属

插件可以带技能：放在插件目录下的 `skills/<技能目录>/SKILL.md`。索引的根是「宿主技能根 +
已启用插件的 `skills/`」，宿主根排第一，因此同名时宿主优先；插件之间按 id 排序，先到先得。
`contributes.skills` 只用来声明，声明了但目录里没有只会在 stderr 留一行告警，不影响加载。

- 停用插件后，它的技能不再进索引——**新对话**看不到；正在进行的对话沿用冻结快照。
- system prompt 的 Skill 目录按来源分组：宿主条目在前、格式不变；插件条目在名字后补
  「（插件展示名）」。
- 存在插件工具或插件技能时，system prompt 多一节「插件提供的能力」，逐行
  `工具 <name> — <插件名>` / `技能 <name> — <插件名>`，按名字排序；没有插件条目时这一节
  完全不出现，无插件态的提示词逐字节不变。
- 工具与技能目录的提醒（`tool_catalog` / `skill_catalog`）带归属：插件条目显示为
  `名称（插件名）`，移除项从基线读归属；启停/安装/卸载时提醒首行会写一句上下文
  （如「插件 MAA 自动化 已停用」）。

## 八、命令（前端 ↔ 后端）

| 命令 | 参数 | 返回 |
|---|---|---|
| `list_plugins` | — | `PluginList` |
| `pick_plugin_folder` | — | 选中的路径或 `null` |
| `set_plugin_enabled` | `{id, enabled}` | `PluginList` |
| `preview_plugin_install` | `{path}` | `PluginPreview` |
| `confirm_plugin_install` | `{token}` | `PluginList` |
| `uninstall_plugin` | `{id, deleteData}` | `PluginList` |
| `retry_plugin` | `{id}` | `PluginList` |

除 `list_plugins` 外全部是 `async` 并经 `spawn_blocking` 执行，磁盘操作不占主线程。
`PluginList = { plugins: PluginView[], unregistered: {dirName, reason}[] }`。

## 九、版本

`protocol` 目前是 `1`。加字段属于兼容变更；删字段、改语义要升 `protocol`，`requires.protocol`
不匹配的插件会被拒绝加载并指出收到的值。
