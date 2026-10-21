# prts-maa-bridge

PRTS 与 [MAA](https://github.com/MaaAssistantArknights/MaaAssistantArknights)（MaaCore）之间的**本地桥**。

## 它是什么

一个**独立进程**。PRTS 需要「借 MAA 的视觉识别」时启动它；它运行时加载**用户已装 MAA** 目录里的 `MaaCore.dll`，把 MAA 的连接 / 任务 / 图像 / 回调包成 PRTS 可用的本地接口。

- 用户没装 MAA → 桥报告 `available:false`，PRTS 关闭该功能、照常运行。
- 桥**不打包**任何 MAA 本体；`MaaCore.dll` 及其依赖（opencv / onnxruntime / …）全部来自用户自己的 MAA 安装目录。

## 许可

**AGPL-3.0-or-later**。

桥因运行时链接 `MaaCore`（AGPL-3.0）而成为 AGPL 组件。这是本仓库中**唯一**带 AGPL 标签的部分；PRTS 核心保持原许可，二者只经进程间通信（IPC）交互，属「聚合」而非「合成作品」。

**纪律：绝不要把本 crate 作为库静态链接进 PRTS。** 一旦链进去，AGPL 就会传染 PRTS 核心。

## 构建

```powershell
cargo build --release
# 产物：target/release/prts-maa-bridge.exe
```

平台：Windows **x64**（PC 客户端与 MAA 均 x64；`ASST_CALL = __stdcall` 在 x64 下与 `__cdecl` 同为一套约定）。

## 运行

```powershell
prts-maa-bridge.exe --maa-dir "<MAA安装目录>" --port 41231 [--user-dir <工作目录>]
```

- `--maa-dir`：用户 MAA 安装目录（内含 `MaaCore.dll` 与 `resource/`）。
- `--port`：协议监听的**本机 TCP 端口**（`127.0.0.1`）。桥作服务端，PRTS 连入。不给则回退 stdin/stdout（会被第三方 DLL 日志污染，仅供应急）。
- `--user-dir`：MAA 的工作/缓存目录，指向 PRTS 自有目录以隔离，避免污染用户 MAA。缺省为系统临时目录下的 `prts-maa-bridge`。

> **为什么不用 stdout？** MaaFramework 的控制单元会直接往进程 stdout 打日志，插进帧流会让读端失步。独立 TCP 通道与之彻底隔离。

## 生命周期与退出

桥是**会话级子进程**，一生只服务一个 PRTS 会话：

- **PRTS 退出或被强杀** → TCP 连接断开 → 桥读到 EOF → 退出（同时 `AsstDestroy` 销毁实例，任务随之终止）。这条不依赖 PRTS 主动收尾，父进程一死就成立。
- **PRTS 在连上之前就没了** → 没有连接可读、也就没有 EOF → 桥等待连接 **30 秒**后自行退出（退出码 `4`），不留孤儿。

退出码：`0` 正常 · `3` 监听或接受连接失败 · `4` 等待连接超时。

所以不会出现「任务还在跑，却找不到是谁派的」这种情况。

## 与 PRTS 的通信（本机 TCP 帧协议）

桥监听 `127.0.0.1:<port>`，PRTS 连入。帧格式：`[4 字节大端 u32 长度][UTF-8 JSON]`。

**事件（桥 → PRTS）**

- `{"event":"ready","available":true,"version":...,"maa_dir":...,"user_dir":...}` — 启动握手；失败时 `available:false` 带 `error`。
- `{"event":"maa","code":<int>,"name":"...","details":<json|null>,"raw":<string|null>}` — MaaCore 回调，`raw` 保真透传。
- `{"event":"recognition","kind":"OperBoxInfo","details":{...},"taskchain":...,"taskid":...,"uuid":...}` — 结构化识别结果（薄的识别路由，见下）。
- `{"event":"result","id":...,"ok":true,"data":{...}}` — 命令响应。
- `{"event":"error","id":...,"error":"..."}` — 错误。

> `recognition` 只做**路由**：当回调是 `SubTaskExtraInfo`（码 20003）且 `what` 命中已知识别结果列表（`StageDrops`/`Recruit*`/`DepotInfo`/`OperBoxInfo`/`EnterFacility` …）时额外发一条。原始 `maa` 事件始终是事实来源。

**命令（PRTS → 桥）**

- 连接：`{"cmd":"connect_win32","hwnd":<int>,"screencap":<mask>,"mouse":<n>,"keyboard":<n>}` · `{"cmd":"connect_adb","adb_path":"...","address":"...","config":"..."}`
- 任务：`task_add`（`{type,params}`）· `task_set`（`{task_id,params}`）· `task_list`
- 运行：`run_start` · `run_stop` · `click`（`{x,y}`）· `back_to_home`
- 图像：`screenshot`（可选 `{path}`，返回写出的 PNG 路径与字节数）
- 查询/配置：`status` · `uuid` · `set_instance_option`（`{key,value}`）· `set_connection_extras`（`{name,extras}`；连接前设，如 MuMu 截图增强）
- 窗口发现（Windows）：`list_windows` · `find_window`（`{title}`）
- 退出：`shutdown`

> 现状：`ffi` + `engine` + `events` + `ipc` 与命令面已就绪；**ADB（模拟器）通道已端到端跑通**（MuMu 12 + 截图增强，OperBox 429，见 `_maa-bridge-设计.md` §7.9）。**Win32（PC 端）曾端到端跑通，评估后搁置**（实验性、暂不启用，见 §7.4）。待办：扩任务覆盖、桥随 PRTS 的分发形态。
