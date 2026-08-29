#Requires -Version 5.1
<#
.SYNOPSIS
    Self-contained validation tests for
    packaging\windows\verify-release-download.ps1.

.DESCRIPTION
    Tests the end-user one-command release download verifier without any
    external test framework (plain PowerShell assertions only), without any
    network access and without touching the real repository. Every test builds
    a throwaway sandbox directory under the sandbox root (default: $env:TEMP)
    that mirrors a GitHub Release: a `release/` tree holding the two fake
    binaries, a manifest.json and the checksum manifest (the same layout as
    https://github.com/<repo>/releases/download/<tag>/), and a `raw/` tree
    holding a copy of the repository's own verify-checksums.ps1 (the layout
    of https://raw.githubusercontent.com/<repo>/<tag>/packaging/windows/).
    The script under test is then run in a child process of the same
    PowerShell edition with -BaseReleaseUrl / -VerifyScriptUrl pointing at
    those file:// trees, so every download step is exercised for real (the
    exit-code contract 0 = success / non-zero = failure is tested for real)
    while the suite itself never touches the network. Nothing in the
    repository is written, renamed or removed; the only thing this script
    ever deletes is its own sandboxes under $SandboxRoot.

    The downloaded consumer verifier in each test is the real
    verify-checksums.ps1 from this repository (copied into the raw tree), so
    the exact script dist consumers use is exercised on every run. The smoke
    gate's success side uses a runnable stub (a copy of the system tree.com,
    which ignores its argument and exits 0); its failure side uses
    a non-runnable text stub.

    Covered scenarios:
      1. happy path (download + checksum verify + CLI smoke)   -> exit 0
      2. happy path with -SkipSmoke                            -> exit 0
      3. invalid tags (main / v0.1 / v0.1.0.1) rejected         -> fails, nothing created
      4. tag disagrees with the release's manifest version      -> fails
      5. corrupt manifest.json                                  -> fails
      6. missing manifest.json                                  -> fails
      7. missing nextide-shell.exe asset                        -> fails
      8. tampered binary (hash mismatch)                        -> fails, no smoke
      9. renamed checksum file (CHECKSUMS.txt) honored          -> exit 0
     10. missing declared checksum asset                        -> fails
     11. missing verifier script in the raw tree                -> fails
     12. stale extra file in a pre-existing OutDir              -> fails (clean-folder contract)
     13. zero-byte CLI asset                                    -> fails (non-empty assertion)
     14. non-runnable CLI smoke                                 -> fails, never verified
     15. -OutDir and -MetadataDir resolving to the same folder  -> fails, nothing created
     16. metadata folder nested inside the verified folder      -> fails, nothing created
     17. verified folder nested inside the metadata folder      -> fails, nothing created
     18. -OutDir resolving to the repository root               -> fails, nothing created/downloaded
     19. -OutDir resolving inside the repository's dist staging -> fails, nothing created/downloaded
     20. -BaseReleaseUrl with an http/ftp/other scheme or no
         scheme (javascript:, plain text)                      -> fails, nothing created/downloaded
     21. -VerifyScriptUrl with an http scheme                   -> fails, nothing created/downloaded

    The script exits 0 when every test passes and 1 when any fails.

.PARAMETER DownloadScript
    Path of the verify-release-download.ps1 under test. Defaults to
    verify-release-download.ps1 next to this script.

.PARAMETER VerifyScript
    Path of the consumer verifier (verify-checksums.ps1) seeded into each
    sandbox's raw tree as the release's own verifier. Defaults to
    verify-checksums.ps1 next to this script.

.PARAMETER SandboxRoot
    Directory under which the per-test sandboxes are created. Defaults to
    $env:TEMP. The real repository is never used as a sandbox.

.PARAMETER KeepSandbox
    Keep the sandbox directories after the run (useful for debugging); by
    default every sandbox created by this script is deleted again. The only
    thing this script ever deletes is its own sandboxes under $SandboxRoot.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-verify-release-download.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-verify-release-download.ps1 -KeepSandbox
#>
[CmdletBinding()]
param(
    [string]$DownloadScript = '',
    [string]$VerifyScript = '',
    [string]$SandboxRoot = $env:TEMP,
    [switch]$KeepSandbox
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $DownloadScript) { $DownloadScript = Join-Path $PSScriptRoot 'verify-release-download.ps1' }
if (-not $VerifyScript)   { $VerifyScript   = Join-Path $PSScriptRoot 'verify-checksums.ps1' }
if (-not (Test-Path -LiteralPath $DownloadScript)) {
    throw "verify-release-download.ps1 under test not found: $DownloadScript"
}
if (-not (Test-Path -LiteralPath $VerifyScript)) {
    throw "verify-checksums.ps1 to seed the raw tree not found: $VerifyScript"
}
if (-not (Test-Path -LiteralPath $SandboxRoot)) {
    throw "Sandbox root does not exist: $SandboxRoot"
}

# Run verify-release-download.ps1 in a child process of the same PowerShell
# edition so the exit-code contract (0 = success, non-zero = failure) is
# tested for real.
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
    # A throwaway directory holding a fake release tree (release/ + raw/) and
    # the download/verification folders. The download script under test is
    # invoked against it; the real repository is never used.
    $Root = Join-Path $SandboxRoot ('nextide-test-download-' + [guid]::NewGuid().ToString('N'))
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
    if ($Leaf -notlike 'nextide-test-download-*') {
        throw "Refusing to remove unexpected path: $Root"
    }
    if (Test-Path -LiteralPath $Root) {
        Remove-Item -LiteralPath $Root -Recurse -Force
    }
}

function ConvertTo-FileUri {
    # Build a file:/// URI from a Windows path with pure string operations
    # (no .NET Uri dependency): 'C:\a\b' -> 'file:///C:/a/b'. Spaces and '#'
    # are percent-encoded because they are not valid in a URI.
    param([Parameter(Mandatory)][string]$Path)
    $SlashPath = $Path -replace '\\', '/'
    $Escaped = $SlashPath -replace ' ', '%20' -replace '#', '%23'
    return 'file:///' + $Escaped
}

function New-FakeFile {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Content
    )
    Set-Content -LiteralPath $Path -Value $Content -Encoding ASCII -NoNewline
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Write-ChecksumManifest {
    # Write a checksum manifest from an ordered list of 'file' -> 'hash'
    # pairs, LF line endings, exactly the '<64-hex>  <file>' format the real
    # producer writes.
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][hashtable]$Entries
    )
    $Lines = @()
    foreach ($Key in $Entries.Keys) {
        $Lines += '{0}  {1}' -f $Entries[$Key], $Key
    }
    Set-Content -LiteralPath $Path -Value (($Lines -join "`n")) -Encoding ASCII -NoNewline
}

function New-FakeRelease {
    # Build a fake GitHub Release layout inside a sandbox:
    #   <root>/release/  nextide-cli.exe, nextide-shell.exe, manifest.json,
    #                    <checksum file>   (the release-asset tree)
    #   <root>/raw/      verify-checksums.ps1   (the raw-content tree)
    # The checksum manifest is generated from the actual hashes of the fake
    # files, so the checksum contract holds unless a test opts into tampering.
    param(
        [Parameter(Mandatory)][string]$Root,
        [string]$Version = '0.1.0',
        [string]$ChecksumFile = 'SHA256SUMS.txt',
        [switch]$RunnableCli,       # copy the system tree.com as the CLI stub (ignores args, exits 0, stdout only)
        [switch]$TamperCli,         # serve different bytes than the manifest declares
        [switch]$NoShell,           # omit nextide-shell.exe from the release
        [switch]$NoManifest,        # omit manifest.json
        [switch]$CorruptManifest,   # manifest.json is invalid JSON
        [switch]$NoChecksumAsset,   # omit the declared checksum asset
        [switch]$NoVerifier,        # omit raw/verify-checksums.ps1
        [switch]$ZeroByteCli,       # nextide-cli.exe is served empty
        [switch]$StaleOutFile       # pre-create the OutDir with a stale extra file
    )
    $Release = Join-Path $Root 'release'
    $Raw     = Join-Path $Root 'raw'
    $Out     = Join-Path $Root 'out'
    $null = New-Item -ItemType Directory -Force -Path $Release, $Raw
    $Cli   = Join-Path $Release 'nextide-cli.exe'
    $Shell = Join-Path $Release 'nextide-shell.exe'

    if ($RunnableCli) {
        # tree.com ignores its argument, writes only to stdout and exits 0 —
        # a boring, dependency-free smoke-pass stub that never trips the
        # Windows PowerShell 5.1 stderr-as-error-record quirk.
        $TreeCom = Join-Path $env:WINDIR 'System32\tree.com'
        if (-not (Test-Path -LiteralPath $TreeCom)) {
            throw "Smoke stub not found: $TreeCom"
        }
        Copy-Item -LiteralPath $TreeCom -Destination $Cli
    }
    elseif ($ZeroByteCli) {
        $null = New-Item -ItemType File -Force -Path $Cli
    }
    else {
        New-FakeFile -Path $Cli -Content 'fake nextide-cli payload'
    }
    if (-not $NoShell) {
        New-FakeFile -Path $Shell -Content 'fake nextide-shell payload'
    }

    if (-not $NoManifest) {
        $Manifest = @{
            schema_version = 1
            product  = @{ name = 'NextIDE'; version = $Version; description = 'fake release for sandboxed tests' }
            target   = @{ os = 'windows'; architecture = 'x86_64'; triple = 'x86_64-pc-windows-msvc' }
            release  = @{ license = 'MIT'; publisher = 'acme'; release_channel = 'stable' }
            stage    = @{ output_dir = 'dist/windows'; checksum_file = $ChecksumFile }
            artifacts = @(
                @{ id = 'nextide-cli';   file = 'nextide-cli.exe';   source = 'target/release/nextide-cli.exe' }
                @{ id = 'nextide-shell'; file = 'nextide-shell.exe'; source = 'target/release/nextide-shell.exe' }
            )
        }
        if ($CorruptManifest) {
            New-FakeFile -Path (Join-Path $Release 'manifest.json') -Content '{ not json'
        }
        else {
            ($Manifest | ConvertTo-Json -Depth 6) | Set-Content -LiteralPath (Join-Path $Release 'manifest.json') -Encoding UTF8
        }
    }

    if (-not $NoChecksumAsset) {
        $Entries = @{
            'nextide-cli.exe' = (Get-Sha256Hex $Cli)
        }
        if (-not $NoShell) { $Entries['nextide-shell.exe'] = (Get-Sha256Hex $Shell) }
        if ($TamperCli) {
            # The manifest is written for the pristine payload; the served
            # file is then replaced with different bytes, so the download
            # hashes differently than the manifest declares.
            New-FakeFile -Path $Cli -Content 'TAMPERED nextide-cli payload'
        }
        Write-ChecksumManifest -Path (Join-Path $Release $ChecksumFile) -Entries $Entries
    }

    if (-not $NoVerifier) {
        Copy-Item -LiteralPath $VerifyScript -Destination (Join-Path $Raw 'verify-checksums.ps1')
    }

    if ($StaleOutFile) {
        $null = New-Item -ItemType Directory -Force -Path $Out
        New-FakeFile -Path (Join-Path $Out 'stale.txt') -Content 'stale leftover file'
    }

    return [pscustomobject]@{
        Root        = $Root
        Release     = $Release
        BaseUrl     = (ConvertTo-FileUri $Release)
        RawUrl      = (ConvertTo-FileUri (Join-Path $Raw 'verify-checksums.ps1'))
        OutDir      = $Out
        MetadataDir = Join-Path $Root 'meta'
    }
}

function Invoke-DownloadScript {
    # Run the download script under test in a child process. Never turn a
    # non-zero native exit code into a terminating error here: the exit code
    # is the value under test. Note: do NOT pre-assign $LASTEXITCODE in this
    # function — that would shadow the automatic variable and the engine
    # would no longer update it.
    param(
        [Parameter(Mandatory)][pscustomobject]$Release,
        [string]$Tag = 'v0.1.0',
        [string]$Repo = 'acme/nextide',
        [switch]$SkipSmoke,
        # Override the -OutDir/-MetadataDir handed to the script under test
        # (defaults: the sandbox's own folders). Needed by the folder-shape
        # cases, which probe equal/nested/repository paths instead.
        [string]$OutDirOverride = '',
        [string]$MetadataDirOverride = '',
        # Override the -BaseReleaseUrl/-VerifyScriptUrl handed to the script
        # under test (defaults: the sandbox's file:// URLs). Needed by the
        # URL-scheme cases, which probe http/ftp/other schemes instead.
        [string]$BaseUrlOverride = '',
        [string]$VerifyUrlOverride = ''
    )
    $ChildOut = if ($OutDirOverride)      { $OutDirOverride }      else { $Release.OutDir }
    $ChildMeta = if ($MetadataDirOverride) { $MetadataDirOverride } else { $Release.MetadataDir }
    $ChildBase = if ($BaseUrlOverride)     { $BaseUrlOverride }     else { $Release.BaseUrl }
    $ChildRaw  = if ($VerifyUrlOverride)   { $VerifyUrlOverride }   else { $Release.RawUrl }
    $ChildArgs = @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $DownloadScript,
        '-Tag', $Tag,
        '-Repo', $Repo,
        '-OutDir', $ChildOut,
        '-MetadataDir', $ChildMeta,
        '-BaseReleaseUrl', $ChildBase,
        '-VerifyScriptUrl', $ChildRaw
    )
    if ($SkipSmoke) { $ChildArgs += '-SkipSmoke' }
    # Under Windows PowerShell 5.1 a native stderr line becomes an error
    # record, and with EAP=Stop the merged 2>&1 pipeline aborts on the first
    # line (truncating the capture). EAP=Continue lets the full output through
    # so the child's error text still lands in $Result.Output on both
    # PowerShell editions.
    $PSNativeCommandUseErrorActionPreference = $false
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

function Get-FileNames {
    param([Parameter(Mandatory)][string]$Dir)
    if (-not (Test-Path -LiteralPath $Dir)) { return @() }
    return @(Get-ChildItem -LiteralPath $Dir -File | ForEach-Object { $_.Name } | Sort-Object)
}

Write-Host '=================================================='
Write-Host 'End-user release download verification tests'
Write-Host "  download script under test : $DownloadScript"
Write-Host "  consumer verifier seeded    : $VerifyScript"
Write-Host "  sandbox root                : $SandboxRoot"
Write-Host '  every run happens in a throwaway sandbox over file:// URLs;'
Write-Host '  the real repository is never modified and the network is never touched'
Write-Host '=================================================='

# ---------------------------------------------------------------------------
# 1. Happy path: full flow — download, checksum verify, CLI smoke.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'happy path verifies a downloaded release (checksums + smoke)' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match '==> OK\.') "expected success banner in output: $($Result.Output)"
        Assert ($Result.Output -match 'ok nextide-cli\.exe') 'expected the verifier to print the ok line for nextide-cli.exe'
        Assert ($Result.Output -match 'ok nextide-shell\.exe') 'expected the verifier to print the ok line for nextide-shell.exe'
        Assert ($Result.Output -match 'Smoke-testing the downloaded CLI') 'expected the smoke step to run'
        $Expected = @('nextide-cli.exe', 'nextide-shell.exe', 'SHA256SUMS.txt')
        $Actual = Get-FileNames $Release.OutDir
        Assert (($Actual -join ',') -eq ($Expected -join ',')) "verified folder must hold exactly $($Expected -join ', '), got: $($Actual -join ', ')"
        $Meta = Get-FileNames $Release.MetadataDir
        Assert (($Meta -join ',') -eq 'manifest.json,verify-checksums.ps1') "metadata folder must hold exactly manifest.json + verify-checksums.ps1, got: $($Meta -join ', ')"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 2. Happy path with -SkipSmoke: checksum verification still runs.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'happy path with -SkipSmoke verifies checksums without smoke' -Body {
    $Root = New-Sandbox
    try {
        # The CLI stub is a plain text file here — it would fail the smoke
        # step, proving -SkipSmoke really skips it.
        $Release = New-FakeRelease -Root $Root
        $Result = Invoke-DownloadScript -Release $Release -SkipSmoke
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match '==> OK\.') "expected success banner in output: $($Result.Output)"
        Assert ($Result.Output -notmatch 'Smoke-testing the downloaded CLI') 'the smoke step must not run with -SkipSmoke'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 3. Invalid tags are rejected before anything is created or downloaded.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'invalid tags (main / v0.1 / v0.1.0.1) are rejected before any download' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root
        foreach ($BadTag in @('main', 'v0.1', 'v0.1.0.1', '0.1.0')) {
            $Result = Invoke-DownloadScript -Release $Release -Tag $BadTag
            Assert ($Result.ExitCode -ne 0) "tag '$BadTag': expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
            Assert ($Result.Output -match 'not a v-prefixed semver tag') "tag '$BadTag': expected the tag-rule message: $($Result.Output)"
            Assert (-not (Test-Path -LiteralPath $Release.OutDir)) "tag '$BadTag': no download folder may be created"
            Assert (-not (Test-Path -LiteralPath $Release.MetadataDir)) "tag '$BadTag': no metadata folder may be created"
        }
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 4. Tag/version consistency: the release's manifest version must equal the
#    tag version — the final consistency link, checked on the consumer side.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'tag disagreeing with the release manifest version is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -Version '0.2.0'
        $Result = Invoke-DownloadScript -Release $Release -Tag 'v0.1.0'
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'product\.version') "expected the version-mismatch message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 5. Corrupt manifest.json.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'corrupt manifest.json is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -CorruptManifest
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'not valid JSON') "expected the invalid-JSON message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 6. Missing manifest.json.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing manifest.json is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -NoManifest
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'manifest\.json') "expected a manifest.json message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 7. Missing release asset (nextide-shell.exe).
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing release asset (nextide-shell.exe) is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -NoShell
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'Failed to download') "expected a download-failure message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 8. Tampered binary: the served bytes differ from the checksum manifest, so
#    the consumer verifier rejects the download — and the smoke step never
#    runs on unverified bytes.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'tampered binary fails checksum verification and never reaches smoke' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -TamperCli
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'SHA-256 mismatch') "expected the verifier's hash-mismatch message: $($Result.Output)"
        Assert ($Result.Output -notmatch 'Smoke-testing the downloaded CLI') 'the smoke step must not run on unverified bytes'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 9. Renamed checksum file: the name flows from the release's own
#    manifest.json (stage.checksum_file), never a hardcoded default.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'renamed checksum file (CHECKSUMS.txt) is resolved from the release manifest' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli -ChecksumFile 'CHECKSUMS.txt'
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
        $Actual = Get-FileNames $Release.OutDir
        Assert ($Actual -contains 'CHECKSUMS.txt') "expected CHECKSUMS.txt in the verified folder, got: $($Actual -join ', ')"
        Assert ($Actual -notcontains 'SHA256SUMS.txt') 'the hardcoded default name must not appear'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 10. Missing declared checksum asset.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing declared checksum asset is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -NoChecksumAsset
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'Failed to download') "expected a download-failure message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 11. Missing consumer verifier in the raw tree.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing consumer verifier script is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -NoVerifier
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'verify-checksums\.ps1') "expected a verifier-download message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 12. Stale extra file in a pre-existing OutDir: the exact-match contract of
#     the consumer verifier rejects it — the clean-folder contract working.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'stale extra file in the download folder fails the exact-match contract' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -StaleOutFile
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'declares no entry') "expected the exact-match rejection of the stale file: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 13. Zero-byte CLI asset: the checksum manifest still matches (the empty
#     file hashes consistently), but the non-empty assertion — the release
#     job's own checklist rule — closes the gap before any smoke.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'zero-byte CLI asset is rejected by the non-empty assertion' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -ZeroByteCli
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'missing or empty') "expected the non-empty message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 14. Non-runnable CLI: checksums pass (the stub's bytes match the manifest),
#     the smoke gate fails, the verification exits non-zero.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'non-runnable CLI fails the smoke gate' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root
        $Result = Invoke-DownloadScript -Release $Release
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'smoke') "expected a smoke-failure message: $($Result.Output)"
        Assert ($Result.Output -notmatch '==> OK\. Release v0\.1\.0 verified') 'the download script success banner must not appear'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 15. Folder-shape validation (read-only, before anything is created or
#     downloaded): -OutDir and -MetadataDir must be two distinct folders.
# ---------------------------------------------------------------------------
Invoke-Test -Name '-OutDir and -MetadataDir resolving to the same folder are rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        $Shared = Join-Path $Root 'shared'
        $Result = Invoke-DownloadScript -Release $Release -OutDirOverride $Shared -MetadataDirOverride $Shared
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'two different folders') "expected the distinct-folders message: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath $Shared)) 'no folder may be created before the shape validation'
        Assert ($Result.Output -notmatch 'Smoke-testing the downloaded CLI') 'no download/smoke may happen'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 16. Metadata folder nested inside the verified folder: the consumer verifier
#     lists only top-level files, so the metadata would sit inside the
#     verified tree and silently bypass the exact-match contract — rejected
#     up front, before any download.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'metadata folder nested inside the verified folder is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        $Out = Join-Path $Root 'out-nested'
        $Meta = Join-Path $Out 'meta'
        $Result = Invoke-DownloadScript -Release $Release -OutDirOverride $Out -MetadataDirOverride $Meta
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'must be outside the verified folder') "expected the outside-the-verified-folder message: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath $Out)) 'no folder may be created before the shape validation'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 17. Symmetric nesting: the verified folder inside the metadata folder.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'verified folder nested inside the metadata folder is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        $Meta = Join-Path $Root 'meta-out'
        $Out = Join-Path $Meta 'out'
        $Result = Invoke-DownloadScript -Release $Release -OutDirOverride $Out -MetadataDirOverride $Meta
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'must be outside the metadata folder') "expected the outside-the-metadata-folder message: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath $Meta)) 'no folder may be created before the shape validation'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 18. The documented 'never into the repository' rule, enforced: -OutDir
#     resolving to the repository root is refused. The script under test
#     derives the repo root from its own location, so -OutDir <real repo
#     root> must fail read-only, before anything is created or downloaded —
#     nothing in the real repository is ever touched.
# ---------------------------------------------------------------------------
Invoke-Test -Name '-OutDir resolving to the repository root is rejected (nothing written)' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        $RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
        $Result = Invoke-DownloadScript -Release $Release -OutDirOverride $RepoRoot -MetadataDirOverride (Join-Path $Root 'meta')
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'Refusing to write into the repository') "expected the repo-refusal message: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath (Join-Path $RepoRoot 'nextide-cli.exe'))) 'no binary may be downloaded into the repository'
        Assert (-not (Test-Path -LiteralPath (Join-Path $RepoRoot 'manifest.json'))) 'no metadata may be downloaded into the repository'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 19. Same rule for the repository's staging folder: -OutDir resolving inside
#     dist/ (where the release artifacts are staged) is refused.
# ---------------------------------------------------------------------------
Invoke-Test -Name '-OutDir resolving inside the repository staging folder (dist/) is rejected' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        $RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
        $Dist = Join-Path $RepoRoot 'dist\windows'
        $Result = Invoke-DownloadScript -Release $Release -OutDirOverride $Dist -MetadataDirOverride (Join-Path $Root 'meta')
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'Refusing to write into the repository') "expected the repo-refusal message: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 20. Download URL scheme gate: only https and file are allowed; http/ftp and
#     any other scheme (and non-absolute URLs) are refused read-only, before
#     anything is created or downloaded. The binaries, the checksum manifest
#     and the consumer verifier all travel over these URLs, so an
#     unauthenticated transport would let a network attacker substitute the
#     binaries AND their manifest together — the verifier must never download
#     over one.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'release asset URL with an http/ftp/other scheme or no scheme is rejected before any download' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        foreach ($BadUrl in @(
            'http://mirror.example.com/ai-code/releases/download/v0.1.0',
            'ftp://mirror.example.com/ai-code/releases/download/v0.1.0',
            'javascript:alert(1)',
            'not a url'
        )) {
            $Result = Invoke-DownloadScript -Release $Release -BaseUrlOverride $BadUrl
            Assert ($Result.ExitCode -ne 0) "'$BadUrl': expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
            Assert ($Result.Output -match 'https:// or file://') "'$BadUrl': expected the scheme-gate message: $($Result.Output)"
            Assert (-not (Test-Path -LiteralPath $Release.OutDir)) "'$BadUrl': no download folder may be created"
            Assert (-not (Test-Path -LiteralPath $Release.MetadataDir)) "'$BadUrl': no metadata folder may be created"
        }
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 21. Same scheme gate for the consumer verifier URL: the verifier script is
#     fetched over the same integrity channel, so http is refused there too.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'consumer verifier URL with an http scheme is rejected before any download' -Body {
    $Root = New-Sandbox
    try {
        $Release = New-FakeRelease -Root $Root -RunnableCli
        $Result = Invoke-DownloadScript -Release $Release -VerifyUrlOverride 'http://mirror.example.com/ai-code/raw/v0.1.0/packaging/windows/verify-checksums.ps1'
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'https:// or file://') "expected the scheme-gate message: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath $Release.OutDir)) 'no download folder may be created'
        Assert (-not (Test-Path -LiteralPath $Release.MetadataDir)) 'no metadata folder may be created'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
Write-Host ''
Write-Host '=================================================='
Write-Host "Result: $($script:Passed) passed, $($script:Failed) failed"
if ($script:Failed -gt 0) {
    Write-Host 'Failures:'
    foreach ($Failure in $script:Failures) { Write-Host "  - $Failure" }
    Write-Host '=================================================='
    exit 1
}
Write-Host '=================================================='
exit 0
