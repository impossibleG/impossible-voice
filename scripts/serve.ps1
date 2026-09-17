param([string]$ArtifactRoot = '')

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ArtifactRoot)) {
    $ArtifactRoot = Join-Path $repositoryRoot 'runtime-artifacts'
}

$packagedBinary = Join-Path $repositoryRoot $(if ($IsWindows) { 'impossible-voice.exe' } else { 'impossible-voice' })
if (Test-Path -LiteralPath $packagedBinary -PathType Leaf) {
    & $packagedBinary serve --artifact-root $ArtifactRoot
}
else {
    Push-Location $repositoryRoot
    try {
        & cargo run --locked --release --bin impossible-voice -- serve --artifact-root $ArtifactRoot
    }
    finally {
        Pop-Location
    }
}

if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
