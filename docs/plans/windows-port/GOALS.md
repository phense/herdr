# Windows Port — Testable `/goal` Conditions

> See `PLAN.md` for the architecture and reasoning. This file is the **execution surface**: every entry is a self-contained completion condition you can paste into `/goal` (Claude Code v2.1.139+).

## How to use this file

- Goals are numbered to match `PLAN.md` phases. Run them **in order** — each goal assumes earlier goals are complete.
- Each goal is structured as:
  - **Condition** — the literal text to type after `/goal`. Up to 4000 chars. Includes the check command and the success criterion.
  - **Files in play** — what the goal touches (informational).
  - **Done when** — independent acceptance check, mirroring the condition's check command.
  - **Sub-goals** — finer-grained `/goal` conditions you can use instead, one at a time, if the parent goal is too large for a single autonomous run.
- Every condition ends with `or stop after N turns and report the blocker` so the evaluator can give up cleanly when stuck.
- Run condition commands from the repo root (`C:/AI/Claude/herdr/repo`) inside `pwsh` unless noted otherwise.

---

## Goal 0 — Toolchain & build baseline

**Condition (paste after `/goal`):**

```text
cargo check --target x86_64-pc-windows-msvc --no-default-features --offline=false succeeds with exit code 0 on the current branch, AND `vendor/libghostty-vt` can be built for windows via `zig build -Dtarget=x86_64-windows-msvc -Demit-lib-vt` (capture the log either way), AND `scripts/build_vendored_libghostty_vt.ps1` exists and is referenced from `build.rs`'s comments. Constraints: do not modify any code in src/server, src/client, src/api, or src/transport during this goal — only build.rs, Cargo.toml, scripts/, and docs/plans/windows-port/. Show the exact `cargo check` output proving exit 0. Or stop after 25 turns and report the blocker.
```

**Files in play:** `build.rs`, `Cargo.toml`, `scripts/build_vendored_libghostty_vt.ps1` (new), `docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md` (new if needed).

**Done when:**
```pwsh
cargo check --target x86_64-pc-windows-msvc 2>&1 | Select-String -Pattern 'error\[|^error:' -Quiet
# Expected: $false (no errors found)
$LASTEXITCODE
# Expected: 0
```

**Sub-goals:**

- **0.1** — `build.rs` knows the Windows MSVC and GNU triples; running `rustc --print target-spec-json --target x86_64-pc-windows-msvc` followed by `cargo check --target x86_64-pc-windows-msvc -p herdr --features '' 2>&1 | findstr unsupported` returns no matches.
- **0.2** — `zig build -Dtarget=x86_64-windows-msvc -Demit-lib-vt` inside `vendor/libghostty-vt` either succeeds (write success log) or produces a failure captured into `docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md` with next steps.
- **0.3** — `Cargo.toml` contains `[target.'cfg(windows)'.dependencies] windows-sys = …` with at least `Win32_Foundation`, `Win32_System_Pipes`, `Win32_System_Threading`, `Win32_Security` features; `cargo tree --target x86_64-pc-windows-msvc -i windows-sys` shows herdr depending on it.
- **0.4** — Running `pwsh -File scripts/build_vendored_libghostty_vt.ps1 -Help` prints usage without erroring.

---

## Goal 1 — Cross-platform paths

**Condition:**

```text
On both Linux and Windows targets, `config_dir()`, `state_dir()`, and `data_dir_for(name)` return paths under `%APPDATA%\herdr` / `%LOCALAPPDATA%\herdr` on Windows and `$XDG_CONFIG_HOME`/`$HOME/.config/herdr` on Unix (with the existing `/tmp/herdr` fallback only on Unix). Prove it by running `cargo test config::io::tests::config_dir_` and `cargo test session::tests::data_dir_` — both must pass with the same code on both targets. No test uses a hard-coded `/tmp/` literal anymore; running `Select-String -Path src,tests -Pattern '"/tmp/' -Recurse` returns either zero hits or only matches that are inside `#[cfg(unix)]` blocks. Or stop after 20 turns and report the blocker.
```

**Files in play:** `src/config/io.rs`, `src/session.rs`, `src/server/socket_paths.rs`, anywhere in `src/` or `tests/` that contains `/tmp/`.

**Done when:**
```pwsh
cargo test --lib config::io::tests
cargo test --lib session::tests
# Both exit 0.
Select-String -Path src,tests -Pattern '"/tmp/' -Recurse | Where-Object { $_.Line -notmatch '#\[cfg\(unix\)' }
# Empty output.
```

**Sub-goals:**

- **1.1** — `config_dir()` honors `%APPDATA%` on Windows; new unit test `config_dir_uses_appdata_on_windows` (cfg-gated) passes.
- **1.2** — `state_dir()` honors `%LOCALAPPDATA%`; sibling unit test passes.
- **1.3** — Every `/tmp/` literal in tests is replaced with `std::env::temp_dir().join(...)`; the grep above produces zero hits outside `#[cfg(unix)]`.

---

## Goal 2 — Platform module: Windows variant

**Condition:**

```text
A new file `src/platform/windows.rs` exists and is wired into `src/platform/mod.rs` behind `#[cfg(windows)]`, replacing the `fallback.rs` stubs for Windows targets. It provides working implementations of `foreground_job`, `foreground_process_group_id`, `process_cwd`, `session_processes`, `signal_processes`, `process_exists`, `write_clipboard`, `read_clipboard_image`, and `show_desktop_notification`, plus a new helper `assign_to_job(pid: u32) -> io::Result<()>`. Each function has at least one `#[cfg(windows)] #[test]` validating it on a tiny fixture (typically: spawn `cmd /c timeout 5`, query state, then signal kill). Running `cargo test --target x86_64-pc-windows-msvc -p herdr platform::windows::` produces a passing summary line of the form `test result: ok. N passed`, with N >= 8. Linux/macOS targets still compile (`cargo build` on the host succeeds). The `fallback.rs` file remains intact for non-windows-non-unix targets. Constraints: no `#[cfg(target_os)]` is added outside `src/platform/`. Or stop after 60 turns and report the blocker.
```

**Files in play:** `src/platform/mod.rs`, `src/platform/windows.rs` (new), `src/platform/shell.rs` (new), `Cargo.toml` (additional `windows-sys` features).

**Done when:**
```pwsh
cargo test --target x86_64-pc-windows-msvc -p herdr platform::windows -- --nocapture 2>&1 | Select-String 'test result: ok\. (?<n>\d+) passed' | ForEach-Object { [int]$_.Matches.Groups['n'].Value -ge 8 }
# Expected: True
cargo build 2>&1 | Select-String '^error:' -Quiet
# Expected: False
```

**Sub-goals (one per function):**

- **2.1** — `process_exists(pid)` works: a test that spawns `cmd /c timeout 30`, asserts `process_exists(pid)` is true, kills the child, asserts it becomes false within 2 seconds.
- **2.2** — `signal_processes(&[pid], Signal::Terminate)` actually terminates a child; the test from 2.1 uses this as the kill step.
- **2.3** — `process_cwd(pid)` returns the child's CWD using `NtQueryInformationProcess`; test spawns `cmd /c "cd /D C:\Windows && timeout 30"` and asserts the returned path is `C:\Windows`.
- **2.4** — `foreground_job(pid)` and `foreground_process_group_id(pid)` return a sensible value for a console child; test spawns `cmd /c timeout 30` and checks the result is non-empty and includes the spawned pid.
- **2.5** — `assign_to_job(pid)` + `session_processes(pid)` together let you enumerate a process tree; test spawns `cmd /c "start cmd /c timeout 30 && timeout 30"` and asserts `session_processes` returns both PIDs, then `signal_processes(..., Kill)` cleans them up.
- **2.6** — `write_clipboard("herdr-test-marker")` followed by a PowerShell `Get-Clipboard` round-trip returns the same string.
- **2.7** — `read_clipboard_image()` returns `Some(ClipboardImage)` after PowerShell sets a known PNG via `Set-Clipboard -Path test.png` (or skips the test cleanly if the helper isn't available).
- **2.8** — `show_desktop_notification("herdr test", Some("body"))` returns `Ok(true)` when `BurntToast` is installed and `Ok(false)` otherwise — *never* `Err`.
- **2.9** — `src/platform/shell.rs::default_shell()` returns the first existing of `pwsh.exe`/`powershell.exe`/`cmd.exe` on Windows and `/bin/sh` on Unix; unit-tested with mocked env.

---

## Goal 3 — Local-socket transport abstraction

**Condition:**

```text
A new module `src/transport/` exposes `LocalStream` and `LocalListener` with the API documented in `PLAN.md §3.1`. The Unix backend (`src/transport/unix.rs`) wraps `std::os::unix::net::*`; the Windows backend (`src/transport/windows.rs`) wraps Win32 named pipes via `windows-sys`. The module replaces the helpers previously in `src/ipc.rs` (which becomes a thin re-export). Running `cargo test transport::` on the host platform AND on `x86_64-pc-windows-msvc` (via `--target`) yields `test result: ok. N passed` with N >= 15, covering: connect/accept happy path, set_nonblocking, set_read_timeout fires after the timeout, try_clone yields an independently-usable handle, pair() returns two ends that round-trip a 64 KiB payload, shutdown(Write) makes the peer see EOF, oversized writes don't panic. NO file outside `src/transport/` or `src/ipc.rs` references `std::os::unix::net` anymore (verify with `Select-String -Path src -Pattern 'std::os::unix::net' -Recurse | Where-Object { $_.Path -notmatch 'transport|ipc' }` returning empty). Or stop after 90 turns and report the blocker.
```

**Files in play:** `src/transport/mod.rs`, `src/transport/unix.rs`, `src/transport/windows.rs` (all new), `src/ipc.rs` (rewritten to re-export).

**Done when:**
```pwsh
cargo test --lib transport 2>&1 | Select-String 'test result: ok\.'
Select-String -Path src -Pattern 'std::os::unix::net' -Recurse | Where-Object { $_.Path -notmatch 'transport|ipc' }
# Empty
```

**Sub-goals:**

- **3.1** — `LocalStream`/`LocalListener` types + Unix backend implement `connect`, `bind`, `accept`, `try_clone`, `set_nonblocking`, `set_read_timeout`, `shutdown`, `pair`; existing tests against `UnixStream` adapted to the new API still pass on Linux.
- **3.2** — Windows backend implements the same API via `CreateNamedPipeW`/`CreateFileW`/`DuplicateHandle`/`SetNamedPipeHandleState`/overlapped IO with `WaitForSingleObject`; `cargo test --target x86_64-pc-windows-msvc transport::windows` passes ≥10 tests.
- **3.3** — `pair()` works on both platforms (Windows uses a random `\\.\pipe\herdr-test-{pid}-{nonce}` instance + connect from the same process).
- **3.4** — `set_read_timeout(Some(50ms))` then a `read` on an idle stream returns `ErrorKind::WouldBlock` (or `TimedOut`) within 200 ms on both platforms — single behavioral test that runs on both targets.

---

## Goal 4 — Migrate IPC consumers to the transport

**Condition:**

```text
Every use of `std::os::unix::net::UnixStream` or `UnixListener` in src/ (outside `src/transport/` and test modules in `src/transport/`) is replaced with `crate::transport::LocalStream` / `LocalListener`. The 18 files listed in `PLAN.md §1.1` all compile against the new API. `cargo build` and `cargo test --no-run` succeed on the host platform AND on `--target x86_64-pc-windows-msvc`. All existing Linux/macOS tests still pass (`cargo nextest run` exits 0). `Select-String -Path src -Pattern 'UnixStream|UnixListener' -Recurse | Where-Object { $_.Path -notmatch 'transport' }` returns either no hits or only hits inside `#[cfg(unix)]`-gated test blocks. Behavior is unchanged on Unix — no protocol bump, no permissions change. Or stop after 80 turns and report the blocker.
```

**Files in play:** all 18 from `PLAN.md §1.1`.

**Done when:**
```pwsh
cargo build --target x86_64-pc-windows-msvc 2>&1 | Select-String 'error\[' -Quiet
# Expected: False
cargo nextest run --target $(rustc -vV | Select-String 'host:' | %{ ($_ -split ' ')[-1] }) 2>&1 | Select-String 'failed'
# Expected: empty or 0 failed
```

**Sub-goals (group by area, do them in order):**

- **4.1** — Move helpers: `src/ipc.rs` reduces to `pub use crate::transport::{prepare_socket_path, restrict_socket_permissions};` (or similar); no `UnixStream` in this file.
- **4.2** — Server side: `src/server/{socket_paths,client_accept,client_transport,autodetect,headless}.rs` all migrated; `cargo test server::` passes.
- **4.3** — Client side: `src/client/mod.rs` migrated; `src/client/input.rs` gets a `#[cfg(windows)]` sibling using `crossterm::event::poll` instead of `RawFd` polling.
- **4.4** — JSON API: `src/api/{server,client,wait}.rs` migrated; `cargo test api::` passes.
- **4.5** — Session/update/remote: `src/session.rs`, `src/update.rs`, `src/raw_input.rs`, `src/remote.rs` migrated. For `remote.rs::run_remote`, leave a `cfg(windows)` early-return with a friendly "SSH bridge not yet supported on Windows" error.
- **4.6** — `src/protocol/wire.rs::framing_over_unix_socketpair` renamed to `framing_over_pair` and uses `LocalStream::pair()`.

---

## Goal 5 — PTY shell + restore wrapper

**Condition:**

```text
`src/pane.rs` no longer hardcodes `/bin/sh`. The default shell is selected via `crate::platform::shell::default_shell()` (Unix → `/bin/sh`; Windows → first existing of pwsh.exe / powershell.exe / cmd.exe). `RESTORE_WRAPPER_SCRIPT` is replaced by `crate::platform::shell::restore_wrapper()`, which on Windows returns a PowerShell equivalent. Newly-spawned children are assigned to a kill-on-close Job Object via `crate::platform::assign_to_job` on Windows (Unix path unchanged). A new unit test `pane::tests::default_shell_is_platform_appropriate` passes on both platforms. A new integration test `tests/pane_smoke.rs::spawns_shell_runs_command` spawns the platform default shell with a "print 'herdr-ok' then exit" command and asserts the bytes show up in the pane's visible text within 5 seconds — passes on both platforms. Or stop after 40 turns and report the blocker.
```

**Files in play:** `src/pane.rs`, `src/platform/shell.rs`, `tests/pane_smoke.rs` (new).

**Done when:**
```pwsh
cargo nextest run pane::tests::default_shell_is_platform_appropriate
cargo nextest run --test pane_smoke
# Both pass.
```

**Sub-goals:**

- **5.1** — `pane_shell` reads from `platform::shell::default_shell()`; tests with mocked env cover both branches.
- **5.2** — `restore_command_builder` / `restore_command_args` switch on `cfg!(windows)` and use the right script.
- **5.3** — `spawn_shell_command` builds `pwsh -NoProfile -Command <cmd>` on Windows, `/bin/sh -c <cmd>` on Unix.
- **5.4** — `PaneRuntime::spawn_command_builder` assigns the child process to a kill-on-close Job Object on Windows; a regression test verifies the child dies when the runtime is dropped.

---

## Goal 6 — Windows smoke + CI

**Condition:**

```text
A `windows-smoke` GitHub Actions job runs on every push, executing on `windows-latest`: `cargo build --release --locked` then `cargo nextest run --no-default-features --workspace -- --skip integration_unix`. The job is green on the current branch (verify via `gh run list --workflow=ci --branch <current-branch> --limit 1`). A new file `docs/plans/windows-port/SMOKE.md` documents a 15-step manual smoke test for Windows Terminal — launching herdr, creating a workspace, spawning pwsh, splitting, detaching, reattaching, killing the server. Or stop after 30 turns and report the blocker.
```

**Files in play:** `.github/workflows/ci.yml` (added job), `docs/plans/windows-port/SMOKE.md` (new).

**Done when:**
```pwsh
gh run list --workflow=ci --branch (git branch --show-current) --limit 1 --json status,conclusion | ConvertFrom-Json | ForEach-Object { $_.status -eq 'completed' -and $_.conclusion -eq 'success' }
# Expected: True
Test-Path docs/plans/windows-port/SMOKE.md
# Expected: True
```

**Sub-goals:**

- **6.1** — `.github/workflows/ci.yml` includes a Windows job; pushing to a branch triggers it and it succeeds.
- **6.2** — `SMOKE.md` is written, exhaustive enough to follow without help.

---

## Goal 7 — Clipboard, notifications, integration scripts

**Condition:**

```text
The Windows clipboard read/write functions in `src/platform/windows.rs` are wired into `src/server/clipboard_image.rs` (already cross-platform via the platform trait). `show_desktop_notification` is wired up in `src/server/notifications.rs`. Each `.sh` file under `src/integration/assets/{claude,codex,opencode}/` has a working `.ps1` sibling; `src/integration/mod.rs::install_for_*` picks the right script per host OS at install time. Running `cargo test integration::tests` passes on both platforms; running `pwsh src/integration/assets/claude/herdr-agent-state.ps1 --self-test` exits 0. Or stop after 45 turns and report the blocker.
```

**Files in play:** `src/platform/windows.rs` (flesh out clipboard + notifications), `src/server/{clipboard_image,notifications}.rs`, `src/integration/mod.rs`, `src/integration/assets/*/herdr-agent-state.ps1` (new).

**Done when:**
```pwsh
cargo nextest run integration::tests
pwsh src/integration/assets/claude/herdr-agent-state.ps1 --self-test
# Both exit 0.
```

**Sub-goals:**

- **7.1** — `write_clipboard` / `read_clipboard_image` implemented and unit-tested in `src/platform/windows.rs` (Goal 2 already covers tests, this is the wire-up step into the rest of the app).
- **7.2** — `show_desktop_notification` implemented (PowerShell BurntToast fallback, returns `Ok(false)` cleanly when unavailable).
- **7.3** — Claude PS1: behavioral parity with the Bash version.
- **7.4** — Codex PS1: same.
- **7.5** — OpenCode PS1: same.
- **7.6** — `install_for_claude(...)` writes the `.ps1` to the right location on Windows and the `.sh` on Unix; tests cover both.

---

## Goal 8 — Test suite portability

**Condition:**

```text
`cargo nextest run --target x86_64-pc-windows-msvc` produces zero failures (skipped Unix-only tests are fine — counts as a pass). Every test under `tests/` that touches paths or shells goes through `tests/support/local_socket::unique_socket_path` or `crate::platform::shell::shell_command` — no hard-coded `/tmp/` or `sh -c` outside `#[cfg(unix)]` blocks. Verify with: `Select-String -Path tests -Pattern '"/tmp/|sh -c' -Recurse | Where-Object { $_.Line -notmatch '#\[cfg\(unix\)' }` returning empty. Linux nextest run still green. Or stop after 50 turns and report the blocker.
```

**Files in play:** every file under `tests/`, plus `tests/support/local_socket.rs` (new).

**Done when:**
```pwsh
cargo nextest run --target x86_64-pc-windows-msvc 2>&1 | Select-String '^.*tests? failed' | Where-Object { $_.Line -notmatch '0 failed' }
# Empty.
Select-String -Path tests -Pattern '"/tmp/|sh -c' -Recurse | Where-Object { $_.Line -notmatch '#\[cfg\(unix\)' }
# Empty.
```

**Sub-goals:**

- **8.1** — `tests/support/local_socket.rs` exists and is exported from the test support module.
- **8.2** — `tests/api_ping.rs`, `tests/server_headless.rs`, `tests/multi_client.rs`, `tests/auto_detect.rs`, `tests/detach_reattach.rs`, `tests/cross_area.rs`, `tests/cli_wrapper.rs`, `tests/client_mode.rs` all use the helper and run on Windows.
- **8.3** — Genuinely-Unix tests (chmod, signals beyond Hangup/Terminate/Kill, raw-fd polling) are `#[cfg(unix)]`-gated.

---

## Goal 9 — Updater + release manifest schema

**Condition:**

```text
`src/update.rs::asset_key()` returns `"windows-x86_64"` on `cfg(windows)` + `target_arch="x86_64"` and `"windows-aarch64"` on `cfg(windows)` + `target_arch="aarch64"`. A unit test `update::tests::asset_key_for_windows` covers both branches and passes on the host. `AGENTS.md::Releases` documents the two new keys in the `latest.json` schema. `website/latest.json` is NOT edited (per AGENTS.md, release workflow owns that file). Or stop after 15 turns and report the blocker.
```

**Files in play:** `src/update.rs`, `AGENTS.md`.

**Done when:**
```pwsh
cargo test update::tests::asset_key_for_windows
# Passes.
Select-String -Path AGENTS.md -Pattern 'windows-x86_64' -Quiet
# True.
```

---

## Goal 10 — Documentation

**Condition:**

```text
`docs/next/README.md`, `docs/next/CHANGELOG.md` (`## Unreleased` section), and `docs/next/website/src/content/docs/installation.mdx` document Windows support. The supported-platforms table in the README includes Windows. A new section in `AGENTS.md` notes that Windows code lives in `src/platform/windows.rs` and `src/transport/windows.rs`. Running `just release-docs-check` (or its analogue) shows the `docs/next/` mirror is still 1:1 with the released docs (i.e., the Windows additions are only in `docs/next/`). Or stop after 20 turns and report the blocker.
```

**Files in play:** `docs/next/README.md`, `docs/next/CHANGELOG.md`, `docs/next/website/src/content/docs/installation.mdx`, `AGENTS.md`.

**Done when:**
```pwsh
Select-String -Path docs/next/README.md -Pattern 'windows' -Quiet
# True.
Select-String -Path docs/next/CHANGELOG.md -Pattern 'windows' -Quiet
# True.
```

---

## Appendix A — One-shot mega goal (NOT recommended)

For reference only. Don't actually use this — it's too long for a single autonomous run.

```text
The herdr crate builds, tests, and runs on x86_64-pc-windows-msvc with feature parity for local single-user mode: cargo build --release succeeds; cargo nextest run produces zero failures; tests/api_ping.rs and tests/auto_detect.rs both pass; the manual smoke checklist in docs/plans/windows-port/SMOKE.md has been executed and recorded passing. Linux and macOS builds + tests remain green at every commit. No #[cfg(target_os)] outside src/platform/ or src/transport/. Or stop after 600 turns and report the blocker.
```

---

## Appendix B — Setting up an interactive run

```pwsh
# From inside the worktree:
git switch -c windows-port
# Then in Claude Code, after reading PLAN.md:
/goal <paste the Goal 0 condition>
# After it succeeds and the goal clears:
/goal <paste the Goal 1 condition>
# …and so on through Goal 10.
```

A typical solo Windows port runs ~15-30 hours of wall clock across roughly a week. Plan one goal per session.
