; SimpChinese.nsh —— PRTS Terminal 的简体中文安装器文案
;
; 由 tauri.conf.json 的 bundle.windows.nsis.customLanguageFiles 引入，
; 语言键 "SimpChinese" 必须同时出现在 bundle.windows.nsis.languages 里。
;
; 它**整体替换** Tauri 自带的那份同语言文件，所以内容必须是完整的：
; 本文件即 Tauri 的 SimpChinese.nsh 全文，只改了 deleteAppData 一条。
;
; 为什么改 deleteAppData：卸载确认页那个勾选框删的是
; `%APPDATA%\<identifier>` 与 `%LOCALAPPDATA%\<identifier>`（WebView2 缓存等），
; 与用户数据根 `~/.prts` 无关。原标题「删除应用程序数据」会被误读成"删掉我的对话"，
; 改为明确说明不含对话与设置。
;
; 维护提醒：Tauri 若在某版本新增/改名 LangString，本文件需同步补齐，否则安装器
; 会缺字串。来源：crates/tauri-bundler/src/bundle/windows/nsis/languages/SimpChinese.nsh

LangString addOrReinstall ${LANG_SIMPCHINESE} "添加/重新安装组件"
LangString alreadyInstalled ${LANG_SIMPCHINESE} "已安装"
LangString alreadyInstalledLong ${LANG_SIMPCHINESE} "${PRODUCTNAME} ${VERSION} 已经安装了。选择你想要执行的操作后点击下一步以继续。"
LangString appRunning ${LANG_SIMPCHINESE} "{{product_name}} 正在运行！请关闭后再试。"
LangString appRunningOkKill ${LANG_SIMPCHINESE} "{{product_name}} 正在运行！$\n点击确定以终止运行。"
LangString chooseMaintenanceOption ${LANG_SIMPCHINESE} "选择要执行的维护操作。"
LangString choowHowToInstall ${LANG_SIMPCHINESE} "选择你想要安装 ${PRODUCTNAME} 的方式。"
LangString createDesktop ${LANG_SIMPCHINESE} "创建桌面快捷方式"
LangString deleteAppData ${LANG_SIMPCHINESE} "同时删除应用缓存（不含对话与设置）"
LangString dontUninstall ${LANG_SIMPCHINESE} "请勿卸载"
LangString dontUninstallDowngrade ${LANG_SIMPCHINESE} "请勿卸载（此安装程序禁止未卸载就进行版本降级的操作）"
LangString failedToKillApp ${LANG_SIMPCHINESE} "无法终止 {{product_name}}。请关闭后再试。"
LangString installingWebview2 ${LANG_SIMPCHINESE} "正在安装 WebView2..."
LangString newerVersionInstalled ${LANG_SIMPCHINESE} "有一个更新版本的 ${PRODUCTNAME} 已经安装了！不推荐你安装旧的版本。如果你真的想要安装这个旧的版本，推荐先卸载当前版本。选择你想要执行的操作后点击下一步以继续。"
LangString older ${LANG_SIMPCHINESE} "旧的"
LangString olderOrUnknownVersionInstalled ${LANG_SIMPCHINESE} "系统中已存在版本为 $R4 的 ${PRODUCTNAME}。推荐先卸载当前版本后再进行安装。选择你想要执行的操作后点击下一步以继续。"
LangString silentDowngrades ${LANG_SIMPCHINESE} "降级操作在此安装程序上已禁用，无法进行安静安装，请使用图形操作界面。$\n"
LangString unableToUninstall ${LANG_SIMPCHINESE} "无法卸载！"
LangString uninstallApp ${LANG_SIMPCHINESE} "卸载 ${PRODUCTNAME}"
LangString uninstallBeforeInstalling ${LANG_SIMPCHINESE} "安装前卸载"
LangString unknown ${LANG_SIMPCHINESE} "未知"
LangString webview2AbortError ${LANG_SIMPCHINESE} "无法安装 WebView2！没有它，此应用就无法运行。尝试重启安装程序。"
LangString webview2DownloadError ${LANG_SIMPCHINESE} "错误：无法下载 WebView2 - $0"
LangString webview2DownloadSuccess ${LANG_SIMPCHINESE} "WebView2 引导程序下载成功"
LangString webview2Downloading ${LANG_SIMPCHINESE} "正在下载 WebView2 引导程序..."
LangString webview2InstallError ${LANG_SIMPCHINESE} "错误：安装 WebView2 时失败，错误代码：$1"
LangString webview2InstallSuccess ${LANG_SIMPCHINESE} "成功安装 WebView2"
