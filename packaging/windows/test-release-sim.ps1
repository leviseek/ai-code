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
    metadata from manifest (channel + version vs tag)" step, mirrored exactly:
    same checks, same order, same messages):
      - channel must be exactly 'stable' or 'beta'
      - product.version must be non-blank
      - the pushed tag must match '^v[0-9]+\.[0-9]+\.[0-9]+$'
      - the tag minus its leading 'v' must equal product.version
    Every violation is rejected; the committed manifest resolves with the
    matching v<version> tag.

    Part 2 — release file manifest (the four files the publish step would
    attach — dist/windows/nextide-cli.exe, dist/windows/nextide-shell.exe,
    dist/windows/SHA256SUMS.txt and release-metadata/manifest.json):
      - every attachment exists and is a non-empty regular file (a zero-byte
        or absent binary could not pass the job's CLI smoke step either)
      - dist/windows passes the repository's consumer-side verifier
        (verify-checksums.ps1 -DistDir dist\windows), the exact gate the
        release job runs before publishing
      - the checksum manifest entry set equals the manifest artifact list
        (the two release binaries) and the staged checksum file name equals
        stage.checksum_file
      - release-metadata/manifest.json is byte-identical (SHA-256) to the
        committed manifest.json the pipeline validated

    Part 3 — self-audit: the script's own source contains no network or
    release-creation command invocations (it can never touch the network and
    can never create a Release).

    Covered scenarios (22 tests):
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
     12.  complete, consistent release file manifest passes
     13.  missing nextide-cli.exe fails
     14.  missing nextide-shell.exe fails
     15.  missing SHA256SUMS.txt fails
     16.  missing release-metadata/manifest.json fails
     17.  tampered binary fails consumer verification
     18.  extra unlisted file in dist fails consumer verification
     19.  checksum entries not matching the manifest artifact list fail
     20.  release-metadata manifest not byte-identical to the committed one fails
     21.  zero-byte CLI binary fails (smoke-gate simulation)
     22.  self-audit: no network or release-creation commands in the source

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
$script:GoldenHash = (Get-FileHash -LiteralPath $ManifestSource -Algorithm SHA256).Hash

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
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; Error = "Missing manifest: $ManifestPath" }
    }
    try {
        $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    }
    catch {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; Error = "Manifest is not valid JSON: $($_.Exception.Message)" }
    }
    $Release = Get-JsonProperty -Object $Manifest -Name 'release'
    $Channel = ''
    if ($null -ne $Release) { $Channel = [string](Get-JsonProperty -Object $Release -Name 'release_channel') }
    if ($Channel -ne 'stable' -and $Channel -ne 'beta') {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; Error = "Manifest release.release_channel must be exactly 'stable' or 'beta', got: '$Channel'" }
    }
    $Product = Get-JsonProperty -Object $Manifest -Name 'product'
    $Version = ''
    if ($null -ne $Product) { $Version = [string](Get-JsonProperty -Object $Product -Name 'version') }
    if ([string]::IsNullOrWhiteSpace($Version)) {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; Error = 'Manifest product.version is missing or blank' }
    }
    if ($Tag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; Error = "Release tag '$Tag' is not a v-prefixed semver tag (expected e.g. v0.1.0)" }
    }
    if ($Tag.Substring(1) -ne $Version) {
        return [pscustomobject]@{ Ok = $false; Channel = ''; Version = ''; Error = "Release tag '$Tag' does not match manifest product.version '$Version' (tag must be v<product.version>)" }
    }
    return [pscustomobject]@{ Ok = $true; Channel = $Channel; Version = $Version; Error = '' }
}

# ---------------------------------------------------------------------------
# Part 2 — release file manifest.
# ---------------------------------------------------------------------------
function Invoke-ConsumerVerifier {
    # Run the repository's consumer-side verifier against a sandbox dist
    # directory — the exact gate the release job runs before publishing.
    # Never turn a non-zero native exit code into a terminating error here:
    # the exit code is the value under test. Note: do NOT pre-assign
    # $LASTEXITCODE in this function — that would shadow the automatic
    # variable and the engine would no longer update it.
    param([Parameter(Mandatory)][string]$Sandbox)
    $Dist = Join-Path $Sandbox 'dist\windows'
    $ChildArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $VerifyScript, '-DistDir', $Dist)
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

    # 1. Every attachment exists and is a non-empty regular file. A zero-byte
    #    or absent binary could not pass the job's CLI smoke step either, so
    #    non-empty is the file-level proxy for that gate in the simulation.
    $Attachments = @(
        (Join-Path $Dist 'nextide-cli.exe'),
        (Join-Path $Dist 'nextide-shell.exe'),
        (Join-Path $Dist 'SHA256SUMS.txt'),
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
    #    (the exact gate the release job runs before publishing).
    $Result = Invoke-ConsumerVerifier -Sandbox $Sandbox
    if ($Result.ExitCode -ne 0) {
        $Errors.Add("consumer verifier rejected dist/windows (exit $($Result.ExitCode))")
    }

    # 3. The checksum manifest entry set must equal the manifest artifact list
    #    (the release binaries), so the release attachments match the single
    #    source of truth — and the staged checksum file name must equal
    #    stage.checksum_file (the publish step attaches SHA256SUMS.txt).
    $MetaManifest = Join-Path $MetaDir 'manifest.json'
    $ChecksumFile = Join-Path $Dist 'SHA256SUMS.txt'
    if ((Test-Path -LiteralPath $MetaManifest -PathType Leaf) -and (Test-Path -LiteralPath $ChecksumFile -PathType Leaf)) {
        try {
            $Meta = Get-Content -Raw -LiteralPath $MetaManifest | ConvertFrom-Json
        }
        catch {
            $Errors.Add('release-metadata manifest.json is not valid JSON')
            $Meta = $null
        }
        if ($null -ne $Meta) {
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
            $Stage = Get-JsonProperty -Object $Meta -Name 'stage'
            $Declared = ''
            if ($null -ne $Stage) { $Declared = [string](Get-JsonProperty -Object $Stage -Name 'checksum_file') }
            if ($Declared -ne 'SHA256SUMS.txt') {
                $Errors.Add("stage.checksum_file '$Declared' does not match the release attachment 'SHA256SUMS.txt'")
            }
        }
    }

    # 4. The release-metadata manifest must be byte-identical to the committed
    #    manifest the pipeline validated (the job's SHA-256 compare).
    if (Test-Path -LiteralPath $MetaManifest -PathType Leaf) {
        $DownloadedHash = (Get-FileHash -LiteralPath $MetaManifest -Algorithm SHA256).Hash
        if ($DownloadedHash -ne $script:GoldenHash) {
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
    Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
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
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 2. 'beta' is a valid channel (the same rule build.ps1 enforces).
Invoke-Test -Name 'release-meta: beta channel resolves (valid channel)' -Body {
    $Root = New-Sandbox
    try {
        Write-SandboxManifest -Root $Root -Json ('{"release":{"release_channel":"beta"},"product":{"version":"' + $script:GoldenVersion + '"}}')
        $Result = Resolve-ReleaseMeta -ManifestPath (Join-Path $Root 'packaging\windows\manifest.json') -Tag ('v' + $script:GoldenVersion)
        Assert $Result.Ok "expected resolution to succeed, got error: $($Result.Error)"
        Assert ($Result.Channel -eq 'beta') "expected channel 'beta', got '$($Result.Channel)'"
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

# ---------------------------------------------------------------------------
# Part 2 — release file manifest.
# ---------------------------------------------------------------------------

# 12. A complete, consistent release file manifest passes every gate,
#     including the real consumer verifier against the sandbox dist.
Invoke-Test -Name 'release checklist: complete consistent release passes' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert $Result.Ok "expected the release checklist to pass: $($Result.Errors -join '; ')"
        # The consumer verifier itself must also exit 0 on the sandbox dist
        # (the release job's gate), proving the whole chain agrees.
        $Verify = Invoke-ConsumerVerifier -Sandbox $Root
        Assert ($Verify.ExitCode -eq 0) "expected consumer verifier exit 0, got $($Verify.ExitCode)`n$($Verify.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 13. Missing nextide-cli.exe fails.
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

# 14. Missing nextide-shell.exe fails.
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

# 15. Missing SHA256SUMS.txt fails.
Invoke-Test -Name 'release checklist: missing SHA256SUMS.txt fails' -Body {
    $Root = New-ReleaseSandbox
    try {
        Remove-Item -LiteralPath (Join-Path $Root 'dist\windows\SHA256SUMS.txt') -Force
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the missing checksum manifest to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'release attachment missing') "expected a missing-attachment error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 16. Missing release-metadata/manifest.json fails.
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

# 17. A tampered binary is caught by the consumer verifier (hash mismatch).
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

# 18. An extra unlisted file in dist fails the consumer verifier's exact-match
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

# 19. A checksum manifest whose entry set does not match the manifest artifact
#     list fails — even though every listed file exists and hashes match (the
#     release would attach checksums for files the manifest never declared).
Invoke-Test -Name 'release checklist: checksum entries not matching the artifact list fail' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $Extra = Join-Path $Dist 'extra.bin'
        New-FakeFile -Path $Extra -Content 'undisclosed binary'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'nextide-cli.exe'   = (Get-Sha256Hex (Join-Path $Dist 'nextide-cli.exe'))
            'nextide-shell.exe' = (Get-Sha256Hex (Join-Path $Dist 'nextide-shell.exe'))
            'extra.bin'         = (Get-Sha256Hex $Extra)
        }
        # The consumer verifier alone would accept this directory (every
        # listed file exists and matches); the artifact-list cross-check is
        # what catches it.
        $Verify = Invoke-ConsumerVerifier -Sandbox $Root
        Assert ($Verify.ExitCode -eq 0) "expected consumer verifier exit 0 here, got $($Verify.ExitCode)`n$($Verify.Output)"
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the extra checksum entry to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'do not match the manifest artifact files') "expected an entry-set error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# 20. A release-metadata manifest that is not byte-identical to the committed
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

# 21. A zero-byte CLI binary fails the non-empty (smoke-gate simulation)
#     assertion — the job would attach a binary that could not run.
Invoke-Test -Name 'release checklist: zero-byte CLI binary fails (smoke-gate simulation)' -Body {
    $Root = New-ReleaseSandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $Cli = Join-Path $Dist 'nextide-cli.exe'
        $null = New-Item -ItemType File -Path $Cli -Force
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'nextide-cli.exe'   = (Get-Sha256Hex $Cli)
            'nextide-shell.exe' = (Get-Sha256Hex (Join-Path $Dist 'nextide-shell.exe'))
        }
        # The checksum contract still holds for a zero-byte file, so only the
        # non-empty gate rejects it — exactly the gap the smoke step closes.
        $Verify = Invoke-ConsumerVerifier -Sandbox $Root
        Assert ($Verify.ExitCode -eq 0) "expected consumer verifier exit 0 here, got $($Verify.ExitCode)`n$($Verify.Output)"
        $Result = Assert-ReleaseFileManifest -Sandbox $Root -GoldenManifestPath $ManifestSource
        Assert (-not $Result.Ok) 'expected the zero-byte binary to fail the checklist'
        Assert ($Result.Errors -join ';' -match 'release attachment is empty') "expected an empty-attachment error: $($Result.Errors -join '; ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# Part 3 — self-audit.
# ---------------------------------------------------------------------------

# 22. The script's own source must not contain any network or release-creation
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
