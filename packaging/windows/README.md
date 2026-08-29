# Windows Packaging Baseline

Minimal, safe packaging baseline for NextIDE on Windows. This is a **baseline**:
it builds and stages the release executables so later phases (installer,
signing, CI publish) have something to consume. It intentionally does not
install anything.

## Scope

| Area | Status |
| --- | --- |
| Release build (`cargo build --release`) | Done — performed and verified by the script |
| Stage CLI + Desktop Shell executables under `dist/windows/` | Done — performed by the script |
| Installer (WiX / Inno Setup / NSIS) | Not yet — later phase |
| Code signing | Not yet — later phase |
| CI packaging job | Not yet — later phase |

## Layout

```
packaging/
  windows/
    build.ps1   # the only entry point
    README.md   # this file
dist/           # build output, git-ignored (not committed)
  windows/
    nextide-cli.exe
    nextide-shell.exe
```

`dist/` is generated output and is ignored by git (see `.gitignore`).

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

1. runs `cargo build --release` from the repository root and **fails with a
   non-zero exit code** if the build fails;
2. checks that every expected executable exists in `target\release\`
   (`nextide-cli.exe`, `nextide-shell.exe`) — an unexpected missing artifact
   is a hard error, so a stale `dist` can never look like a successful build;
3. creates `dist\windows\` if needed and copies the executables there
   (overwriting in place);
4. verifies the staged copies exist and prints each one's size and SHA-256.

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
- reads or writes anything outside the repository;
- accesses the network.

Every failure path exits non-zero with a message explaining what was missing.

## Verify

After a successful run, smoke-test the staged binaries from `dist\windows\`:

```powershell
.\dist\windows\nextide-cli.exe            # prints CLI usage/help
.\dist\windows\nextide-cli.exe smoke      # runs the editor-kernel smoke flow
.\dist\windows\nextide-shell.exe          # starts the headless Desktop Shell
```

Note: `cargo test --workspace` and `cargo clippy` are **not** part of this
script — they remain covered by the CI workflow (`.github/workflows/ci.yml`),
which is unchanged.
