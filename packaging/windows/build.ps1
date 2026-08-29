<#
.SYNOPSIS
    Minimal Windows packaging baseline for NextIDE.

.DESCRIPTION
    This script does exactly two safe things, nothing more:

      1. Builds the release binaries: `cargo build --release`
      2. Copies the CLI and Desktop Shell executables into dist\windows\

    It deliberately performs NO dangerous operations:
      - no deleting / cleaning of existing directories (dist is overwritten
        in place with Copy-Item -Force, never removed first)
      - no registry changes, no code signing, no installation
      - no writes outside the repository (dist lives under the repo root)
      - no network access

    The script fails loudly (non-zero exit code) if cargo is missing, the
    release build fails, or an expected artifact is missing before or after
    staging.

.PARAMETER SkipBuild
    Skip the `cargo build --release` step and only (re)stage the artifacts
    that are already present in target\release\. Useful when the release
    build was already performed by hand.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 -SkipBuild
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Repo root is two levels above this script: packaging\windows\build.ps1
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$DistDir  = Join-Path $RepoRoot 'dist\windows'

# Expected release executables produced by the workspace binaries.
$Artifacts = @(
    (Join-Path $RepoRoot 'target\release\nextide-cli.exe'),
    (Join-Path $RepoRoot 'target\release\nextide-shell.exe')
)

function Write-Step {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "==> $Message"
}

Write-Step "NextIDE packaging baseline (repo root: $RepoRoot)"

if (-not $SkipBuild) {
    Write-Step "Building release binaries: cargo build --release"
    Push-Location $RepoRoot
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}
else {
    Write-Step "Skipping build (-SkipBuild); staging existing release artifacts only"
}

Write-Step "Checking that every expected release artifact exists"
foreach ($Artifact in $Artifacts) {
    if (-not (Test-Path -LiteralPath $Artifact)) {
        throw "Missing release artifact: $Artifact (run without -SkipBuild, or inspect the cargo profile)"
    }
}

Write-Step "Staging artifacts into $DistDir"
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
foreach ($Artifact in $Artifacts) {
    Copy-Item -LiteralPath $Artifact -Destination $DistDir -Force
    Write-Host "  copied $(Split-Path -Leaf $Artifact)"
}

Write-Step "Verifying staged artifacts"
foreach ($Artifact in $Artifacts) {
    $Staged = Join-Path $DistDir (Split-Path -Leaf $Artifact)
    if (-not (Test-Path -LiteralPath $Staged)) {
        throw "Staged artifact missing after copy: $Staged"
    }
    $StagedFile = Get-Item -LiteralPath $Staged
    $Sha256     = (Get-FileHash -LiteralPath $Staged -Algorithm SHA256).Hash
    Write-Host ("  {0}  {1} bytes  sha256 {2}" -f $StagedFile.Name, $StagedFile.Length, $Sha256)
}

Write-Step "Done. Release artifacts staged under $DistDir"
