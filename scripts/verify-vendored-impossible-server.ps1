[CmdletBinding()]
param(
    [string]$Root = (Join-Path $PSScriptRoot '../vendor/impossible-server'),

    [switch]$Release
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'source-snapshot-security.ps1')

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-SafeRelativePath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or
        [IO.Path]::IsPathRooted($Path) -or
        $Path.Contains('\') -or
        $Path.Split('/') -contains '..' -or
        $Path.Split('/') -contains '.' -or
        $Path.IndexOf([char]0) -ge 0) {
        return $false
    }

    $prohibited = @('.git', 'target', 'dist', '.worktrees', '.agents', '.codex')
    foreach ($segment in $Path.Split('/')) {
        if ($segment -in $prohibited) {
            return $false
        }
    }
    return $true
}

$rootPath = [IO.Path]::GetFullPath($Root)
if (-not (Test-Path -LiteralPath $rootPath -PathType Container)) {
    throw "Source snapshot does not exist: $rootPath"
}
$treeEntries = @(Get-SafeTreeInventory $rootPath)
$rootPath = Get-CanonicalExistingPath $rootPath

$manifestPath = Join-Path $rootPath 'source-sync.json'
$schemaPath = Join-Path $rootPath 'schemas/source-sync-manifest.schema.json'
foreach ($requiredPath in @($manifestPath, $schemaPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "Required snapshot metadata is missing: $requiredPath"
    }
}

$manifestJson = Get-Content -LiteralPath $manifestPath -Raw
$schemaJson = Get-Content -LiteralPath $schemaPath -Raw
$schema = $schemaJson | ConvertFrom-Json -Depth 64
if ([string]$schema.'$id' -cne 'https://github.com/impossibleG/impossible-server/schemas/source-sync-manifest.schema.json') {
    throw 'Source snapshot schema has the wrong product/schema identity marker.'
}
if (-not ($manifestJson | Test-Json -Schema $schemaJson)) {
    throw 'source-sync.json does not conform to its committed schema.'
}
$manifest = $manifestJson | ConvertFrom-Json -Depth 64
if ([int]$manifest.schema_version -ne 1 -or [string]$manifest.product -cne 'impossible-server-source-snapshot') {
    throw 'source-sync.json has the wrong product/schema identity marker.'
}
if ($Release -and [bool]$manifest.provisional) {
    throw 'Release verification rejects a provisional source snapshot. Canonicalize identity/history and regenerate it first.'
}

$requiredFiles = @(
    'Cargo.lock',
    'Cargo.toml',
    'LICENSE-APACHE',
    'LICENSE-MIT',
    'NOTICE.md',
    'crates/impossible-server-core/Cargo.toml',
    'crates/impossible-server-core/src/lib.rs',
    'crates/impossible-server-testkit/Cargo.toml',
    'crates/impossible-server-testkit/src/lib.rs',
    'rust-toolchain.toml',
    'schemas/source-sync-manifest.schema.json'
)

$manifestPaths = [string[]]@($manifest.files | ForEach-Object { [string]$_.path })
$sortedManifestPaths = [string[]]$manifestPaths.Clone()
[Array]::Sort($sortedManifestPaths, [StringComparer]::Ordinal)
if (($manifestPaths -join "`n") -cne ($sortedManifestPaths -join "`n")) {
    throw 'Manifest file entries must be sorted by ordinal relative path.'
}
if (($manifestPaths | Select-Object -Unique).Count -ne $manifestPaths.Count) {
    throw 'Manifest contains duplicate file paths.'
}

foreach ($path in $manifestPaths) {
    if (-not (Test-SafeRelativePath $path)) {
        throw "Manifest contains an unsafe or prohibited path: $path"
    }
}
foreach ($required in $requiredFiles) {
    if ($required -notin $manifestPaths) {
        throw "Manifest omits required source snapshot file: $required"
    }
}

$actualPaths = [string[]]@(
    $treeEntries |
        Where-Object { -not $_.IsDirectory -and $_.Relative -ne 'source-sync.json' } |
        ForEach-Object { [string]$_.Relative }
)
[Array]::Sort($actualPaths, [StringComparer]::Ordinal)
if (($actualPaths -join "`n") -cne ($manifestPaths -join "`n")) {
    $missing = @($manifestPaths | Where-Object { $_ -notin $actualPaths })
    $extra = @($actualPaths | Where-Object { $_ -notin $manifestPaths })
    $firstDifference = 0..([Math]::Max($actualPaths.Count, $manifestPaths.Count) - 1) |
        Where-Object {
            $_ -ge $actualPaths.Count -or $_ -ge $manifestPaths.Count -or
            $actualPaths[$_] -cne $manifestPaths[$_]
        } |
        Select-Object -First 1
    $actualDifference = if ($firstDifference -lt $actualPaths.Count) { $actualPaths[$firstDifference] } else { '<none>' }
    $manifestDifference = if ($firstDifference -lt $manifestPaths.Count) { $manifestPaths[$firstDifference] } else { '<none>' }
    throw "Snapshot inventory differs from its manifest. Missing=[$($missing -join ', ')] Extra=[$($extra -join ', ')] FirstDifference=$firstDifference actual=[$actualDifference] manifest=[$manifestDifference]"
}

$expectedDirectories = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($path in @($manifestPaths) + @('source-sync.json')) {
    $parent = [IO.Path]::GetDirectoryName($path.Replace('/', [IO.Path]::DirectorySeparatorChar))
    while (-not [string]::IsNullOrEmpty($parent)) {
        $expectedDirectories.Add($parent.Replace('\', '/')) | Out-Null
        $parent = [IO.Path]::GetDirectoryName($parent)
    }
}
$actualDirectories = [string[]]@(
    $treeEntries | Where-Object IsDirectory | ForEach-Object { [string]$_.Relative }
)
[Array]::Sort($actualDirectories, [StringComparer]::Ordinal)
$expectedDirectoryArray = [string[]]@($expectedDirectories)
[Array]::Sort($expectedDirectoryArray, [StringComparer]::Ordinal)
if (($actualDirectories -join "`n") -cne ($expectedDirectoryArray -join "`n")) {
    throw "Snapshot directory inventory differs from its manifest-derived structure. Expected=[$($expectedDirectoryArray -join ', ')] Actual=[$($actualDirectories -join ', ')]"
}

$treeLines = [Collections.Generic.List[string]]::new()
foreach ($entry in $manifest.files) {
    $path = [string]$entry.path
    $fullPath = [IO.Path]::GetFullPath((Join-Path $rootPath $path))
    Assert-CanonicalContainedPath $fullPath $rootPath 'Manifest file' | Out-Null
    $item = Get-Item -LiteralPath $fullPath -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Snapshot files must not be reparse points: $path"
    }
    $actualHash = Get-Sha256 $fullPath
    $actualSize = [IO.File]::ReadAllBytes($fullPath).LongLength
    if ($actualHash -cne [string]$entry.sha256 -or $actualSize -ne [long]$entry.size) {
        throw "Snapshot file digest or size mismatch: $path"
    }
    $treeLines.Add("$path`t$actualHash`t$actualSize`n")
}

$treeBytes = [Text.Encoding]::UTF8.GetBytes(($treeLines -join ''))
$treeHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($treeBytes)).ToLowerInvariant()
if ($treeHash -cne [string]$manifest.provisional_source_tree_sha256) {
    throw 'Snapshot source tree digest does not match the verified file inventory.'
}

$expectedLicensePaths = @('LICENSE-APACHE', 'LICENSE-MIT')
$licensePaths = @($manifest.license.files | ForEach-Object { [string]$_.path })
[Array]::Sort($licensePaths, [StringComparer]::Ordinal)
if (($licensePaths -join "`n") -cne ($expectedLicensePaths -join "`n")) {
    throw 'License metadata must identify exactly LICENSE-APACHE and LICENSE-MIT.'
}
foreach ($license in $manifest.license.files) {
    $entry = @($manifest.files | Where-Object { $_.path -ceq $license.path })
    if ($entry.Count -ne 1 -or [string]$entry[0].sha256 -cne [string]$license.sha256) {
        throw "License metadata digest is inconsistent for $($license.path)."
    }
}

$workspaceManifest = Get-Content -LiteralPath (Join-Path $rootPath 'Cargo.toml') -Raw
$versionMatch = [regex]::Match($workspaceManifest, '(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success -or $versionMatch.Groups[1].Value -cne [string]$manifest.foundation_api_version) {
    throw 'foundation_api_version must match [workspace.package].version.'
}

Invoke-SourceSnapshotCargoMetadata $rootPath | Out-Null

Write-Host "Verified deterministic Impossible Server source snapshot ($($manifest.files.Count) files, tree $treeHash)."
