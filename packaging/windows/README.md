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
| Self-tests for the packaging baseline | Done — `test-build.ps1` (zero external framework, sandboxed, never touches the repo) |
| Installer (WiX / Inno Setup / NSIS) | Not yet — later phase |
| Code signing | Not yet — later phase |
| CI packaging job | Done — `.github/workflows/ci.yml` `packaging-windows` job: runs after the `ci` checks pass, runs this script's self-tests (`test-build.ps1`) as a pre-packaging gate, builds the release binaries, runs `build.ps1` with `-SkipBuild`, uploads `dist/windows` as an artifact |

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
   and SHA-256.

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
  invocation is the pinned release build);
- accesses the network.

Every failure path — missing cargo, failed build, invalid manifest,
`product.version`/Cargo.toml version mismatch, manifest target block mismatch
with the pinned `x86_64-pc-windows-msvc` packaging target, missing or invalid
release block (blank `license`/`publisher` or a `release_channel` other than
`stable`/`beta`), missing artifact, size/hash mismatch — exits non-zero with a
message explaining what was wrong.

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

Covered scenarios (23 tests):

| Scenario | Expected result |
| --- | --- |
| `-SkipBuild` happy path (staging + size/SHA-256 verification + report, sentinel file preserved) | pass |
| Correct pinned `expected_sha256` | pass |
| `release.release_channel` = `beta` (valid channel) | pass |
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

The script exits `0` when every test passes and `1` when any fails. Useful
switches: `-BuildScript <path>` points at another copy of `build.ps1` under
test, `-SandboxRoot <dir>` overrides where sandboxes are created, and
`-KeepSandbox` keeps the sandboxes for inspection instead of deleting them.

Note: the `cargo build --release` *success* path is not exercised by the
sandboxed suite (there is no `Cargo.toml` in a sandbox); it is covered by CI
(`.github/workflows/ci.yml`). The suite does verify that a failing build makes
the script exit non-zero without staging anything.

## Verify

After a successful run, smoke-test the staged binaries from `dist\windows\`:

```powershell
.\dist\windows\nextide-cli.exe            # prints CLI usage/help
.\dist\windows\nextide-cli.exe smoke      # runs the editor-kernel smoke flow
.\dist\windows\nextide-shell.exe          # starts the headless Desktop Shell
```

Note: `cargo test --workspace` and `cargo clippy` are **not** part of this
script — they remain covered by the `ci` job of the CI workflow
(`.github/workflows/ci.yml`), which is unchanged. A second `packaging-windows`
job was added to that workflow: it runs only after the `ci` checks pass
(`needs: ci`, fail fast), first runs `test-build.ps1` itself as a
pre-packaging self-test (a failing self-test stops the run before anything is
built), then builds the release binaries with `cargo build --release`, invokes
this script with `-SkipBuild` to stage and verify them under `dist/windows/`,
and uploads the staged directory as the `nextide-windows-dist` workflow
artifact. That job is what exercises the real-repository
`cargo build --release` success path end to end on CI.
