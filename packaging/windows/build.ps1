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

    After the staged artifacts are verified, the script writes a checksum
    manifest into the staging directory: one '<64-hex-sha256>  <file>' line per
    staged artifact, in manifest artifact order, LF line endings (GNU
    sha256sum -c compatible), named by the manifest's stage.checksum_file
    (committed value: SHA256SUMS.txt). The file name is validated read-only
    before anything is built or staged — it must be a plain leaf name (no path
    separators, no '.'/'..') and must not collide with any declared artifact
    file, since the checksum manifest is generated, not staged. The written
    file is then parsed back and every line is matched against the verified
    staged hashes, so the checksum manifest that ships is proven consistent
    with the staged files, not just present.

    Additionally, manifest product.version must equal the [workspace.package]
    version declared in the root Cargo.toml (the single source of truth for the
    workspace version). The script parses Cargo.toml read-only as plain text —
    no cargo invocation, no TOML dependency — and fails loudly on a mismatch
    before anything is built or staged, so a stale manifest can never ship a
    release that claims a version the workspace does not declare.

    The manifest target block is enforced the same way: target.os must be
    'windows', target.architecture must be 'x86_64', and target.triple must be
    'x86_64-pc-windows-msvc' — the pinned packaging target this script
    produces. A manifest declaring any other target (wrong os, wrong
    architecture, wrong triple, or an architecture/triple pair that disagrees)
    fails before anything is built or staged, so a stale or cross-arch manifest
    can never stage an artifact set for the wrong platform.

    The manifest release block carries the release metadata and is validated
    read-only the same way: release.license and release.publisher must be
    non-empty, and release.release_channel must be exactly 'stable' or 'beta'
    (beta is a valid channel; any other value — e.g. 'alpha' or 'nightly' — is
    a hard error). A missing release block or a missing/blank field fails
    before anything is built or staged. This block only carries metadata — the
    script performs no signing and no installation.

    It deliberately performs NO dangerous operations:
      - no deleting / cleaning of existing directories (dist is overwritten
        in place with Copy-Item -Force, never removed first)
      - no registry changes, no code signing, no installation
      - no writes outside the repository (dist lives under the repo root)
      - no network access

    The script fails loudly (non-zero exit code) if cargo is missing, the
    release build fails, the manifest is invalid, product.version does not
    match the Cargo.toml workspace version, the manifest target block does not
    match the pinned packaging target x86_64-pc-windows-msvc, the manifest
    release block is missing or invalid (license/publisher required,
    release_channel must be stable or beta), the manifest stage.checksum_file
    is missing/invalid (must be a plain leaf name that does not collide with an
    artifact file), or an expected artifact is missing or mismatched before or
    after staging — or the generated checksum manifest fails to parse back or
    disagree with a verified staged hash.

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
    if ($null -eq $Manifest.target -or [string]::IsNullOrWhiteSpace($Manifest.target.os)) {
        throw 'Manifest is missing target.os'
    }
    if ($null -eq $Manifest.target -or [string]::IsNullOrWhiteSpace($Manifest.target.architecture)) {
        throw 'Manifest is missing target.architecture'
    }
    if ($null -eq $Manifest.target -or [string]::IsNullOrWhiteSpace($Manifest.target.triple)) {
        throw 'Manifest is missing target.triple'
    }
    # The release block is required and validated read-only: license and
    # publisher must be non-empty, release_channel must be exactly 'stable' or
    # 'beta'. It is metadata only — the script never signs or installs.
    $Release = Get-ManifestField $Manifest 'release'
    if ($null -eq $Release) {
        throw 'Manifest is missing the release block (release.license, release.publisher and release.release_channel are required)'
    }
    $ReleaseLicense   = Get-ManifestField $Release 'license'
    $ReleasePublisher = Get-ManifestField $Release 'publisher'
    $ReleaseChannel   = Get-ManifestField $Release 'release_channel'
    if ([string]::IsNullOrWhiteSpace([string]$ReleaseLicense)) {
        throw 'Manifest release.license is required (non-empty)'
    }
    if ([string]::IsNullOrWhiteSpace([string]$ReleasePublisher)) {
        throw 'Manifest release.publisher is required (non-empty)'
    }
    if ([string]::IsNullOrWhiteSpace([string]$ReleaseChannel)) {
        throw 'Manifest release.release_channel is required (must be exactly stable or beta)'
    }
    if ($ReleaseChannel -ne 'stable' -and $ReleaseChannel -ne 'beta') {
        throw "Manifest release.release_channel '$ReleaseChannel' is not allowed (must be exactly 'stable' or 'beta')"
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

    # The checksum manifest file name (stage.checksum_file) is required and
    # validated read-only before anything is built or staged: it must be a
    # plain leaf name (no path separators, no '.'/'..') and must not collide
    # with any declared artifact file — the checksum manifest is generated by
    # this script, never staged from a build output, so a name that collides
    # with an artifact file is a manifest error.
    $StageBlock    = Get-ManifestField $Manifest 'stage'
    $ChecksumFile  = Get-ManifestField $StageBlock 'checksum_file'
    if ([string]::IsNullOrWhiteSpace([string]$ChecksumFile)) {
        throw 'Manifest is missing stage.checksum_file (the generated SHA-256 checksum manifest file name, e.g. SHA256SUMS.txt)'
    }
    if ($ChecksumFile -match '[\\/]' -or $ChecksumFile -eq '.' -or $ChecksumFile -eq '..' -or $ChecksumFile -match '[\x00-\x1f]') {
        throw "Manifest stage.checksum_file must be a plain file name in the staging directory, got: '$ChecksumFile'"
    }
    foreach ($Artifact in @($Manifest.artifacts)) {
        if ($Artifact.file -eq $ChecksumFile) {
            throw "Manifest stage.checksum_file '$ChecksumFile' collides with artifact file '$($Artifact.file)' (the checksum manifest is generated, not staged)"
        }
    }
    return $Manifest
}

function Get-CargoWorkspaceVersion {
    # Read-only text parse of the root Cargo.toml: extract the version declared
    # in the [workspace.package] table. No cargo invocation, no TOML library —
    # just a line scan that tracks the current TOML section and matches
    # `version = "x.y.z"` (double-quoted, per Cargo.toml convention) inside
    # [workspace.package]. This is the single source of truth the manifest
    # product.version must match.
    $CargoToml = Join-Path $RepoRoot 'Cargo.toml'
    if (-not (Test-Path -LiteralPath $CargoToml)) {
        throw "Missing Cargo.toml at the repo root: $CargoToml (required to validate manifest product.version)"
    }

    $InWorkspacePackage = $false
    $Version = $null
    foreach ($Line in (Get-Content -LiteralPath $CargoToml -Encoding UTF8)) {
        $Trimmed = $Line.Trim()
        if ($Trimmed -match '^\[') {
            # New TOML section header: only [workspace.package] is of interest.
            $InWorkspacePackage = ($Trimmed -eq '[workspace.package]')
            continue
        }
        if ($InWorkspacePackage -and $Trimmed -match '^version\s*=\s*"(?<v>[^"]+)"') {
            $Version = $Matches['v']
        }
    }

    if ([string]::IsNullOrWhiteSpace($Version)) {
        throw "Cargo.toml at $CargoToml does not declare a [workspace.package] version (the single source of truth manifest product.version must match)"
    }
    return $Version
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

$Manifest  = Get-ProductManifest

# The manifest target block must match the pinned packaging target this script
# produces: x86_64-pc-windows-msvc (64-bit x86 Windows with the MSVC
# toolchain). A manifest declaring any other target (wrong os, wrong
# architecture, wrong triple, or an architecture/triple pair that disagrees
# with each other) is a hard error before anything is built or staged, so a
# stale or cross-arch manifest can never stage an artifact set for the wrong
# platform.
$PinnedTargetOs     = 'windows'
$PinnedTargetArch   = 'x86_64'
$PinnedTargetTriple = 'x86_64-pc-windows-msvc'

$TargetOs     = Get-ManifestField $Manifest.target 'os'
$TargetArch   = Get-ManifestField $Manifest.target 'architecture'
$TargetTriple = Get-ManifestField $Manifest.target 'triple'
if ($TargetOs -ne $PinnedTargetOs) {
    throw "Manifest target.os '$TargetOs' does not match the pinned packaging target os '$PinnedTargetOs' (x86_64-pc-windows-msvc)"
}
if ($TargetArch -ne $PinnedTargetArch) {
    throw "Manifest target.architecture '$TargetArch' does not match the pinned packaging target architecture '$PinnedTargetArch' (x86_64-pc-windows-msvc)"
}
if ($TargetTriple -ne $PinnedTargetTriple) {
    throw "Manifest target.triple '$TargetTriple' does not match the pinned packaging target triple '$PinnedTargetTriple'"
}
if ($TargetTriple.Split('-')[0] -ne $TargetArch) {
    throw "Manifest target.architecture '$TargetArch' is inconsistent with target.triple '$TargetTriple' (triple must start with the declared architecture)"
}

# product.version must equal the [workspace.package] version in the root
# Cargo.toml. Fail fast (before any build or staging) so a stale manifest can
# never ship a release that claims a version the workspace does not declare.
$ProductVersion = Get-ManifestField $Manifest.product 'version'
$CargoVersion   = Get-CargoWorkspaceVersion
if ($null -eq $ProductVersion) {
    throw 'Manifest is missing product.version'
}
if ($ProductVersion -ne $CargoVersion) {
    throw "Manifest product.version '$ProductVersion' does not match Cargo.toml [workspace.package] version '$CargoVersion'"
}

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
$Strategy       = Get-ManifestField $Manifest.verification 'strategy'

# The checksum manifest file name (validated read-only in Get-ProductManifest).
# Re-read it safely for the staging step below.
$StageBlock   = Get-ManifestField $Manifest 'stage'
$ChecksumFile = Get-ManifestField $StageBlock 'checksum_file'

# Release metadata was validated in Get-ProductManifest; re-read it (safely,
# Get-ManifestField never trips strict mode) for the header printout.
$ReleaseBlock   = Get-ManifestField $Manifest 'release'
$ReleaseLicense = Get-ManifestField $ReleaseBlock 'license'
$ReleasePub     = Get-ManifestField $ReleaseBlock 'publisher'
$ReleaseChannel = Get-ManifestField $ReleaseBlock 'release_channel'

Write-Step "NextIDE packaging baseline (repo root: $RepoRoot)"
Write-Host "  manifest: $ManifestPath"
Write-Host "  product:  $($Manifest.product.name) $ProductVersion"
Write-Host "  version:  manifest '$ProductVersion' == Cargo.toml [workspace.package] '$CargoVersion'"
Write-Host "  target:   $TargetOs/$TargetArch ($TargetTriple)"
Write-Host "  release:  license=$ReleaseLicense publisher=$ReleasePub channel=$ReleaseChannel"
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
$Verified = @()
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

    $Verified += [pscustomobject]@{ File = $StagedFile.Name; Sha256 = $StagedSha }

    $ReportParts = @($StagedFile.Name)
    if ($Report -contains 'size_bytes') { $ReportParts += "$($StagedFile.Length) bytes" }
    if ($Report -contains 'sha256')     { $ReportParts += "sha256 $StagedSha" }
    Write-Host ('  ' + ($ReportParts -join '  '))
}

Write-Step "Writing $ChecksumFile checksum manifest"
# One '<64-hex-sha256>  <file>' line per staged artifact, in manifest artifact
# order, LF line endings (GNU sha256sum -c compatible). Built from the verified
# hashes above — never re-hashed — so the manifest can only contain hashes that
# already proved to match the staged files.
$ChecksumPath = Join-Path $DistDir $ChecksumFile
$ChecksumText = (($Verified | ForEach-Object { '{0}  {1}' -f $_.Sha256, $_.File }) -join "`n")
Set-Content -LiteralPath $ChecksumPath -Value $ChecksumText -Encoding Ascii -NoNewline
Write-Host "  wrote $ChecksumFile ($($Verified.Count) entries)"

# Re-verify the checksum manifest by parsing it back: every line must be in
# '<hash>  <file>' format, there must be exactly one entry per staged artifact,
# and every entry's hash must equal the verified staged hash. A checksum
# manifest that does not parse back or does not match is a hard error — the
# file that ships is proven consistent, not just present.
$ReadBack = Get-Content -Raw -LiteralPath $ChecksumPath
$Parsed = @{}
foreach ($Line in ($ReadBack -split "`n")) {
    if ([string]::IsNullOrEmpty($Line)) { continue }
    if ($Line -notmatch '^([0-9a-fA-F]{64})  ([^\r\n]+)$') {
        throw "Checksum manifest line is not in '<hash>  <file>' format: '$Line'"
    }
    $Parsed[$Matches[2]] = $Matches[1].ToUpperInvariant()
}
if ($Parsed.Count -ne $Verified.Count) {
    throw "Checksum manifest entry count mismatch: $ChecksumPath has $($Parsed.Count) entries, expected $($Verified.Count)"
}
foreach ($Entry in $Verified) {
    if (-not $Parsed.ContainsKey($Entry.File)) {
        throw "Checksum manifest is missing an entry for staged artifact $($Entry.File)"
    }
    if ($Parsed[$Entry.File] -ne $Entry.Sha256) {
        throw "Checksum manifest hash mismatch for $($Entry.File): file says $($Parsed[$Entry.File]), verified is $($Entry.Sha256)"
    }
}
Write-Host "  verified ${ChecksumFile}: every line parses and matches the staged SHA-256"

Write-Step "Done. Release artifacts staged under $DistDir (incl. $ChecksumFile)"
