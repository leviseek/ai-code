#Requires -Version 5.1
<#
.SYNOPSIS
    Consumer-side verification of a SHA256SUMS.txt checksum manifest.

.DESCRIPTION
    Verifies a staged release directory against its SHA-256 checksum manifest
    (dist\windows\SHA256SUMS.txt by default). This is the *consumer*
    counterpart to build.ps1's generation step: it proves that the files in a
    staging/artifact directory match the checksum manifest that ships with
    them — no rebuild, no re-staging, no cargo invocation, no external tools.

    The script is strictly read-only and deliberately performs NO dangerous
    operations:
      - it never writes, renames or removes any file or directory (it contains
        no Set-Content, New-Item, Copy-Item or Remove-Item anywhere);
      - it never executes the files it verifies;
      - it never touches the registry, signs, installs or accesses the
        network;
      - it only reads: the checksum manifest (Get-Content), the staging
        directory listing (Get-ChildItem) and the SHA-256 of each file
        (Get-FileHash — a built-in cmdlet, no dependencies).

    What it verifies, in order (any failure exits non-zero with a message
    naming the problem):
      1. the checksum manifest exists and is non-empty;
      2. every line parses as '<64-hex-sha256>  <file>' — exactly 64 hex
         characters, exactly two spaces, then a plain leaf file name (no path
         separators, no '.'/'..', no control characters), which is also the
         exact format build.ps1 generates (GNU sha256sum -c compatible);
         blank lines are ignored; an entry naming a file more than once is
         rejected;
      3. every named file exists in the staging directory;
      4. every named file's actual SHA-256 equals the manifest hash
         (case-insensitive — Get-FileHash prints uppercase, GNU tools
         lowercase, both are accepted);
      5. the directory matches the manifest exactly: every file in the
         staging directory (except the checksum manifest itself, which is
         generated and never lists itself) has exactly one entry, and the
         manifest declares no entry for a file that is not present.

    The exit code is 0 when every check passes and non-zero (1) on any
    failure, so the script can be used directly as a gate in scripts and CI.

.PARAMETER DistDir
    The staging directory whose contents are verified. Defaults to
    <repo root>\dist\windows (the directory build.ps1 stages into). A relative
    path is resolved against the current working directory.

.PARAMETER ChecksumFile
    The checksum manifest to verify. Defaults to SHA256SUMS.txt inside
    DistDir. Handy when a release uses a custom stage.checksum_file name or
    when the manifest lives outside the staging directory.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\verify-checksums.ps1

    Verifies the repository's own dist\windows\ against its SHA256SUMS.txt.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\verify-checksums.ps1 -DistDir C:\downloads\nextide-dist

    Verifies a downloaded/extracted release directory against the
    SHA256SUMS.txt inside it.
#>
[CmdletBinding()]
param(
    [string]$DistDir = '',
    [string]$ChecksumFile = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Repo root is two levels above this script: packaging\windows\verify-checksums.ps1
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $DistDir)      { $DistDir = Join-Path $RepoRoot 'dist\windows' }
if (-not $ChecksumFile) { $ChecksumFile = Join-Path $DistDir 'SHA256SUMS.txt' }

if (-not (Test-Path -LiteralPath $DistDir -PathType Container)) {
    throw "Staging directory not found: $DistDir"
}
if (-not (Test-Path -LiteralPath $ChecksumFile -PathType Leaf)) {
    throw "Checksum manifest not found: $ChecksumFile"
}

Write-Host '==> Verifying checksum manifest against the staging directory (read-only)'
Write-Host "  staging dir : $DistDir"
Write-Host "  checksum    : $ChecksumFile"

$ChecksumName = Split-Path -Leaf $ChecksumFile

# 1. Parse the manifest: every line must be '<64-hex>  <file>' (exactly two
#    spaces), naming a plain leaf file inside the staging directory. Blank
#    lines are ignored (GNU sha256sum -c does the same). A duplicate entry for
#    the same file is a format/consistency violation.
$Entries = @{}   # file name -> uppercase SHA-256
$Order   = @()   # manifest order, for deterministic reporting
foreach ($Line in (Get-Content -LiteralPath $ChecksumFile)) {
    if ([string]::IsNullOrWhiteSpace($Line)) { continue }
    if ($Line -notmatch '^([0-9a-fA-F]{64})  ([^\r\n]+)$') {
        throw "Checksum manifest line is not in '<64-hex-sha256>  <file>' format: '$Line'"
    }
    $Hash = $Matches[1].ToUpperInvariant()
    $File = $Matches[2]
    if ($File -match '[\\/]' -or $File -eq '.' -or $File -eq '..' -or $File -match '[\x00-\x1f]') {
        throw "Checksum manifest entry must name a plain file inside the staging directory (no path separators, no '.'/'..'), got: '$File'"
    }
    if ($Entries.ContainsKey($File)) {
        throw "Checksum manifest declares '$File' more than once"
    }
    $Entries[$File] = $Hash
    $Order += $File
}
if ($Entries.Count -eq 0) {
    throw "Checksum manifest is empty: $ChecksumFile"
}

# 2/3/4. Every named file must exist and its actual SHA-256 must equal the
#    manifest hash. Hashes are compared case-insensitively (uppercase).
foreach ($File in $Order) {
    $Path = Join-Path $DistDir $File
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Checksum manifest names '$File', but the file does not exist in $DistDir"
    }
    $Actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($Actual -ne $Entries[$File]) {
        throw "SHA-256 mismatch for '$File': manifest $($Entries[$File]), actual $Actual"
    }
    Write-Host "  ok $File"
}

# 5. Reverse direction: every file in the staging directory (except the
#    checksum manifest itself, which is generated and never lists itself) must
#    have exactly one entry, and the manifest must not declare entries for
#    files that are not there — the directory and the manifest must match
#    exactly.
$Staged = @(Get-ChildItem -LiteralPath $DistDir -File)
foreach ($File in $Staged) {
    if ($File.Name -eq $ChecksumName) { continue }
    if (-not $Entries.ContainsKey($File.Name)) {
        throw "Staging directory contains '$($File.Name)', but the checksum manifest declares no entry for it"
    }
}
$OtherCount = @($Staged | Where-Object { $_.Name -ne $ChecksumName }).Count
if ($Entries.Count -ne $OtherCount) {
    throw "Checksum manifest declares $($Entries.Count) entries, but the staging directory holds $OtherCount other files"
}

Write-Host "  verified: $($Entries.Count) entries - every line parses, every file exists, every hash matches, directory matches exactly"
Write-Host '==> OK. Checksum manifest verified against the staging directory (read-only).'
exit 0
