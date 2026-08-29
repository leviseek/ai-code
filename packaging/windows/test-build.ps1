#Requires -Version 5.1
<#
.SYNOPSIS
    Self-contained validation tests for packaging\windows\build.ps1.

.DESCRIPTION
    Tests the Windows packaging baseline without any external test framework
    (plain PowerShell assertions only) and without touching the real
    repository: every test runs `build.ps1` against a throwaway sandbox copy
    created under the sandbox root (default: $env:TEMP), and the sandbox is
    deleted again when the run finishes. Nothing in the repository is written,
    renamed or removed; in particular `dist\windows\` of the real repo is never
    touched, and one test proves the script itself preserves pre-existing
    files in the staging directory (it only overwrites, never deletes).

    Each sandbox also gets a minimal root Cargo.toml whose [workspace.package]
    version is read from the manifest template, so the version-consistency
    check (manifest product.version == Cargo.toml [workspace.package] version)
    is exercised on every run; the mismatch cases overwrite that Cargo.toml.

    Covered scenarios:
      1. -SkipBuild happy path: artifacts staged, size+SHA-256 verified,
         report printed, pre-existing files in dist\windows\ preserved
      2. missing manifest.json                  -> fails
      3. missing release artifact (source)      -> fails
      4. wrong pinned expected_sha256           -> fails
      5. correct pinned expected_sha256         -> passes
      6. path escape via '..' component         -> fails
      7. path escape via rooted absolute path   -> fails
      8. invalid manifest JSON                  -> fails
      9. artifact file != source file name      -> fails
     10. unsupported manifest schema_version    -> fails
     11. failed release build (workspace without packages, no -SkipBuild)
                                                -> fails loudly
     12. product.version != Cargo.toml workspace version -> fails
     13. missing Cargo.toml at the repo root    -> fails
     14. Cargo.toml without [workspace.package] version -> fails
     15. target.architecture mismatch vs pinned x86_64-pc-windows-msvc -> fails
     16. target.triple mismatch vs pinned x86_64-pc-windows-msvc      -> fails
     17. target.os mismatch vs pinned windows target                  -> fails
     18. missing release block (license/publisher/release_channel)    -> fails
     19. missing release.license                -> fails
     20. missing release.publisher              -> fails
     21. missing release.release_channel        -> fails
     22. release.release_channel not stable/beta (e.g. 'alpha')       -> fails
     23. release.release_channel 'beta' (valid channel)               -> passes

    The script exits 0 when every test passes and 1 when any fails.

.PARAMETER BuildScript
    Path of the build.ps1 under test. Defaults to build.ps1 next to this
    script; the manifest.json sitting next to it is used as the template.

.PARAMETER SandboxRoot
    Directory under which the per-test sandboxes are created. Defaults to
    $env:TEMP. The real repository is never used as a sandbox.

.PARAMETER KeepSandbox
    Keep the sandbox directories after the run (useful for debugging); by
    default every sandbox created by this script is deleted again. The only
    thing this script ever deletes is its own sandboxes under $SandboxRoot.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-build.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\test-build.ps1 -KeepSandbox
#>
[CmdletBinding()]
param(
    [string]$BuildScript = '',
    [string]$SandboxRoot = $env:TEMP,
    [switch]$KeepSandbox
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $BuildScript) { $BuildScript = Join-Path $PSScriptRoot 'build.ps1' }
if (-not (Test-Path -LiteralPath $BuildScript)) {
    throw "build.ps1 under test not found: $BuildScript"
}
$ManifestSource = Join-Path (Split-Path -Parent $BuildScript) 'manifest.json'
if (-not (Test-Path -LiteralPath $ManifestSource)) {
    throw "manifest.json for the build script under test not found: $ManifestSource"
}
if (-not (Test-Path -LiteralPath $SandboxRoot)) {
    throw "Sandbox root does not exist: $SandboxRoot"
}

# The sandbox Cargo.toml's [workspace.package] version is derived from the
# manifest template so the tests stay version-agnostic.
$TemplateManifest = Get-Content -Raw -LiteralPath $ManifestSource | ConvertFrom-Json
$script:TemplateVersion = $TemplateManifest.product.version
if ([string]::IsNullOrWhiteSpace($script:TemplateVersion)) {
    throw "Manifest template does not declare product.version: $ManifestSource"
}

# Run build.ps1 in a child process of the same PowerShell edition so the exit
# code contract (0 = success, non-zero = failure) is tested for real.
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

function Write-SandboxCargoToml {
    # Every sandbox gets a minimal root Cargo.toml so the version-consistency
    # check (manifest product.version == [workspace.package] version) runs on
    # every test; version defaults to the manifest template's product.version.
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [string]$Version = $script:TemplateVersion
    )
    $CargoToml = @(
        '[workspace]'
        'resolver = "2"'
        ''
        '[workspace.package]'
        "version = `"$Version`""
    ) -join "`r`n"
    Set-Content -LiteralPath (Join-Path $Sandbox 'Cargo.toml') -Value $CargoToml -Encoding ASCII
}

function New-Sandbox {
    param([switch]$WithManifest)
    $Root = Join-Path $SandboxRoot ('nextide-test-build-' + [guid]::NewGuid().ToString('N'))
    $Pkg = Join-Path $Root 'packaging\windows'
    $null = New-Item -ItemType Directory -Force -Path $Pkg
    Copy-Item -LiteralPath $BuildScript -Destination (Join-Path $Pkg 'build.ps1')
    if ($WithManifest) {
        Copy-Item -LiteralPath $ManifestSource -Destination (Join-Path $Pkg 'manifest.json')
    }
    Write-SandboxCargoToml -Sandbox $Root
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
    if ($Leaf -notlike 'nextide-test-build-*') {
        throw "Refusing to remove unexpected path: $Root"
    }
    if (Test-Path -LiteralPath $Root) {
        Remove-Item -LiteralPath $Root -Recurse -Force
    }
}

function Invoke-BuildScript {
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [switch]$SkipBuild
    )
    $ChildScript = Join-Path $Sandbox 'packaging\windows\build.ps1'
    $ChildArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $ChildScript)
    if ($SkipBuild) { $ChildArgs += '-SkipBuild' }
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

function New-FakeBinaries {
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][string[]]$Names
    )
    $Rel = Join-Path $Sandbox 'target\release'
    $null = New-Item -ItemType Directory -Force -Path $Rel
    foreach ($Name in $Names) {
        Set-Content -LiteralPath (Join-Path $Rel $Name) -Value "fake payload for $Name" -Encoding ASCII -NoNewline
    }
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Save-Manifest {
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)]$Manifest
    )
    $ManifestPath = Join-Path $Sandbox 'packaging\windows\manifest.json'
    # ASCII: no BOM, safe for both Windows PowerShell 5.1 and PowerShell Core.
    $Manifest | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $ManifestPath -Encoding ASCII
}

function Set-ManifestArtifact {
    # Overwrite (or add) a field on one artifact entry of the sandbox manifest.
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][int]$Index,
        [Parameter(Mandatory)][hashtable]$Set
    )
    $ManifestPath = Join-Path $Sandbox 'packaging\windows\manifest.json'
    $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    $Artifact = $Manifest.artifacts[$Index]
    foreach ($Key in $Set.Keys) {
        $Artifact | Add-Member -NotePropertyName $Key -NotePropertyValue $Set[$Key] -Force
    }
    Save-Manifest -Sandbox $Sandbox -Manifest $Manifest
}

function Set-ManifestField {
    # Overwrite (or add) a top-level field of the sandbox manifest.
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][string]$Key,
        [Parameter(Mandatory)]$Value
    )
    $ManifestPath = Join-Path $Sandbox 'packaging\windows\manifest.json'
    $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    $Manifest | Add-Member -NotePropertyName $Key -NotePropertyValue $Value -Force
    Save-Manifest -Sandbox $Sandbox -Manifest $Manifest
}

function Set-ManifestTargetField {
    # Overwrite (or add) a field inside the target block of the sandbox manifest.
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][string]$Key,
        [Parameter(Mandatory)]$Value
    )
    $ManifestPath = Join-Path $Sandbox 'packaging\windows\manifest.json'
    $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    $Manifest.target | Add-Member -NotePropertyName $Key -NotePropertyValue $Value -Force
    Save-Manifest -Sandbox $Sandbox -Manifest $Manifest
}

function Set-ManifestReleaseField {
    # Overwrite (or add) a field inside the release block of the sandbox
    # manifest. Passing $null blanks the field out (the build script treats a
    # present-but-blank field the same as a missing one).
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][string]$Key,
        $Value
    )
    $ManifestPath = Join-Path $Sandbox 'packaging\windows\manifest.json'
    $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    $Manifest.release | Add-Member -NotePropertyName $Key -NotePropertyValue $Value -Force
    Save-Manifest -Sandbox $Sandbox -Manifest $Manifest
}

function Remove-ManifestProperty {
    # Remove a top-level property from the sandbox manifest (e.g. the whole
    # release block) to test missing-block rejection.
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        [Parameter(Mandatory)][string]$Key
    )
    $ManifestPath = Join-Path $Sandbox 'packaging\windows\manifest.json'
    $Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
    $null = $Manifest.PSObject.Properties.Remove($Key)
    Save-Manifest -Sandbox $Sandbox -Manifest $Manifest
}

Write-Host '=================================================='
Write-Host "Windows packaging baseline tests"
Write-Host "  build.ps1 under test : $BuildScript"
Write-Host "  manifest template     : $ManifestSource"
Write-Host "  sandbox root          : $SandboxRoot"
Write-Host '  every run happens in a throwaway sandbox; the real repository is never modified'
Write-Host '=================================================='

# ---------------------------------------------------------------------------
# 1. Happy path: -SkipBuild stages artifacts, verifies them, preserves files.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'SkipBuild happy path stages artifacts and preserves pre-existing files' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')

        # A pre-existing file in dist\windows must survive the run untouched.
        $Dist = Join-Path $Root 'dist\windows'
        $null = New-Item -ItemType Directory -Force -Path $Dist
        $Keep = Join-Path $Dist 'keep-me.txt'
        Set-Content -LiteralPath $Keep -Value 'sentinel: must survive' -Encoding ASCII -NoNewline

        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"

        $Cli   = Join-Path $Dist 'nextide-cli.exe'
        $Shell = Join-Path $Dist 'nextide-shell.exe'
        Assert (Test-Path -LiteralPath $Cli) 'nextide-cli.exe was not staged'
        Assert (Test-Path -LiteralPath $Shell) 'nextide-shell.exe was not staged'

        $CliSrc   = Join-Path $Root 'target\release\nextide-cli.exe'
        $ShellSrc = Join-Path $Root 'target\release\nextide-shell.exe'
        Assert ((Get-Sha256Hex $Cli) -eq (Get-Sha256Hex $CliSrc)) 'staged cli SHA-256 differs from source'
        Assert ((Get-Sha256Hex $Shell) -eq (Get-Sha256Hex $ShellSrc)) 'staged shell SHA-256 differs from source'
        Assert ((Get-Item -LiteralPath $Cli).Length -eq (Get-Item -LiteralPath $CliSrc).Length) 'staged cli size differs from source'

        Assert ((Get-Content -Raw -LiteralPath $Keep) -eq 'sentinel: must survive') 'pre-existing file in dist\windows was modified or deleted'
        Assert ($Result.Output -match 'Done\.') 'expected completion banner in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 2. Missing manifest.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing manifest.json fails' -Body {
    $Root = New-Sandbox
    try {
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'Missing manifest') "expected 'Missing manifest' in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 3. Missing release artifact (source file absent).
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing release artifact fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        # Only the first manifest artifact exists; the second is missing.
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe')
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'Missing release artifact') "expected 'Missing release artifact' in output: $($Result.Output)"
        Assert ($Result.Output -match 'nextide-shell\.exe') 'expected the missing file name in output'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 4. Wrong pinned expected_sha256.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'wrong pinned expected_sha256 fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')
        Set-ManifestArtifact -Sandbox $Root -Index 0 -Set @{ expected_sha256 = ('0' * 64) }
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'expected_sha256') "expected 'expected_sha256' mismatch in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 5. Correct pinned expected_sha256 (positive counterpart of test 4).
# ---------------------------------------------------------------------------
Invoke-Test -Name 'correct pinned expected_sha256 passes' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')
        $Hash = Get-Sha256Hex (Join-Path $Root 'target\release\nextide-cli.exe')
        Set-ManifestArtifact -Sandbox $Root -Index 0 -Set @{ expected_sha256 = $Hash }
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 6. Path escape via '..' component.
# ---------------------------------------------------------------------------
Invoke-Test -Name "path escape via '..' component fails" -Body {
    $Root = New-Sandbox -WithManifest
    try {
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')
        Set-ManifestArtifact -Sandbox $Root -Index 0 -Set @{ source = '..\..\outside.txt'; file = 'outside.txt' }
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'must not escape the repo root') "expected escape rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 7. Path escape via rooted absolute path.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'path escape via rooted absolute path fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')
        Set-ManifestArtifact -Sandbox $Root -Index 0 -Set @{ source = 'C:\Windows\System32\notepad.exe'; file = 'notepad.exe' }
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'must be a repo-relative path') "expected rooted-path rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 8. Invalid manifest JSON.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'invalid manifest JSON fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-Content -LiteralPath (Join-Path $Root 'packaging\windows\manifest.json') -Value '{ this is not json' -Encoding ASCII
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'Invalid manifest JSON') "expected 'Invalid manifest JSON' in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 9. Artifact file name differs from source file name.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'artifact file name differs from source fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')
        Set-ManifestArtifact -Sandbox $Root -Index 0 -Set @{ file = 'renamed.exe' }
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'must equal the source file name') "expected leaf-mismatch rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 10. Unsupported manifest schema_version.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'unsupported manifest schema_version fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestField -Sandbox $Root -Key 'schema_version' -Value 2
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'Unsupported manifest schema_version') "expected schema rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 11. Failed release build fails loudly (sandbox workspace has no packages).
# ---------------------------------------------------------------------------
Invoke-Test -Name 'failed release build fails loudly' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        # No target\release binaries and no crates: the sandbox Cargo.toml
        # passes the version-consistency check but the memberless workspace
        # cannot produce the release artifacts. Without -SkipBuild the script
        # must reach the build step and fail there (cargo missing or failing
        # -> non-zero exit; either way the script exits non-zero and stages
        # nothing).
        $Result = Invoke-BuildScript -Sandbox $Root
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'Building release binaries') 'expected the build step to be reached'
        Assert (-not (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe'))) 'no artifact may be staged after a failed build'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 12. Manifest product.version does not match Cargo.toml [workspace.package].
# ---------------------------------------------------------------------------
Invoke-Test -Name 'manifest product.version mismatch with Cargo.toml workspace version fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')
        Write-SandboxCargoToml -Sandbox $Root -Version '9.9.9'
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'does not match Cargo.toml') "expected version-mismatch rejection in output: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe'))) 'no artifact may be staged after a version mismatch'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 13. Cargo.toml missing at the repo root.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing Cargo.toml fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Remove-Item -LiteralPath (Join-Path $Root 'Cargo.toml') -Force
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'Missing Cargo.toml') "expected 'Missing Cargo.toml' in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 14. Cargo.toml present but no [workspace.package] version.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'Cargo.toml without [workspace.package] version fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-Content -LiteralPath (Join-Path $Root 'Cargo.toml') -Value "[workspace]`r`nresolver = `"2`"`r`n" -Encoding ASCII
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match '\[workspace\.package\] version') "expected missing-workspace-version rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 15. Manifest target.architecture does not match the pinned packaging target.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'manifest target.architecture mismatch with pinned packaging target fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestTargetField -Sandbox $Root -Key 'architecture' -Value 'arm64'
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'does not match the pinned packaging target') "expected pinned-target rejection in output: $($Result.Output)"
        Assert ($Result.Output -match 'target\.architecture') "expected 'target.architecture' in output: $($Result.Output)"
        Assert ($Result.Output -match 'arm64') 'expected the offending architecture value in output'
        Assert (-not (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe'))) 'no artifact may be staged after a target mismatch'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 16. Manifest target.triple does not match the pinned packaging target.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'manifest target.triple mismatch with pinned packaging target fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestTargetField -Sandbox $Root -Key 'triple' -Value 'x86_64-pc-windows-gnu'
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'does not match the pinned packaging target') "expected pinned-target rejection in output: $($Result.Output)"
        Assert ($Result.Output -match 'target\.triple') "expected 'target.triple' in output: $($Result.Output)"
        Assert ($Result.Output -match 'x86_64-pc-windows-gnu') 'expected the offending triple value in output'
        Assert (-not (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe'))) 'no artifact may be staged after a target mismatch'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 17. Manifest target.os does not match the pinned packaging target.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'manifest target.os mismatch with pinned packaging target fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestTargetField -Sandbox $Root -Key 'os' -Value 'linux'
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'does not match the pinned packaging target') "expected pinned-target rejection in output: $($Result.Output)"
        Assert ($Result.Output -match 'target\.os') "expected 'target.os' in output: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe'))) 'no artifact may be staged after a target mismatch'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 18. Manifest without the release block entirely.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing release block fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Remove-ManifestProperty -Sandbox $Root -Key 'release'
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'release block') "expected missing-release-block rejection in output: $($Result.Output)"
        Assert (-not (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe'))) 'no artifact may be staged without release metadata'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 19. Release block present but release.license blank.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing release.license fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestReleaseField -Sandbox $Root -Key 'license' -Value $null
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'release\.license') "expected 'release.license' rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 20. Release block present but release.publisher blank.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing release.publisher fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestReleaseField -Sandbox $Root -Key 'publisher' -Value $null
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'release\.publisher') "expected 'release.publisher' rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 21. Release block present but release.release_channel blank.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'missing release.release_channel fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestReleaseField -Sandbox $Root -Key 'release_channel' -Value $null
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'release\.release_channel') "expected 'release.release_channel' rejection in output: $($Result.Output)"
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 22. Release channel that is neither stable nor beta.
# ---------------------------------------------------------------------------
Invoke-Test -Name 'release.release_channel not stable/beta fails' -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestReleaseField -Sandbox $Root -Key 'release_channel' -Value 'alpha'
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -ne 0) "expected non-zero exit, got $($Result.ExitCode)"
        Assert ($Result.Output -match 'not allowed') "expected channel rejection in output: $($Result.Output)"
        Assert ($Result.Output -match 'alpha') 'expected the offending channel value in output'
        Assert (-not (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe'))) 'no artifact may be staged after a channel mismatch'
    }
    finally {
        Remove-Sandbox $Root
    }
}

# ---------------------------------------------------------------------------
# 23. Release channel 'beta' is a valid channel (positive counterpart of 22).
# ---------------------------------------------------------------------------
Invoke-Test -Name "release.release_channel 'beta' passes" -Body {
    $Root = New-Sandbox -WithManifest
    try {
        Set-ManifestReleaseField -Sandbox $Root -Key 'release_channel' -Value 'beta'
        New-FakeBinaries -Sandbox $Root -Names @('nextide-cli.exe', 'nextide-shell.exe')
        $Result = Invoke-BuildScript -Sandbox $Root -SkipBuild
        Assert ($Result.ExitCode -eq 0) "expected exit 0, got $($Result.ExitCode)`n$($Result.Output)"
        Assert ($Result.Output -match 'channel=beta') 'expected the release channel line in output'
        Assert (Test-Path -LiteralPath (Join-Path $Root 'dist\windows\nextide-cli.exe')) 'nextide-cli.exe was not staged'
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
        if ($Leaf -like 'nextide-test-build-*' -and (Test-Path -LiteralPath $S)) {
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
