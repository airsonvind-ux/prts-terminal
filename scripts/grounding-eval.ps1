# Run the offline grounding evaluator against a recorded PRTS session.
[CmdletBinding()]
param(
    [string]$Session = "",
    [string]$Case = "",
    [string]$Turn = "",
    [switch]$Check,
    [switch]$List
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$script = Join-Path $root 'evals/grounding/run.py'

if (-not (Test-Path -LiteralPath $script -PathType Leaf)) {
    Write-Error "Missing grounding evaluator: $script"
    exit 1
}

$pythonArgs = @($script)
if ($Check) { $pythonArgs += '--check' }
if ($List) { $pythonArgs += '--list' }
if ($Session) { $pythonArgs += @('--session', $Session) }
if ($Case) { $pythonArgs += @('--case', $Case) }
if ($Turn) { $pythonArgs += @('--turn', $Turn) }

Push-Location $root
try {
    & python @pythonArgs
    $code = $LASTEXITCODE
}
finally {
    Pop-Location
}

exit $code
