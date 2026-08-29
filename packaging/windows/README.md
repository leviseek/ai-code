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
| Self-tests for the packaging baseline | Done — `test-build.ps1` (zero external framework, sandboxed, never touches the repo) |
| Installer (WiX / Inno Setup / NSIS) | Not yet — later phase |
| Code signing | Not yet — later phase |
| CI packaging job | Done — `.github/workflows/ci.yml` `packaging-windows` job: runs after the `ci` checks pass, **explicitly verifies the manifest in all four required areas** (manifest validity, version, target, release metadata — plus the checksum file name) — first via the sandboxed `test-build.ps1` self-test gate (26 cases), then read-only against the committed manifest by `build.ps1 -SkipBuild` before staging — builds the release binaries, stages `dist/windows` (including the generated `SHA256SUMS.txt` checksum manifest, which `build.ps1` writes from the verified staged hashes and re-verifies on the spot), and uploads it as a **channel-suffixed** artifact (`nextide-windows-dist-<channel>`): right after checkout the job reads `release.release_channel` from the committed manifest and uses it as the artifact name suffix, so stable and beta runs can never overwrite each other's artifact. Immediately before the dist upload a dedicated gate re-checks the exact `SHA256SUMS.txt` about to be uploaded with PowerShell's built-in `Get-FileHash` (no dependencies, no network) — every staged artifact must have exactly one `<hash>  <file>` entry and every entry's hash must equal the actual file — so the checksum manifest that ships is proven correct, not just present. The same resolved channel names a second upload: the committed `packaging/windows/manifest.json` is uploaded as a **channel-suffixed release-metadata artifact** (`nextide-windows-manifest-<channel>`) alongside the binaries, so the validated product identity, pinned target, release metadata (license / publisher / release_channel) and artifact list that produced this release travel with it for later release/publish steps. Immediately before that manifest upload a lightweight gate re-reads the exact file about to be uploaded with PowerShell's built-in `ConvertFrom-Json` (no dependencies, no network, no cargo invocation) and re-asserts the validated identity inline — valid JSON, `schema_version` 1, product name/version, the pinned target block, the release metadata, a non-empty artifact list and a `stage.checksum_file` that is a plain leaf name not colliding with any artifact file, plus a cross-check that the file's `release_channel` equals the channel resolved for the run — so the uploaded manifest is proven valid JSON and consistent with the manifest the pipeline validated |

## Layout

```
packaging/
  windows/
    build.ps1       # the only entry point
    manifest.json   # product metadata, target, artifact list, verification strategy
    test-build.ps1  # self-contained, sandboxed validation tests for build.ps1
    README.md       # this file
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
CI's `packaging-windows` job also re-checks the exact `SHA256SUMS.txt` about
to be uploaded against the actual `dist/windows` contents immediately before
the dist upload (every staged artifact must have exactly one entry, every
entry's hash must equal the real file), so the checksum manifest that ships in
the `nextide-windows-dist-<channel>` artifact is proven correct.

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

Note: the `cargo build --release` *success* path is not exercised by the
sandboxed suite. Every sandbox does get a minimal root `Cargo.toml` (a
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
# Git Bash / WSL / any GNU coreutils sha256sum:
cd dist\windows && sha256sum -c SHA256SUMS.txt

# PowerShell (no external tools — parse and compare every line):
Get-Content SHA256SUMS.txt | ForEach-Object {
  $h, $f = $_ -split '  ', 2
  if ((Get-FileHash $f -Algorithm SHA256).Hash -ne $h.ToUpperInvariant()) { throw "hash mismatch: $f" }
}
```

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
its 26 sandboxed cases. Then it builds the release binaries with
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
dist upload a dedicated gate re-checks the exact `SHA256SUMS.txt` about to be
uploaded with PowerShell's built-in `Get-FileHash` (no dependencies, no
network): the file must parse as `<hash>  <file>` lines, every staged artifact
must have exactly one entry, and every entry's hash must equal the actual
SHA-256 of the file in `dist/windows` — so the checksum manifest that ships is
proven correct, not just present. The same resolved channel
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
