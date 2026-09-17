$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$testRoot = Join-Path $repositoryRoot '.shame/package-test'
$binary = Join-Path $testRoot 'impossible-voice.exe'
$output = Join-Path $testRoot 'dist'

try {
    New-Item -ItemType Directory -Path $testRoot -Force | Out-Null
    [IO.File]::WriteAllBytes($binary, [byte[]](77, 90, 0, 0))
    & (Join-Path $PSScriptRoot 'package.ps1') -PlatformLabel 'windows-x86_64' -BinaryPath $binary -OutputDirectory '.shame/package-test/dist' | Out-Null

    $archive = @(Get-ChildItem -LiteralPath $output -Filter '*.zip' -File)
    if ($archive.Count -ne 1) { throw 'Packaging did not produce exactly one archive.' }
    $checksum = @(Get-ChildItem -LiteralPath $output -Filter '*.zip.sha256' -File)
    if ($checksum.Count -ne 1) { throw 'Packaging did not produce exactly one checksum.' }
    $expectedHash = ((Get-Content -LiteralPath $checksum[0].FullName -Raw) -split '\s+')[0]
    $actualHash = (Get-FileHash -LiteralPath $archive[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($expectedHash -ne $actualHash) { throw 'Release archive checksum is invalid.' }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive[0].FullName)
    try {
        $entries = @($zip.Entries | ForEach-Object { $_.FullName.Replace('\', '/') })
        if (-not [bool]($entries -match '/impossible-voice\.exe$')) { throw 'Packaged binary is missing.' }
        if ([bool]($entries -match '(?i)(runtime-artifacts|\.shame|target/|\.wav$|\.onnx$|\.dll$|\.so$)')) {
            throw 'Archive contains a forbidden runtime, model, build, or audio artifact.'
        }
        if (-not [bool]($entries -match '/THIRD_PARTY_NOTICES\.md$')) { throw 'Third-party notices are missing.' }
        if (-not [bool]($entries -match '/scripts/smoke-public\.ps1$')) { throw 'Public release smoke is missing.' }
    }
    finally {
        $zip.Dispose()
    }
    Write-Output 'Release package allowlist test passed.'
}
finally {
    if (Test-Path -LiteralPath $testRoot) { Remove-Item -LiteralPath $testRoot -Recurse -Force }
}
