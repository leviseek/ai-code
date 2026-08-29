#Requires -Version 5.1
<#
.SYNOPSIS
    End-user one-command verification of a GitHub Release download
    (checksums + CLI smoke).

.DESCRIPTION
    Downloads the release's two Windows binaries and its checksum manifest
    from a GitHub Release, verifies the checksums with the repository's own
    consumer verifier (verify-checksums.ps1, fetched from the same tag), and
    smoke-tests the downloaded CLI — the same two gates the CI `release` job
    runs against the exact bytes it publishes. Exit code 0 means every gate
    passed; any failure exits non-zero with a message. A failed verification
    means: do NOT run the downloaded binaries.

    The script is the scripted form of the manual "Verifying a GitHub Release
    download (end user)" steps in packaging/windows/README.md. It never
    re-implements checksum verification or the smoke flow — it delegates to
    them. It downloads exactly the files an end user would download manually
    and resolves the checksum manifest file name from the release's own
    manifest.json (stage.checksum_file), never from a hardcoded default, so a
    release that renamed its checksum file keeps verifying instead of breaking.

    Flow:
      1. validate -Tag (must be exactly v<semver>), -Repo (owner/name) and the
         -BaseReleaseUrl/-VerifyScriptUrl schemes (each must be an absolute
         https:// or file:// URL — http and any other scheme are refused);
      2. resolve -OutDir/-MetadataDir to full paths and validate their shape
         read-only — the two folders must be distinct, neither inside the
         other (the metadata must stay OUTSIDE the verified folder: a nested
         metadata folder would sit inside the verified tree and silently
         bypass the verifier's exact-match contract) and neither inside the
         repository (the repository root — derived from this script's own
         location, or stated explicitly with -RepositoryRoot — and its
         staging folder dist/ are refused) — nothing is created or downloaded
         yet;
      3. create -OutDir (the verified folder) and -MetadataDir (its sibling
         by default, '<OutDir>-metadata');
      4. download the release's manifest.json into MetadataDir and parse it
         read-only: it must be valid JSON, product.version must be non-blank
         and exactly the -Tag version, and stage.checksum_file must be a
         non-blank plain file name — the tag/version and checksum-file-name
         rules the CI release job enforces, checked here on the consumer side;
      5. download nextide-cli.exe, nextide-shell.exe and the declared checksum
         file into OutDir, each proven present and non-empty after download;
      6. download verify-checksums.ps1 from the same tag into MetadataDir —
         outside the verified folder, because the checksum manifest covers
         only the two binaries and the verifier enforces an exact-match
         contract: any unlisted file in the verified folder, the verifier
         itself included, is rejected;
      7. run the downloaded verifier read-only against OutDir (-DistDir) with
         the declared checksum file (-ChecksumFile);
      8. unless -SkipSmoke, run the downloaded nextide-cli.exe smoke — the
         editor-kernel smoke flow, the same gate CI runs;
      9. print a summary and exit 0.

    Safety: this script downloads files — the one consumer step that must
    write — and writes only into -OutDir and -MetadataDir, never into the
    repository: it refuses -OutDir/-MetadataDir that resolve to the
    repository root or to its staging folder (dist/), and it keeps the
    metadata folder out of the verified folder by refusing equal or nested
    folder pairs. The repository root is derived from this script's own
    location (packaging\windows under the checkout) and may be stated
    explicitly with -RepositoryRoot; the derived default is only trusted
    when that layout is real (the script's sibling verify-checksums.ps1 is
    present) — a copy of the script run from elsewhere fails and asks for
    -RepositoryRoot instead of silently protecting a guessed root. The
    defaults resolve under the current directory
    (.\nextide-download / .\nextide-download-metadata), so running from the
    repository root places the download folder inside the checkout — run from
    a scratch/download directory, or pass -OutDir and -MetadataDir, to keep
    the repository untouched. It downloads only over https (real releases) or
    file (local mirrors, the sandboxed self-test) — http and any other scheme
    are refused, because the checksum manifest and the consumer verifier
    travel over the same URLs and an unauthenticated transport would let a
    network attacker substitute binaries and manifest together. It never
    executes a downloaded binary before its checksum has been verified, it
    never executes nextide-shell.exe at all, and the only executable it runs
    is the downloaded nextide-cli.exe smoke flow.

.PARAMETER Tag
    The GitHub Release tag to verify, e.g. v0.1.0. Must be exactly
    v<semver> — the same rule the CI release job enforces.

.PARAMETER Repo
    The GitHub repository as "owner/name". Defaults to leviseek/ai-code.

.PARAMETER OutDir
    The clean folder the two binaries and the checksum manifest are
    downloaded into and verified against. Defaults to .\nextide-download
    (resolved against the current directory). The folder must hold nothing
    else when verification starts: the verifier enforces an exact-match
    contract, so any pre-existing extra file makes verification fail (that
    failure is the contract working — use a fresh folder, or remove stale
    files first). Must be a distinct folder from -MetadataDir, and neither
    may be inside the other — validated read-only before anything is created
    or downloaded. The repository root and its staging folder (dist/) are
    refused as targets.

.PARAMETER MetadataDir
    Folder outside OutDir that receives the release's manifest.json and the
    downloaded verify-checksums.ps1. Defaults to <OutDir>-metadata (derived
    from the normalized OutDir, so a trailing separator in -OutDir cannot
    make it a subfolder of the verified folder). Both are release metadata,
    not checksum-covered files, so they must stay out of the verified
    folder: -OutDir/-MetadataDir pairs that are equal or nested are rejected
    before anything is created or downloaded, so the exact-match contract
    cannot be bypassed by nesting the metadata folder inside the verified
    folder.

.PARAMETER SkipSmoke
    Skip the nextide-cli.exe smoke step. Checksum verification still runs.

.PARAMETER BaseReleaseUrl
    Override the base URL of the release assets (default:
    https://github.com/<Repo>/releases/download/<Tag>). Intended for mirrors
    and for the sandboxed self-test (file:// URIs); not needed for normal use.
    Must be an absolute https:// or file:// URL — http and any other scheme
    are refused: the checksum manifest and the consumer verifier are fetched
    over the same URLs, so an unauthenticated transport would let a network
    attacker substitute the binaries and their manifest together.

.PARAMETER VerifyScriptUrl
    Override the URL of the consumer verifier (default:
    https://raw.githubusercontent.com/<Repo>/<Tag>/packaging/windows/verify-checksums.ps1).
    Same override intent as -BaseReleaseUrl, and the same scheme rule: must
    be an absolute https:// or file:// URL — http and any other scheme are
    refused.

.PARAMETER RepositoryRoot
    The repository checkout that must never be written into: -OutDir/
    -MetadataDir resolving to this root or to its staging folder (dist/) are
    refused. Defaults to the root derived from this script's own location
    (packaging\windows under the checkout). The derived default is only
    trusted when that layout is real (the script's sibling
    verify-checksums.ps1 is present) — a copy of the script run from
    elsewhere fails and asks for -RepositoryRoot instead of silently
    protecting a guessed root, so the default is safe by construction. Pass
    it explicitly when running a copy of the script or a checkout with a
    different layout. Normalized to a full path and must be an existing
    directory; validated read-only before anything is created or downloaded.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\verify-release-download.ps1 -Tag v0.1.0

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\verify-release-download.ps1 -Tag v0.1.0 -OutDir C:\downloads\nextide -SkipSmoke

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File C:\tools\verify-release-download.ps1 -Tag v0.1.0 -RepositoryRoot C:\work\ai-code
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$Repo = 'leviseek/ai-code',
    [string]$OutDir = '',
    [string]$MetadataDir = '',
    [switch]$SkipSmoke,
    [string]$BaseReleaseUrl = '',
    [string]$VerifyScriptUrl = '',
    [string]$RepositoryRoot = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail {
    # Report a verification failure and exit non-zero. A failed release
    # verification means the downloaded binaries must not be run.
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "VERIFICATION FAILED: $Message"
    Write-Host '==> Do NOT run the downloaded binaries.'
    exit 1
}

function Download-File {
    # Download one file with a single transport (System.Net.WebClient — a
    # .NET Framework/Core built-in, no dependencies) that handles both https
    # (real releases) and file:// (mirrors and the sandboxed self-test). The
    # downloaded file must exist and be non-empty afterwards: the release-job
    # checklist treats a zero-byte binary as a failure even when the checksum
    # manifest still matches.
    param(
        [Parameter(Mandatory)][string]$Url,
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][string]$What
    )
    $WebClient = New-Object System.Net.WebClient
    try {
        $WebClient.DownloadFile($Url, $Destination)
    }
    catch {
        throw "Failed to download $What from $Url : $($_.Exception.Message)"
    }
    finally {
        $WebClient.Dispose()
    }
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf) -or (Get-Item -LiteralPath $Destination).Length -eq 0) {
        throw "Downloaded $What is missing or empty: $Destination"
    }
}

# 1. Validate the tag and the repository shape before anything is created or
#    downloaded — same tag rule the CI release job enforces.
if ($Tag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
    Fail "Release tag '$Tag' is not a v-prefixed semver tag (expected e.g. v0.1.0)"
}
if ($Repo -notmatch '^[^\s/]+/[^\s/]+$') {
    Fail "Repo '$Repo' must be 'owner/name' without spaces or slashes"
}

if (-not $BaseReleaseUrl)  { $BaseReleaseUrl  = "https://github.com/$Repo/releases/download/$Tag" }
if (-not $VerifyScriptUrl) { $VerifyScriptUrl = "https://raw.githubusercontent.com/$Repo/$Tag/packaging/windows/verify-checksums.ps1" }

# The download transport is the release verifier's integrity channel: the two
# binaries, the checksum manifest and the consumer verifier all travel over
# these URLs, so an unauthenticated transport (http, ftp, ...) would let a
# network attacker substitute the binaries AND their checksum manifest
# together — verification would then "pass" on attacker-chosen bytes. Only
# https (real releases) and file (local mirrors, the sandboxed self-test) are
# allowed; anything else is refused read-only, before any folder is created or
# file downloaded.
function Test-AllowedDownloadUrl {
    # True when $Url is an absolute URI whose scheme is exactly https or file
    # (case-insensitive).
    param([Parameter(Mandatory)][string]$Url)
    $Uri = $null
    if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref]$Uri)) { return $false }
    return ($Uri.Scheme -eq 'https') -or ($Uri.Scheme -eq 'file')
}
foreach ($Entry in @(
    @{ Url = $BaseReleaseUrl;  What = 'release asset' },
    @{ Url = $VerifyScriptUrl; What = 'consumer verifier' }
)) {
    if (-not (Test-AllowedDownloadUrl $Entry.Url)) {
        Fail "The $($Entry.What) URL must be an absolute https:// or file:// URL — a release verifier must never download over an unauthenticated channel (http and any other scheme are refused) — got: '$($Entry.Url)'"
    }
}

# 2. Resolve -OutDir/-MetadataDir to full paths and validate their shape —
#    all read-only, before anything is created or downloaded. The metadata
#    folder must stay OUTSIDE the verified folder: the consumer verifier
#    enforces an exact-match contract over the verified folder, so a metadata
#    folder equal to it (its manifest.json and the verifier itself would land
#    in the verified folder) or nested inside it (they would land inside the
#    verified tree, silently bypassing the exact-match contract) would break
#    the documented guarantee.
function Test-SamePath {
    # Case-insensitive path equality (Windows paths), tolerating a trailing
    # separator on either side.
    param([Parameter(Mandatory)][string]$A, [Parameter(Mandatory)][string]$B)
    return [string]::Equals($A.TrimEnd([char[]]@('/', '\')), $B.TrimEnd([char[]]@('/', '\')), [System.StringComparison]::OrdinalIgnoreCase)
}

function Test-PathWithin {
    # True when $Child is strictly inside $Parent (case-insensitive, with a
    # path boundary: 'C:\dl2' is not inside 'C:\dl'). A drive root like
    # 'C:\' trims to 'C:' and still works as a prefix.
    param([Parameter(Mandatory)][string]$Child, [Parameter(Mandatory)][string]$Parent)
    return $Child.StartsWith($Parent.TrimEnd([char[]]@('/', '\')) + '\', [System.StringComparison]::OrdinalIgnoreCase)
}

if (-not $OutDir) { $OutDir = Join-Path (Get-Location) 'nextide-download' }
try {
    $OutDir = [System.IO.Path]::GetFullPath($OutDir)
}
catch {
    Fail "Invalid -OutDir path: $($_.Exception.Message)"
}
# The metadata default derives from the normalized OutDir, so a trailing
# separator in -OutDir cannot make the default metadata folder a subfolder of
# the verified folder (previously '<OutDir>-metadata' with -OutDir 'C:\foo\'
# resolved to 'C:\foo\-metadata', silently nested inside the verified folder).
if (-not $MetadataDir) { $MetadataDir = "$OutDir-metadata" }
try {
    $MetadataDir = [System.IO.Path]::GetFullPath($MetadataDir)
}
catch {
    Fail "Invalid -MetadataDir path: $($_.Exception.Message)"
}

if (Test-SamePath $OutDir $MetadataDir) {
    Fail "-OutDir and -MetadataDir must be two different folders (both resolve to '$OutDir'): the release metadata would land inside the verified folder and be rejected by the exact-match contract"
}
if (Test-PathWithin $MetadataDir $OutDir) {
    Fail "-MetadataDir ('$MetadataDir') must be outside the verified folder -OutDir ('$OutDir'): a nested metadata folder would sit inside the verified tree and silently bypass the verifier's exact-match contract. Pick a sibling folder (default '<OutDir>-metadata')"
}
if (Test-PathWithin $OutDir $MetadataDir) {
    Fail "-OutDir ('$OutDir') must be outside the metadata folder -MetadataDir ('$MetadataDir'): the verified folder and the metadata folder must be two separate folders. Pick a sibling folder (default '<OutDir>-metadata')"
}

# The documented 'never into the repository' rule, enforced: refuse the
# repository root and its staging folder (dist/) as download targets. The
# root is derived from this script's own location (packaging\windows under
# the checkout) by default; -RepositoryRoot states it explicitly.
$RepositoryRootDerived = $false
if (-not $RepositoryRoot) {
    $RepositoryRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $RepositoryRootDerived = $true
}
try {
    $RepositoryRoot = [System.IO.Path]::GetFullPath($RepositoryRoot)
}
catch {
    Fail "Invalid -RepositoryRoot path: $($_.Exception.Message)"
}
if (-not (Test-Path -LiteralPath $RepositoryRoot -PathType Container)) {
    Fail "-RepositoryRoot must be an existing directory (the repository checkout that must never be written into), got: '$RepositoryRoot'"
}
if ($RepositoryRootDerived) {
    # Fail-safe default: the derived root is only trusted when this script
    # really sits in packaging\windows of that checkout (its sibling
    # verify-checksums.ps1 is present). A copy of the script run from
    # elsewhere derives a guessed root; protecting the wrong folder would
    # silently disable the guarantee, so fail and ask for -RepositoryRoot
    # instead — the default is safe by construction.
    if (-not (Test-Path -LiteralPath (Join-Path $RepositoryRoot 'packaging\windows\verify-checksums.ps1') -PathType Leaf)) {
        Fail "Cannot trust the repository root derived from this script's location ('$RepositoryRoot'): the script is expected to live in packaging\windows under the repository root, next to verify-checksums.ps1. Pass -RepositoryRoot <path> explicitly to state the repository checkout that must never be written into."
    }
}
$StagingRoot = Join-Path $RepositoryRoot 'dist'
foreach ($Folder in @($OutDir, $MetadataDir)) {
    if ((Test-SamePath $Folder $RepositoryRoot) -or (Test-SamePath $Folder $StagingRoot) -or (Test-PathWithin $Folder $StagingRoot)) {
        Fail "Refusing to write into the repository: -OutDir/-MetadataDir must not resolve to the repository root ('$RepositoryRoot') or its staging folder (dist/), got: '$Folder'. Run from a scratch/download directory, or pass -OutDir/-MetadataDir outside the repository checkout."
    }
}

Write-Host "==> Verifying GitHub Release $Tag ($Repo)"
Write-Host "  release assets : $BaseReleaseUrl"
Write-Host "  verified folder: $OutDir"
Write-Host "  metadata folder: $MetadataDir"
Write-Host "  repository root: $RepositoryRoot (never written into)"

# 3. Create the two folders. All downloads land here — nothing else is ever
#    written.
$null = New-Item -ItemType Directory -Force -Path $OutDir, $MetadataDir

# 4. The release's own manifest.json is the single source of truth for the
#    version and the checksum file name. A release whose manifest is missing,
#    unparseable, version-disagreeing or checksum-file-less is broken by
#    construction — fail before downloading the binaries.
$ManifestPath = Join-Path $MetadataDir 'manifest.json'
Download-File -Url "$BaseReleaseUrl/manifest.json" -Destination $ManifestPath -What 'release metadata manifest.json'
try {
    $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
}
catch {
    Fail "Release manifest.json is not valid JSON: $($_.Exception.Message)"
}
if ($null -eq $Manifest) {
    Fail 'Release manifest.json is empty'
}
$Version = ''
if ($null -ne $Manifest.product) { $Version = [string]$Manifest.product.version }
if ([string]::IsNullOrWhiteSpace($Version)) {
    Fail 'Release manifest.json declares no product.version'
}
if ($Tag.Substring(1) -ne $Version) {
    Fail "Release tag '$Tag' does not match the release's manifest product.version '$Version' (tag must be v<product.version>)"
}
$ChecksumFile = ''
if ($null -ne $Manifest.stage) { $ChecksumFile = [string]$Manifest.stage.checksum_file }
if ([string]::IsNullOrWhiteSpace($ChecksumFile)) {
    Fail 'Release manifest.json declares no stage.checksum_file'
}
if ($ChecksumFile -match '[\\/]' -or $ChecksumFile -eq '.' -or $ChecksumFile -eq '..') {
    Fail "Release manifest.json stage.checksum_file must be a plain file name, got: '$ChecksumFile'"
}

# 5. Download the two binaries and the manifest-declared checksum file into
#    the verified folder.
$CliPath       = Join-Path $OutDir 'nextide-cli.exe'
$ShellPath     = Join-Path $OutDir 'nextide-shell.exe'
$ChecksumPath  = Join-Path $OutDir $ChecksumFile
Download-File -Url "$BaseReleaseUrl/nextide-cli.exe"   -Destination $CliPath       -What 'release binary nextide-cli.exe'
Download-File -Url "$BaseReleaseUrl/nextide-shell.exe" -Destination $ShellPath     -What 'release binary nextide-shell.exe'
Download-File -Url "$BaseReleaseUrl/$ChecksumFile"     -Destination $ChecksumPath  -What "checksum manifest $ChecksumFile"

# 6. Fetch the repository's consumer verifier from the same tag — outside the
#    verified folder (the exact-match contract rejects any unlisted file, the
#    verifier itself included).
$VerifierPath = Join-Path $MetadataDir 'verify-checksums.ps1'
Download-File -Url $VerifyScriptUrl -Destination $VerifierPath -What 'consumer verifier verify-checksums.ps1'

# 7. Run the downloaded verifier in a child process of the same PowerShell
#    edition so the real exit-code contract (0 = success) is honored. Its
#    stderr under Windows PowerShell 5.1 becomes error records; capture with
#    EAP=Continue so the exit code below is the value under test, then fail on
#    a non-zero exit explicitly.
Write-Host "==> Verifying checksums with the release's own consumer verifier (read-only)"
if ($PSVersionTable.PSEdition -eq 'Core') {
    $ShellExe = Join-Path $PSHOME 'pwsh.exe'
}
else {
    $ShellExe = Join-Path $PSHOME 'powershell.exe'
}
if (-not (Test-Path -LiteralPath $ShellExe)) {
    Fail "PowerShell interpreter not found: $ShellExe"
}
$PreviousEAP = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
try {
    & $ShellExe -NoProfile -ExecutionPolicy Bypass -File $VerifierPath -DistDir $OutDir -ChecksumFile $ChecksumPath
}
finally {
    $ErrorActionPreference = $PreviousEAP
}
if ($LASTEXITCODE -ne 0) {
    Fail "Checksum verification of the downloaded files failed (exit $LASTEXITCODE) - the download does not match its checksum manifest"
}

# 8. Smoke-test the downloaded CLI — the same editor-kernel smoke flow the CI
#    release job runs against the exact bytes it publishes. Runs only after
#    the checksums passed; nextide-shell.exe is never executed.
if (-not $SkipSmoke) {
    Write-Host '==> Smoke-testing the downloaded CLI (nextide-cli.exe smoke)'
    try {
        & $CliPath smoke
    }
    catch {
        Fail "Smoke test could not run nextide-cli.exe smoke: $($_.Exception.Message)"
    }
    if ($LASTEXITCODE -ne 0) {
        Fail "nextide-cli.exe smoke failed (exit $LASTEXITCODE) - the binary is not trustworthy"
    }
}

Write-Host "==> OK. Release $Tag verified: checksums match the manifest and the CLI smoke passes."
Write-Host "  verified folder : $OutDir (nextide-cli.exe, nextide-shell.exe, $ChecksumFile)"
Write-Host "  metadata folder : $MetadataDir (manifest.json, verify-checksums.ps1)"
exit 0
