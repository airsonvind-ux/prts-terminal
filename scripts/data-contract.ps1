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

if (-not (Test-Path -LiteralPath $db -PathType Leaf)) {
    Write-Error "Missing data-contract database: $db. Prepare a local arknights.db first; it is not tracked by git."
    exit 1
}

$file = Get-Item -LiteralPath $db
$hash = (Get-FileHash -LiteralPath $db -Algorithm SHA256).Hash
Write-Host ("data-contract: {0} ({1:N0} bytes, SHA-256 {2})" -f $file.FullName, $file.Length, $hash)

Push-Location $root
try {
    # cargo writes progress to stderr; Stop would treat native stderr as terminating.
    $ErrorActionPreference = 'Continue'
    & cargo test --lib --manifest-path $manifest --features data-contract @CargoArgs
    $code = $LASTEXITCODE
}
finally {
    Pop-Location
}

exit $code
