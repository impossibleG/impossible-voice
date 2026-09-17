param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9_]+-[a-z0-9_]+$')]
    [string]$PlatformLabel,
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath,
    [string]$OutputDirectory = 'dist'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$binary = [IO.Path]::GetFullPath($BinaryPath)
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw 'Release binary does not exist.'
}

$dirty = & git -c "safe.directory=$($repositoryRoot.Replace('\', '/'))" -C $repositoryRoot status --porcelain --untracked-files=all
if ($LASTEXITCODE -ne 0) { throw 'Unable to verify repository cleanliness.' }
if ($null -ne $dirty -and @($dirty).Count -ne 0) {
    throw 'Release packaging requires a clean tracked source tree.'
}

& (Join-Path $PSScriptRoot 'privacy-scan.ps1')
if ($LASTEXITCODE -ne 0) { throw 'Privacy scan failed.' }

$outputRoot = [IO.Path]::GetFullPath((Join-Path $repositoryRoot $OutputDirectory))
$relativeOutput = [IO.Path]::GetRelativePath($repositoryRoot, $outputRoot).Replace('\', '/')
if ([IO.Path]::IsPathRooted($relativeOutput) -or $relativeOutput.Split('/') -contains '..') {
    throw 'Output directory must be inside the repository.'
}

$versionLine = Select-String -LiteralPath (Join-Path $repositoryRoot 'Cargo.toml') -Pattern '^version = "([0-9]+\.[0-9]+\.[0-9]+)"$' | Select-Object -First 1
if ($null -eq $versionLine) { throw 'Workspace version could not be determined.' }
$version = $versionLine.Matches[0].Groups[1].Value
$packageName = "impossible-voice-$version-$PlatformLabel"
$stage = Join-Path $outputRoot $packageName
$archive = Join-Path $outputRoot "$packageName.zip"
if ((Test-Path -LiteralPath $stage) -or (Test-Path -LiteralPath $archive)) {
    throw 'Release output already exists; use an empty output directory.'
}

New-Item -ItemType Directory -Path (Join-Path $stage 'scripts') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $stage 'config') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $stage 'docs') -Force | Out-Null

$binaryName = if ($PlatformLabel.StartsWith('windows-')) { 'impossible-voice.exe' } else { 'impossible-voice' }
Copy-Item -LiteralPath $binary -Destination (Join-Path $stage $binaryName)
foreach ($name in @('README.md', 'NOTICE.md', 'THIRD_PARTY_NOTICES.md', 'LICENSE-MIT', 'LICENSE-APACHE')) {
    Copy-Item -LiteralPath (Join-Path $repositoryRoot $name) -Destination (Join-Path $stage $name)
}
foreach ($name in @('setup.ps1', 'serve.ps1', 'setup.sh', 'serve.sh', 'smoke-public.ps1')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $stage 'scripts' $name)
}
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'config/impossible-voice.example.toml') -Destination (Join-Path $stage 'config/impossible-voice.example.toml')
foreach ($name in @('api.md', 'artifact-licenses.md', 'operations.md', 'privacy.md', 'product-contract.md')) {
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs' $name) -Destination (Join-Path $stage 'docs' $name)
}

Compress-Archive -LiteralPath $stage -DestinationPath $archive -CompressionLevel Optimal
Write-Output $archive
