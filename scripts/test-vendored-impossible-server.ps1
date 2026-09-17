[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'source-snapshot-security.ps1')

$verifier = Join-Path $PSScriptRoot 'verify-vendored-impossible-server.ps1'
$source = Get-CanonicalExistingPath (Join-Path $PSScriptRoot '../vendor/impossible-server')
$tempBase = Get-CanonicalExistingPath ([IO.Path]::GetTempPath())
$tempRoot = Join-Path $tempBase ('impossible-voice-vendor-tests-' + [guid]::NewGuid().ToString('N'))
$createdLinks = [Collections.Generic.List[string]]::new()
[IO.Directory]::CreateDirectory($tempRoot) | Out-Null

function Assert-Rejected([string]$Name, [scriptblock]$Action) {
    try { & $Action } catch { Write-Host "PASS rejected $Name"; return }
    throw "Consumer verifier accepted invalid vendor mutation: $Name"
}

function New-Fixture([string]$Name) {
    $path = Join-Path $tempRoot $Name
    [IO.Directory]::CreateDirectory($path) | Out-Null
    Get-ChildItem -LiteralPath $source -Force | Copy-Item -Destination $path -Recurse -Force
    return $path
}

function Write-Manifest([string]$Root, [object]$Manifest) {
    [IO.File]::WriteAllText(
        (Join-Path $Root 'source-sync.json'),
        (($Manifest | ConvertTo-Json -Depth 64) + "`n"),
        [Text.UTF8Encoding]::new($false)
    )
}

function Update-ManifestDigests([string]$Root) {
    $manifest = Get-Content -LiteralPath (Join-Path $Root 'source-sync.json') -Raw | ConvertFrom-Json -Depth 64
    $treeLines = [Collections.Generic.List[string]]::new()
    foreach ($entry in $manifest.files) {
        $fullPath = Join-Path $Root ([string]$entry.path)
        $entry.sha256 = (Get-FileHash -LiteralPath $fullPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $entry.size = [IO.File]::ReadAllBytes($fullPath).LongLength
        $treeLines.Add("$($entry.path)`t$($entry.sha256)`t$($entry.size)`n")
    }
    $manifest.provisional_source_tree_sha256 = [Convert]::ToHexString(
        [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes(($treeLines -join '')))
    ).ToLowerInvariant()
    Write-Manifest $Root $manifest
}

function New-DirectoryReparsePoint([string]$Link, [string]$Target) {
    if ($IsWindows) { $null = New-Item -ItemType Junction -Path $Link -Target $Target }
    else { $null = [IO.Directory]::CreateSymbolicLink($Link, $Target) }
    $createdLinks.Add($Link)
}

function Remove-CreatedLink([string]$Link) {
    if (Test-Path -LiteralPath $Link) { [IO.Directory]::Delete($Link) }
    $createdLinks.Remove($Link) | Out-Null
}

try {
    & $verifier

    $changed = New-Fixture 'changed'
    [IO.File]::AppendAllText((Join-Path $changed 'NOTICE.md'), "`nchanged", [Text.UTF8Encoding]::new($false))
    Assert-Rejected 'changed payload' { & $verifier -Root $changed }

    $extra = New-Fixture 'extra-directory'
    [IO.Directory]::CreateDirectory((Join-Path $extra 'empty-extra')) | Out-Null
    Assert-Rejected 'unmanifested empty directory' { & $verifier -Root $extra }

    $wrongProduct = New-Fixture 'wrong-product'
    $manifest = Get-Content -LiteralPath (Join-Path $wrongProduct 'source-sync.json') -Raw | ConvertFrom-Json -Depth 64
    $manifest.product = 'spoofed-source-snapshot'
    Write-Manifest $wrongProduct $manifest
    Assert-Rejected 'wrong product marker' { & $verifier -Root $wrongProduct }

    $wrongSchema = New-Fixture 'wrong-schema'
    $schemaPath = Join-Path $wrongSchema 'schemas/source-sync-manifest.schema.json'
    $schema = Get-Content -LiteralPath $schemaPath -Raw | ConvertFrom-Json -Depth 64
    $schema.'$id' = 'https://example.invalid/spoofed-schema.json'
    [IO.File]::WriteAllText($schemaPath, (($schema | ConvertTo-Json -Depth 64) + "`n"), [Text.UTF8Encoding]::new($false))
    Update-ManifestDigests $wrongSchema
    Assert-Rejected 'wrong schema identity marker' { & $verifier -Root $wrongSchema }

    $outside = Join-Path $tempRoot 'outside'
    [IO.Directory]::CreateDirectory($outside) | Out-Null
    [IO.File]::WriteAllText((Join-Path $outside 'private.txt'), 'private', [Text.UTF8Encoding]::new($false))
    $reparse = New-Fixture 'reparse'
    $link = Join-Path $reparse 'crates/impossible-server-core/src/linked'
    New-DirectoryReparsePoint $link $outside
    Assert-Rejected 'directory reparse point' { & $verifier -Root $reparse }
    Remove-CreatedLink $link

    Assert-Rejected 'provisional release snapshot' { & $verifier -Release }
    Write-Host 'Vendored Impossible Server consumer mutation tests passed.'
}
finally {
    foreach ($link in @($createdLinks)) { Remove-CreatedLink $link }
    if (Test-Path -LiteralPath $tempRoot) {
        $resolved = Get-CanonicalExistingPath $tempRoot
        if (-not (Test-PathWithinOrEqual $resolved $tempBase) -or
            -not ([IO.Path]::GetFileName($resolved)).StartsWith('impossible-voice-vendor-tests-', [StringComparison]::Ordinal)) {
            throw "Refusing unsafe consumer fixture cleanup: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
