param(
    [string]$ArtifactRoot = '',
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ArtifactRoot)) {
    $ArtifactRoot = Join-Path $repositoryRoot 'runtime-artifacts'
}

$packagedBinary = Join-Path $repositoryRoot $(if ($IsWindows) { 'impossible-voice.exe' } else { 'impossible-voice' })
$arguments = @('setup', '--artifact-root', $ArtifactRoot)
if ($Offline) { $arguments += '--offline' }
if (Test-Path -LiteralPath $packagedBinary -PathType Leaf) {
    & $packagedBinary @arguments
}
else {
    Push-Location $repositoryRoot
    try {
        & cargo run --locked --release --bin impossible-voice -- @arguments
    }
    finally {
        Pop-Location
    }
}

if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
