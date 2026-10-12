# PRTS Terminal · 发布构建脚本
#
# 用法（在项目根目录，用你自己的终端运行）：
#   powershell -ExecutionPolicy Bypass -File .\build.ps1
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -Clean      # 先清空构建目录，全量重编
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -Jobs 4     # 限制并行编译数（内存紧张时）
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -DryRun     # 只做检查与预告，不构建
#
# 屏幕上看得到：
#   1) 前置检查（工具是否就绪、构建盘剩余空间）
#   2) 构建过程：cargo 会实时刷新进度条，形如 Building [====> ] 326/574
#   3) 结果汇总：耗时、安装包路径与大小、target 目录体积
# 完整日志同时落盘到 build-log-<时间>.txt；失败时会自动把日志末尾 40 行打出来。

param(
    [switch]$Clean,
    [int]$Jobs = 0,
    [switch]$DryRun
)

$ErrorActionPreference = 'Continue'
$root = $PSScriptRoot
Set-Location $root

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$script:logFile = Join-Path $root "build-log-$stamp.txt"
# 单一写入通道：控制台与日志文件各写一次，且日志只由本流写。
# 曾经用 Add-Content 与 Tee-Object 同时写同一文件，结果报“正由另一进程使用”。
$script:logStream = [System.IO.StreamWriter]::new($script:logFile, $true, [System.Text.Encoding]::UTF8)

function Say([string]$msg) {
    $line = '[{0}] {1}' -f (Get-Date -Format 'HH:mm:ss'), $msg
    Write-Host $line
    $script:logStream.WriteLine($line)
    $script:logStream.Flush()
}

function WriteLog([string]$text) {
    $script:logStream.WriteLine($text)
    $script:logStream.Flush()
}

function SizeOf([string]$p) {
    if (-not (Test-Path $p)) { return '不存在' }
    $m = Get-ChildItem $p -Recurse -File -Force -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum
    return ('{0:N2} GB' -f ($m.Sum / 1GB))
}

# 与 SizeOf 同源，但返回数值（用于阈值判断）；不存在返回 -1。
function SizeGB([string]$p) {
    if (-not (Test-Path $p)) { return -1 }
    $m = Get-ChildItem $p -Recurse -File -Force -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum
    return ($m.Sum / 1GB)
}

Write-Host ''
Say '========================================'
Say 'PRTS Terminal 发布构建'
Say '========================================'

# ── 1/3 前置检查 ──
Say ''
Say '[1/3] 前置检查'
$missing = @()
foreach ($tool in 'node', 'npm', 'python', 'cargo', 'rustc') {
    if (Get-Command $tool -ErrorAction SilentlyContinue) {
        Say ('      {0,-6} 就绪' -f $tool)
    } else {
        Say ('      {0,-6} 缺失，需要先安装' -f $tool)
        $missing += $tool
    }
}
if ($missing.Count -gt 0) { Say '存在缺失工具，已中止。'; exit 1 }

$drive = $root.Substring(0, 1)
$freeGB = [math]::Round((Get-PSDrive -Name $drive).Free / 1GB, 1)
Say ('      构建盘 {0}: 剩余 {1} GB' -f $drive, $freeGB)
if ($freeGB -lt 20) { Say '      注意：剩余空间偏低，构建可能失败。' }

# ── 2/3 构建 ──
Say ''
Say '[2/3] 开始构建（前端 → MAA 桥 → Rust 发布编译 → NSIS 打包）'
Say '      冷启动通常十几分钟起；热启动约 2-3 分钟。'
Say '      cargo 的进度条会显示 已完成/总数，例如 Building [====> ] 326/574。'
Say ''

if ($DryRun) {
    Say '      -DryRun：只做检查与预告，不执行构建。'
    Say ''
    Say '[3/3] 跳过构建（-DryRun）'
    Say ('      target 目录当前：' + (SizeOf (Join-Path $root 'src-tauri\target')))
    Say ''
    Say '检查完成。去掉 -DryRun 即可真正构建。'
    exit 0
}

if ($Clean) {
    Say '      -Clean：清空构建目录'
    Push-Location (Join-Path $root 'src-tauri'); cargo clean | Out-Null; Pop-Location
    Push-Location (Join-Path $root 'bridge'); cargo clean | Out-Null; Pop-Location
}
if ($Jobs -gt 0) { $env:CARGO_BUILD_JOBS = "$Jobs"; Say ('      并行编译数限制为 {0}' -f $Jobs) }

$sw = [System.Diagnostics.Stopwatch]::StartNew()
& npm run tauri build 2>&1 | ForEach-Object { Write-Host $_; WriteLog $_ }
$code = $LASTEXITCODE
$sw.Stop()

# ── 3/3 结果 ──
Say ''
if ($code -eq 0) {
    Say ('[3/3] 构建成功，耗时 {0}' -f $sw.Elapsed.ToString('hh\:mm\:ss'))
    Say '      安装包：'
    $nsis = Join-Path $root 'src-tauri\target\release\bundle\nsis'
    $pkgs = Get-ChildItem $nsis -Filter '*.exe' -ErrorAction SilentlyContinue
    if ($pkgs) {
        $pkgs | ForEach-Object { Say ('        {0}  ({1:N2} MB)' -f $_.Name, ($_.Length / 1MB)) }
    } else {
        Say '        未找到安装包（构建成功但 bundle 目录为空，请检查）'
    }
    $targetGB = SizeGB (Join-Path $root 'src-tauri\target')
    Say ('      target 目录：' + (SizeOf (Join-Path $root 'src-tauri\target')))
    # 陈旧产物会随“换过多少次编译输入”缓慢堆积（cargo 不主动回收旧 hash 的产物）。
    # 超线时提示回收：cargo-sweep 只删历史产物，保留最近一次构建，下次仍是热启动。
    if ($targetGB -gt 20) {
        Say ('      注意：target 已超过 20 GB（{0:N2} GB）。可回收陈旧产物：' -f $targetGB)
        Say '        cd src-tauri; cargo sweep --maxsize 8GB .'
    }
    Say ''
    Say '完成。'
} else {
    Say ('[3/3] 构建失败（退出码 {0}），耗时 {1}' -f $code, $sw.Elapsed.ToString('hh\:mm\:ss'))
    Say '      日志末尾 40 行：'
    Say '      ----------------------------------------'
    if (Test-Path $script:logFile) {
        Get-Content -LiteralPath $script:logFile -Tail 40 | ForEach-Object { Write-Host $_ }
    }
    Say '      ----------------------------------------'
    Say ('      完整日志：' + $script:logFile)
    exit $code
}
