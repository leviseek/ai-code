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
      1. validate -Tag (must be exactly v<semver>) and -Repo (owner/name);
      2. create -OutDir (the verified folder) and -MetadataDir (its sibling);
      3. download the release's manifest.json into MetadataDir and parse it
         read-only: it must be valid JSON, product.version must be non-blank
         and exactly the -Tag version, and stage.checksum_file must be a
         non-blank plain file name — the tag/version and checksum-file-name
         rules the CI release job enforces, checked here on the consumer side;
      4. download nextide-cli.exe, nextide-shell.exe and the declared checksum
         file into OutDir, each proven present and non-empty after download;
      5. download verify-checksums.ps1 from the same tag into MetadataDir —
         outside the verified folder, because the checksum manifest covers
         only the two binaries and the verifier enforces an exact-match
         contract: any unlisted file in the verified folder, the verifier
         itself included, is rejected;
      6. run the downloaded verifier read-only against OutDir (-DistDir) with
         the declared checksum file (-ChecksumFile);
      7. unless -SkipSmoke, run the downloaded nextide-cli.exe smoke — the
         editor-kernel smoke flow, the same gate CI runs;
      8. print a summary and exit 0.

    Safety: this script downloads files — the one consumer step that must
    write — and writes only into -OutDir and -MetadataDir, never into the
    repository. It never executes a downloaded binary before its checksum has
    been verified, it never executes nextide-shell.exe at all, and the only
    executable it runs is the downloaded nextide-cli.exe smoke flow.

.PARAMETER Tag
    The GitHub Release tag to verify, e.g. v0.1.0. Must be exactly
    v<semver> — the same rule the CI release job enforces.

.PARAMETER Repo
    The GitHub repository as "owner/name". Defaults to leviseek/ai-code.

.PARAMETER OutDir
    The clean folder the two binaries and the checksum manifest are
    downloaded into and verified against. Defaults to .\nextide-download.
    The folder must hold nothing else when verification starts: the verifier
    enforces an exact-match contract, so any pre-existing extra file makes
    verification fail (that failure is the contract working — use a fresh
    folder, or remove stale files first).

.PARAMETER MetadataDir
    Folder outside OutDir that receives the release's manifest.json and the
    downloaded verify-checksums.ps1. Defaults to <OutDir>-metadata. Both are
    release metadata, not checksum-covered files, so they must stay out of
    the verified folder.

.PARAMETER SkipSmoke
    Skip the nextide-cli.exe smoke step. Checksum verification still runs.

.PARAMETER BaseReleaseUrl
    Override the base URL of the release assets (default:
    https://github.com/<Repo>/releases/download/<Tag>). Intended for mirrors
    and for the sandboxed self-test (file:// URIs); not needed for normal use.

.PARAMETER VerifyScriptUrl
    Override the URL of the consumer verifier (default:
    https://raw.githubusercontent.com/<Repo>/<Tag>/packaging/windows/verify-checksums.ps1).
    Same override intent as -BaseReleaseUrl.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\verify-release-download.ps1 -Tag v0.1.0

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\verify-release-download.ps1 -Tag v0.1.0 -OutDir C:\downloads\nextide -SkipSmoke
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$Repo = 'leviseek/ai-code',
    [string]$OutDir = '',
    [string]$MetadataDir = '',
    [switch]$SkipSmoke,
    [string]$BaseReleaseUrl = '',
    [string]$VerifyScriptUrl = ''
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
if (-not $OutDir)          { $OutDir      = Join-Path (Get-Location) 'nextide-download' }
if (-not $MetadataDir)     { $MetadataDir = "$OutDir-metadata" }

Write-Host "==> Verifying GitHub Release $Tag ($Repo)"
Write-Host "  release assets : $BaseReleaseUrl"
Write-Host "  verified folder: $OutDir"
Write-Host "  metadata folder: $MetadataDir"

# 2. Create the two folders. All downloads land here — nothing else is ever
#    written.
$null = New-Item -ItemType Directory -Force -Path $OutDir, $MetadataDir

# 3. The release's own manifest.json is the single source of truth for the
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

# 4. Download the two binaries and the manifest-declared checksum file into
#    the verified folder.
$CliPath       = Join-Path $OutDir 'nextide-cli.exe'
$ShellPath     = Join-Path $OutDir 'nextide-shell.exe'
$ChecksumPath  = Join-Path $OutDir $ChecksumFile
Download-File -Url "$BaseReleaseUrl/nextide-cli.exe"   -Destination $CliPath       -What 'release binary nextide-cli.exe'
Download-File -Url "$BaseReleaseUrl/nextide-shell.exe" -Destination $ShellPath     -What 'release binary nextide-shell.exe'
Download-File -Url "$BaseReleaseUrl/$ChecksumFile"     -Destination $ChecksumPath  -What "checksum manifest $ChecksumFile"

# 5. Fetch the repository's consumer verifier from the same tag — outside the
#    verified folder (the exact-match contract rejects any unlisted file, the
#    verifier itself included).
$VerifierPath = Join-Path $MetadataDir 'verify-checksums.ps1'
Download-File -Url $VerifyScriptUrl -Destination $VerifierPath -What 'consumer verifier verify-checksums.ps1'

# 6. Run the downloaded verifier in a child process of the same PowerShell
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

# 7. Smoke-test the downloaded CLI — the same editor-kernel smoke flow the CI
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
