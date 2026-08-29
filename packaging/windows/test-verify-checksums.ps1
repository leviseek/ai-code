#Requires -Version 5.1
<#
.SYNOPSIS
    Self-contained validation tests for packaging\windows\verify-checksums.ps1.

.DESCRIPTION
    Tests the consumer-side checksum verifier without any external test
    framework (plain PowerShell assertions only) and without touching the real
    repository: every test builds a throwaway sandbox directory under the
    sandbox root (default: $env:TEMP) containing a staging directory, fake
    files and a checksum manifest, runs `verify-checksums.ps1` against that
    sandbox in a child process of the same PowerShell edition (so the exit
    code contract 0 = success / non-zero = failure is tested for real), and
    deletes the sandbox again when the run finishes. Nothing in the repository
    is written, renamed or removed; in particular `dist\windows\` of the real
    repo is never touched. One test proves the verifier is strictly read-only:
    it snapshots every sandbox file's name/size/last-write-time and the
    checksum manifest content before and after a successful run and asserts
    nothing changed.

    Covered scenarios:
      1. happy path (correct manifest, two files)            -> exit 0
      2. bad hash (one entry's hash is wrong)                -> fails
      3. missing file (entry names a file that is absent)    -> fails
      4. bad format (line not '<64-hex>  <file>')            -> fails
      5. missing checksum manifest file                      -> fails
      6. empty checksum manifest                             -> fails
      7. entry name with '..' path escape                    -> fails
      8. entry name with a path separator                    -> fails
      9. extra file in the directory without an entry        -> fails
     10. duplicate entry for the same file                   -> fails
     11. lowercase hashes (GNU-style) accepted               -> exit 0
     12. CRLF line endings accepted                          -> exit 0
     13. custom -ChecksumFile name honored                   -> exit 0
     14. run is strictly read-only (sandbox snapshot equal)  -> passes

    The script exits 0 when every test passes and 1 when any fails.

.PARAMETER VerifyScript
    Path of the verify-checksums.ps1 under test. Defaults to
    verify-checksums.ps1 next to this script.

.PARAMETER SandboxRoot
    Directory under which the per-test sandboxes are created. Defaults to
    $env:TEMP. The real repository is never used as a sandbox.

.PARAMETER KeepSandbox
    Keep the sandbox directories after the run (useful for debugging); by
    default every sandbox created by this script is deleted again. The only
    thing this script ever deletes is its own sandboxes under $SandboxRoot.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-verify-checksums.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-verify-checksums.ps1 -KeepSandbox
#>
[CmdletBinding()]
param(
    [string]$VerifyScript = '',
    [string]$SandboxRoot = $env:TEMP,
    [switch]$KeepSandbox
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $VerifyScript) { $VerifyScript = Join-Path $PSScriptRoot 'verify-checksums.ps1' }
if (-not (Test-Path -LiteralPath $VerifyScript)) {
    throw "verify-checksums.ps1 under test not found: $VerifyScript"
}
if (-not (Test-Path -LiteralPath $SandboxRoot)) {
    throw "Sandbox root does not exist: $SandboxRoot"
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
    # A throwaway directory whose dist\windows holds the files and checksum
    # manifest under test. The verifier is invoked from its original path; the
    # sandbox only ever contains the fake staging directory.
    $Root = Join-Path $SandboxRoot ('nextide-test-verify-' + [guid]::NewGuid().ToString('N'))
    $Dist = Join-Path $Root 'dist\windows'
    $null = New-Item -ItemType Directory -Force -Path $Dist
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
    if ($Leaf -notlike 'nextide-test-verify-*') {
        throw "Refusing to remove unexpected path: $Root"
    }
    if (Test-Path -LiteralPath $Root) {
        Remove-Item -LiteralPath $Root -Recurse -Force
    }
}

function Invoke-VerifyScript {
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [string]$DistDir = '',
        [string]$ChecksumFile = ''
    )
    if (-not $DistDir)      { $DistDir = Join-Path $Sandbox 'dist\windows' }
    if (-not $ChecksumFile) { $ChecksumFile = Join-Path $DistDir 'SHA256SUMS.txt' }
    $ChildArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $VerifyScript, '-DistDir', $DistDir, '-ChecksumFile', $ChecksumFile)
    # Never turn a non-zero native exit code into a terminating error here:
    # the exit code is the value under test. Note: do NOT pre-assign
    # $LASTEXITCODE in this function — that would shadow the automatic
    # variable and the engine would no longer update it.
    $PSNativeCommandUseErrorActionPreference = $false
    # Under Windows PowerShell 5.1 a native stderr line becomes an error
    # record, and with EAP=Stop the merged 2>&1 pipeline aborts on the first
    # line (truncating the capture). EAP=Continue lets the full output through
    # so the child's error text still lands in $Result.Output on both
    # PowerShell editions.
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
    # Write a checksum manifest from an ordered list of 'file' -> 'hash' pairs.
    # $LineEnding controls the line terminator ('`n' LF by default, as the
    # producer writes; '`r`n' CRLF for the compatibility test).
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][hashtable]$Entries,
        [string]$LineEnding = "`n"
    )
    $Lines = @()
    foreach ($Key in $Entries.Keys) {
        $Lines += '{0}  {1}' -f $Entries[$Key], $Key
    }
    Set-Content -LiteralPath $Path -Value (($Lines -join $LineEnding)) -Encoding ASCII -NoNewline
}

function Get-SandboxSnapshot {
    # Read-only snapshot of a sandbox: file name -> (length, last-write ticks,
    # sha256 of content). Used to prove the verifier never writes anything.
    param([Parameter(Mandatory)][string]$Sandbox)
    $Snapshot = @{}
    foreach ($File in (Get-ChildItem -LiteralPath $Sandbox -Recurse -File)) {
        $Snapshot[$File.FullName] = [pscustomobject]@{
            Length      = $File.Length
            LastWrite   = $File.LastWriteTimeUtc.Ticks
            ContentHash = (Get-Sha256Hex $File.FullName)
        }
    }
    return $Snapshot
}

function Assert-SnapshotsEqual {
    param(
        [Parameter(Mandatory)]$Before,
        [Parameter(Mandatory)]$After,
        [Parameter(Mandatory)][string]$Sandbox
    )
    $BeforePaths = @($Before.Keys | Sort-Object)
    $AfterPaths  = @($After.Keys  | Sort-Object)
    Assert ($BeforePaths.Count -eq $AfterPaths.Count) "file count changed: $($BeforePaths.Count) -> $($AfterPaths.Count)"
    for ($i = 0; $i -lt $BeforePaths.Count; $i++) {
        Assert ($BeforePaths[$i] -eq $AfterPaths[$i]) "file set changed: '$($BeforePaths[$i])' vs '$($AfterPaths[$i])'"
    }
    foreach ($Path in $BeforePaths) {
        Assert ($After.ContainsKey($Path)) "file disappeared: $Path"
        Assert ($Before[$Path].Length -eq $After[$Path].Length) "file length changed: $Path"
        Assert ($Before[$Path].LastWrite -eq $After[$Path].LastWrite) "file last-write-time changed: $Path"
        Assert ($Before[$Path].ContentHash -eq $After[$Path].ContentHash) "file content changed: $Path"
    }
}

Write-Host '=================================================='
Write-Host "Windows checksum consumer verification tests"
Write-Host "  verify script under test : $VerifyScript"
Write-Host "  sandbox root             : $SandboxRoot"
Write-Host '  every run happens in a throwaway sandbox; the real repository is never modified'
Write-Host '=================================================='

# ---------------------------------------------------------------------------
# 1. Happy path: correct manifest, both files present with matching hashes.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'happy path verifies a consistent directory and manifest' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        $AppB = Join-Path $Dist 'app-b.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        New-FakeFile -Path $AppB -Content 'payload for app-b'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'app-a.bin' = (Get-Sha256Hex $AppA)
            'app-b.bin' = (Get-Sha256Hex $AppB)
        }
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'OK\.') "expected success banner in output: $($Result.Output)"
        Assert ($Result.Output -match 'app-a\.bin') 'expected app-a.bin verification line in output'
        Assert ($Result.Output -match 'app-b\.bin') 'expected app-b.bin verification line in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 2. Bad hash: one entry's hash does not match the actual file.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'bad hash fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        $AppB = Join-Path $Dist 'app-b.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        New-FakeFile -Path $AppB -Content 'payload for app-b'
        $GoodHash = Get-Sha256Hex $AppA
        # Flip the last hex digit of the real hash (keep it a valid hex
        # string): the entry parses, the file exists, only the hash is wrong.
        $LastDigit = $GoodHash.Substring($GoodHash.Length - 1)
        $FlipDigit  = if ($LastDigit -eq '0') { '1' } else { '0' }
        $BadHash    = $GoodHash.Substring(0, $GoodHash.Length - 1) + $FlipDigit
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'app-a.bin' = $BadHash
            'app-b.bin' = (Get-Sha256Hex $AppB)
        }
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'SHA-256 mismatch') "expected 'SHA-256 mismatch' in output: $($Result.Output)"
        Assert ($Result.Output -match 'app-a\.bin') 'expected the mismatched file name in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 3. Missing file: the manifest names a file that does not exist.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing file fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        # The manifest declares a plausible hash for a file that was never
        # staged (and a correct entry for the file that exists).
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'app-a.bin' = (Get-Sha256Hex $AppA)
            'ghost.bin' = ('0' * 64)
        }
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'does not exist') "expected 'does not exist' in output: $($Result.Output)"
        Assert ($Result.Output -match 'ghost\.bin') 'expected the missing file name in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 4. Bad format: a line that is not '<64-hex>  <file>' (exactly two spaces).
# ---------------------------------------------------------------------------
Invoke-Test -Name 'bad format fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        $Hash = Get-Sha256Hex $AppA
        # Three distinct format violations: a garbage line, a hash that is not
        # 64 hex characters (with two spaces), and a correct hash separated by
        # a single space instead of two.
        $BadLines = @(
            'not-a-checksum-line',
            'deadbeef  app-a.bin',
            ('{0} {1}' -f $Hash, 'app-a.bin')
        )
        foreach ($BadLine in $BadLines) {
            Set-Content -LiteralPath (Join-Path $Dist 'SHA256SUMS.txt') -Value $BadLine -Encoding ASCII -NoNewline
            $Result = Invoke-VerifyScript -Sandbox $Root
            Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode) for line '$BadLine'"
            Assert ($Result.Output -match 'not in') "expected format rejection in output for line '$BadLine': $($Result.Output)"
        }
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 5. Missing checksum manifest file.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing checksum manifest fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        New-FakeFile -Path (Join-Path $Dist 'app-a.bin') -Content 'payload for app-a'
        # No SHA256SUMS.txt is written at all.
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'not found') "expected 'not found' in output: $($Result.Output)"
        Assert ($Result.Output -match 'SHA256SUMS\.txt') 'expected the manifest file name in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 6. Empty checksum manifest.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'empty checksum manifest fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        New-FakeFile -Path (Join-Path $Dist 'app-a.bin') -Content 'payload for app-a'
        # An empty manifest: the file exists but declares no entries.
        $null = New-Item -ItemType File -Path (Join-Path $Dist 'SHA256SUMS.txt') -Force
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'is empty') "expected 'is empty' in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 7. Entry name with a '..' path escape (safe read-only hardening).
# ---------------------------------------------------------------------------
Invoke-Test -Name "entry name with '..' path escape fails" -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            '..\escape.bin' = (Get-Sha256Hex $AppA)
        }
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'plain file') "expected leaf-name rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 8. Entry name with a path separator (must stay inside the staging dir).
# ---------------------------------------------------------------------------
Invoke-Test -Name 'entry name with a path separator fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'sub\app-a.bin' = (Get-Sha256Hex $AppA)
        }
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'plain file') "expected leaf-name rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 9. Extra file in the directory with no manifest entry (exact-match contract).
# ---------------------------------------------------------------------------
Invoke-Test -Name 'extra file without a manifest entry fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        New-FakeFile -Path (Join-Path $Dist 'extra.bin') -Content 'not declared anywhere'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'app-a.bin' = (Get-Sha256Hex $AppA)
        }
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'declares no entry') "expected extra-file rejection in output: $($Result.Output)"
        Assert ($Result.Output -match 'extra\.bin') 'expected the extra file name in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 10. Duplicate entry for the same file.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'duplicate entry fails' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        $Hash = Get-Sha256Hex $AppA
        Set-Content -LiteralPath (Join-Path $Dist 'SHA256SUMS.txt') -Value ("{0}  app-a.bin`n{0}  app-a.bin" -f $Hash) -Encoding ASCII -NoNewline
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'more than once') "expected duplicate rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 11. Lowercase hashes (as GNU sha256sum prints them) are accepted.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'lowercase hashes are accepted' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'app-a.bin' = ((Get-Sha256Hex $AppA).ToLowerInvariant())
        }
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 12. CRLF line endings (e.g. after zip extraction) are accepted.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'CRLF line endings are accepted' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'app-a.bin' = (Get-Sha256Hex $AppA)
        } -LineEnding "`r`n"
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 13. A custom -ChecksumFile name (e.g. the manifest's stage.checksum_file
#     renamed for a release) is honored.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'custom checksum file name is honored' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        $Custom = Join-Path $Dist 'CHECKSUMS.txt'
        Write-ChecksumManifest -Path $Custom -Entries @{
            'app-a.bin' = (Get-Sha256Hex $AppA)
        }
        # No SHA256SUMS.txt exists; only the explicitly named file is read.
        $Result = Invoke-VerifyScript -Sandbox $Root -ChecksumFile $Custom
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'CHECKSUMS\.txt') 'expected the custom checksum file name in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 14. Read-only safety: a successful run must not write, rename or remove
#     anything in the sandbox.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'run is strictly read-only' -Body {
    $Root = New-Sandbox
    try {
        $Dist = Join-Path $Root 'dist\windows'
        $AppA = Join-Path $Dist 'app-a.bin'
        $AppB = Join-Path $Dist 'app-b.bin'
        New-FakeFile -Path $AppA -Content 'payload for app-a'
        New-FakeFile -Path $AppB -Content 'payload for app-b'
        Write-ChecksumManifest -Path (Join-Path $Dist 'SHA256SUMS.txt') -Entries @{
            'app-a.bin' = (Get-Sha256Hex $AppA)
            'app-b.bin' = (Get-Sha256Hex $AppB)
        }
        $Before = Get-SandboxSnapshot $Root
        $Result = Invoke-VerifyScript -Sandbox $Root
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
        $After = Get-SandboxSnapshot $Root
        Assert-SnapshotsEqual -Before $Before -After $After -Sandbox $Root
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------
if (-not $KeepSandbox) {
    foreach ($S in $script:Sandboxes) {
        $Leaf = Split-Path -Leaf $S
        if ($Leaf -like 'nextide-test-verify-*' -and (Test-Path -LiteralPath $S)) {
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
    Write-Host "The real repository was not modified; only sandboxes under $SandboxRoot were written and removed."
}
if ($script:Failed -gt 0) { exit 1 }
exit 0
