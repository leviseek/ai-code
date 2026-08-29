# Windows Packaging Baseline

Minimal, safe packaging baseline for NextIDE on Windows. This is a **baseline**:
it builds and stages the release executables so later phases (installer,
signing, CI publish) have something to consume. It intentionally does not
install anything.

## Scope

| Area | Status |
| --- | --- |
| Release build (`cargo build --release`) | Done — performed and verified by the script |
| Manifest-driven artifact list + verification | Done — `manifest.json` is the single source of truth; `build.ps1` validates and enforces it |
| Release metadata validation (`license` / `publisher` / `release_channel`) | Done — required, validated read-only; channel must be `stable` or `beta` |
| Stage CLI + Desktop Shell executables under `dist/windows/` | Done — performed by the script |
| `SHA256SUMS.txt` checksum manifest for staged artifacts | Done — generated and re-verified by the script from the verified staged SHA-256 hashes; ships inside the `dist/windows` CI artifact |
| Consumer-side verification of `SHA256SUMS.txt` | Done — `verify-checksums.ps1` reads the checksum manifest and proves every line parses (`<hash>  <file>`), every named file exists, every hash matches the actual file, and the directory matches the manifest exactly; strictly read-only, exits non-zero on any failure |
| Self-tests for the packaging baseline | Done — `test-build.ps1` (zero external framework, sandboxed, never touches the repo) |
| Self-tests for the checksum verifier | Done — `test-verify-checksums.ps1` (zero external framework, sandboxed, never touches the repo) |
| Self-tests for the CI release job (tag/version rules + release file manifest) | Done — `test-release-sim.ps1` (zero external framework, sandboxed simulation, never touches the repo, no network access, never creates a GitHub Release) |
| Installer (WiX / Inno Setup / NSIS) | Not yet — later phase |
| Code signing | Not yet — later phase |
| CI release job (publish artifacts to GitHub Release) | Done — `.github/workflows/ci.yml` `release` job: on version-tag pushes (`refs/tags/v*`) only, downloads the `packaging-windows` job's channel-suffixed dist + manifest artifacts, re-verifies the downloaded dist with the consumer-side `verify-checksums.ps1` and smoke-tests the downloaded CLI, proves the downloaded manifest is byte-identical to the committed one, asserts the tag is exactly `v<product.version>`, and publishes a GitHub Release with both binaries, the manifest-declared checksum file (`stage.checksum_file`) and `manifest.json` attached (see [Verify](#verify)) |
| CI packaging job | Done — `.github/workflows/ci.yml` `packaging-windows` job: runs after the `ci` checks pass, **explicitly verifies the manifest in all four required areas** (manifest validity, version, target, release metadata — plus the checksum file name) — first via the sandboxed `test-build.ps1` self-test gate (26 cases) and the consumer-side checksum verifier self-test `test-verify-checksums.ps1` (14 cases), then read-only against the committed manifest by `build.ps1 -SkipBuild` before staging — builds the release binaries, stages `dist/windows` (including the generated `SHA256SUMS.txt` checksum manifest, which `build.ps1` writes from the verified staged hashes and re-verifies on the spot), and uploads it as a **channel-suffixed** artifact (`nextide-windows-dist-<channel>`): right after checkout the job reads `release.release_channel` from the committed manifest and uses it as the artifact name suffix, so stable and beta runs can never overwrite each other's artifact. Immediately before the dist upload a dedicated gate runs the repository's consumer-side verifier `verify-checksums.ps1` read-only against the exact `dist/windows` directory about to be uploaded (no dependencies, no network), passing the manifest-declared checksum file name via `-ChecksumFile` — the same value the resolve step read from `stage.checksum_file` right after checkout — so a renamed checksum file is verified under its declared name, not a hardcoded default — every line must parse as `<hash>  <file>`, every named file must exist, every entry's hash must equal the actual file, and the directory must match the manifest exactly — so the checksum manifest that ships is proven correct, not just present. The same resolved channel names a second upload: the committed `packaging/windows/manifest.json` is uploaded as a **channel-suffixed release-metadata artifact** (`nextide-windows-manifest-<channel>`) alongside the binaries, so the validated product identity, pinned target, release metadata (license / publisher / release_channel) and artifact list that produced this release travel with it for later release/publish steps. Immediately before that manifest upload a lightweight gate re-reads the exact file about to be uploaded with PowerShell's built-in `ConvertFrom-Json` (no dependencies, no network, no cargo invocation) and re-asserts the validated identity inline — valid JSON, `schema_version` 1, product name/version, the pinned target block, the release metadata, a non-empty artifact list and a `stage.checksum_file` that is a plain leaf name not colliding with any artifact file, plus a cross-check that the file's `release_channel` equals the channel resolved for the run — so the uploaded manifest is proven valid JSON and consistent with the manifest the pipeline validated |

## Layout

```
packaging/
  windows/
    build.ps1                 # the staging entry point (build + stage + generate)
    verify-checksums.ps1      # consumer-side, strictly read-only verification of SHA256SUMS.txt
    manifest.json             # product metadata, target, artifact list, verification strategy
    test-build.ps1            # self-contained, sandboxed validation tests for build.ps1
    test-verify-checksums.ps1 # self-contained, sandboxed validation tests for verify-checksums.ps1
    test-release-sim.ps1      # self-contained, sandboxed simulation tests for the CI release job
    README.md                 # this file
dist/              # build output, git-ignored (not committed)
  windows/
    nextide-cli.exe
    nextide-shell.exe
    SHA256SUMS.txt  # generated checksum manifest (one '<hash>  <file>' line per staged artifact)
```

`dist/` is generated output and is ignored by git (see `.gitignore`).

## Manifest (`manifest.json`)

`manifest.json` describes what this packaging baseline produces and how it
verifies it. `build.ps1` reads it as the **single source of truth** — the
artifact list is no longer hardcoded in the script.

| Field | Meaning |
| --- | --- |
| `schema_version` | Manifest schema version (currently `1`). |
| `product.name` / `product.version` | Product identity (`NextIDE`, `0.1.0`). `product.version` must equal the `[workspace.package]` version in the root `Cargo.toml`; `build.ps1` enforces the match (see [Version consistency](#version-consistency)). |
| `target.os` / `target.architecture` / `target.triple` | Declared target platform (`windows`, `x86_64`, `x86_64-pc-windows-msvc`). **Enforced**: each field must equal the pinned packaging target this script produces — `os` = `windows`, `architecture` = `x86_64`, `triple` = `x86_64-pc-windows-msvc` — and the triple's architecture component must agree with `architecture` (see [Target consistency](#target-consistency)). |
| `release.license` / `release.publisher` / `release.release_channel` | Release metadata (`MIT`, `leviseek`, `stable`). **Enforced**: `license` and `publisher` must be non-empty, and `release_channel` must be exactly `stable` or `beta` (see [Release metadata](#release-metadata)). |
| `build.profile` / `build.command` / `build.output_dir` | Declared build metadata (informational — the script deliberately always runs the pinned, safe `cargo build --release` from the repo root; it never executes an arbitrary command read from a manifest). |
| `stage.output_dir` | Declared staging location (informational — the script deliberately pins staging to the repo-local `dist/windows` so a tampered manifest can never redirect writes outside the repository). |
| `stage.checksum_file` | Name of the generated SHA-256 checksum manifest written into the staging directory (`SHA256SUMS.txt`). **Enforced**: required, must be a plain leaf name (no path separators, no `.`/`..`) and must not collide with any declared artifact file name — the checksum manifest is generated, never staged (see [Checksum manifest](#checksum-manifest-sha256sumstxt)). |
| `artifacts[]` | Each entry: `id`, `file` (staged file name), `source` (repo-relative path of the release binary), optional `description`, optional `expected_sha256`. `file` must equal the source file name. |
| `verification.strategy` / `verification.algorithm` | Verification strategy (`exists-and-sha256` with SHA-256). |
| `verification.rules` | `source_must_exist`, `staged_must_exist`, `staged_must_match_source` (all default to enforce when absent) and `report` (which fields to print: `size_bytes`, `sha256`). |

Every artifact `source` must be a repo-relative path — rooted paths and `..`
segments are rejected by the script.

### Pinning hashes

An artifact may declare `expected_sha256`. When present, the staged copy's
SHA-256 must match it **exactly**, otherwise the script fails. The committed
manifest leaves it unset because release binaries change with every build;
pin it in a release flow when you want bit-for-bit reproducibility.

### Checksum manifest (`SHA256SUMS.txt`)

After every staged artifact is verified, `build.ps1` writes a checksum
manifest into the staging directory (named by `stage.checksum_file`,
committed value `SHA256SUMS.txt`):

- one line per staged artifact, in manifest artifact order;
- each line is `<64-hex-sha256>  <file>` — the uppercase/lowercase SHA-256 as
  printed by `Get-FileHash`, two spaces, then the staged file name;
- LF line endings — the GNU `sha256sum -c` format, so the file can be verified
  with `sha256sum -c SHA256SUMS.txt` on any platform, or parsed by
  PowerShell/`certutil` consumers.

The manifest is built **from the verified staged hashes** — never re-hashed —
so it can only contain hashes that already proved to match the staged files.
The script then re-verifies the written file by parsing it back: every line
must be in `<hash>  <file>` format, there must be exactly one entry per staged
artifact, and every entry's hash must equal the verified staged hash — a
checksum manifest that does not parse back or does not match is a hard error,
so the file that ships is proven consistent, not just present. The manifest
does **not** list itself (it covers the staged binaries only), and it is
regenerated on every run.

The file name is validated read-only **before** anything is built or staged:
`stage.checksum_file` is required, must be a plain leaf name (no `/` or `\`
separators, no `.`/`..`) and must not collide with any declared artifact
`file` — the checksum manifest is generated, never staged from a build output.
CI's `packaging-windows` job also re-checks the exact checksum manifest
(named by `stage.checksum_file`, committed value `SHA256SUMS.txt`, resolved
from the committed manifest right after checkout and passed to the verifier
as `-ChecksumFile`) about to be uploaded against the actual `dist/windows`
contents immediately before
the dist upload (every staged artifact must have exactly one entry, every
entry's hash must equal the real file), so the checksum manifest that ships in
the `nextide-windows-dist-<channel>` artifact is proven correct. That
re-check now runs the repository's own consumer-side verifier
(`verify-checksums.ps1`, see below), so the exact script dist consumers use is
exercised on every packaging run.

### Consumer verification (`verify-checksums.ps1`)

`verify-checksums.ps1` is the **consumer** counterpart to the generation done
by `build.ps1`: it verifies a staged release directory against the checksum
manifest that ships with it — no rebuild, no re-staging, no cargo invocation,
no external tools. By default it reads `dist/windows/SHA256SUMS.txt` (the
committed `stage.checksum_file` value); `-DistDir` points it at any extracted
release directory and `-ChecksumFile` at a custom manifest name, so a
downloaded artifact can be verified exactly like the staged directory:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\verify-checksums.ps1
powershell -ExecutionPolicy Bypass -File packaging\windows\verify-checksums.ps1 -DistDir C:\downloads\nextide-dist
```

The script proves, in order (any failure exits non-zero with a message):

1. the manifest exists and is non-empty;
2. every line parses as `<64-hex-sha256>  <file>` — exactly 64 hex
   characters, exactly two spaces (the GNU `sha256sum -c` compatible format
   `build.ps1` writes), then a plain leaf file name (no path separators, no
   `.`/`..`); blank lines are ignored and duplicate entries are rejected;
3. every named file exists in the staging directory;
4. every named file's actual SHA-256 equals the manifest hash
   (case-insensitive — `Get-FileHash` prints uppercase, GNU tools lowercase,
   both accepted);
5. the directory matches the manifest exactly: every file in the directory
   (except the checksum manifest itself, which never lists itself) has
   exactly one entry, and no entry names a file that is absent.

The script is **strictly read-only** — it contains no `Set-Content`,
`New-Item`, `Copy-Item` or `Remove-Item` anywhere; it only reads the manifest,
lists the directory and hashes files with the built-in `Get-FileHash` (no
dependencies, no network). It never executes the files it verifies and never
touches the registry. `test-verify-checksums.ps1` proves all of this in a
sandbox (see [Testing](#testing)), including a read-only snapshot assertion
that a successful run changes nothing in the verified directory. CI's
`packaging-windows` job runs the same verifier against the exact
`dist/windows` contents immediately before the dist artifact upload, so the
consumer path is exercised on every run.

### Version consistency

`product.version` is not free-form: it must equal the `[workspace.package]`
version declared in the root `Cargo.toml`, which is the single source of truth
for the workspace version (every member crate inherits it via
`version.workspace = true`). `build.ps1` enforces the match by parsing
`Cargo.toml` **read-only as plain text** — a line scan that tracks TOML
sections and reads `version = "x.y.z"` inside `[workspace.package]`. No
`cargo` invocation and no TOML dependency is involved, and the check runs
before anything is built or staged, so a stale manifest can never ship a
release that claims a version the workspace does not declare. A missing
`Cargo.toml`, a missing `[workspace.package]` version, or a mismatch with
`product.version` is a hard error (non-zero exit).

### Release metadata

The `release` block is not free-form either: it carries the release metadata
and `build.ps1` validates it **read-only** before anything is built or staged.
`release.license` and `release.publisher` must be non-empty (the committed
manifest declares `MIT` — mirroring the SPDX license of every member crate —
and `leviseek`, the publishing identity), and `release.release_channel` must be
exactly `stable` or `beta` — `beta` is a valid channel, anything else (for
example `alpha` or `nightly`) is a hard error, so a release can never be
labelled with an undeclared channel. A missing `release` block or a
missing/blank field is also a hard error (non-zero exit). This block is
metadata only — the script performs **no signing and no installation**; the
channel is a label the script validates, not a behavior it switches.

### Target consistency

`target.os` / `target.architecture` / `target.triple` are not free-form
either: each must match the **pinned packaging target** this script produces —
`windows` / `x86_64` / `x86_64-pc-windows-msvc` (64-bit x86 Windows with the
MSVC toolchain). `build.ps1` compares all three against the pinned values
before anything is built or staged and fails loudly on any mismatch, so a
stale or cross-arch manifest (wrong os, wrong architecture such as `arm64`,
wrong triple such as `x86_64-pc-windows-gnu`, or an architecture/triple pair
that disagrees with each other) can never stage an artifact set for the wrong
platform. The release build itself still runs with the host stable Rust
toolchain (`cargo build --release`) — the pins guarantee the manifest agrees
with the target the artifacts are actually built for.

## Prerequisites

- Windows 10/11 (the project targets Windows).
- A stable Rust toolchain with `cargo` on `PATH` (same toolchain CI installs:
  stable with `rustfmt` and `clippy`).
- PowerShell 5.1+ (any edition — the script uses only core cmdlets).

## Usage

From the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1
```

The script:

1. loads and validates `manifest.json` — an invalid or incomplete manifest is
   a hard error (non-zero exit);
2. checks that `product.version` equals the `[workspace.package]` version in
   the root `Cargo.toml` (read-only text parse of `Cargo.toml` — no `cargo`
   invocation, no dependencies) and fails before building or staging on any
   mismatch;
3. checks that the manifest target block matches the pinned packaging target
   `x86_64-pc-windows-msvc` (`target.os` = `windows`, `target.architecture` =
   `x86_64`, `target.triple` = `x86_64-pc-windows-msvc`, mutually consistent)
   and fails before building or staging on any mismatch;
4. checks the release block read-only — `release.license` and
   `release.publisher` must be non-empty and `release.release_channel` must be
   exactly `stable` or `beta` — and fails before building or staging on any
   missing field or disallowed channel;
5. runs `cargo build --release` from the repository root and **fails with a
   non-zero exit code** if the build fails;
6. checks that every artifact listed in the manifest exists in
   `target\release\` — an unexpected missing artifact is a hard error, so a
   stale `dist` can never look like a successful build;
7. creates `dist\windows\` if needed and copies the executables there
   (overwriting in place);
8. verifies the staged copies exist, match the release binaries by size and
   SHA-256, honor any pinned `expected_sha256`, and prints each file's size
   and SHA-256;
9. writes the `SHA256SUMS.txt` checksum manifest from the verified staged
   hashes (one `<hash>  <file>` line per staged artifact, in manifest order,
   LF line endings — GNU `sha256sum -c` compatible), parses it back and
   proves every line matches the staged SHA-256, then prints the result.

To re-stage already-built executables without rebuilding:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 -SkipBuild
```

## Safety contract

The script is deliberately boring. It never:

- deletes or cleans directories (it overwrites individual files with
  `Copy-Item -Force`; it never runs `Remove-Item`);
- touches the registry, the Start Menu, or any system location;
- signs, installs, or registers anything;
- reads or writes anything outside the repository (the version check reads
  `Cargo.toml` at the repo root as plain text only; the only `cargo`
  invocation is the pinned release build; the only files written are the
  staged binaries and the generated `SHA256SUMS.txt` checksum manifest, both
  under the repo-local `dist\windows\`);
- accesses the network.

Every failure path — missing cargo, failed build, invalid manifest,
`product.version`/Cargo.toml version mismatch, manifest target block mismatch
with the pinned `x86_64-pc-windows-msvc` packaging target, missing or invalid
release block (blank `license`/`publisher` or a `release_channel` other than
`stable`/`beta`), missing or invalid `stage.checksum_file` (blank, path
separator, or colliding with an artifact file name), missing artifact,
size/hash mismatch, or a checksum manifest that fails to parse back or
disagrees with a verified staged hash — exits non-zero with a message
explaining what was wrong.

## Testing

`test-build.ps1` validates `build.ps1` end-to-end with plain PowerShell
assertions — **no external test framework** — and without ever touching the
real repository: every test runs `build.ps1` against a throwaway sandbox copy
under the sandbox root (default `%TEMP%`), then deletes its own sandbox again.
Each sandbox also gets a minimal root `Cargo.toml` whose
`[workspace.package]` version is read from the manifest template, so the
version-consistency check runs on every test (the mismatch cases overwrite
that `Cargo.toml`). The only thing the suite ever deletes is those sandboxes;
the repository's `dist\windows\` is never written to, and one test plants a
sentinel file in the sandbox's staging directory to prove the script preserves
pre-existing files (it only overwrites, never deletes). The suite runs under
both Windows PowerShell 5.1 and PowerShell 7.

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\test-build.ps1
```

Covered scenarios (26 tests):

| Scenario | Expected result |
| --- | --- |
| `-SkipBuild` happy path (staging + size/SHA-256 verification + report + `SHA256SUMS.txt` checksum manifest written and matching the staged hashes, sentinel file preserved) | pass |
| Correct pinned `expected_sha256` | pass |
| `release.release_channel` = `beta` (valid channel) | pass |
| Custom `stage.checksum_file` name (`CHECKSUMS.txt`) | pass |
| Missing `manifest.json` | fail |
| Missing release artifact (source absent) | fail |
| Wrong pinned `expected_sha256` | fail |
| Path escape via `..` component | fail |
| Path escape via rooted absolute path | fail |
| Invalid manifest JSON | fail |
| `file` differs from the source file name | fail |
| Unsupported `schema_version` | fail |
| Failed release build (memberless workspace, no `-SkipBuild`) | fail loudly, nothing staged |
| `product.version` mismatch with Cargo.toml `[workspace.package]` version | fail before build/staging |
| Missing root `Cargo.toml` | fail |
| `Cargo.toml` without `[workspace.package]` version | fail |
| `target.architecture` mismatch with pinned packaging target (`x86_64`) | fail before build/staging, nothing staged |
| `target.triple` mismatch with pinned packaging target (`x86_64-pc-windows-msvc`) | fail before build/staging, nothing staged |
| `target.os` mismatch with pinned packaging target (`windows`) | fail before build/staging, nothing staged |
| Missing `release` block entirely | fail before build/staging, nothing staged |
| Missing/blank `release.license` | fail before build/staging |
| Missing/blank `release.publisher` | fail before build/staging |
| Missing/blank `release.release_channel` | fail before build/staging |
| `release.release_channel` not `stable`/`beta` (e.g. `alpha`) | fail before build/staging, nothing staged |
| `stage.checksum_file` with a path separator | fail before build/staging, nothing staged |
| `stage.checksum_file` colliding with an artifact file name | fail before build/staging, nothing staged |

The script exits `0` when every test passes and `1` when any fails. Useful
switches: `-BuildScript <path>` points at another copy of `build.ps1` under
test, `-SandboxRoot <dir>` overrides where sandboxes are created, and
`-KeepSandbox` keeps the sandboxes for inspection instead of deleting them.

`test-verify-checksums.ps1` validates the consumer verifier the same way —
zero external framework, every case runs `verify-checksums.ps1` in a child
process against a throwaway sandbox under the sandbox root (default `%TEMP%`),
never touching the real repository, and the only thing the suite ever deletes
is its own sandboxes:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\test-verify-checksums.ps1
```

Covered scenarios (14 tests):

| Scenario | Expected result |
| --- | --- |
| Happy path (correct manifest, two files) | pass |
| Bad hash (one entry's hash is wrong) | fail |
| Missing file (entry names an absent file) | fail |
| Bad format (line not `<hash>  <file>`) | fail |
| Missing `SHA256SUMS.txt` | fail |
| Empty `SHA256SUMS.txt` | fail |
| Entry name with `..` path escape | fail |
| Entry name with a path separator | fail |
| Extra file in the directory without an entry | fail |
| Duplicate entry for the same file | fail |
| Lowercase hashes (GNU-style) accepted | pass |
| CRLF line endings accepted | pass |
| Custom `-ChecksumFile` name honored | pass |
| Run is strictly read-only (sandbox snapshot unchanged) | pass |

The suite runs under both Windows PowerShell 5.1 and PowerShell 7 and exits
`0` when every test passes, `1` otherwise. Useful switches:
`-VerifyScript <path>` points at another copy of `verify-checksums.ps1` under
test, `-SandboxRoot <dir>` overrides where sandboxes are created, and
`-KeepSandbox` keeps them for inspection.

`test-release-sim.ps1` validates the CI `release` job's own gate rules the same
way — zero external framework, every case runs against a throwaway sandbox
under the sandbox root (default `%TEMP%`), never touching the real repository,
**no network access and no GitHub Release ever created** (one case audits the
script's own source for network/release-creation command invocations, with the
pattern strings assembled from fragments so the audit cannot match its own
literals). The release job's rules are inline pwsh in the workflow, so the
suite implements them as plain functions and proves them:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\test-release-sim.ps1
```

Covered scenarios (26 tests):

| Scenario | Expected result |
| --- | --- |
| Tag/version resolution: committed manifest + matching `v<version>` tag | resolves (channel + version + checksum file) |
| Tag/version resolution: `beta` channel (valid channel) | resolves |
| Tag/version resolution: non-v tag (`main`) | rejected |
| Tag/version resolution: malformed semver tag (`v0.1`, `v0.1.0.1`, trailing space) | rejected |
| Tag/version resolution: pre-release tag (`v0.1.0-beta`) | rejected |
| Tag/version resolution: tag disagreeing with `product.version` (`v9.9.9`; `v01.0.0` passes the shape but fails the version link) | rejected |
| Tag/version resolution: invalid release channel (`alpha`) | rejected |
| Tag/version resolution: blank `product.version` | rejected |
| Tag/version resolution: missing `release` block (channel rule fires) | rejected |
| Tag/version resolution: invalid manifest JSON | rejected |
| Tag/version resolution: missing manifest | rejected |
| Tag/version resolution: blank `stage.checksum_file` (missing `stage` block or blank value) | rejected |
| Release file manifest: complete consistent release (both binaries + matching checksum manifest under the declared `stage.checksum_file` name + byte-identical `manifest.json`; consumer verifier runs on the sandbox dist with `-ChecksumFile`) | pass |
| Release file manifest: missing `nextide-cli.exe` | fail |
| Release file manifest: missing `nextide-shell.exe` | fail |
| Release file manifest: missing checksum manifest (the `stage.checksum_file` name) | fail |
| Release file manifest: missing `release-metadata/manifest.json` | fail |
| Release file manifest: tampered binary (consumer verifier hash mismatch) | fail |
| Release file manifest: extra unlisted file in `dist` (exact-match contract) | fail |
| Release file manifest: checksum entries not matching the manifest artifact list (even when every listed file exists and matches — the artifact-list cross-check, not the verifier, catches it) | fail |
| Release file manifest: metadata manifest not byte-identical to the committed one (SHA-256 compare) | fail |
| Release file manifest: zero-byte CLI binary (smoke-gate simulation: the checksum contract still holds, the non-empty assertion closes the gap) | fail |
| Release file manifest: renamed `stage.checksum_file` (`CHECKSUMS.txt`) honored by the checklist — attachment list, consumer verifier and entry-set cross-check all resolve the declared name; only the byte-identity contract rejects the modified metadata manifest | fail (byte-identity only) |
| Publish attachment naming: the attachment list is built from the resolved `stage.checksum_file` — `dist/windows/<checksum file>` for the checksum entry, never a hardcoded default; a renamed declaration changes the list accordingly | pass |
| Publish attachment naming: a renamed `stage.checksum_file` in the committed-style manifest flows through resolve → verify → checklist → attachment list coherently (the legitimate-rename happy path; the metadata manifest stays byte-identical and every gate passes) | pass |
| Self-audit: no network or release-creation commands in the script source | pass |

The four files the publish step attaches (`nextide-cli.exe`,
`nextide-shell.exe`, the checksum manifest named by `stage.checksum_file` and
`release-metadata/manifest.json`) are
checked present, non-empty, checksum-consistent, artifact-list-consistent and
byte-identical to the committed manifest — the same checklist the `release`
job enforces before publishing (see [Verify](#verify), "A third `release`
job"). The checksum file name is never hardcoded: the suite resolves it from
the golden manifest's `stage.checksum_file`, exactly like the workflow's
resolve step, so a renamed checksum file keeps every test consistent.

Note: the `cargo build --release` *success* path is not exercised by the
`test-build.ps1` sandboxed suite above. Every build-suite sandbox does get a
minimal root `Cargo.toml` (a
memberless workspace whose `[workspace.package]` version comes from the
manifest template, so the version-consistency check runs on every test), but
that workspace has no member crates and can therefore never produce the
release binaries — only the happy-path staging of fake binaries is sandboxed.
The real build is covered by CI (`.github/workflows/ci.yml`), which also
smoke-tests the staged `nextide-cli.exe` before uploading it. The suite does
verify that a failing build makes the script exit non-zero without staging
anything.

## Verify

After a successful run, smoke-test the staged binaries from `dist\windows\`:

```powershell
.\dist\windows\nextide-cli.exe            # prints CLI usage/help
.\dist\windows\nextide-cli.exe smoke      # runs the editor-kernel smoke flow
.\dist\windows\nextide-shell.exe          # starts the headless Desktop Shell
```

Verify the checksum manifest against the staged files:

```powershell
# The repository's consumer verifier (strictly read-only; exits non-zero on
# any failure — line format, file existence and SHA-256 match):
powershell -ExecutionPolicy Bypass -File packaging\windows\verify-checksums.ps1

# Git Bash / WSL / any GNU coreutils sha256sum:
cd dist\windows && sha256sum -c SHA256SUMS.txt
```

End users verifying a GitHub Release download (not a local build) should
follow [Verifying a GitHub Release download (end user)](#verifying-a-github-release-download-end-user).

Note: `cargo test --workspace` and `cargo clippy` are **not** part of this
script — they remain covered by the `ci` job of the CI workflow
(`.github/workflows/ci.yml`), which is unchanged. A second `packaging-windows`
job was added to that workflow: it runs only after the `ci` checks pass
(`needs: ci`, fail fast) and **explicitly verifies the manifest in all four
required areas** — manifest validity, `product.version` vs the Cargo workspace
version, the target block vs the pinned `x86_64-pc-windows-msvc` packaging
target, and the release metadata (`license` / `publisher` / `release_channel`)
— plus the `stage.checksum_file` checksum manifest name — twice, with no
duplicated logic. First it runs `test-build.ps1` itself as a
pre-packaging self-test gate (a failing self-test stops the run before
anything is built), which proves the script rejects every violation through
its 26 sandboxed cases; alongside it a second gate runs
`test-verify-checksums.ps1`, the sandboxed self-test of the consumer-side
checksum verifier (14 cases, including a read-only snapshot proof that a
successful run changes nothing in the verified directory). Then it builds the release binaries with
`cargo build --release`, invokes this script with `-SkipBuild` to stage and
verify them under `dist/windows/` — that run re-applies the same read-only
manifest/version/target/metadata/checksum-file validation to the committed
`manifest.json` and the real `Cargo.toml` before staging anything, and writes
the `SHA256SUMS.txt` checksum manifest from the verified staged hashes (then
re-verifies it by parsing it back) — and uploads the staged directory as the
`nextide-windows-dist-<channel>` workflow artifact, where
`<channel>` is `release.release_channel` read from the committed manifest by a
dedicated step right after checkout (read-only local parse, no network, same
`stable`/`beta` rule `build.ps1` enforces, so an undeclared channel fails the
run before the self-test gate, the build, and staging). The channel-suffixed
name guarantees stable and beta runs produce distinct artifacts and can never
overwrite each other in the Actions artifact store. Immediately before the
dist upload a dedicated gate runs the repository's consumer-side verifier
`verify-checksums.ps1` read-only against the exact `dist/windows` directory
about to be uploaded (no dependencies, no network): the file must parse as
`<hash>  <file>` lines, every named file must exist, every entry's hash must
equal the actual SHA-256 of the file, and the directory must match the
manifest exactly (no missing, corrupt, extra or duplicate entries) — so the
checksum manifest that ships is proven correct, not just present. The same resolved channel
also names a second upload: the committed `packaging/windows/manifest.json`
is uploaded as a **release-metadata artifact** (`nextide-windows-manifest-
<channel>`) alongside the binaries — the exact single source of truth the
pipeline consumed and validated (product identity, pinned target, release
metadata `license` / `publisher` / `release_channel`, artifact list,
`stage.checksum_file`), so later
release/publish steps can attach it to the release without re-deriving
anything, and stable/beta runs keep their release metadata separate too.
Before the upload the job
also smoke-tests the staged CLI binary itself (`.\dist\windows\nextide-cli.exe
smoke` — the same flow listed above, panicking on any failure), so the
artifact that ships is proven to run, not just to hash-match its source.
Immediately before the release-metadata upload the job then re-reads the
committed `packaging/windows/manifest.json` — the exact file about to be
uploaded — with PowerShell's built-in `ConvertFrom-Json` and re-asserts the
validated identity inline: the file must parse as valid JSON (a missing,
corrupt or invalid-JSON file fails the run before any upload), `schema_version`
must be 1, `product.name`/`product.version` must be present, the target block
must equal the pinned `windows` / `x86_64` / `x86_64-pc-windows-msvc` packaging
target, the release block must carry non-empty `license`/`publisher` with a
`release_channel` of exactly `stable` or `beta`, `stage.checksum_file` must be
a plain leaf name that does not collide with any artifact file, the artifact
list must be
non-empty, and the file's `release_channel` must equal the channel this run
resolved right after checkout (so the uploaded manifest is proven consistent
with the manifest the pipeline validated and with the channel embedded in the
artifact name). No dependencies and no network — the step only reads the
committed file. That
job is what exercises the real-repository `cargo build --release` success path
end to end on CI.

A third `release` job completes the chain on version-tag pushes (`refs/tags/v*`)
only (it never runs for branches or pull requests, and it runs only after
`packaging-windows`, via `needs:`). Right after checkout it first runs the
sandboxed release-job simulation gate (`packaging/windows/test-release-sim.ps1`,
26 cases — see [Testing](#testing)), which proves the tag/version rejection
rules, the four-file release manifest and the publish step's attachment
naming against throwaway `%TEMP%` sandboxes
(no network access, no GitHub Release ever created, the repository never
touched), so a failing gate stops the run before anything is downloaded or
published. It then downloads the same run's
`nextide-windows-dist-<channel>` and `nextide-windows-manifest-<channel>`
artifacts and, with no rebuild, no re-staging and no recomputed checksums:

1. asserts the pushed tag is exactly `v<product.version>` from the committed
   manifest (read-only `ConvertFrom-Json`, same `stable`/`beta` rule) — the
   final consistency link of the chain, since `product.version` was already
   proven equal to the `[workspace.package]` Cargo version by `build.ps1`;
2. proves the downloaded `manifest.json` is byte-identical to the committed
   manifest the pipeline validated (SHA-256 compare);
3. re-verifies the downloaded `dist/windows` with the repository's
   consumer-side verifier `verify-checksums.ps1 -DistDir dist\windows
   -ChecksumFile dist\windows\<stage.checksum_file>` — the exact consumer
   path, with the checksum file name resolved from the committed manifest
   (never hardcoded) — so the bytes about to be published match their
   checksum manifest exactly;
4. smoke-tests the downloaded `nextide-cli.exe` (`smoke`, panics on failure);
5. publishes the GitHub Release with `gh release create` (preinstalled on the
   runner), attaching `nextide-cli.exe`, `nextide-shell.exe`, the
   manifest-declared checksum file (`stage.checksum_file`) and `manifest.json`,
   with notes naming the
   version/channel and the checksum verification commands. A release for an
   existing tag fails loudly (no silent overwrite). The job's token is
   scoped to `contents: write` (create the release) + `actions: read`
   (download the artifacts) only.

To publish a release: push a `v<version>` tag matching the manifest version —
`git tag v0.1.0 && git push origin v0.1.0` — which triggers the full pipeline
(ci checks → packaging → release).

## Verifying a GitHub Release download (end user)

The `release` job attaches four assets to each GitHub Release:
`nextide-cli.exe`, `nextide-shell.exe`, the manifest-declared checksum file
(`stage.checksum_file`, committed value `SHA256SUMS.txt`) and `manifest.json`
(release metadata). An end user verifies a download in two steps — checksums
first, then a CLI smoke — the same two gates CI runs on the exact bytes it
publishes. Any verification failure means: do **not** run the binaries.

### Release assets and the checksum file name consistency

Each Release page's "Assets" section lists exactly four files, with two
distinct roles — two are checksum-covered binaries, two are not:

| Asset | Role | Covered by the checksum manifest? |
| --- | --- | --- |
| `nextide-cli.exe` | CLI binary | Yes — has a `<hash>  <file>` entry |
| `nextide-shell.exe` | Desktop Shell binary | Yes — has a `<hash>  <file>` entry |
| `<checksum file>` (committed value `SHA256SUMS.txt`) | The SHA-256 checksum manifest listing the two binaries | No — it is the manifest itself and never lists itself |
| `manifest.json` | Release metadata (product identity, pinned target, license, publisher, channel, artifact list) | **No** — release metadata, not a release binary |

Two consistency rules follow, and both guard the same trap: the four assets
are *not* interchangeable files.

1. **The checksum asset's file name is exactly `stage.checksum_file` from the
   committed manifest — never an arbitrary name.** The name on the Release
   page, the file `build.ps1` generates inside `dist/windows`, the name
   `verify-checksums.ps1` reads by default, and the name the release notes
   reference in `sha256sum -c <file>` are one and the same value (committed
   value `SHA256SUMS.txt`). A release whose checksum asset was renamed only on
   the Release page — without renaming `stage.checksum_file` in
   `packaging/windows/manifest.json` — is inconsistent by construction: verify
   against the name shown on the page, and treat the release as broken if that
   name does not equal the manifest-declared one.

2. **`manifest.json` is release metadata, not a checksum-covered file — never
   put it in the checksum directory.** The checksum manifest covers exactly the
   two binaries. The consumer verifier (and GNU `sha256sum -c`) enforce an
   exact-match contract: any file in the verified folder that has no entry is
   rejected. `manifest.json` has no entry by design, so a folder holding all
   four Release assets can **never** verify — the rejection is the contract
   working, not a broken download. Keep `manifest.json` outside the verified
   folder (next to it is fine); it is metadata *about* the release, not
   something the checksum manifest covers.

### 1. Download

Create a clean folder and put **only** the two binaries and the checksum file
in it. `manifest.json` is release metadata and is *not* covered by the checksum
manifest — the verifier's exact-match rule rejects any file that has no entry,
so keep it out of the verified folder (next to it is fine; see
[Release assets and the checksum file name consistency](#release-assets-and-the-checksum-file-name-consistency)
above for why a folder with all four assets in it can never verify):

```powershell
# PowerShell — set $Tag to the tag of the release you downloaded:
$Tag  = 'v0.1.0'
$Dest = '.\nextide-download'
New-Item -ItemType Directory -Force $Dest | Out-Null
$Base = "https://github.com/leviseek/ai-code/releases/download/$Tag"
Invoke-WebRequest -Uri "$Base/nextide-cli.exe"   -OutFile "$Dest\nextide-cli.exe"
Invoke-WebRequest -Uri "$Base/nextide-shell.exe" -OutFile "$Dest\nextide-shell.exe"
Invoke-WebRequest -Uri "$Base/SHA256SUMS.txt"    -OutFile "$Dest\SHA256SUMS.txt"
```

(Downloading the three assets from the release page's "Assets" section with a
browser is equivalent — just keep `manifest.json` outside `$Dest`.)

### 2. Verify the checksums

The consumer verifier ships in the repository, not on the Release page — fetch
it from the **same tag** and keep it **outside** `$Dest` (the exact-match rule
rejects any file the checksum manifest does not list, the script itself
included):

```powershell
Invoke-WebRequest -Uri "https://raw.githubusercontent.com/leviseek/ai-code/$Tag/packaging/windows/verify-checksums.ps1" -OutFile ".\verify-checksums.ps1"
powershell -ExecutionPolicy Bypass -File .\verify-checksums.ps1 -DistDir $Dest
```

Exit code `0` — every file printed as `ok <file>`, ending with
`==> OK. Checksum manifest verified against the staging directory (read-only).`
— proves every line parses, every named file exists, every SHA-256 matches the
manifest, and the folder contains exactly the listed files. Any failure prints
the reason and exits non-zero. If a release renames its checksum file (a
different `stage.checksum_file`), pass the name explicitly:
`-ChecksumFile "$Dest\<that-name>"`.

In Git Bash / WSL the same check works with GNU coreutils:

```bash
cd nextide-download && sha256sum -c SHA256SUMS.txt
```

### 3. Smoke-test the CLI

```powershell
.\nextide-download\nextide-cli.exe        # prints usage/help
.\nextide-download\nextide-cli.exe smoke  # edit → undo → redo, asserts the document text
```

`smoke` applies an edit, undoes and redoes it, asserts the document text and
prints `status: ok` on success; it panics on any failure, so a non-zero exit
means the binary is not trustworthy — this is the same flow the CI `release`
job runs against the exact bytes it publishes.
