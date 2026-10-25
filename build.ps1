# PRTS Terminal · 发布构建脚本
#
# 用法（在项目根目录，用你自己的终端运行）：
#   powershell -ExecutionPolicy Bypass -File .\build.ps1
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -Clean      # 先清空构建目录，全量重编
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -Jobs 4     # 限制并行编译数（内存紧张时）
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -DryRun     # 只做检查与预告，不构建
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -AllowDirty # 工作树不干净也继续；仍跑质量门禁，产物不作为发布候选
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -Config src-tauri\tauri.updater-test.conf.json
#                                                                    # 合并测试配置（本地更新源）
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -NoSign       # 未签名构建；只用于本地冒烟，不能作为更新包
#   powershell -ExecutionPolicy Bypass -File .\build.ps1 -GiteeRepo airsonvind0/prts-terminal-release -GitHubRepo airsonvind-ux/prts-terminal-release
#                                                                    # 签名构建后生成 release-staging/<version>/latest.json
#
# 屏幕上看得到：
#   1) 前置检查（工具是否就绪、版本一致性、构建盘剩余空间）
#   2) 质量门禁（工作树、测试、格式、clippy）
#   3) 构建过程：cargo 会实时刷新进度条，形如 Building [====> ] 326/574
#   4) 结果汇总：耗时、安装包路径与大小、target 目录体积
# 完整日志同时落盘到 build-log-<时间>.txt；失败时会自动把日志末尾 40 行打出来。

param(
    [switch]$Clean,
    [int]$Jobs = 0,
    [switch]$DryRun,
    [switch]$AllowDirty,
    [string]$Config = "",
    [switch]$NoSign,
    [string]$GiteeRepo = "",
    [string]$GitHubRepo = "",
    [string]$ReleaseNotes = ""
)

$ErrorActionPreference = 'Continue'
$root = $PSScriptRoot
Set-Location $root

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$script:logFile = Join-Path $root "build-log-$stamp.txt"
# 单一写入通道：控制台与日志文件各写一次，且日志只由本流写。
# 曾经用 Add-Content 与 Tee-Object 同时写同一文件，结果报“正由另一进程使用”。
$script:logStream = [System.IO.StreamWriter]::new($script:logFile, $true, [System.Text.Encoding]::UTF8)
$script:sigMissing = $false
$script:sigInvalid = $false

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

# 质量门禁：任一检查失败立即中止，避免从红测试/格式漂移的工作树出包。
function Invoke-Check([string]$label, [scriptblock]$command) {
    Say ('      ' + $label)
    & $command 2>&1 | ForEach-Object { Write-Host $_; WriteLog $_ }
    if ($LASTEXITCODE -ne 0) {
        Say ('      ' + $label + ' 失败，已中止。')
        exit 1
    }
}

function Test-WorktreeClean {
    $dirty = @(git status --porcelain)
    if ($LASTEXITCODE -ne 0) {
        Say '      git status 失败，已中止。'
        exit 1
    }
    if ($dirty.Count -eq 0) {
        Say '      工作树干净'
        return
    }
    if (-not $AllowDirty) {
        Say '      工作树不干净，已中止。请先提交或清理改动，或显式使用 -AllowDirty。'
        $dirty | ForEach-Object { Say ('        ' + $_) }
        exit 1
    }
    Say '      工作树不干净（-AllowDirty）：继续，但本次产物不作为发布候选。'
    $dirty | ForEach-Object { Say ('        ' + $_) }
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

# 读取 .sig 的 trusted comment，返回 CLI 写入的版本号。
# .sig 是 base64 编码的 minisign 文本；兼容直接读取裸文本的情况。
# 旧版 CLI（< 2.11.5）不写 version，返回 $null。
function Get-SigVersion([string]$path) {
    try {
        $raw = (Get-Content -Raw -LiteralPath $path).Trim()
        try {
            $text = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($raw))
        } catch {
            $text = $raw
        }
        $line = ($text -split "`r?`n" | Where-Object { $_ -like 'trusted comment:*' } | Select-Object -First 1)
        if (-not $line) { return $null }
        $m = [regex]::Match($line, '(?:^|\s)version:(\S+)')
        if ($m.Success) { return $m.Groups[1].Value }
    } catch {
        return $null
    }
    return $null
}

# 发布门的最后一道：用 tauri.conf 的公钥真正验证安装包 .sig，
# 而不是只看文件名里的版本。脚本和 prepare-release 共用同一套 minisign 解析。
function Invoke-ReleaseSignatureCheck([string]$installer, [string]$signature) {
    $configPath = Join-Path $root 'src-tauri\tauri.conf.json'
    $nodeArgs = @(
        'scripts/verify-update-signature.mjs',
        '--installer', $installer,
        '--signature', $signature,
        '--config', $configPath,
        '--version', $tauriVersion,
        '--file', (Split-Path -Leaf $installer)
    )
    & node @nodeArgs 2>&1 | ForEach-Object { Write-Host $_; WriteLog $_ }
    if ($LASTEXITCODE -ne 0) {
        Say '      发布签名校验失败，本次构建不能作为更新包发布。'
        exit 1
    }
    Say '      发布签名校验通过'
}

# 签名通过后生成 latest.json / SHA256SUMS。未提供发布仓时只做签名校验，
# 避免把某个 GitHub 账号硬编码进构建脚本。
function Invoke-ReleaseStaging([string]$installer, [string]$signature) {
    $nodeArgs = @(
        'scripts/prepare-release.mjs',
        '--installer', $installer,
        '--signature', $signature,
        '--version', $tauriVersion
    )
    if ($GiteeRepo) { $nodeArgs += @('--gitee-repo', $GiteeRepo) }
    if ($GitHubRepo) { $nodeArgs += @('--github-repo', $GitHubRepo) }
    if ($ReleaseNotes) { $nodeArgs += @('--notes-file', $ReleaseNotes) }
    & node @nodeArgs 2>&1 | ForEach-Object { Write-Host $_; WriteLog $_ }
    if ($LASTEXITCODE -ne 0) {
        Say '      发布暂存生成失败，本次构建不能作为更新包发布。'
        exit 1
    }
}

Write-Host ''
Say '========================================'
Say 'PRTS Terminal 发布构建'
Say '========================================'

# ── 1/3 前置检查 ──
Say ''
Say '[1/4] 前置检查'
$missing = @()
foreach ($tool in 'node', 'npm', 'python', 'cargo', 'rustc', 'git') {
    if (Get-Command $tool -ErrorAction SilentlyContinue) {
        Say ('      {0,-6} 就绪' -f $tool)
    } else {
        Say ('      {0,-6} 缺失，需要先安装' -f $tool)
        $missing += $tool
    }
}
if ($missing.Count -gt 0) { Say '存在缺失工具，已中止。'; exit 1 }

# 版本一致性：关于页读 Cargo 版本，更新器比较 tauri.conf 版本；
# 两者不一致时，界面显示与升级判断会各说各话。
Say ''
Say '      版本一致性'
$pkgVersion = (Get-Content -Raw -LiteralPath (Join-Path $root 'package.json') | ConvertFrom-Json).version
$tauriVersion = (Get-Content -Raw -LiteralPath (Join-Path $root 'src-tauri\tauri.conf.json') | ConvertFrom-Json).version
$cargoLine = Select-String -LiteralPath (Join-Path $root 'src-tauri\Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
$cargoVersion = if ($cargoLine) { $cargoLine.Matches[0].Groups[1].Value } else { '' }
Say ('      package.json     {0}' -f $pkgVersion)
Say ('      tauri.conf.json  {0}' -f $tauriVersion)
Say ('      Cargo.toml       {0}' -f $cargoVersion)
if ($pkgVersion -ne $tauriVersion -or $pkgVersion -ne $cargoVersion) {
    Say '      版本不一致，已中止。请先把三处版本改成同一个值。'
    exit 1
}

$drive = $root.Substring(0, 1)
$freeGB = [math]::Round((Get-PSDrive -Name $drive).Free / 1GB, 1)
Say ('      构建盘 {0}: 剩余 {1} GB' -f $drive, $freeGB)
if ($freeGB -lt 20) { Say '      注意：剩余空间偏低，构建可能失败。' }

# ── 2/4 质量门禁 ──
Say ''
Say '[2/4] 质量门禁'
if ($DryRun) {
    Say '      -DryRun：跳过测试、格式与静态检查。'
} else {
    Test-WorktreeClean
    Invoke-Check 'npm test' { npm test }
    Invoke-Check 'cargo fmt --check' { cargo fmt --manifest-path src-tauri/Cargo.toml -- --check }
    Invoke-Check 'cargo clippy' { cargo clippy --tests --lib --manifest-path src-tauri/Cargo.toml -- -D warnings }
    Invoke-Check 'cargo test --lib' { cargo test --lib --manifest-path src-tauri/Cargo.toml }
}

# ── 3/4 构建 ──
Say ''
Say '[3/4] 开始构建（前端 → MAA 桥 → Rust 发布编译 → NSIS 打包）'
Say '      冷启动通常十几分钟起；热启动约 2-3 分钟。'
Say '      cargo 的进度条会显示 已完成/总数，例如 Building [====> ] 326/574。'
Say ''

if ($DryRun) {
    Say '      -DryRun：只做检查与预告，不执行构建。'
    Say ''
    Say '[4/4] 跳过构建（-DryRun）'
    Say ('      target 目录当前：' + (SizeOf (Join-Path $root 'src-tauri\target')))
    Say ''
    Say '检查完成。去掉 -DryRun 即可真正构建。'
    exit 0
}

# 签名链：缺少私钥或密码时直接中止，避免产出没有 .sig 的安装包。
# -NoSign 是显式的本地冒烟模式：跳过签名，同时关闭 updater artifact。
# 只检查存在性，不输出私钥内容或密码。
Say ''
Say '      更新签名'
if ($NoSign) {
    Say '      -NoSign：跳过更新签名；本次产物只能用于本地安装/冒烟，不能作为更新包。'
} else {
    $keyPath = $env:TAURI_SIGNING_PRIVATE_KEY_PATH
    $keyInline = $env:TAURI_SIGNING_PRIVATE_KEY
    if (-not $keyPath -and -not $keyInline) {
        Say '      缺少 TAURI_SIGNING_PRIVATE_KEY_PATH 或 TAURI_SIGNING_PRIVATE_KEY，已中止。'
        exit 1
    }
    if ($keyPath -and -not (Test-Path -LiteralPath $keyPath)) {
        Say ('      私钥文件不存在：{0}' -f $keyPath)
        exit 1
    }
    # Tauri CLI 2.11.x 的构建签名只读 TAURI_SIGNING_PRIVATE_KEY 的内容；
    # TAURI_SIGNING_PRIVATE_KEY_PATH 不参与构建签名。这里把路径转成内容，
    # 仍然不回显私钥，也不把内容写进日志。
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY -and $keyPath) {
        try {
            $env:TAURI_SIGNING_PRIVATE_KEY = (Get-Content -Raw -LiteralPath $keyPath).Trim()
            Say '      已从私钥路径加载签名内容（不输出内容）'
        } catch {
            Say ('      读取私钥文件失败：{0}' -f $_.Exception.Message)
            exit 1
        }
    }
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
        Say '      缺少 TAURI_SIGNING_PRIVATE_KEY_PASSWORD，已中止。'
        exit 1
    }
    Say '      私钥与密码已就位（不输出内容）'
}

if ($Clean) {
    Say '      -Clean：清空构建目录'
    Push-Location (Join-Path $root 'src-tauri'); cargo clean | Out-Null; Pop-Location
    Push-Location (Join-Path $root 'bridge'); cargo clean | Out-Null; Pop-Location
}
if ($Jobs -gt 0) { $env:CARGO_BUILD_JOBS = "$Jobs"; Say ('      并行编译数限制为 {0}' -f $Jobs) }
$tauriArgs = @()
if ($Config) {
    if (-not (Test-Path -LiteralPath $Config)) {
        Say ('      附加配置不存在：{0}' -f $Config)
        exit 1
    }
    Say ('      附加配置：{0}' -f $Config)
    $tauriArgs += @('--config', $Config)
}
if ($ReleaseNotes -and -not (Test-Path -LiteralPath $ReleaseNotes)) {
    Say ('      Release notes 不存在：{0}' -f $ReleaseNotes)
    exit 1
}
if ($NoSign) {
    $noSignDir = Join-Path $root '.tmp'
    New-Item -ItemType Directory -Force -Path $noSignDir | Out-Null
    $noSignConfig = Join-Path $noSignDir ('tauri.nosign.' + $stamp + '.json')
    [System.IO.File]::WriteAllText(
        $noSignConfig,
        '{"bundle":{"createUpdaterArtifacts":false}}',
        [System.Text.UTF8Encoding]::new($false)
    )
    Say ('      未签名配置：{0}' -f $noSignConfig)
    $tauriArgs += @('--config', $noSignConfig)
    $tauriArgs += @('--no-sign')
}

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$npmArgs = @('run', 'tauri', 'build')
if ($tauriArgs.Count -gt 0) {
    $npmArgs += @('--')
    $npmArgs += $tauriArgs
}
& npm @npmArgs 2>&1 | ForEach-Object { Write-Host $_; WriteLog $_ }
$code = $LASTEXITCODE
$sw.Stop()

# ── 3/3 结果 ──
Say ''
if ($code -eq 0) {
    Say ('[4/4] 构建成功，耗时 {0}' -f $sw.Elapsed.ToString('hh\:mm\:ss'))
    Say '      安装包：'
    $nsis = Join-Path $root 'src-tauri\target\release\bundle\nsis'
    $versionToken = '_' + [regex]::Escape($tauriVersion) + '_'
    $pkgs = Get-ChildItem $nsis -Filter '*.exe' -ErrorAction SilentlyContinue
    $currentPkgs = @($pkgs | Where-Object { $_.Name -match $versionToken })
    $stalePkgs = @($pkgs | Where-Object { $_.Name -notmatch $versionToken })
    if ($currentPkgs.Count -gt 0) {
        $currentPkgs | ForEach-Object { Say ('        {0}  ({1:N2} MB)' -f $_.Name, ($_.Length / 1MB)) }
    } else {
        Say '        未找到当前版本的安装包（构建成功但 bundle 目录里没有对应产物，请检查）'
        $script:sigMissing = $true
    }
    if ($stalePkgs.Count -gt 0) {
        Say '      旧版本安装包（保留在目录中，不参与本次校验）：'
        $stalePkgs | ForEach-Object { Say ('        {0}  ({1:N2} MB)' -f $_.Name, ($_.Length / 1MB)) }
    }
    if ($NoSign) {
        if ($currentPkgs.Count -gt 0) {
            $staleCurrentSigs = @(Get-ChildItem $nsis -Filter '*.sig' -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -match $versionToken })
            foreach ($oldSig in $staleCurrentSigs) {
                Remove-Item -LiteralPath $oldSig.FullName -Force
                Say ('      已移除未签名构建前的旧签名：{0}' -f $oldSig.Name)
            }
        }
        Say '      未签名构建：跳过 .sig 校验、latest.json 生成与发布暂存。'
    } else {
        Say '      更新签名：'
        $sigs = Get-ChildItem $nsis -Filter '*.sig' -ErrorAction SilentlyContinue
        $currentSigs = @($sigs | Where-Object { $_.Name -match $versionToken })
        $staleSigs = @($sigs | Where-Object { $_.Name -notmatch $versionToken })
        if ($currentSigs.Count -gt 0) {
            foreach ($sig in $currentSigs) {
                $sigVersion = Get-SigVersion $sig.FullName
                if (-not $sigVersion) {
                    Say ('        {0}  ({1} B)  缺少 version 字段（构建 CLI 可能低于 2.11.5）' -f $sig.Name, $sig.Length)
                    $script:sigInvalid = $true
                } elseif ($sigVersion -ne $tauriVersion) {
                    Say ('        {0}  ({1} B)  签名版本 {2} 与配置版本 {3} 不一致' -f $sig.Name, $sig.Length, $sigVersion, $tauriVersion)
                    $script:sigInvalid = $true
                } else {
                    Say ('        {0}  ({1} B, version {2})' -f $sig.Name, $sig.Length, $sigVersion)
                }
            }
        } else {
            Say '        错误：未找到当前版本的 .sig 更新签名。'
            $script:sigMissing = $true
        }
        if ($staleSigs.Count -gt 0) {
            Say '      旧版本签名（保留在目录中，不参与本次校验）：'
            foreach ($sig in $staleSigs) {
                $sigVersion = Get-SigVersion $sig.FullName
                $sigLabel = if ($sigVersion) { $sigVersion } else { '未标注' }
                Say ('        {0}  ({1} B, version {2})' -f $sig.Name, $sig.Length, $sigLabel)
            }
        }
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
    if ($NoSign) {
        if ($currentPkgs.Count -eq 0) {
            Say '未找到当前版本的安装包，本次未签名构建失败。'
            exit 1
        }
        Say '未签名构建完成：产物只能用于本地安装/冒烟，不能作为更新包。'
        Say '完成。'
    } elseif ($script:sigMissing -or $script:sigInvalid) {
        Say '签名产物缺失或版本校验失败，本次构建不能作为更新包发布。'
        exit 1
    } else {
        if ($currentPkgs.Count -gt 1) {
            Say '      注意：发现多个同版本安装包，发布门使用第一个。'
        }
        $installer = $currentPkgs[0]
        $sig = $currentSigs | Where-Object { $_.Name -eq ($installer.Name + '.sig') } | Select-Object -First 1
        if (-not $sig) {
            Say ('      错误：找不到与安装包配对的签名：{0}.sig' -f $installer.Name)
            exit 1
        }
        Invoke-ReleaseSignatureCheck -installer $installer.FullName -signature $sig.FullName
        if ($GiteeRepo -or $GitHubRepo) {
            Invoke-ReleaseStaging -installer $installer.FullName -signature $sig.FullName
            Say '发布暂存已生成。'
        } else {
            Say '未提供 -GiteeRepo/-GitHubRepo，跳过 latest.json 生成；只完成签名校验。'
        }
        Say '完成。'
    }
} else {
    Say ('[4/4] 构建失败（退出码 {0}），耗时 {1}' -f $code, $sw.Elapsed.ToString('hh\:mm\:ss'))
    Say '      日志末尾 40 行：'
    Say '      ----------------------------------------'
    if (Test-Path $script:logFile) {
        Get-Content -LiteralPath $script:logFile -Tail 40 | ForEach-Object { Write-Host $_ }
    }
    Say '      ----------------------------------------'
    Say ('      完整日志：' + $script:logFile)
    exit $code
}
