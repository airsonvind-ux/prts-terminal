# Run data-contract tests that depend on the real arknights.db.
# The default suite skips these via cfg_attr(ignore); this script enables the feature.
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$db = Join-Path $root 'src-tauri/resources/arknights.db'
$manifest = Join-Path $root 'src-tauri/Cargo.toml'
$cargoConfig = Join-Path $root 'src-tauri/.cargo/config.toml'

if (-not (Test-Path -LiteralPath $db -PathType Leaf)) {
    Write-Error "Missing data-contract database: $db. Prepare a local arknights.db first; it is not tracked by git."
    exit 1
}

$file = Get-Item -LiteralPath $db
$hash = (Get-FileHash -LiteralPath $db -Algorithm SHA256).Hash
Write-Host ("data-contract: {0} ({1:N0} bytes, SHA-256 {2})" -f $file.FullName, $file.Length, $hash)

Push-Location $root
try {
    # 不能只靠当前目录加载 src-tauri/.cargo/config.toml；从仓库根目录
    # 带 --manifest-path 运行时，Cargo 不会读取 src-tauri 下的配置，
    # libsqlite3-sys 会按动态 CRT 编译，最终以 LNK2019 失败。
    # 显式传入配置文件，保证 crt-static 与 Tauri 构建一致。
    # cargo writes progress to stderr; Stop would treat native stderr as terminating.
    $ErrorActionPreference = 'Continue'
    & cargo test --lib --manifest-path $manifest --features data-contract --config $cargoConfig @CargoArgs
    $code = $LASTEXITCODE
}
finally {
    Pop-Location
}

exit $code
