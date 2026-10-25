<#
.SYNOPSIS
    推送权威数据库到发布资源目录，并完成数据契约与清单回写。

.DESCRIPTION
    日常使用的数据推送入口。脚本会：
      1. 从 data-source/ArknightsGameData 读取数据源提交；
      2. 默认用当天日期作为 dataVersion；
      3. 调用 maintenance/tools/push.py 推送数据库；
      4. 运行 data-contract.ps1；
      5. 把契约结果回写到 DATA_MANIFEST.json。

    推送前请先关闭正在运行的 PRTS 终端，避免数据库文件被占用。

.PARAMETER DryRun
    只打印计划、数据版本和表行数，不写数据库、不跑契约、不写清单。

.PARAMETER DataVersion
    数据快照版本，格式 YYYY.MM.DD[.N]。省略时使用当天日期；
    如果目标库已经有当天的版本，会自动递增 .1、.2。

.PARAMETER SourceCommit
    数据源提交或维护快照标识。省略时读取本地数据源的 HEAD。

.PARAMETER SourceNote
    写入清单的来源说明。省略时根据数据源提交自动生成。

.PARAMETER SchemaVersion
    数据库结构版本，默认 1。只有数据库结构变化时才需要改。

.PARAMETER SkipContract
    跳过数据契约。清单会保持 pending，不建议用于发布。

.EXAMPLE
    .\scripts\push-data.ps1 -DryRun

.EXAMPLE
    .\scripts\push-data.ps1

.EXAMPLE
    .\scripts\push-data.ps1 -DataVersion 2026.10.23.1

.EXAMPLE
    .\scripts\push-data.ps1 -SourceCommit "maintenance-2026-10-23" -SourceNote "数据源不在本机，使用维护快照"
#>
[CmdletBinding()]
param(
    [switch]$DryRun,
    [string]$DataVersion,
    [string]$SourceCommit,
    [string]$SourceNote,
    [int]$SchemaVersion = 1,
    [switch]$SkipContract
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$sourceDb = Join-Path $root 'data-pipeline\maintenance\db\arknights.db'
$targetDb = Join-Path $root 'src-tauri\resources\arknights.db'
$manifestPath = Join-Path $root 'src-tauri\resources\DATA_MANIFEST.json'
$pushScript = Join-Path $root 'data-pipeline\maintenance\tools\push.py'
$contractScript = Join-Path $root 'scripts\data-contract.ps1'
$dataSource = Join-Path $root 'data-source\ArknightsGameData'
$dataVersionPattern = '^\d{4}\.\d{2}\.\d{2}(?:\.\d+)?$'

function Say {
    param([string]$Text)
    Write-Host $Text
}

function Fail {
    param([string]$Text)
    throw $Text
}

function Get-PythonExe {
    $cmd = Get-Command python -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $cmd) {
        Fail '找不到 python。请安装 Python 3 并加入 PATH。'
    }
    return $cmd.Source
}

function Get-DbMeta {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    $code = "import json,sqlite3,sys; con=sqlite3.connect(sys.argv[1]); row=con.execute('SELECT dataVersion, generatedAt, sourceCommit, schemaVersion FROM meta LIMIT 1').fetchone(); con.close(); print(json.dumps(None if row is None else {'dataVersion': row[0], 'generatedAt': row[1], 'sourceCommit': row[2], 'schemaVersion': int(row[3])}, ensure_ascii=False))"
    $raw = & $pythonExe -c $code $Path 2>$null
    if ($LASTEXITCODE -ne 0) {
        return $null
    }
    $text = ($raw -join "`n").Trim()
    if ([string]::IsNullOrWhiteSpace($text) -or $text -eq 'null') {
        return $null
    }
    try {
        return $text | ConvertFrom-Json
    }
    catch {
        return $null
    }
}

function Get-FileSha {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Get-ShortCommit {
    param([string]$Commit)
    if ($Commit.Length -ge 7) {
        return $Commit.Substring(0, 7)
    }
    return $Commit
}

try {
    $pythonExe = Get-PythonExe

    if (-not (Test-Path -LiteralPath $sourceDb -PathType Leaf)) {
        Fail "权威库不存在：$sourceDb"
    }
    if (-not (Test-Path -LiteralPath $pushScript -PathType Leaf)) {
        Fail "推送脚本不存在：$pushScript"
    }
    if ($SchemaVersion -lt 1) {
        Fail 'SchemaVersion 必须 >= 1。'
    }

    $git = Get-Command git -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    $commitSubject = $null

    if ([string]::IsNullOrWhiteSpace($SourceCommit)) {
        if (-not (Test-Path -LiteralPath (Join-Path $dataSource '.git'))) {
            Fail "数据源目录不存在或不是 Git 仓库：$dataSource。请先同步数据源，或用 -SourceCommit 指定快照标识。"
        }
        if (-not $git) {
            Fail '找不到 git，无法读取数据源提交。请安装 Git，或用 -SourceCommit 指定快照标识。'
        }
        $SourceCommit = (& $git.Source -C $dataSource rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($SourceCommit)) {
            Fail '读取数据源提交失败。请检查 data-source/ArknightsGameData。'
        }
    }

    if ($git -and (Test-Path -LiteralPath (Join-Path $dataSource '.git'))) {
        $commitSubject = (& $git.Source -C $dataSource log -1 --format=%s $SourceCommit 2>$null).Trim()
        if ($LASTEXITCODE -ne 0) {
            $commitSubject = $null
        }
    }

    if ([string]::IsNullOrWhiteSpace($SourceNote)) {
        $SourceNote = "数据源 $(Get-ShortCommit $SourceCommit)"
        if (-not [string]::IsNullOrWhiteSpace($commitSubject)) {
            $SourceNote += "；$commitSubject"
        }
    }

    if ([string]::IsNullOrWhiteSpace($DataVersion)) {
        $today = Get-Date -Format 'yyyy.MM.dd'
        $targetMeta = Get-DbMeta $targetDb
        if ($targetMeta -and $targetMeta.dataVersion) {
            $m = [regex]::Match([string]$targetMeta.dataVersion, '^(\d{4}\.\d{2}\.\d{2})(?:\.(\d+))?$')
            if ($m.Success -and $m.Groups[1].Value -eq $today) {
                $next = if ($m.Groups[2].Success) { [int]$m.Groups[2].Value + 1 } else { 1 }
                $DataVersion = "$today.$next"
            }
            else {
                $DataVersion = $today
            }
        }
        else {
            $DataVersion = $today
        }
    }
    elseif ($DataVersion -notmatch $dataVersionPattern) {
        Fail "DataVersion 格式无效：$DataVersion（应为 YYYY.MM.DD[.N]）。"
    }

    $sourceHash = Get-FileSha $sourceDb
    $targetHash = Get-FileSha $targetDb

    Say '====== PRTS 数据推送 ======'
    Say ("源库       {0}" -f $sourceDb)
    Say ("目标       {0}" -f $targetDb)
    Say ("数据版本   {0}" -f $DataVersion)
    Say ("数据源     {0}" -f $SourceCommit)
    Say ("来源说明   {0}" -f $SourceNote)
    Say ("源库 SHA   {0}" -f $sourceHash)
    if ($targetHash) {
        Say ("目标 SHA   {0}" -f $targetHash)
    }
    else {
        Say '目标 SHA   （不存在）'
    }

    $pushArgs = @(
        '--data-version', $DataVersion,
        '--source-commit', $SourceCommit,
        '--source-note', $SourceNote,
        '--schema-version', "$SchemaVersion"
    )

    $previousEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        if ($DryRun) {
            & $pythonExe $pushScript '--dry-run' @pushArgs
            $pushExit = $LASTEXITCODE
        }
        else {
            & $pythonExe $pushScript @pushArgs
            $pushExit = $LASTEXITCODE
        }
    }
    finally {
        $ErrorActionPreference = $previousEap
    }
    if ($pushExit -ne 0) {
        if ($DryRun) { Fail 'push.py --dry-run 失败。' }
        Fail 'push.py 推送失败。'
    }
    if ($DryRun) {
        Say ''
        Say '干跑完成：未写数据库、未跑契约、未写清单。'
        exit 0
    }

    $targetMeta = Get-DbMeta $targetDb
    if (-not $targetMeta) {
        Fail '推送后目标库缺少 meta 表。'
    }
    if ([string]$targetMeta.dataVersion -ne $DataVersion) {
        Fail "推送后 meta.dataVersion 不一致：$($targetMeta.dataVersion) != $DataVersion"
    }
    if ([string]$targetMeta.sourceCommit -ne $SourceCommit) {
        Fail "推送后 meta.sourceCommit 不一致：$($targetMeta.sourceCommit) != $SourceCommit"
    }
    if ([int]$targetMeta.schemaVersion -ne $SchemaVersion) {
        Fail "推送后 meta.schemaVersion 不一致：$($targetMeta.schemaVersion) != $SchemaVersion"
    }

    $passed = 0
    $ignored = 0

    if ($SkipContract) {
        Say ''
        Say '已跳过 data-contract：清单保持 pending，不建议用于发布。'
    }
    else {
        Say ''
        Say '====== 数据契约 ======'
        $powershellExe = Get-Command powershell.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $powershellExe) {
            Fail '找不到 powershell.exe。'
        }
        $previousEap = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $contractOutput = & $powershellExe.Source -NoProfile -ExecutionPolicy Bypass -File $contractScript 2>&1
            $contractExit = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $previousEap
        }
        $contractText = ($contractOutput | Out-String)

        if ($contractExit -ne 0) {
            Write-Host $contractText
            Fail "data-contract 失败；数据库已推送，但清单未回写。修复后用 -DataVersion $DataVersion 重跑，避免生成新版本。"
        }

        $contractSummary = ($contractText -split "`n" | Where-Object { $_ -match '^test result:' }) -join "`n"
        if (-not [string]::IsNullOrWhiteSpace($contractSummary)) {
            Say $contractSummary.TrimEnd()
        }
        else {
            Write-Host $contractText
        }

        $contractMatches = [regex]::Matches(
            $contractText,
            'test result:\s+(ok|FAILED)\.\s+(\d+) passed;\s+(\d+) failed;\s+(\d+) ignored'
        )
        if ($contractMatches.Count -eq 0) {
            Fail '无法从 data-contract 输出解析测试结果。'
        }

        $lastMatch = $contractMatches[$contractMatches.Count - 1]
        $passed = [int]$lastMatch.Groups[2].Value
        $failed = [int]$lastMatch.Groups[3].Value
        $ignored = [int]$lastMatch.Groups[4].Value
        if ($lastMatch.Groups[1].Value -ne 'ok' -or $failed -ne 0) {
            Fail "data-contract 未通过：$failed failed。清单未回写。"
        }

        $manifestArgs = @(
            '--manifest-only',
            '--data-contract-status', 'passed',
            '--data-contract-passed', "$passed",
            '--data-contract-ignored', "$ignored",
            '--source-note', $SourceNote
        )
        $previousEap = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            & $pythonExe $pushScript @manifestArgs
            $manifestExit = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $previousEap
        }
        if ($manifestExit -ne 0) {
            Fail 'DATA_MANIFEST.json 回写失败。'
        }
    }

    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ([string]$manifest.dataVersion -ne $DataVersion) {
        Fail "清单 dataVersion 不一致：$($manifest.dataVersion) != $DataVersion"
    }
    $finalHash = Get-FileSha $targetDb
    if ([string]$manifest.database.sha256 -ne $finalHash) {
        Fail '清单中的数据库 SHA-256 与目标库不一致。'
    }
    if (-not $SkipContract) {
        if ([string]$manifest.dataContract.status -ne 'passed') {
            Fail '清单 dataContract.status 不是 passed。'
        }
        if ([int]$manifest.dataContract.passed -ne $passed) {
            Fail '清单 dataContract.passed 与契约结果不一致。'
        }
        if ([int]$manifest.dataContract.ignored -ne $ignored) {
            Fail '清单 dataContract.ignored 与契约结果不一致。'
        }
    }

    Say ''
    Say '====== 完成 ======'
    Say ("数据版本   {0}" -f $DataVersion)
    Say ("数据源     {0}" -f $SourceCommit)
    Say ("数据库 SHA {0}" -f $finalHash)
    if (-not $SkipContract) {
        Say ("数据契约   passed {0} / ignored {1}" -f $passed, $ignored)
    }
    Say '推送完成：数据库、meta 与清单已更新。'
    exit 0
}
catch {
    Write-Host ("错误：" + $_.Exception.Message) -ForegroundColor Red
    exit 1
}
