<#
.SYNOPSIS
    Minimal Windows packaging baseline for NextIDE (manifest-driven).

.DESCRIPTION
    This script does exactly two safe things, nothing more:

      1. Builds the release binaries: `cargo build --release`
      2. Copies the executables declared by manifest.json into dist\windows\

    manifest.json (next to this script) is the single source of truth for the
    product metadata, the target architecture, the artifact list and the
    verification strategy. Every artifact listed there is checked before
    staging (the release binary must exist) and after staging (the staged copy
    must exist and match the release binary by size and SHA-256). An artifact
    may pin an `expected_sha256`; when present, the staged hash must equal it.

    It deliberately performs NO dangerous operations:
      - no deleting / cleaning of existing directories (dist is overwritten
        in place with Copy-Item -Force, never removed first)
      - no registry changes, no code signing, no installation
      - no writes outside the repository (dist lives under the repo root)
      - no network access

    The script fails loudly (non-zero exit code) if cargo is missing, the
    release build fails, the manifest is invalid, or an expected artifact is
    missing or mismatched before or after staging.

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
$RepoRoot     = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$ManifestPath = Join-Path $PSScriptRoot 'manifest.json'
$DistDir      = Join-Path $RepoRoot 'dist\windows'

function Write-Step {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "==> $Message"
}

# Read an optional field from a PSObject (ConvertFrom-Json output) without
# tripping Set-StrictMode when the JSON object does not define the property.
function Get-ManifestField {
    param(
        [Parameter(Mandatory)][AllowNull()]$Object,
        [Parameter(Mandatory)][string]$Name
    )
    if ($null -eq $Object -or $null -eq $Object.PSObject.Properties[$Name]) {
        return $null
    }
    return $Object.$Name
}

function Assert-RepoRelativePath {
    # Manifest source paths must stay inside the repository: reject rooted
    # paths (drive letter or leading separator) and any '..' segment.
    param([Parameter(Mandatory)][string]$Path)
    if ($Path -match '^[a-zA-Z]:[\\/]' -or $Path.StartsWith('/') -or $Path.StartsWith('\')) {
        throw "Manifest artifact source must be a repo-relative path, got: $Path"
    }
    if (($Path -split '[\\/]') -contains '..') {
        throw "Manifest artifact source must not escape the repo root, got: $Path"
    }
}

function Get-ProductManifest {
    if (-not (Test-Path -LiteralPath $ManifestPath)) {
        throw "Missing manifest: $ManifestPath"
    }
    try {
        $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    }
    catch {
        throw "Invalid manifest JSON at $ManifestPath : $($_.Exception.Message)"
    }

    if ($Manifest.schema_version -ne 1) {
        throw "Unsupported manifest schema_version: $($Manifest.schema_version) (expected 1)"
    }
    if ($null -eq $Manifest.product -or [string]::IsNullOrWhiteSpace($Manifest.product.name)) {
        throw 'Manifest is missing product.name'
    }
    if ($null -eq $Manifest.target -or [string]::IsNullOrWhiteSpace($Manifest.target.arch)) {
        throw 'Manifest is missing target.arch'
    }
    if ($null -eq $Manifest.verification -or [string]::IsNullOrWhiteSpace($Manifest.verification.algorithm)) {
        throw 'Manifest is missing verification.algorithm'
    }
    if ($null -eq $Manifest.artifacts -or @($Manifest.artifacts).Count -eq 0) {
        throw 'Manifest declares no artifacts'
    }

    foreach ($Artifact in @($Manifest.artifacts)) {
        if ([string]::IsNullOrWhiteSpace($Artifact.id)) {
            throw 'Every manifest artifact needs a non-empty id'
        }
        if ([string]::IsNullOrWhiteSpace($Artifact.file) -or [string]::IsNullOrWhiteSpace($Artifact.source)) {
            throw "Artifact '$($Artifact.id)' is missing file or source"
        }
        Assert-RepoRelativePath $Artifact.source
        if ($Artifact.file -ne (Split-Path -Leaf $Artifact.source)) {
            throw "Artifact '$($Artifact.id)': file '$($Artifact.file)' must equal the source file name '$(Split-Path -Leaf $Artifact.source)'"
        }
    }
    return $Manifest
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

$Manifest  = Get-ProductManifest
$Artifacts = @($Manifest.artifacts) | ForEach-Object {
    [pscustomobject]@{
        Id             = $_.id
        File           = $_.file
        Source         = Join-Path $RepoRoot ($_.source -replace '/', '\')
        Description    = (Get-ManifestField $_ 'description')
        ExpectedSha256 = (Get-ManifestField $_ 'expected_sha256')
    }
}
$Rules = Get-ManifestField $Manifest.verification 'rules'
function Get-Rule {
    param([string]$Name)
    $Value = Get-ManifestField $Rules $Name
    if ($null -eq $Value) { return $true }  # default: enforce the check
    return [bool]$Value
}
$SourceMustExist     = Get-Rule 'source_must_exist'
$StagedMustExist     = Get-Rule 'staged_must_exist'
$StagedMustMatchSrc  = Get-Rule 'staged_must_match_source'
$Report = Get-ManifestField $Rules 'report'
if ($null -eq $Report) { $Report = @('size_bytes', 'sha256') }
$Report = @($Report) | ForEach-Object { [string]$_ }

# Note: manifest build.command is declared metadata only. The script always
# runs the pinned, safe `cargo build --release` from the repo root, never an
# arbitrary command read from a manifest, so a tampered manifest cannot make
# the script execute something else or write outside the repository.
$ProductVersion = Get-ManifestField $Manifest.product 'version'
$TargetOs       = Get-ManifestField $Manifest.target 'os'
$TargetTriple   = Get-ManifestField $Manifest.target 'triple'
$Strategy       = Get-ManifestField $Manifest.verification 'strategy'

Write-Step "NextIDE packaging baseline (repo root: $RepoRoot)"
Write-Host "  manifest: $ManifestPath"
Write-Host "  product:  $($Manifest.product.name) $ProductVersion"
Write-Host "  target:   $TargetOs/$($Manifest.target.arch) ($TargetTriple)"
Write-Host "  verify:   $Strategy / $($Manifest.verification.algorithm)"

if (-not $SkipBuild) {
    Write-Step 'Building release binaries: cargo build --release'
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

Write-Step "Checking that every manifest artifact exists in the release output"
foreach ($Artifact in $Artifacts) {
    if ($SourceMustExist -and -not (Test-Path -LiteralPath $Artifact.Source)) {
        throw "Missing release artifact: $($Artifact.Source) (run without -SkipBuild, or inspect the cargo profile)"
    }
    Write-Host "  ok $($Artifact.Source)"
}

Write-Step "Staging artifacts into $DistDir"
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
foreach ($Artifact in $Artifacts) {
    Copy-Item -LiteralPath $Artifact.Source -Destination $DistDir -Force
    Write-Host "  copied $($Artifact.File)"
}

Write-Step "Verifying staged artifacts"
foreach ($Artifact in $Artifacts) {
    $Staged = Join-Path $DistDir $Artifact.File
    if ($StagedMustExist -and -not (Test-Path -LiteralPath $Staged)) {
        throw "Staged artifact missing after copy: $Staged"
    }
    $StagedFile = Get-Item -LiteralPath $Staged
    $StagedSha  = Get-Sha256 $Staged

    if ($StagedMustMatchSrc) {
        $SourceSha = Get-Sha256 $Artifact.Source
        if ($StagedFile.Length -ne (Get-Item -LiteralPath $Artifact.Source).Length) {
            throw "Staged artifact size mismatch: $Staged ($($StagedFile.Length) bytes vs source)"
        }
        if ($StagedSha -ne $SourceSha) {
            throw "Staged artifact hash mismatch: $Staged (staged $StagedSha vs source $SourceSha)"
        }
    }

    if (-not [string]::IsNullOrWhiteSpace($Artifact.ExpectedSha256)) {
        if ($StagedSha -ne $Artifact.ExpectedSha256.ToUpperInvariant()) {
            throw "Staged artifact hash does not match expected_sha256: $Staged (got $StagedSha, expected $($Artifact.ExpectedSha256.ToUpperInvariant()))"
        }
    }

    $ReportParts = @($StagedFile.Name)
    if ($Report -contains 'size_bytes') { $ReportParts += "$($StagedFile.Length) bytes" }
    if ($Report -contains 'sha256')     { $ReportParts += "sha256 $StagedSha" }
    Write-Host ('  ' + ($ReportParts -join '  '))
}

Write-Step "Done. Release artifacts staged under $DistDir"
