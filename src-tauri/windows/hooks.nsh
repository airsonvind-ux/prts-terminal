; hooks.nsh —— PRTS Terminal 安装器的 NSIS 钩子
;
; 由 tauri.conf.json 的 bundle.windows.nsis.installerHooks 引入。模板在 MUI2 之后、
; 定义页面之前 !include 本文件，所以这里可以覆盖 MUI 文案，也可以定义 Var / Function。
;
; 本文件只做三件事：
;   1. 覆盖安装前清掉自己铺下的安装树，避免新版删掉的文件留在原地；
;   2. 安装完成后自检四样关键文件，缺件当场报错并退出（与启动前自检同一套判据）；
;   3. 覆盖卸载确认页文字，说明对话与设置会保留。
;
; 明确不做：
;   - 不碰用户数据（数据根 ~/.prts 不在安装目录内，模板也不会删它）；
;   - 不额外杀进程（模板自带的 CheckIfAppIsRunning 已处理，见 utils.nsh）；
;   - 不写卸载动作（模板已覆盖文件、快捷方式、注册表与自启项）。

!include LogicLib.nsh

; ── 卸载确认页文案 ──
; NSIS 自带的中文确认文字写的是"以及所有它的设置"，与本项目"卸载不动用户数据"
; 的事实相反，会误导用户，故覆盖。MUI_UNTEXT_* 必须在对应页面宏之前定义。
!define MUI_UNTEXT_CONFIRM_TITLE "卸载 PRTS Terminal"
!define MUI_UNTEXT_CONFIRM_TEXT "确定要卸载 PRTS Terminal 吗？$\r$\n$\r$\n程序本体将被移除，你的对话与设置会保留。"

; ── 1. 覆盖安装前清理自有安装树 ──
; 只在已存在安装时执行（首次安装没有可清理的东西）。只碰安装目录里自己拥有的路径：
; 资源树、MAA 桥、上次自检留下的诊断文件。
!macro NSIS_HOOK_PREINSTALL
  Push $0
  IfFileExists "$INSTDIR\${MAINBINARYNAME}.exe" 0 prts_preinstall_done
    DetailPrint "清理旧版安装文件..."
    RMDir /r "$INSTDIR\resources"
    Delete "$INSTDIR\prts-maa-bridge.exe"
    Delete "$INSTDIR\install-diagnostics.log"
  prts_preinstall_done:
  Pop $0
!macroend

; ── 2. 安装完成后自检 ──
; 判据与启动前自检（src/lib.rs 的 precheck_install_surface）保持一致的四样。
!macro NSIS_HOOK_POSTINSTALL
  Push $0
  StrCpy $R0 ""
  ${IfNot} ${FileExists} "$INSTDIR\arknights.db"
    StrCpy $R0 "$R0$\r$\n- 数据库初始副本：$INSTDIR\arknights.db"
  ${EndIf}
  ${IfNot} ${FileExists} "$INSTDIR\resources\knowledge\*.*"
    StrCpy $R0 "$R0$\r$\n- 知识库目录：$INSTDIR\resources\knowledge"
  ${EndIf}
  ${IfNot} ${FileExists} "$INSTDIR\resources\skills\*.*"
    StrCpy $R0 "$R0$\r$\n- 内置技能源：$INSTDIR\resources\skills"
  ${EndIf}
  ${IfNot} ${FileExists} "$INSTDIR\prts-maa-bridge.exe"
    StrCpy $R0 "$R0$\r$\n- MAA 桥：$INSTDIR\prts-maa-bridge.exe"
  ${EndIf}

  ${If} $R0 != ""
    DetailPrint "安装面自检未通过。"
    FileOpen $0 "$INSTDIR\install-diagnostics.log" w
    FileWrite $0 "PRTS Terminal 安装面自检未通过。$\r$\n"
    FileWrite $0 "安装目录：$INSTDIR$\r$\n"
    FileWrite $0 "缺失或不可读的文件：$R0$\r$\n"
    FileClose $0
    MessageBox MB_OK|MB_ICONSTOP "安装不完整，以下文件缺失或不可读：$R0$\r$\n$\r$\n诊断文件：$\r$\n$INSTDIR\install-diagnostics.log"
    SetErrorLevel 1
    Quit
  ${EndIf}
  Pop $0
!macroend

; ── 3. 卸载前 ──
; 无需额外动作：那句"对话与设置会保留"由上面的 MUI_UNTEXT_CONFIRM_TEXT 承载，
; 用户数据根不在卸载范围内由模板保证。
!macro NSIS_HOOK_PREUNINSTALL
!macroend
