#Requires -Version 5.1
<#
.SYNOPSIS
    Self-contained sandbox simulation tests for the CI release job
    (.github/workflows/ci.yml, `release` job): tag/version rejection rules and
    the release file manifest.

.DESCRIPTION
    Simulates the release job's gate rules in a pure PowerShell sandbox with no
    external test framework, no network access and no GitHub Release ever
    being created. The release job itself is inline pwsh in the workflow (it
    consumes GitHub Actions artifacts and calls the GitHub CLI at the very
    end), so this suite re-implements the two rule sets it gates as plain
    functions and proves them against throwaway sandbox copies under the
    sandbox root (default: $env:TEMP) — the real repository is never written,
    renamed or removed, and the only child process ever launched is the
    repository's own consumer-side verifier (verify-checksums.ps1) against a
    sandbox directory.

    Part 1 — tag/version rejection rules (the workflow's "Resolve release
    metadata from manifest (channel + version + checksum file vs tag)" step,
    mirrored exactly: same checks, same order, same messages):
      - channel must be exactly 'stable' or 'beta'
      - product.version must be non-blank
      - stage.checksum_file must be non-blank (the checksum manifest name is
        not hardcoded anywhere in the chain — it flows from the committed
        manifest, so the verify gates and the publish step attach exactly the
        file build.ps1 generates)
      - the pushed tag must match '^v[0-9]+\.[0-9]+\.[0-9]+$'
      - the tag minus its leading 'v' must equal product.version
    Every violation is rejected; the committed manifest resolves with the
    matching v<version> tag.

    Part 2 — release file manifest (the four files the publish step would
    attach — dist/windows/nextide-cli.exe, dist/windows/nextide-shell.exe,
    the checksum manifest named by stage.checksum_file and
    release-metadata/manifest.json):
      - every attachment exists and is a non-empty regular file (a zero-byte
        or absent binary could not pass the job's CLI smoke step either)
      - dist/windows passes the repository's consumer-side verifier
        (verify-checksums.ps1 -DistDir dist\windows -ChecksumFile
        dist\windows\<stage.checksum_file>), the exact gate the release job
        runs before publishing — the checksum file is verified under the
        declared name, exactly like the workflow
      - the checksum manifest entry set equals the manifest artifact list
        (the two release binaries) and the checksum file is staged under the
        name stage.checksum_file declares
      - release-metadata/manifest.json is byte-identical (SHA-256) to the
        committed manifest.json the pipeline validated

    Part 2b — publish attachment naming (the exact file list the publish
    step hands to the GitHub CLI's release-creation command): the four paths
    are built from the resolved stage.checksum_file — `dist/windows/<checksum
    file>` for the checksum entry, never a hardcoded default — so a renamed
    checksum file in the committed manifest flows coherently through resolve,
    verify, checklist and the release's attachment list.

    Part 3 — self-audit: the script's own source contains no network or
    release-creation command invocations (it can never touch the network and
    can never create a Release).

    Covered scenarios (26 tests):
      1.  committed manifest + matching v<version> tag resolves
      2.  beta channel (valid channel) resolves
      3.  non-v tag (e.g. 'main') is rejected
      4.  malformed semver tag (e.g. 'v0.1') is rejected
      5.  pre-release tag (e.g. 'v0.1.0-beta') is rejected
      6.  tag disagreeing with manifest product.version is rejected
      7.  invalid release channel (e.g. 'alpha') is rejected
      8.  blank product.version is rejected
      9.  missing release block is rejected
     10.  invalid manifest JSON is rejected
     11.  missing manifest is rejected
     12.  blank stage.checksum_file is rejected
     13.  complete, consistent release file manifest passes
     14.  missing nextide-cli.exe fails
     15.  missing nextide-shell.exe fails
     16.  missing checksum manifest (the stage.checksum_file name) fails
     17.  missing release-metadata/manifest.json fails
     18.  tampered binary fails consumer verification
     19.  extra unlisted file in dist fails consumer verification
     20.  checksum entries not matching the manifest artifact list fail
     21.  release-metadata manifest not byte-identical to the committed one fails
     22.  zero-byte CLI binary fails (smoke-gate simulation)
     23.  renamed stage.checksum_file is honored by the checklist (byte-identity still enforced)
     24.  self-audit: no network or release-creation commands in the source
     25.  publish attachment list uses the resolved checksum file name (never hardcoded)
     26.  renamed stage.checksum_file flows through resolve -> verify -> checklist -> attachment list

    The script exits 0 when every test passes and 1 when any fails.

.PARAMETER ManifestSource
    Path of the committed packaging manifest used as the golden baseline
    (read-only). Defaults to manifest.json next to this script. The sandbox
    copies and the byte-identity checks all derive from this file; it is never
    modified.

.PARAMETER VerifyScript
    Path of the verify-checksums.ps1 consumer verifier invoked against sandbox
    dist directories. Defaults to verify-checksums.ps1 next to this script.

.PARAMETER SandboxRoot
    Directory under which the per-test sandboxes are created. Defaults to
    $env:TEMP. The real repository is never used as a sandbox.

.PARAMETER KeepSandbox
    Keep the sandbox directories after the run (useful for debugging); by
    default every sandbox created by this script is deleted again. The only
    thing this script ever deletes is its own sandboxes under $SandboxRoot.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-release-sim.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-release-sim.ps1 -KeepSandbox
#>
[CmdletBinding()]
param(
    [string]$ManifestSource = '',
    [string]$VerifyScript = '',
    [string]$SandboxRoot = $env:TEMP,
    [switch]$KeepSandbox
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $ManifestSource) { $ManifestSource = Join-Path $PSScriptRoot 'manifest.json' }
if (-not (Test-Path -LiteralPath $ManifestSource)) {
    throw "Manifest source not found: $ManifestSource"
}
if (-not $VerifyScript) { $VerifyScript = Join-Path $PSScriptRoot 'verify-checksums.ps1' }
if (-not (Test-Path -LiteralPath $VerifyScript)) {
    throw "verify-checksums.ps1 under test not found: $VerifyScript"
}
if (-not (Test-Path -LiteralPath $SandboxRoot)) {
    throw "Sandbox root does not exist: $SandboxRoot"
}

function Get-JsonProperty {
    # Strict-mode-safe read of a possibly-missing JSON property. The release
    # job's inline pwsh runs without strict mode and would simply see $null
    # for a missing key; this mirrors that without tripping
    # Set-StrictMode -Version Latest here.
    param(
        [Parameter(Mandatory)]$Object,
        [Parameter(Mandatory)][string]$Name
    )
    $Prop = $Object.PSObject.Properties[$Name]
    if ($null -eq $Prop) { return $null }
    return $Prop.Value
}

# The committed manifest is the golden single source of truth: its version
# drives the happy-path tags so the tests stay version-agnostic, and its bytes
# are the baseline for the byte-identity check. It is only ever read.
$Golden = Get-Content -Raw -LiteralPath $ManifestSource | ConvertFrom-Json
$GoldenProduct = Get-JsonProperty -Object $Golden -Name 'product'
$script:GoldenVersion = ''
if ($null -ne $GoldenProduct) {
    $script:GoldenVersion = [string](Get-JsonProperty -Object $GoldenProduct -Name 'version')
}
if ([string]::IsNullOrWhiteSpace($script:GoldenVersion)) {
    throw "Manifest source does not declare product.version: $ManifestSource"
}
$GoldenRelease = Get-JsonProperty -Object $Golden -Name 'release'
$script:GoldenChannel = ''
if ($null -ne $GoldenRelease) {
    $script:GoldenChannel = [string](Get-JsonProperty -Object $GoldenRelease -Name 'release_channel')
}

# The checksum manifest name is not hardcoded anywhere in this suite: it flows
# from the golden manifest's stage.checksum_file (the same single source of
# truth build.ps1 generates from and the release job resolves), so renaming
# the checksum file in the committed manifest keeps every test and the whole
# simulated checklist consistent without code edits.
$script:GoldenChecksumFile = 'SHA256SUMS.txt'
$GoldenStage = Get-JsonProperty -Object $Golden -Name 'stage'
if ($null -ne $GoldenStage) {
    $DeclaredChecksum = [string](Get-JsonProperty -Object $GoldenStage -Name 'checksum_file')
    if (-not [string]::IsNullOrWhiteSpace($DeclaredChecksum)) {
        $script:GoldenChecksumFile = $DeclaredChecksum
    }
}

# Run verify-checksums.ps1 in a child process of the same PowerShell edition so
# the exit code contract (0 = success, non-zero = failure) is tested for real.
if ($PSVersionTable.PSEdition -eq 'Core') {
    $ShellExe = Join-Path $PSHOME 'pwsh.exe'
}
else {
    $ShellExe = Join-Path $PSHOME 'powershell.exe'
}
if (-not (Test-Path -LiteralPath $ShellExe)) {
    throw "PowerShell interpreter not found: $ShellExe"
}

$script:Passed    = 0
$script:Failed    = 0
$script:Failures  = @()
$script:Detail    = $null
$script:Sandboxes = @()

function Invoke-Test {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Body
    )
    Write-Host ''
    Write-Host "==> $Name"
    $script:Detail = $null
    $Passed = $false
    try {
        & $Body
        $Passed = $true
    }
    catch {
        $Passed = $false
        if (-not $script:Detail) { $script:Detail = $_.Exception.Message }
    }
    if ($Passed) {
        $script:Passed++
        Write-Host "    [PASS] $Name"
    }
    else {
        $script:Failed++
        Write-Host "    [FAIL] $Name"
        if ($script:Detail) { Write-Host "           $($script:Detail)" }
        $script:Failures += "$Name -- $($script:Detail)"
    }
}

function Assert {
    param(
        [Parameter(Mandatory)][bool]$Condition,
        [Parameter(Mandatory)][string]$Message
    )
    if (-not $Condition) {
        $script:Detail = $Message
        throw $Message
    }
}

function New-Sandbox {
    # A throwaway directory under $SandboxRoot. The real repository is never
    # used as a sandbox.
    $Root = Join-Path $SandboxRoot ('nextide-test-release-sim-' + [guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Force -Path $Root
    $script:Sandboxes += $Root
    return $Root
}

function Remove-Sandbox {
    param([Parameter(Mandatory)][string]$Root)
    if ($KeepSandbox) {
        Write-Host "    (sandbox kept for inspection: $Root)"
        return
    }
    $Leaf = Split-Path -Leaf $Root
    if ($Leaf -notlike 'nextide-test-release-sim-*') {
        throw "Refusing to remove unexpected path: $Root"
    }
    if (Test-Path -LiteralPath $Root) {
        Remove-Item -LiteralPath $Root -Recurse -Force
    }
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function New-FakeFile {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Content
    )
    Set-Content -LiteralPath $Path -Value $Content -Encoding ASCII -NoNewline
}

function Write-ChecksumManifest {
    # Write a GNU-style '<64-hex>  <file>' checksum manifest from an ordered
    # list of 'file' -> 'hash' pairs (LF line endings, as build.ps1 writes).
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][hashtable]$Entries
    )
    $Lines = @()
    foreach ($Key in $Entries.Keys) {
        $Lines += '{0}  {1}' -f $Entries[$Key], $Key
    }
    Set-Content -LiteralPath $Path -Value ($Lines -join "`n") -Encoding ASCII -NoNewline
}

function Write-SandboxManifest {
    # Write an arbitrary manifest JSON under <sandbox>\packaging\windows\ so
    # the release-meta simulation reads the same relative path the workflow
    # uses.
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Json
    )
    $Dir = Join-Path $Root 'packaging\windows'
    $null = New-Item -ItemType Directory -Force -Path $Dir
    Set-Content -LiteralPath (Join-Path $Dir 'manifest.json') -Value $Json -Encoding UTF8
}

function Copy-GoldenManifest {
    # Copy the committed manifest into a sandbox as packaging/windows/manifest.json.
    param([Parameter(Mandatory)][string]$Root)
    $Dir = Join-Path $Root 'packaging\windows'
    $null = New-Item -ItemType Directory -Force -Path $Dir
    Copy-Item -LiteralPath $ManifestSource -Destination (Join-Path $Dir 'manifest.json') -Force
}

# ---------------------------------------------------------------------------
# Part 1 — tag/version rejection rules.
#
# Faithful simulation of the release job's "Resolve release metadata from
# manifest (channel + version vs tag)" step (.github/workflows/ci.yml): same
# checks, same order, same failure messages, returning a structured result
# instead of throwing (so the tests can assert on it). This is the final link
# of the release chain — tag -> manifest product.version -> (proven by the
# packaging job) [workspace.package] Cargo version.
# ---------------------------------------------------------------------------
function Resolve-ReleaseMeta {
    param(
        [Parameter(Mandatory)][string]$ManifestPath,
        [Parameter(Mandatory)][string]$Tag
    )
    if (-not (Test-Path -LiteralPath $ManifestPath)) {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; ChecksumFile = ''; Error = "Missing manifest: $ManifestPath" }
    }
    try {
        $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    }
    catch {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; ChecksumFile = ''; Error = "Manifest is not valid JSON: $($_.Exception.Message)" }
    }
    $Release = Get-JsonProperty -Object $Manifest -Name 'release'
    $Channel = ''
    if ($null -ne $Release) { $Channel = [string](Get-JsonProperty -Object $Release -Name 'release_channel') }
    if ($Channel -ne 'stable' -and $Channel -ne 'beta') {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; ChecksumFile = ''; Error = "Manifest release.release_channel must be exactly 'stable' or 'beta', got: '$Channel'" }
    }
    $Product = Get-JsonProperty -Object $Manifest -Name 'product'
    $Version = ''
    if ($null -ne $Product) { $Version = [string](Get-JsonProperty -Object $Product -Name 'version') }
    if ([string]::IsNullOrWhiteSpace($Version)) {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; ChecksumFile = ''; Error = 'Manifest product.version is missing or blank' }
    }
    $Stage = Get-JsonProperty -Object $Manifest -Name 'stage'
    $ChecksumFile = ''
    if ($null -ne $Stage) { $ChecksumFile = [string](Get-JsonProperty -Object $Stage -Name 'checksum_file') }
    if ([string]::IsNullOrWhiteSpace($ChecksumFile)) {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; ChecksumFile = ''; Error = 'Manifest stage.checksum_file is missing or blank' }
    }
    if ($Tag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; ChecksumFile = $ChecksumFile; Error = "Release tag '$Tag' is not a v-prefixed semver tag (expected e.g. v0.1.0)" }
    }
    if ($Tag.Substring(1) -ne $Version) {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; ChecksumFile = $ChecksumFile; Error = "Release tag '$Tag' does not match manifest product.version '$Version' (tag must be v<product.version>)" }
    }
    return [pscustomobject]@{ Ok = $true; Channel = $Channel; Version = $Version; ChecksumFile = $ChecksumFile; Error = '' }
}

# ---------------------------------------------------------------------------
# Part 2 — release file manifest.
# ---------------------------------------------------------------------------
function Invoke-ConsumerVerifier {
    # Run the repository's consumer-side verifier against a sandbox dist
    # directory — the exact gate the release job runs before publishing. The
    # checksum manifest is verified under the declared stage.checksum_file
    # name (passed as -ChecksumFile), exactly like the workflow's verify
    # steps. Never turn a non-zero native exit code into a terminating error
    # here: the exit code is the value under test. Note: do NOT pre-assign
    # $LASTEXITCODE in this function — that would shadow the automatic
    # variable and the engine would no longer update it.
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][string]$ChecksumName
    )
    $Dist = Join-Path $Sandbox 'dist\windows'
    $ChecksumPath = Join-Path $Dist $ChecksumName
    $ChildArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $VerifyScript, '-DistDir', $Dist, '-ChecksumFile', $ChecksumPath)
    $PSNativeCommandUseErrorActionPreference = $false
    # Under Windows PowerShell 5.1 a native stderr line becomes an error
    # record, and with EAP=Stop the merged 2>&1 pipeline aborts on the first
    # line (truncating the capture). EAP=Continue lets the full output through
    # on both PowerShell editions.
    $PreviousEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $Output = (& $ShellExe $ChildArgs 2>&1 | Out-String)
    }
    finally {
        $ErrorActionPreference = $PreviousEAP
    }
    return [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Output   = $Output
    }
}

function Get-ChecksumManifestEntries {
    # Parse a GNU-style '<64-hex>  <file>' checksum manifest into its ordered
    # file names. Throws on a malformed line (the consumer verifier would
    # reject it too).
    param([Parameter(Mandatory)][string]$Path)
    $Names = [System.Collections.Generic.List[string]]::new()
    foreach ($Line in (Get-Content -LiteralPath $Path)) {
        if ([string]::IsNullOrWhiteSpace($Line)) { continue }
        if ($Line -notmatch '^[0-9a-fA-F]{64}  ([^ ]+)$') {
            throw "checksum manifest line is not '<64-hex>  <file>': '$Line'"
        }
        $Names.Add($Matches[1])
    }
    return @($Names)
}

function Get-ReleaseAttachmentList {
    # Simulate the publish step's attachment list construction: the exact
    # four file paths the workflow hands to the GitHub CLI's release-creation
    # command —
    #   'dist/windows/nextide-cli.exe'
    #   'dist/windows/nextide-shell.exe'
    #   "dist/windows/<resolved stage.checksum_file>"
    #   'release-metadata/manifest.json'
    # — with the checksum entry built from the resolved checksum file name
    # (the release-meta step's output), never a hardcoded default, so a
    # renamed stage.checksum_file changes the release's attachments
    # coherently. The list is the contract under test: exactly four entries
    # in this order.
    param(
        [Parameter(Mandatory)][string]$ChecksumFile
    )
    return @(
        'dist/windows/nextide-cli.exe',
        'dist/windows/nextide-shell.exe',
        ('dist/windows/' + $ChecksumFile),
        'release-metadata/manifest.json'
    )
}

function Assert-ReleaseFileManifest {
    # Simulate the release job's publish gate: the four files the publish step
    # attaches must form a consistent release file manifest. Returns
    # { Ok, Errors } with every failure collected.
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][string]$GoldenManifestPath
    )
    $Errors = [System.Collections.Generic.List[string]]::new()
    $Dist    = Join-Path $Sandbox 'dist\windows'
    $MetaDir = Join-Path $Sandbox 'release-metadata'
    $MetaManifest = Join-Path $MetaDir 'manifest.json'

    # The checksum file name is not hardcoded: it is resolved from the
    # release-metadata manifest's stage.checksum_file — the same manifest the
    # release job downloads and the workflow's resolve step reads — so the
    # attachment list and the verifier below use exactly the name the
    # pipeline generated and validated. When the metadata manifest is missing
    # or unparseable the default name is used so the missing-attachment errors
    # below still name a concrete file.
    $ChecksumName = 'SHA256SUMS.txt'
    $Meta = $null
    if (Test-Path -LiteralPath $MetaManifest -PathType Leaf) {
        try {
            $Meta = Get-Content -Raw -LiteralPath $MetaManifest | ConvertFrom-Json
        }
        catch {
            $Errors.Add('release-metadata manifest.json is not valid JSON')
        }
    }
    if ($null -ne $Meta) {
        $Stage = Get-JsonProperty -Object $Meta -Name 'stage'
        if ($null -ne $Stage) {
            $DeclaredName = [string](Get-JsonProperty -Object $Stage -Name 'checksum_file')
            if (-not [string]::IsNullOrWhiteSpace($DeclaredName)) { $ChecksumName = $DeclaredName }
        }
    }

    # 1. Every attachment exists and is a non-empty regular file. A zero-byte
    #    or absent binary could not pass the job's CLI smoke step either, so
    #    non-empty is the file-level proxy for that gate in the simulation.
    $Attachments = @(
        (Join-Path $Dist 'nextide-cli.exe'),
        (Join-Path $Dist 'nextide-shell.exe'),
        (Join-Path $Dist $ChecksumName),
        (Join-Path $MetaDir 'manifest.json')
    )
    foreach ($File in $Attachments) {
        if (-not (Test-Path -LiteralPath $File -PathType Leaf)) {
            $Errors.Add("release attachment missing: $File")
        }
    }
    foreach ($File in $Attachments) {
        if (Test-Path -LiteralPath $File -PathType Leaf) {
            if ((Get-Item -LiteralPath $File).Length -le 0) {
                $Errors.Add("release attachment is empty: $File")
            }
        }
    }

    # 2. The dist directory must pass the repository's consumer-side verifier
    #    (the exact gate the release job runs before publishing), with the
    #    resolved checksum file name passed as -ChecksumFile — exactly like
    #    the workflow's verify steps.
    $Result = Invoke-ConsumerVerifier -Sandbox $Sandbox -ChecksumName $ChecksumName
    if ($Result.ExitCode -ne 0) {
        $Errors.Add("consumer verifier rejected dist/windows (exit $($Result.ExitCode))")
    }

    # 3. The checksum manifest entry set must equal the manifest artifact list
    #    (the release binaries), so the release attachments match the single
    #    source of truth. The checksum file is staged under exactly the name
    #    stage.checksum_file declares — the attachment list above already
    #    resolved it, so the name and the file are consistent by construction.
    $ChecksumFile = Join-Path $Dist $ChecksumName
    if (($null -ne $Meta) -and (Test-Path -LiteralPath $ChecksumFile -PathType Leaf)) {
        $Expected = @()
        $Artifacts = @(Get-JsonProperty -Object $Meta -Name 'artifacts')
        if ($Artifacts.Count -eq 1 -and $null -eq $Artifacts[0]) { $Artifacts = @() }
        foreach ($Artifact in $Artifacts) {
            if ($null -ne $Artifact) { $Expected += [string]$Artifact.file }
        }
        $Actual = @(Get-ChecksumManifestEntries -Path $ChecksumFile)
        $ExpectedKey = (@($Expected | Sort-Object) -join '|')
        $ActualKey   = (@($Actual   | Sort-Object) -join '|')
        if ($ExpectedKey -ne $ActualKey) {
            $Errors.Add("checksum manifest entries '$($Actual -join ', ')' do not match the manifest artifact files '$($Expected -join ', ')'")
        }
    }

    # 4. The release-metadata manifest must be byte-identical to the golden
    #    manifest the pipeline validated (the job's SHA-256 compare). The
    #    golden is the manifest passed in — the committed manifest for the
    #    normal cases, or a sandbox manifest when a test exercises a
    #    legitimate rename — so the contract is always "the downloaded
    #    metadata is exactly what the resolve step read", never a hardcoded
    #    baseline.
    if (Test-Path -LiteralPath $MetaManifest -PathType Leaf) {
        $DownloadedHash = (Get-FileHash -LiteralPath $MetaManifest -Algorithm SHA256).Hash
        $GoldenHash = (Get-FileHash -LiteralPath $GoldenManifestPath -Algorithm SHA256).Hash
        if ($DownloadedHash -ne $GoldenHash) {
            $Errors.Add('release-metadata manifest.json is not byte-identical to the committed manifest.json')
        }
    }

    return [pscustomobject]@{
        Ok     = ($Errors.Count -eq 0)
        Errors = @($Errors)
    }
}

function New-ReleaseSandbox {
    # A complete, consistent simulated release download: fake binaries under
    # dist\windows, a matching checksum manifest (covers exactly the two
    # binaries with their real hashes), and a byte-identical copy of the
    # committed manifest under release-metadata. Returns the sandbox root.
    $Root = New-Sandbox
    $Dist = Join-Path $Root 'dist\windows'
    $Meta = Join-Path $Root 'release-metadata'
    $null = New-Item -ItemType Directory -Force -Path $Dist
    $null = New-Item -ItemType Directory -Force -Path $Meta
    $Cli   = Join-Path $Dist 'nextide-cli.exe'
    $Shell = Join-Path $Dist 'nextide-shell.exe'
    New-FakeFile -Path $Cli   -Content 'fake nextide-cli.exe payload'
    New-FakeFile -Path $Shell -Content 'fake nextide-shell.exe payload'
    Write-ChecksumManifest -Path (Join-Path $Dist $script:GoldenChecksumFile) -Entries @{
        'nextide-cli.exe'   = (Get-Sha256Hex $Cli)
        'nextide-shell.exe' = (Get-Sha256Hex $Shell)
    }
    Copy-Item -LiteralPath $ManifestSource -Destination (Join-Path $Meta 'manifest.json') -Force
    return $Root
}

Write-Host '=================================================='
Write-Host 'Windows release job simulation tests (sandboxed)'
Write-Host "  manifest source        : $ManifestSource"
Write-Host "  consumer verifier      : $VerifyScript"
Write-Host "  sandbox root           : $SandboxRoot"
Write-Host '  no network access; no GitHub Release is ever created; the real repository is never modified'
Write-Host '=================================================='

# ---------------------------------------------------------------------------
# Part 1 — tag/version rejection rules.
# ---------------------------------------------------------------------------

# 1. The committed manifest resolves with the matching v<version> tag.
Invoke-Test -Name 'release-meta: committed manifest + matching v<version> tag resolves' -Body {
    $Root = New-Sandbox
    try {
        Copy-GoldenManifest $Root
        $ManifestPath = Join-Path $Root 'packaging\windows\manifest.json'
        $Result = Resolve-ReleaseMeta -ManifestPath $ManifestPath -Tag ('v' + $script:GoldenVersion)
        Assert $Result.Ok "expected resolution to succeed, got error: $($Result.Error)"
        Assert ($Result.Version -eq $script:GoldenVersion) "expected version '$($script:GoldenVersion)', got '$($Result.Version)'"
        Assert ($Result.Channel -eq $script:GoldenChannel) "expected channel '$($script:GoldenChannel)', got '$($Result.Channel)'"
        Assert ($Result.ChecksumFile -eq $script:GoldenChecksumFile) "expected checksum file '$($script:GoldenChecksumFile)', got '$($Result.ChecksumFile)'"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 2. 'beta' is a valid channel (the same rule build.ps1 enforces).
Invoke-Test -Name 'release-meta: beta channel resolves (valid channel)' -Body {
    $Root = New-Sandbox
    try {
        Write-SandboxManifest -Root $Root -Json ('{"release":{"release_channel":"beta"},"product":{"version":"' + $script:GoldenVersion + '"},"stage":{"checksum_file":"' + $script:GoldenChecksumFile + '"}}')
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
        Assert $Result.Ok "expected resolution to succeed, got error: $($Result.Error)"
        Assert ($Result.Channel -eq 'beta') "expected channel 'beta', got '$($Result.Channel)'"
        Assert ($Result.ChecksumFile -eq $script:GoldenChecksumFile) "expected checksum file '$($script:GoldenChecksumFile)', got '$($Result.ChecksumFile)'"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 3. A non-v tag is rejected.
Invoke-Test -Name 'release-meta: non-v tag is rejected' -Body {
    $Root = New-Sandbox
    try {
        Copy-GoldenManifest $Root
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag 'main'
        Assert (-not $Result.Ok) 'expected the non-v tag to be rejected'
        Assert ($Result.Error -match 'not a v-prefixed semver') "expected semver rejection, got: $($Result.Error)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 4. A malformed semver tag is rejected.
Invoke-Test -Name 'release-meta: malformed semver tag is rejected' -Body {
    $Root = New-Sandbox
    try {
        Copy-GoldenManifest $Root
        $ManifestPath = Join-Path $Root 'packaging\windows\manifest.json'
        # Tags that fail the '^v<major>.<minor>.<patch>$' shape outright.
        foreach ($BadTag in @('v0.1', 'v0.1.0.1', 'v0.1.0 ')) {
            $Result = Resolve-ReleaseMeta -ManifestPath $ManifestPath -Tag $BadTag
            Assert (-not $Result.Ok) "expected tag '$BadTag' to be rejected"
            Assert ($Result.Error -match 'not a v-prefixed semver') "expected semver rejection for '$BadTag', got: $($Result.Error)"
        }
        # 'v01.0.0' passes the shape but its version differs from the manifest
        # (leading zero), so it is still rejected — by the version link.
        $Result = Resolve-ReleaseMeta -ManifestPath $ManifestPath -Tag 'v01.0.0'
        Assert (-not $Result.Ok) 'expected tag v01.0.0 to be rejected'
        Assert ($Result.Error -match 'does not match manifest product.version') "expected version-mismatch rejection for 'v01.0.0', got: $($Result.Error)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 5. A pre-release tag (e.g. v0.1.0-beta) is rejected: the release job only
#    accepts exactly v<major>.<minor>.<patch>.
Invoke-Test -Name 'release-meta: pre-release tag is rejected' -Body {
    $Root = New-Sandbox
    try {
        Copy-GoldenManifest $Root
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag 'v0.1.0-beta'
        Assert (-not $Result.Ok) 'expected the pre-release tag to be rejected'
        Assert ($Result.Error -match 'not a v-prefixed semver') "expected semver rejection, got: $($Result.Error)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 6. A tag that disagrees with the manifest product.version is rejected (the
#    final consistency link: tag must be v<product.version>).
Invoke-Test -Name 'release-meta: tag disagreeing with manifest version is rejected' -Body {
    $Root = New-Sandbox
    try {
        Copy-GoldenManifest $Root
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag 'v9.9.9'
        Assert (-not $Result.Ok) 'expected the mismatched tag to be rejected'
        Assert ($Result.Error -match 'does not match manifest product.version') "expected version-mismatch rejection, got: $($Result.Error)"
        Assert ($Result.Error -match 'v9\.9\.9') 'expected the offending tag in the error message'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 7. A channel other than stable/beta is rejected.
Invoke-Test -Name 'release-meta: invalid release channel is rejected' -Body {
    $Root = New-Sandbox
    try {
        Write-SandboxManifest -Root $Root -Json ('{"release":{"release_channel":"alpha"},"product":{"version":"' + $script:GoldenVersion + '"}}')
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
        Assert (-not $Result.Ok) 'expected the invalid channel to be rejected'
        Assert ($Result.Error -match "exactly 'stable' or 'beta'") "expected channel rejection, got: $($Result.Error)"
        Assert ($Result.Error -match 'alpha') 'expected the offending channel in the error message'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 8. A blank product.version is rejected.
Invoke-Test -Name 'release-meta: blank product.version is rejected' -Body {
    $Root = New-Sandbox
    try {
        Write-SandboxManifest -Root $Root -Json '{"release":{"release_channel":"stable"},"product":{"version":"   "}}'
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
        Assert (-not $Result.Ok) 'expected the blank version to be rejected'
        Assert ($Result.Error -match 'product\.version is missing or blank') "expected version rejection, got: $($Result.Error)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 9. A missing release block is rejected (the channel rule fires, as in the
#    workflow where a missing release block yields a null channel).
Invoke-Test -Name 'release-meta: missing release block is rejected' -Body {
    $Root = New-Sandbox
    try {
        Write-SandboxManifest -Root $Root -Json ('{"product":{"version":"' + $script:GoldenVersion + '"}}')
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
        Assert (-not $Result.Ok) 'expected the missing release block to be rejected'
        Assert ($Result.Error -match "exactly 'stable' or 'beta'") "expected channel rejection, got: $($Result.Error)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 10. An invalid-JSON manifest is rejected.
Invoke-Test -Name 'release-meta: invalid manifest JSON is rejected' -Body {
    $Root = New-Sandbox
    try {
        Write-SandboxManifest -Root $Root -Json '{"release": broken'
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
        Assert (-not $Result.Ok) 'expected the invalid JSON manifest to be rejected'
        Assert ($Result.Error -match 'not valid JSON') "expected JSON rejection, got: $($Result.Error)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 11. A missing manifest is rejected.
Invoke-Test -Name 'release-meta: missing manifest is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
        Assert (-not $Result.Ok) 'expected the missing manifest to be rejected'
        Assert ($Result.Error -match 'Missing manifest') "expected missing-manifest rejection, got: $($Result.Error)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 12. A blank stage.checksum_file is rejected: the checksum manifest name is
#     not hardcoded anywhere in the chain, so the release job refuses to
#     proceed without a declared name (the packaging job already proved the
#     full leaf-name validation; this is the "I need the value" assertion,
#     mirroring the workflow's resolve step).
Invoke-Test -Name 'release-meta: blank stage.checksum_file is rejected' -Body {
    $Root = New-Sandbox
    try {
        foreach ($StageJson in @('"stage":{}', '"stage":{"checksum_file":"   "}')) {
            Write-SandboxManifest -Root $Root -Json ('{"release":{"release_channel":"stable"},"product":{"version":"' + $script:GoldenVersion + '"},' + $StageJson + '}')
            $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
            Assert (-not $Result.Ok) "expected the blank checksum file to be rejected for $StageJson"
            Assert ($Result.Error -match 'stage\.checksum_file is missing or blank') "expected a checksum-file rejection, got: $($Result.Error)"
        }
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# Part 2 — release file manifest.
# ---------------------------------------------------------------------------

# 13. A complete, consistent release file manifest passes every gate,
#     including the real consumer verifier against the sandbox dist.
Invoke-Test -Name 'release checklist: complete consistent release passes' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert $Result.Ok "expected the release checklist to pass: $($Result.Errors -join '; ')"
        # The consumer verifier itself must also exit 0 on the sandbox dist
        # (the release job's gate), proving the whole chain agrees.
        $Verify = Invoke-ConsumerVerifier -Sandbox $Root -ChecksumName $script:GoldenChecksumFile
        Assert ($Verify.ExitCode -eq 0) "expected consumer verifier exit 0, got $($Verify.ExitCode)`n$($Verify.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 14. Missing nextide-cli.exe fails.
Invoke-Test -Name 'release checklist: missing nextide-cli.exe fails' -Body {
    $Root = New-ReleaseSandbox
    try {
        Remove-Item -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe') -Force
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the missing CLI binary to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'release attachment missing') "expected a missing-attachment error: $($Result.Errors -join '; ')"
        Assert ($Result.Errors -join ';' -match 'nextide-cli\.exe') 'expected the missing file name in the errors'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 15. Missing nextide-shell.exe fails.
Invoke-Test -Name 'release checklist: missing nextide-shell.exe fails' -Body {
    $Root = New-ReleaseSandbox
    try {
        Remove-Item -LiteralPath (Join-Path $Root 'dist\windows\nextide-shell.exe') -Force
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the missing shell binary to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'release attachment missing') "expected a missing-attachment error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 16. Missing checksum manifest (the stage.checksum_file name) fails.
Invoke-Test -Name 'release checklist: missing checksum manifest fails' -Body {
    $Root = New-ReleaseSandbox
    try {
        Remove-Item -LiteralPath (Join-Path $Root ('dist\windows\' + $script:GoldenChecksumFile)) -Force
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the missing checksum manifest to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'release attachment missing') "expected a missing-attachment error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 17. Missing release-metadata/manifest.json fails.
Invoke-Test -Name 'release checklist: missing release-metadata/manifest.json fails' -Body {
    $Root = New-ReleaseSandbox
    try {
        Remove-Item -LiteralPath (Join-Path $Root 'release-metadata\manifest.json') -Force
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the missing metadata manifest to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'release attachment missing') "expected a missing-attachment error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 18. A tampered binary is caught by the consumer verifier (hash mismatch).
Invoke-Test -Name 'release checklist: tampered binary fails consumer verification' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Cli = Join-Path $Root 'dist\windows\nextide-cli.exe'
        Set-Content -LiteralPath $Cli -Value 'tampered payload' -Encoding ASCII -NoNewline
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the tampered binary to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'consumer verifier rejected') "expected a consumer-verifier error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 19. An extra unlisted file in dist fails the consumer verifier's exact-match
#     contract.
Invoke-Test -Name 'release checklist: extra unlisted file in dist fails' -Body {
    $Root = New-ReleaseSandbox
    try {
        New-FakeFile -Path (Join-Path $Root 'dist\windows\nextide.pdb') -Content 'stray debug symbols'
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the extra file to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'consumer verifier rejected') "expected a consumer-verifier error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 20. A checksum manifest whose entry set does not match the manifest artifact
#     list fails — even though every listed file exists and hashes match (the
#     release would attach checksums for files the manifest never declared).
Invoke-Test -Name 'release checklist: checksum entries not matching the artifact list fail' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $Extra = Join-Path $Dist 'extra.bin'
        New-FakeFile -Path $Extra -Content 'undisclosed binary'
        Write-ChecksumManifest -Path (Join-Path $Dist $script:GoldenChecksumFile) -Entries @{
            'nextide-cli.exe'   = (Get-Sha256Hex (Join-Path $Dist 'nextide-cli.exe'))
            'nextide-shell.exe' = (Get-Sha256Hex (Join-Path $Dist 'nextide-shell.exe'))
            'extra.bin'         = (Get-Sha256Hex $Extra)
        }
        # The consumer verifier alone would accept this directory (every
        # listed file exists and matches); the artifact-list cross-check is
        # what catches it.
        $Verify = Invoke-ConsumerVerifier -Sandbox $Root -ChecksumName $script:GoldenChecksumFile
        Assert ($Verify.ExitCode -eq 0) "expected consumer verifier exit 0 here, got $($Verify.ExitCode)`n$($Verify.Output)"
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the extra checksum entry to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'do not match the manifest artifact files') "expected an entry-set error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 21. A release-metadata manifest that is not byte-identical to the committed
#     manifest fails (the job's SHA-256 compare before publishing).
Invoke-Test -Name 'release checklist: metadata manifest not byte-identical fails' -Body {
    $Root = New-ReleaseSandbox
    try {
        $MetaManifest = Join-Path $Root 'release-metadata\manifest.json'
        $Json = Get-Content -Raw -LiteralPath $MetaManifest | ConvertFrom-Json
        $Json.release.publisher = 'tampered'
        Set-Content -LiteralPath $MetaManifest -Value ($Json | ConvertTo-Json -Depth 10) -Encoding UTF8
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the tampered metadata manifest to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'byte-identical') "expected a byte-identity error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 22. A zero-byte CLI binary fails the non-empty (smoke-gate simulation)
#     assertion — the job would attach a binary that could not run.
Invoke-Test -Name 'release checklist: zero-byte CLI binary fails (smoke-gate simulation)' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $Cli = Join-Path $Dist 'nextide-cli.exe'
        $null = New-Item -ItemType File -Path $Cli -Force
        Write-ChecksumManifest -Path (Join-Path $Dist $script:GoldenChecksumFile) -Entries @{
            'nextide-cli.exe'   = (Get-Sha256Hex $Cli)
            'nextide-shell.exe' = (Get-Sha256Hex (Join-Path $Dist 'nextide-shell.exe'))
        }
        # The checksum contract still holds for a zero-byte file, so only the
        # non-empty gate rejects it — exactly the gap the smoke step closes.
        $Verify = Invoke-ConsumerVerifier -Sandbox $Root -ChecksumName $script:GoldenChecksumFile
        Assert ($Verify.ExitCode -eq 0) "expected consumer verifier exit 0 here, got $($Verify.ExitCode)`n$($Verify.Output)"
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the zero-byte binary to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'release attachment is empty') "expected an empty-attachment error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 23. A renamed stage.checksum_file is honored by the checklist: the
#     attachment list, the consumer verifier and the entry-set cross-check all
#     resolve the declared name (the pipeline's single source of truth), so
#     the only thing that rejects a modified metadata manifest is the
#     byte-identity contract — exactly like the real release job, which
#     consumes the same committed manifest for the checksum file name and the
#     byte-identity check.
Invoke-Test -Name 'release checklist: renamed stage.checksum_file is honored (byte-identity still enforced)' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $Meta = Join-Path $Root 'release-metadata'
        $Renamed = 'CHECKSUMS.txt'
        # Re-stage the checksum manifest under the renamed file and re-write
        # the metadata manifest declaring it (no longer byte-identical to the
        # committed one — that is the only failure this checklist may report).
        Move-Item -LiteralPath (Join-Path $Dist $script:GoldenChecksumFile) -Destination (Join-Path $Dist $Renamed) -Force
        $Json = Get-Content -Raw -LiteralPath (Join-Path $Meta 'manifest.json') | ConvertFrom-Json
        $Json.stage.checksum_file = $Renamed
        Set-Content -LiteralPath (Join-Path $Meta 'manifest.json') -Value ($Json | ConvertTo-Json -Depth 10) -Encoding UTF8
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the checklist to reject the modified metadata manifest'
        $Joined = $Result.Errors -join ';'
        Assert ($Joined -match 'byte-identical') "expected the byte-identity error, got: $Joined"
        Assert ($Joined -notmatch 'release attachment missing') 'renamed checksum file must be found under the declared name'
        Assert ($Joined -notmatch 'consumer verifier rejected') 'renamed checksum file must pass the consumer verifier under the declared name'
        Assert ($Joined -notmatch 'do not match the manifest artifact files') 'renamed checksum file must pass the artifact-list cross-check'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 25. The publish step's attachment list is built from the resolved
#     stage.checksum_file — never a hardcoded default. The list mirrors the
#     exact four file paths the workflow hands to the GitHub CLI, with the
#     checksum entry named by the manifest; a renamed declaration changes the
#     list accordingly, and the default name never appears.
Invoke-Test -Name 'release publish: attachment list uses the resolved checksum file name (never hardcoded)' -Body {
    $Root = New-Sandbox
    try {
        Copy-GoldenManifest $Root
        $ManifestPath = Join-Path $Root 'packaging\windows\manifest.json'
        $Result = Resolve-ReleaseMeta -ManifestPath $ManifestPath -Tag ('v' + $script:GoldenVersion)
        Assert $Result.Ok "expected resolution to succeed, got error: $($Result.Error)"

        $Attachments = Get-ReleaseAttachmentList -ChecksumFile $Result.ChecksumFile
        Assert ($Attachments.Count -eq 4) "expected exactly 4 attachments, got $($Attachments.Count)"
        Assert ($Attachments[0] -eq 'dist/windows/nextide-cli.exe') "expected the CLI first, got '$($Attachments[0])'"
        Assert ($Attachments[1] -eq 'dist/windows/nextide-shell.exe') "expected the shell second, got '$($Attachments[1])'"
        Assert ($Attachments[2] -eq ('dist/windows/' + $script:GoldenChecksumFile)) "expected the declared checksum file third, got '$($Attachments[2])'"
        Assert ($Attachments[3] -eq 'release-metadata/manifest.json') "expected the metadata manifest last, got '$($Attachments[3])'"

        # A renamed stage.checksum_file in the manifest is reflected in the
        # attachment list; the hardcoded default never appears.
        $Renamed = 'CHECKSUMS-v2.txt'
        Write-SandboxManifest -Root $Root -Json ('{"release":{"release_channel":"stable"},"product":{"version":"' + $script:GoldenVersion + '"},"stage":{"checksum_file":"' + $Renamed + '"}}')
        $RenamedResult = Resolve-ReleaseMeta -ManifestPath $ManifestPath -Tag ('v' + $script:GoldenVersion)
        Assert $RenamedResult.Ok "expected the renamed resolution to succeed, got error: $($RenamedResult.Error)"
        Assert ($RenamedResult.ChecksumFile -eq $Renamed) "expected '$Renamed', got '$($RenamedResult.ChecksumFile)'"
        $RenamedList = Get-ReleaseAttachmentList -ChecksumFile $RenamedResult.ChecksumFile
        Assert ($RenamedList[2] -eq ('dist/windows/' + $Renamed)) "expected the renamed checksum file in the list, got '$($RenamedList[2])'"
        Assert ($RenamedList -notcontains ('dist/windows/' + $script:GoldenChecksumFile)) 'the default checksum name must never be hardcoded into the publish list'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 26. A renamed stage.checksum_file in the committed-style manifest flows
#     through the whole simulated publish chain coherently: the resolve step
#     returns the declared name, the dist is staged under it, the checklist
#     (with the sandbox manifest as its byte-identity golden) passes in full,
#     and the publish attachment list names exactly that file — never the
#     hardcoded default. Test 23 shows the tampered-metadata failure path;
#     this test shows the legitimate-rename happy path the real job supports
#     (the rename lives in the committed manifest, so the metadata manifest
#     stays byte-identical and every gate passes).
Invoke-Test -Name 'release publish: renamed stage.checksum_file flows through resolve -> checklist -> attachment list' -Body {
    $Root = New-Sandbox
    try {
        $Renamed = 'CHECKSUMS.txt'
        # The sandbox "committed" manifest, re-declaring the checksum file
        # name; the metadata copy below is byte-identical to it.
        $SandboxManifestPath = Join-Path $Root 'packaging\windows\manifest.json'
        Copy-GoldenManifest $Root
        $Json = Get-Content -Raw -LiteralPath $SandboxManifestPath | ConvertFrom-Json
        $Json.stage.checksum_file = $Renamed
        Set-Content -LiteralPath $SandboxManifestPath -Value ($Json | ConvertTo-Json -Depth 10) -Encoding UTF8

        # Resolve step: the declared (renamed) name is returned.
        $Result = Resolve-ReleaseMeta -ManifestPath $SandboxManifestPath -Tag ('v' + $script:GoldenVersion)
        Assert $Result.Ok "expected resolution to succeed, got error: $($Result.Error)"
        Assert ($Result.ChecksumFile -eq $Renamed) "expected checksum file '$Renamed', got '$($Result.ChecksumFile)'"

        # Stage a dist whose checksum manifest lives under the renamed name.
        $Dist = Join-Path $Root 'dist\windows'
        $Meta = Join-Path $Root 'release-metadata'
        $null = New-Item -ItemType Directory -Force -Path $Dist
        $null = New-Item -ItemType Directory -Force -Path $Meta
        $Cli   = Join-Path $Dist 'nextide-cli.exe'
        $Shell = Join-Path $Dist 'nextide-shell.exe'
        New-FakeFile -Path $Cli   -Content 'fake nextide-cli.exe payload'
        New-FakeFile -Path $Shell -Content 'fake nextide-shell.exe payload'
        Write-ChecksumManifest -Path (Join-Path $Dist $Renamed) -Entries @{
            'nextide-cli.exe'   = (Get-Sha256Hex $Cli)
            'nextide-shell.exe' = (Get-Sha256Hex $Shell)
        }
        # The release-metadata manifest is byte-identical to the sandbox
        # (committed) manifest — exactly the release job's contract.
        Copy-Item -LiteralPath $SandboxManifestPath -Destination (Join-Path $Meta 'manifest.json') -Force

        # The full checklist passes when the sandbox manifest is the golden.
        $Checklist = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $SandboxManifestPath
        Assert $Checklist.Ok "expected the checklist to pass: $($Checklist.Errors -join '; ')"

        # The publish attachment list names exactly the renamed file.
        $Attachments = Get-ReleaseAttachmentList -ChecksumFile $Result.ChecksumFile
        $Expected = @(
            'dist/windows/nextide-cli.exe',
            'dist/windows/nextide-shell.exe',
            ('dist/windows/' + $Renamed),
            'release-metadata/manifest.json'
        )
        Assert (($Attachments -join '|') -eq ($Expected -join '|')) "expected attachments '$($Expected -join ', ')', got '$($Attachments -join ', ')'"
        Assert ($Attachments -notcontains ('dist/windows/' + $script:GoldenChecksumFile)) 'publish must not fall back to the hardcoded default checksum name'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# Part 3 — self-audit.
# ---------------------------------------------------------------------------

# 24. The script's own source must not contain any network or release-creation
#     command invocation: this suite can never touch the network and can never
#     create a GitHub Release, by construction. The only child process it ever
#     launches is the repository's consumer-side verifier against sandbox
#     directories.
Invoke-Test -Name 'self-audit: no network or release-creation commands in the script source' -Body {
    $Source = Get-Content -Raw -LiteralPath $PSCommandPath
    # Each pattern is assembled from fragments so the pattern text itself is
    # not present verbatim in this file (otherwise the audit would always
    # match its own pattern literals).
    $Forbidden = @(
        ('invoke-' + 'webrequest'),
        ('invoke-' + 'restmethod'),
        ('curl' + '(\.exe)?\s+'),
        ('wget' + '\s+'),
        ('start-' + 'bitstransfer'),
        ('new-object\s+system\.net\.' + 'webclient'),
        ('system\.net\.http\.' + 'httpclient'),
        ('gh' + '\s+release'),
        ('git' + '\s+(push|clone|fetch|pull)\b')
    )
    foreach ($Pattern in $Forbidden) {
        Assert (-not ($Source -match $Pattern)) "source contains a forbidden invocation matching '$Pattern'"
    }
}

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------
if (-not $KeepSandbox) {
    foreach ($S in $script:Sandboxes) {
        $Leaf = Split-Path -Leaf $S
        if ($Leaf -like 'nextide-test-release-sim-*' -and (Test-Path -LiteralPath $S)) {
            Remove-Item -LiteralPath $S -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

Write-Host ''
Write-Host '=================================================='
Write-Host ("PASSED: {0}   FAILED: {1}" -f $script:Passed, $script:Failed)
if ($script:Failures.Count -gt 0) {
    Write-Host 'FAILURES:'
    foreach ($F in $script:Failures) { Write-Host "  - $F" }
}
if ($KeepSandbox) {
    Write-Host "Sandboxes kept under $SandboxRoot for inspection."
}
else {
    Write-Host "No GitHub Release was created, no network was accessed, and the real repository was not modified; only sandboxes under $SandboxRoot were written and removed."
}
if ($script:Failed -gt 0) { exit 1 }
exit 0
