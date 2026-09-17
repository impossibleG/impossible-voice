$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($IsWindows -and $null -eq ('ImpossibleServer.SourceSnapshotNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace ImpossibleServer {
    public static class SourceSnapshotNative {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(
            string name, uint access, uint share, IntPtr security, uint creation,
            uint flags, IntPtr template);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint GetFinalPathNameByHandleW(
            SafeFileHandle handle, StringBuilder path, uint length, uint flags);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint GetShortPathNameW(
            string longPath, StringBuilder shortPath, uint length);

        public static string FinalPath(string path) {
            const uint ShareAll = 1 | 2 | 4;
            const uint OpenExisting = 3;
            const uint BackupSemantics = 0x02000000;
            using (SafeFileHandle handle = CreateFileW(
                path, 0, ShareAll, IntPtr.Zero, OpenExisting, BackupSemantics, IntPtr.Zero)) {
                if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
                var buffer = new StringBuilder(32768);
                uint result = GetFinalPathNameByHandleW(handle, buffer, (uint)buffer.Capacity, 0);
                if (result == 0 || result >= buffer.Capacity) {
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                }
                string value = buffer.ToString();
                if (value.StartsWith(@"\\?\UNC\", StringComparison.OrdinalIgnoreCase)) {
                    return @"\\" + value.Substring(8);
                }
                if (value.StartsWith(@"\\?\", StringComparison.OrdinalIgnoreCase)) {
                    return value.Substring(4);
                }
                return value;
            }
        }

        public static string ShortPath(string path) {
            var buffer = new StringBuilder(32768);
            uint result = GetShortPathNameW(path, buffer, (uint)buffer.Capacity);
            if (result == 0 || result >= buffer.Capacity) return null;
            return buffer.ToString();
        }
    }
}
'@
}

function Get-CanonicalExistingPath([string]$Path) {
    $resolved = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path
    if ($IsWindows) {
        $resolved = [ImpossibleServer.SourceSnapshotNative]::FinalPath($resolved)
    }
    $full = [IO.Path]::GetFullPath($resolved)
    $pathRoot = [IO.Path]::GetPathRoot($full)
    if ($full.Equals($pathRoot, $(if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }))) {
        return $full
    }
    return $full.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
}

function Get-CanonicalProspectivePath([string]$Path) {
    $fullPath = [IO.Path]::GetFullPath($Path)
    if (Test-Path -LiteralPath $fullPath) {
        return Get-CanonicalExistingPath $fullPath
    }

    $tail = [Collections.Generic.Stack[string]]::new()
    $cursor = $fullPath
    while (-not (Test-Path -LiteralPath $cursor)) {
        $leaf = [IO.Path]::GetFileName($cursor)
        $parent = [IO.Path]::GetDirectoryName($cursor)
        if ([string]::IsNullOrEmpty($leaf) -or [string]::IsNullOrEmpty($parent) -or $parent -eq $cursor) {
            throw "Cannot resolve an existing ancestor for path: $Path"
        }
        $tail.Push($leaf)
        $cursor = $parent
    }
    $canonical = Get-CanonicalExistingPath $cursor
    while ($tail.Count -ne 0) {
        $canonical = Join-Path $canonical $tail.Pop()
    }
    $fullCanonical = [IO.Path]::GetFullPath($canonical)
    $pathRoot = [IO.Path]::GetPathRoot($fullCanonical)
    if ($fullCanonical.Equals($pathRoot, $(if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }))) {
        return $fullCanonical
    }
    return $fullCanonical.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
}

function Test-PathWithinOrEqual([string]$Candidate, [string]$Root) {
    $comparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    if ($Candidate.Equals($Root, $comparison)) {
        return $true
    }
    $prefix = $Root.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    return $Candidate.StartsWith($prefix, $comparison)
}

function Get-SafeTreeInventory([string]$Root) {
    $lexicalRoot = [IO.Path]::GetFullPath($Root)
    $rootItem = Get-Item -LiteralPath $lexicalRoot -Force -ErrorAction Stop
    if (-not $rootItem.PSIsContainer) {
        throw "Inventory root is not a directory: $lexicalRoot"
    }
    if (($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Reparse-point snapshot roots are forbidden: $lexicalRoot"
    }
    $canonicalRoot = Get-CanonicalExistingPath $lexicalRoot
    $queue = [Collections.Generic.Queue[string]]::new()
    $queue.Enqueue($lexicalRoot)
    $entries = [Collections.Generic.List[object]]::new()
    while ($queue.Count -ne 0) {
        $directory = $queue.Dequeue()
        foreach ($item in Get-ChildItem -LiteralPath $directory -Force -ErrorAction Stop) {
            $relative = [IO.Path]::GetRelativePath($lexicalRoot, $item.FullName).Replace('\', '/')
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Reparse points, junctions, and symbolic links are forbidden: $relative"
            }
            $canonical = Get-CanonicalExistingPath $item.FullName
            if (-not (Test-PathWithinOrEqual $canonical $canonicalRoot)) {
                throw "Filesystem entry resolves outside its root: $relative"
            }
            $entries.Add([pscustomobject]@{
                FullName = $item.FullName
                Relative = $relative
                IsDirectory = [bool]$item.PSIsContainer
                Canonical = $canonical
            })
            if ($item.PSIsContainer) {
                $queue.Enqueue($item.FullName)
            }
        }
    }
    return @($entries)
}

function Assert-NoReparsePathFromRoot([string]$Path, [string]$Root) {
    $lexicalRoot = [IO.Path]::GetFullPath($Root)
    $lexicalPath = [IO.Path]::GetFullPath($Path)
    $relative = [IO.Path]::GetRelativePath($lexicalRoot, $lexicalPath)
    if ([IO.Path]::IsPathRooted($relative) -or $relative.Replace('\', '/').Split('/') -contains '..') {
        throw "Path is not lexically beneath its required root: $Path"
    }
    $cursor = $lexicalRoot
    foreach ($segment in @('.') + @($relative -split '[\\/]')) {
        if ($segment -ne '.') { $cursor = Join-Path $cursor $segment }
        $item = Get-Item -LiteralPath $cursor -Force -ErrorAction Stop
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Reparse point, junction, or symbolic link is forbidden in selected path: $cursor"
        }
    }
}

function Assert-CanonicalContainedPath([string]$Path, [string]$Root, [string]$Description, [switch]$MayNotExist) {
    $canonicalRoot = Get-CanonicalExistingPath $Root
    $canonicalPath = if ($MayNotExist) {
        Get-CanonicalProspectivePath $Path
    }
    else {
        Get-CanonicalExistingPath $Path
    }
    if (-not (Test-PathWithinOrEqual $canonicalPath $canonicalRoot)) {
        throw "$Description resolves outside the snapshot root: $Path -> $canonicalPath"
    }
    return $canonicalPath
}

function Invoke-SourceSnapshotCargoMetadata([string]$Root) {
    $manifestPath = Join-Path $Root 'Cargo.toml'
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = 'cargo'
    $start.WorkingDirectory = $Root
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @('metadata', '--locked', '--offline', '--manifest-path', $manifestPath, '--format-version', '1', '--no-deps')) {
        $start.ArgumentList.Add($argument)
    }
    $start.Environment.Remove('CARGO_TARGET_DIR') | Out-Null
    $start.Environment.Remove('CARGO_BUILD_BUILD_DIR') | Out-Null
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    if (-not $process.Start()) {
        throw 'Failed to start cargo metadata for the source snapshot.'
    }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    if ($process.ExitCode -ne 0) {
        throw "Cargo rejected the portable workspace metadata or lockfile: $stderr"
    }
    try {
        $metadata = $stdout | ConvertFrom-Json -Depth 100
    }
    catch {
        throw "Cargo metadata did not return valid JSON: $($_.Exception.Message)"
    }

    $canonicalRoot = Get-CanonicalExistingPath $Root
    $workspaceRoot = Get-CanonicalExistingPath ([string]$metadata.workspace_root)
    if (-not $workspaceRoot.Equals($canonicalRoot, $(if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }))) {
        throw "Cargo workspace root escapes the snapshot: $workspaceRoot"
    }

    $generatedPaths = @([string]$metadata.target_directory)
    if ($metadata.PSObject.Properties.Name -contains 'build_directory') {
        $generatedPaths += [string]$metadata.build_directory
    }
    foreach ($generatedPath in $generatedPaths) {
        if (-not [string]::IsNullOrWhiteSpace($generatedPath)) {
            Assert-CanonicalContainedPath $generatedPath $Root 'Cargo generated-output path' -MayNotExist | Out-Null
        }
    }

    $expectedManifests = @(
        'crates/impossible-server-core/Cargo.toml',
        'crates/impossible-server-testkit/Cargo.toml'
    ) | ForEach-Object { Get-CanonicalExistingPath (Join-Path $Root $_) }
    $actualManifests = [Collections.Generic.List[string]]::new()
    foreach ($package in @($metadata.packages)) {
        if ($null -ne $package.source) {
            throw "Portable workspace package unexpectedly uses an external source: $($package.name)"
        }
        $actualManifests.Add((Assert-CanonicalContainedPath ([string]$package.manifest_path) $Root 'Cargo package manifest'))
        foreach ($target in @($package.targets)) {
            Assert-CanonicalContainedPath ([string]$target.src_path) $Root 'Cargo target source' | Out-Null
        }
        foreach ($dependency in @($package.dependencies)) {
            if ($dependency.PSObject.Properties.Name -contains 'path' -and
                $null -ne $dependency.path -and -not [string]::IsNullOrWhiteSpace([string]$dependency.path)) {
                Assert-CanonicalContainedPath ([string]$dependency.path) $Root 'Cargo path dependency' | Out-Null
            }
        }
        foreach ($optionalPath in @($package.license_file, $package.readme)) {
            if ($null -ne $optionalPath -and -not [string]::IsNullOrWhiteSpace([string]$optionalPath)) {
                Assert-CanonicalContainedPath ([string]$optionalPath) $Root 'Cargo package file' | Out-Null
            }
        }
    }
    $comparison = if ($IsWindows) { [StringComparer]::OrdinalIgnoreCase } else { [StringComparer]::Ordinal }
    $expected = [string[]]$expectedManifests
    $actual = [string[]]$actualManifests.ToArray()
    [Array]::Sort($expected, $comparison)
    [Array]::Sort($actual, $comparison)
    if (($expected -join "`n") -cne ($actual -join "`n")) {
        throw "Cargo workspace packages differ from the approved two-crate surface. Expected=[$($expected -join ', ')] Actual=[$($actual -join ', ')]"
    }
    $packageIds = @($metadata.packages | ForEach-Object { [string]$_.id })
    foreach ($member in @($metadata.workspace_members) + @($metadata.workspace_default_members)) {
        if ([string]$member -notin $packageIds) {
            throw "Cargo workspace member is not an approved contained package: $member"
        }
    }

    foreach ($manifest in @($manifestPath) + @($expectedManifests)) {
        $content = Get-Content -LiteralPath $manifest -Raw
        if ($content -match '(?im)^\s*\[\s*(?:["'']?(?:patch|replace)["'']?)(?:\s*\.|\s*\])' -or
            $content -match '(?im)^\s*["'']?(?:patch|replace)["'']?\s*\.') {
            throw "Cargo patch and replacement declarations are forbidden in the portable snapshot: $manifest"
        }
    }
    return $metadata
}

function Get-WindowsShortPathIfAvailable([string]$Path) {
    if (-not $IsWindows) { return $null }
    return [ImpossibleServer.SourceSnapshotNative]::ShortPath((Get-CanonicalExistingPath $Path))
}
