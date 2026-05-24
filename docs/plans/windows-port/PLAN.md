# Windows Terminal Port — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` or `/goal` (see `GOALS.md`) to drive the work task-by-task.

**Goal:** Make `herdr` build and run end-to-end on Windows 10/11 inside Windows Terminal (and other ConPTY-capable hosts), with feature parity for local single-user use. Linux/macOS behavior must remain unchanged.

**Architecture:** Two cross-cutting refactors guarded by `#[cfg]`: (1) replace direct `std::os::unix::net::{UnixListener, UnixStream}` usage with a thin in-crate **local-socket transport** (`src/transport/`) that maps to Unix sockets on Unix and **Windows Named Pipes** on Windows; (2) complete `src/platform/` with a `windows.rs` module that fills in the existing `fallback.rs` stubs using `windows-sys` for processes, signals, clipboard, notifications, and CWD discovery. `portable-pty` already wraps **ConPTY**, so PTY plumbing is largely free — the only PTY-adjacent change is replacing `/bin/sh -c` agent-restore wrappers with a PowerShell equivalent. Build chain switches from POSIX shell scripts + libghostty-vt Zig build to a Windows-friendly path (Zig already supports Windows targets).

**Tech Stack:**
- Existing: `tokio`, `crossterm 0.29`, `ratatui 0.30`, `portable-pty 0.9`, `bincode`, `serde`, `tracing`, `toml`.
- New (Windows-only): `windows-sys ^0.59` (Win32 FFI), optionally `interprocess ^2` (named-pipe local-socket abstraction — alternative path, see §5.2).
- Removed/gated on Windows: `libc` is unix-only (already declared as `[target.'cfg(unix)'.dependencies]`-eligible; keep cross-platform by feature gating call sites).

---

## 0. Scope, Non-Goals, Success Criteria

### In scope (Phase A — MVP)
- `herdr` binary builds with `cargo build --release` on `x86_64-pc-windows-msvc` (and `gnu`) using stable Rust.
- Local single-process mode (`herdr --no-session`) works inside Windows Terminal: spawn a `pwsh.exe` pane, split, switch tabs, detect Claude/Codex agents by argv heuristics.
- Persistent session server (`herdr` default) works: detach with prefix-q, reattach, agents survive client exit. IPC uses named pipes under `\\.\pipe\herdr-<session>`.
- Socket API (`herdr workspace create`, `herdr pane run`, etc.) works through the same named-pipe transport, accessible to local CLI invocations and integration scripts.
- ConPTY-driven panes render correctly through libghostty-vt; mouse, resize, scrollback work.
- `just check` analogue runs on Windows CI.

### In scope (Phase B — parity)
- `herdr --remote <host>` over SSH (OpenSSH client on Windows).
- Built-in clipboard integration (text + image) via Win32 clipboard API.
- Toast notifications via WinRT/PowerShell `BurntToast` fallback.
- Direct integration scripts for Claude / Codex / Hermes / OpenCode rewritten as PowerShell.
- Updater (`herdr update`) downloads the right Windows asset.

### Out of scope (initial port)
- `flake.nix` / Nix outputs — Linux-only build mechanism, leave untouched.
- macOS-specific terminal-notifier / Wayland clipboard backends — already gated.
- Release CI publishing Windows assets — touch `website/latest.json` schema only enough so the in-app updater on Windows knows where to look; actual release-workflow YAML changes are tracked separately.
- Running the website (`bun`-based Astro Starlight) — unchanged.

### Success criteria
- All `#[cfg(unix)]`-gated tests still pass on Linux/macOS via existing CI.
- A new `#[cfg(windows)]` test surface passes on Windows CI (GitHub Actions `windows-latest` runner).
- A manual smoke checklist (`docs/plans/windows-port/SMOKE.md`, to be created) passes from Windows Terminal.
- No `#[cfg(target_os)]` is added to core modules outside `src/platform/`, `src/transport/`, and a small whitelist (per `AGENTS.md`).

---

## 1. Repo Inventory of Unix Coupling

This section is the **authoritative checklist of files touched**. Every change is one of:
- **(P)** platform module — implement Windows variant
- **(T)** transport refactor — replace `UnixStream`/`UnixListener` with `LocalStream`/`LocalListener`
- **(C)** cfg-gated change in place
- **(B)** build/scripts/integration assets

### 1.1 Local-socket IPC (`std::os::unix::net::*`) — needs (T)

Search confirms direct use in 18 files:

| File | Symbols | Notes |
|---|---|---|
| `src/ipc.rs` | `UnixStream::connect`, `PermissionsExt::set_mode` | shared helper; promote to transport module |
| `src/server/socket_paths.rs` | path derivation, `chmod 0o600` test | rewrite path helpers for `\\.\pipe\...`, drop chmod for ACL on Windows |
| `src/server/client_accept.rs` | `UnixListener::accept` | listener abstraction |
| `src/server/client_transport.rs` | `UnixStream` read/write, `set_nonblocking`, `set_read_timeout`, `try_clone` | hot path, must keep same `Read+Write` semantics |
| `src/server/autodetect.rs` | `UnixStream::connect` for liveness probe | replace with `LocalStream::connect` |
| `src/server/headless.rs` | mostly references via above | indirect via cfg helpers |
| `src/client/mod.rs` | `UnixStream::connect`, `try_clone`, `shutdown` | reader+writer split |
| `src/client/input.rs` | `RawFd` for stdin polling | swap to `crossterm` event loop on Windows |
| `src/api/server.rs` | `UnixListener::bind`, `accept` | line-based JSON protocol |
| `src/api/client.rs` | `BufReader<UnixStream>` | line-based, easy port |
| `src/api/wait.rs` | `UnixStream` long-poll | |
| `src/session.rs` | session liveness probe via `UnixStream::connect` | path derivation also changes |
| `src/update.rs` | similar liveness probes + four test sites | |
| `src/remote.rs` | local-side bridge socket + `fits_unix_socket_path` 108-byte cap | drop cap check on Windows; local bridge name uses pipe |
| `src/raw_input.rs` | `RawFd` of stdin | use `crossterm` raw input |
| `src/protocol/wire.rs` | only `UnixStream::pair()` in tests | swap test helper |
| `src/persist/io.rs` | symlink follow loop | already cross-platform via `fs::symlink_metadata` |

### 1.2 `libc::*` direct calls — needs (P) or (C)

13 files reference `libc` (`kill`, `SIGHUP/TERM/KILL`, `proc_pidinfo`, `getsid`, `sysctl`, `proc_listpids`, `proc_listallpids`, `MAXPATHLEN`, `pid_t`):

- `src/platform/linux.rs`, `src/platform/macos.rs` — Unix-only, no change.
- `src/platform/mod.rs` — wire in new `windows` module.
- `src/platform/fallback.rs` — keep as compile-target safety net for non-Unix non-Windows (e.g. WSL exotic targets, BSD).
- Other files that touched `libc`/raw fds are already routed through `crate::platform::*`. **Confirm no direct `libc::` survives outside `src/platform/{linux,macos}.rs`.** Any survivors get cfg-gated wrappers in the platform module.

### 1.3 Paths and process model — needs (C)

- `src/config/io.rs::config_dir()` / `state_dir()` — XDG with `/tmp` fallback. On Windows use `%APPDATA%\\herdr` (or `%LOCALAPPDATA%\\herdr` for state).
- `src/session.rs::data_dir_for()` — same dir tree.
- `src/pane.rs::pane_shell()` — `/bin/sh` default. On Windows default to `pwsh.exe` → `powershell.exe` → `cmd.exe`.
- `src/pane.rs::RESTORE_WRAPPER_SCRIPT` — POSIX shell. Write a PowerShell sibling; pick at runtime by `cfg!(windows)`.
- `src/pane.rs::restore_command_builder` — `CommandBuilder::new("/bin/sh")` → cmd line built from selected shell.
- `src/pane.rs::spawn_shell_command` — `/bin/sh -c <cmd>` → `pwsh -NoProfile -Command <cmd>`.
- `src/integration/*` — assets and detection helpers (see §1.5).
- Tests in `src/pane.rs`, `tests/*.rs` that shell out to `sh -c` must be cfg-gated or rewritten.

### 1.4 Build chain — needs (B)

- `build.rs::zig_target()` **panics** on any non-Unix target — first blocker. Add `x86_64-pc-windows-msvc → x86_64-windows-msvc`, `x86_64-pc-windows-gnu → x86_64-windows-gnu`, `aarch64-pc-windows-msvc → aarch64-windows-msvc`.
- `vendor/libghostty-vt/build.zig` — already supports `-Dtarget=x86_64-windows-...` per upstream; verify the vendored copy.
- `scripts/build_vendored_libghostty_vt.sh` — POSIX. Add a `.ps1` sibling so Windows contributors / CI can rebuild the vendor snapshot.
- `justfile` — uses `python3`, `cargo nextest`, POSIX shell loops. Either (a) ensure `just` + `bash` + `python3` are available in CI (works on `windows-latest` via msys), or (b) add a `tasks.ps1`. Prefer (a) for low duplication.

### 1.5 Integration assets — needs (B)

- `src/integration/assets/claude/herdr-agent-state.sh`
- `src/integration/assets/codex/herdr-agent-state.sh`
- `src/integration/assets/opencode/herdr-agent-state.sh`
- `src/integration/assets/hermes/__init__.py` — Python, already cross-platform.

Each Bash script needs a `.ps1` sibling and `src/integration/mod.rs::install_for_*` must pick the right one by host OS.

### 1.6 Test fixtures and harnesses — needs (C)

- `tests/api_ping.rs`, `tests/server_headless.rs`, `tests/multi_client.rs`, `tests/auto_detect.rs`, `tests/detach_reattach.rs`, `tests/cross_area.rs`, `tests/cli_wrapper.rs`, `tests/client_mode.rs` — most spawn the binary and connect over a socket. After the transport layer lands, the test helpers in `tests/support/` need a `local_socket_path()` helper that returns `/tmp/...` on Unix and `\\.\pipe\herdr-test-...` on Windows.

---

## 2. Risk register

| Risk | Probability | Impact | Mitigation |
|---|---|---|---|
| libghostty-vt fails to build for Windows targets via vendored Zig | Medium | Blocks everything | Build it standalone first (Phase 0, task 0.4). If it doesn't compile, file upstream issue, evaluate fork; until resolved, port can compile/test only the non-rendering subset by feature-gating `pane.rs`. |
| Named-pipe semantics differ from Unix sockets (no `pair()`, `try_clone` returns `INVALID_HANDLE_VALUE` after close, no `shutdown(Write)` equivalent on overlapped pipes) | High | Subtle bugs in client_transport | Validate every public method of `LocalStream`/`LocalListener` against the in-tree usages with a unit test before migrating callers (Phase 5). |
| Windows ACLs vs `chmod 0o600` socket permissions | Low | Security | Per-user pipe namespace (`\\.\pipe\herdr-<sid>-<session>`) gives implicit user isolation; document and add ACL hardening as Phase B follow-up. |
| `tokio::task::spawn_blocking` interaction with ConPTY's reader thread | Low | Hangs on detach | `portable-pty` already handles this on Windows; only verify with the smoke checklist. |
| Process-tree teardown is fundamentally different (no PGID/SID) | Medium | Stale `pwsh` children after `herdr stop` | Use Win32 Job Objects in `src/platform/windows.rs::shutdown_pane_processes`: assign the spawned child to a job that kills on close. Document the divergence. |
| `crossterm 0.29` raw-input on Windows uses different event source than `raw_fd` polling in `client/input.rs` | Medium | Input loop must be rewritten cfg-gated | crossterm's event reader already abstracts this; replace the fd-based loop with `crossterm::event::poll` on Windows. |
| Tests that hard-code `/tmp/...` paths | High | Test failures | Replace with `std::env::temp_dir()` everywhere. |
| `fits_unix_socket_path` 108-byte cap test failures on Windows | Low | Test failure | Cfg-gate or make the function trivially return `true` on Windows. |
| SSH bridge: `herdr --remote` uses Unix-socket local forwarder | Low (Phase B) | Defer to Phase B | Keep the architecture but stub it on Windows initially with a clear "not supported on Windows yet" error. |
| Updater downloads wrong binary | Low | First Windows release | Add `windows-x86_64` and `windows-aarch64` keys to `website/latest.json` schema; gate by `cfg!(windows)` in `src/update.rs::asset_key()`. |

---

## 3. Architectural Decisions

### 3.1 Local-socket transport (the central decision)

We introduce `src/transport/mod.rs` exposing:

```rust
pub struct LocalStream { /* opaque, holds UnixStream on unix, NamedPipeClient/Server on windows */ }
pub struct LocalListener { /* opaque */ }

impl LocalStream {
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self>;
    pub fn try_clone(&self) -> io::Result<Self>;
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()>;
    #[cfg(test)] pub fn pair() -> io::Result<(Self, Self)>;
}
impl Read for LocalStream { ... }
impl Write for LocalStream { ... }

impl LocalListener {
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self>;
    pub fn accept(&self) -> io::Result<(LocalStream, LocalAddr)>;
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
}
```

**Windows implementation:**
- `bind` calls `CreateNamedPipeW` with `PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED`, security descriptor allowing only the current user (`PIPE_REJECT_REMOTE_CLIENTS`). Each `accept` calls `CreateNamedPipeW` again for the next instance (Windows quirk: each connecting client gets a fresh instance).
- `connect` calls `CreateFileW` on the pipe path. Path is `\\.\pipe\<name>` derived from the abstract path (`/tmp/herdr-test-XYZ` → `\\.\pipe\herdr-test-XYZ`).
- `set_read_timeout` uses `SetCommTimeouts` or wrapping the read in an `OVERLAPPED` + `WaitForSingleObject(timeout)`.
- `try_clone` calls `DuplicateHandle` with `DUPLICATE_SAME_ACCESS`.
- `pair()` uses two named-pipe instances with a randomized name in `\\.\pipe\herdr-test-{pid}-{counter}\\`.

**Decision: hand-rolled abstraction, no extra crate.** Rationale: every API surface used by callers is small (~6 methods) and bincode framing is already done at a layer above. The `interprocess` crate would work but its `LocalSocketStream` is a moving target across versions and adds 30+ transitive deps. Hand-rolling stays in `src/transport/windows.rs` (~300 LOC) and uses only `windows-sys` Win32 APIs we'll already need.

### 3.2 Path policy

Add `crate::transport::socket_name(abstract: &str) -> PathBuf`:
- Unix: `crate::config::config_dir().join(format!("{abstract}.sock"))` (unchanged).
- Windows: `PathBuf::from(format!(r"\\.\pipe\herdr-{user_sid}-{abstract}"))`. The `user_sid` is read once via `GetTokenInformation(TokenUser)` for namespace isolation between Windows users on the same host.

All call sites that previously called `client_socket_path()` etc. get the path from this helper; the actual code path is identical.

### 3.3 Platform module completion

Implement `src/platform/windows.rs` matching the trait surface of `linux.rs`/`macos.rs`:

| Function | Windows implementation |
|---|---|
| `foreground_job(child_pid)` | Enumerate processes attached to the same console via `GetConsoleProcessList`. Treat the topmost non-`conhost.exe` as the foreground job. |
| `foreground_process_group_id(child_pid)` | Windows has no PGID; return the topmost child PID as a stand-in. Document semantics in `mod.rs`. |
| `process_cwd(pid)` | `NtQueryInformationProcess(ProcessBasicInformation)` → PEB → ProcessParameters → CurrentDirectory. Requires `PROCESS_QUERY_LIMITED_INFORMATION` + `PROCESS_VM_READ`. Fallback to `None` on UAC denial. |
| `session_processes(child_pid)` | Use a Win32 Job Object the pane created at spawn; query `JobObjectBasicProcessIdList`. |
| `signal_processes(pids, signal)` | `Signal::Hangup` → `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)`; `Signal::Terminate` → `TerminateProcess(handle, 1)`; `Signal::Kill` → `TerminateProcess(handle, 137)`. |
| `process_exists(pid)` | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid)` → handle valid → check `GetExitCodeProcess != STILL_ACTIVE`. |
| `write_clipboard(bytes)` | `OpenClipboard`, `EmptyClipboard`, `SetClipboardData(CF_UNICODETEXT, …)`, `CloseClipboard`. |
| `read_clipboard_image()` | `OpenClipboard`, `GetClipboardData(CF_DIB)`, convert DIB → PNG bytes using a tiny in-tree encoder (we already depend on `png` crate). |
| `show_desktop_notification(title, body)` | Call PowerShell with `BurntToast` if installed, else `New-BurntToastNotification`; final fallback to `[System.Windows.Forms.MessageBox]::Show` via `powershell -Command`. (Avoid native WinRT to keep build simple; revisit in Phase B with `winrt-notification` crate.) |

Tests live alongside as `#[cfg(test)] mod tests` plus a `#[cfg(windows)]`-gated integration test under `tests/platform_windows.rs`.

### 3.4 Shell + restore wrapper

Add `src/platform/shell.rs` (cross-platform) exposing:

```rust
pub fn default_shell() -> &'static str;            // /bin/sh on unix, pwsh.exe → powershell.exe → cmd.exe on windows
pub fn shell_command_args(cmd: &str) -> Vec<String>;   // ["-c", cmd] vs ["-NoProfile","-Command", cmd]
pub fn restore_wrapper_script() -> &'static str;        // existing POSIX script unchanged; ps1 sibling on windows
pub fn restore_command_args(agent, fallback_shell, argv) -> Vec<String>;
```

`pane.rs::pane_shell_from` becomes a thin wrapper that respects user config first, env (`SHELL` on unix, `COMSPEC` on Windows + `%PSModulePath%` presence to detect pwsh), then this default.

### 3.5 raw input on the client

`src/client/input.rs` polls stdin via `RawFd`. On Windows, replace with the `crossterm::event::poll` API (already used elsewhere in the codebase). Gate the file with `#[cfg_attr(windows, path = "input_windows.rs")]` style, or split into `input_unix.rs` + `input_windows.rs` re-exported from `input/mod.rs`.

### 3.6 Remote SSH bridge (Phase B)

Initial port: `herdr --remote …` returns a friendly error on Windows pointing to the tracking issue. The local-side bridge uses a `LocalListener` instead of `UnixListener`, but the SSH stdio plumbing needs a separate review (Phase B work).

### 3.7 Updater

`src/update.rs::asset_key()` becomes:
```rust
match (cfg!(target_os = "linux"), cfg!(target_os = "macos"), cfg!(windows), cfg!(target_arch = "aarch64")) {
    (true,  _, _, false) => "linux-x86_64",
    (true,  _, _, true ) => "linux-aarch64",
    (_, true,  _, false) => "macos-x86_64",
    (_, true,  _, true ) => "macos-aarch64",
    (_, _, true, false)  => "windows-x86_64",
    (_, _, true, true)   => "windows-aarch64",
    _ => return Err(...),
}
```

`website/latest.json` schema gains two optional keys: `windows-x86_64`, `windows-aarch64`. Document in `AGENTS.md::Releases`.

---

## 4. Cross-cutting Conventions

- All new Windows code lives in `src/platform/windows.rs`, `src/transport/windows.rs`, `src/platform/shell.rs`, or `<file>_windows.rs` siblings. No `#[cfg(windows)]` outside these paths except for very small inline gates (one or two lines).
- Use `windows-sys` (the no-frills FFI crate), not `windows` (the COM-projection crate). `windows-sys` is ~10× smaller and matches the style of existing `libc` calls.
- Add `[target.'cfg(windows)'.dependencies] windows-sys = { version = "0.59", features = ["Win32_Foundation","Win32_System_Pipes","Win32_System_Threading","Win32_System_JobObjects","Win32_Security","Win32_System_Console","Win32_System_Threading","Win32_System_LibraryLoader","Win32_System_ProcessStatus","Win32_System_Diagnostics_ToolHelp","Win32_System_DataExchange","Win32_System_Memory","Win32_UI_Shell"] }` (final feature list pinned in goal 2.1).
- Existing `libc` dependency stays unconditional (it compiles to nothing meaningful on Windows but breaks no code; we only *call* it in cfg-gated modules).
- Every new public function in `src/platform/windows.rs` has a doc comment naming the Win32 API it calls, mirroring the macOS file's style.
- Tests in cfg-shared modules must be `#[cfg(unix)]` or `#[cfg(windows)]`-gated where the test setup itself is platform-specific (e.g. binding a `UnixListener` for a stale-socket regression test).

---

## 5. Phased Implementation

Each phase ends with a goal (see `GOALS.md`) that has a deterministic verification command. Phases are written in execution order; later phases may freely depend on earlier ones.

### Phase 0 — Toolchain & build baseline (no behavior change)

**Files:** `build.rs`, `vendor/libghostty-vt/build.zig`, `scripts/build_vendored_libghostty_vt.ps1` (new), `Cargo.toml`.

0.1 Add Windows MSVC + GNU triples to `build.rs::zig_target` so cargo doesn't panic on `cargo check --target x86_64-pc-windows-msvc`.
0.2 Verify the vendored `libghostty-vt` builds for `x86_64-windows-msvc` via `zig build -Dtarget=x86_64-windows-msvc -Demit-lib-vt`. If it fails: file finding in `docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md` and switch goal 0.2 to "produce the failure log and decide path forward".
0.3 Add `[target.'cfg(windows)'.dependencies]` with `windows-sys 0.59` and a minimal feature set (`Win32_Foundation`, `Win32_System_Pipes`, `Win32_System_Threading`, `Win32_Security`). Expand in later phases.
0.4 `scripts/build_vendored_libghostty_vt.ps1` — PowerShell sibling of the existing shell script, callable from PS or `pwsh`.

**Phase 0 acceptance:** `cargo check --target x86_64-pc-windows-msvc` succeeds (may still fail to *link* because no Windows-side code paths exist yet — that's expected). `cargo build` on Linux still succeeds unchanged.

### Phase 1 — Cross-platform paths

**Files:** `src/config/io.rs`, `src/session.rs`, `src/server/socket_paths.rs`, `src/persist/io.rs` (already mostly fine).

1.1 `config_dir()` and `state_dir()`: introduce `dirs_dir()` helpers that on Windows return `%APPDATA%\\herdr` / `%LOCALAPPDATA%\\herdr` (no `dirs` crate — read env vars directly).
1.2 `data_dir_for(name)`: same pattern.
1.3 Tests use `std::env::temp_dir()` instead of hard-coded `/tmp/...`. Audit every test for `/tmp` literals.

**Phase 1 acceptance:** `cargo test --target host -- config::` and `session::` pass on Linux; on Windows the same tests pass when run later (gating verified in Phase 8).

### Phase 2 — Platform-windows module (process / clipboard / notifications)

**Files:** `src/platform/mod.rs`, `src/platform/windows.rs` (new), `src/platform/shell.rs` (new).

2.1 Wire `#[cfg(windows)] mod windows; #[cfg(windows)] pub use windows::*;` in `src/platform/mod.rs` ahead of the existing fallback gate.
2.2 Implement each function in §3.3 with a single-purpose unit test. Use `windows-sys` only.
2.3 Implement `src/platform/shell.rs` per §3.4.
2.4 Job-object wiring: extend `signal_processes` and add a paired `assign_to_job(pid)` helper so `pane.rs::Spawn` (Phase 6) can assign newly-spawned children to a kill-on-close job.

**Phase 2 acceptance:** `cargo test --target x86_64-pc-windows-msvc -p herdr platform::windows::` passes (manually or in CI) — 8+ tests covering each function on small fixtures.

### Phase 3 — Local-socket transport abstraction

**Files:** `src/transport/mod.rs` (new), `src/transport/unix.rs` (new), `src/transport/windows.rs` (new), `src/ipc.rs` (move helpers into `transport`).

3.1 Define the `LocalStream`/`LocalListener` types per §3.1.
3.2 Implement Unix variant — should be ≈100 LOC mostly forwarding to `std::os::unix::net::*`. Includes `prepare_socket_path`/`restrict_socket_permissions`.
3.3 Implement Windows variant via `CreateNamedPipeW`/`CreateFileW`. Includes `prepare_socket_path` (deletes/recreates pipe name namespace — usually a no-op) and `restrict_socket_permissions` (sets DACL allowing only the current user SID).
3.4 Implement `pair()` for tests.
3.5 A comprehensive `#[cfg(test)]` suite exercising every method: connect/accept happy path, set_nonblocking, set_read_timeout, try_clone, pair, shutdown(Read/Write/Both), oversized reads, EOF detection.

**Phase 3 acceptance:** `cargo test transport::` passes on both Unix and Windows.

### Phase 4 — Migrate IPC consumers to the transport

Mechanically rewrite every `UnixStream`/`UnixListener` reference (18 files from §1.1) to `LocalStream`/`LocalListener`. No behavior change.

4.1 `src/ipc.rs` → re-exports from `src/transport/` (back-compat).
4.2 `src/server/{socket_paths,client_accept,client_transport,autodetect,headless}.rs` — straightforward sed-like substitutions; the Read/Write/try_clone surface matches.
4.3 `src/client/{mod,input}.rs` — same; `input.rs` keeps Unix raw-fd code under `#[cfg(unix)]` and adds a `crossterm`-driven sibling for Windows (see §3.5).
4.4 `src/api/{server,client,wait}.rs` — line-protocol clients move straight over; `BufReader<LocalStream>` works because `LocalStream: Read`.
4.5 `src/session.rs`, `src/update.rs`, `src/raw_input.rs`, `src/remote.rs` — same.
4.6 `src/protocol/wire.rs::tests::framing_over_unix_socketpair` → `framing_over_pair`.
4.7 Replace `fits_unix_socket_path` usages: keep the function but make it short-circuit `true` on Windows.

**Phase 4 acceptance:** `cargo build` succeeds on Linux *and* `cargo build --target x86_64-pc-windows-msvc` succeeds (still links because Phase 0 + Phase 2 + Phase 3 are in place). All existing Linux/macOS tests still pass.

### Phase 5 — PTY / shell / restore wrapper integration

**Files:** `src/pane.rs`.

5.1 Replace `/bin/sh` defaults with `crate::platform::shell::default_shell()`.
5.2 Replace `RESTORE_WRAPPER_SCRIPT` POSIX text with a struct returned by `restore_wrapper_script()` — `{ script: &'static str, args: fn(...) -> Vec<String> }` so a Windows ps1 variant is selected.
5.3 Replace `CommandBuilder::new("/bin/sh"); cmd.arg("-c"); cmd.arg(command);` with `crate::platform::shell::shell_command(cmd)` helper.
5.4 Assign every spawned child to a kill-on-close Job Object on Windows (calls `crate::platform::assign_to_job(pid)` from Phase 2.4).

**Phase 5 acceptance:** A new test `pane::tests::pane_shell_default_is_platform_appropriate` checks the platform-correct default. `tests/api_ping.rs::pane_run_writes_visible_text` (cross-platform smoke) passes when run on a Windows runner.

### Phase 6 — End-to-end smoke on Windows

**Files:** `docs/plans/windows-port/SMOKE.md` (new).

6.1 Document a manual smoke test for Windows Terminal: launch, create workspace, split pane, run `Get-Process | Select -First 5`, detach, reattach, kill server.
6.2 A GitHub Actions job `windows-smoke` on `windows-latest` runner: `cargo build --release` + run the existing `tests/auto_detect.rs` cross-platform path against a `pwsh.exe` agent.

**Phase 6 acceptance:** the `windows-smoke` job is green; the smoke checklist passes when executed manually.

### Phase 7 — Clipboard, notifications, integration scripts

**Files:** `src/platform/windows.rs` (already has stubs from Phase 2 — flesh out), `src/integration/assets/*/herdr-agent-state.ps1` (new), `src/integration/mod.rs`.

7.1 Implement `write_clipboard` and `read_clipboard_image` using `windows-sys` Win32 clipboard APIs.
7.2 Implement `show_desktop_notification` with PowerShell `BurntToast` fallback (best-effort, returns `false` if PowerShell is missing — matches existing macOS behavior).
7.3 Add `.ps1` siblings for every `.sh` integration asset; pick at install time based on host OS.

**Phase 7 acceptance:** `cargo test --target x86_64-pc-windows-msvc platform::windows::clipboard` passes; an integration test installs the Claude hook and verifies it writes to the herdr socket from PowerShell.

### Phase 8 — Test suite portability

**Files:** every file under `tests/`, plus pockets of `#[cfg(test)] mod tests` in `src/`.

8.1 Audit every hard-coded `/tmp/...` and `sh -c` in tests. Replace with `std::env::temp_dir()` and `crate::platform::shell::shell_command`.
8.2 Add `#[cfg(unix)]` to tests that genuinely cannot run on Windows (e.g. tests that verify `chmod 0o600` socket perms).
8.3 Add `tests/support/local_socket.rs` helper exposing `pub fn unique_socket_path(prefix: &str) -> PathBuf` for cross-platform.

**Phase 8 acceptance:** `cargo nextest run --target x86_64-pc-windows-msvc` produces zero failures (skipped Unix-only tests are fine).

### Phase 9 — Updater + release manifest

**Files:** `src/update.rs`, `website/latest.json` (no-op locally; schema doc only), `AGENTS.md` (mention Windows assets).

9.1 Add `windows-x86_64` / `windows-aarch64` to the asset-key match.
9.2 Document the schema change in `AGENTS.md::Releases` (leave actual GitHub Actions workflow update to maintainer).

**Phase 9 acceptance:** `cargo test update::tests::asset_key_for_windows` passes; manifest schema doc updated.

### Phase 10 — Documentation, AGENTS.md, README

**Files:** `docs/next/README.md`, `docs/next/CHANGELOG.md`, `AGENTS.md`, `docs/next/website/src/content/docs/installation.mdx`.

10.1 Add Windows install instructions (download from releases, double-click or `winget install` if maintainer publishes one later).
10.2 Add the new Windows OS row to the `## Principles` doc and supported-platform table.
10.3 Cross-link `SMOKE.md` and `GOALS.md`.

**Phase 10 acceptance:** docs build cleanly; `just release-docs-check` analogue passes.

---

## 6. Verification Strategy

Two levels:

1. **Per-phase /goal conditions** (in `GOALS.md`) — each condition is a shell command whose exit code (0 / non-zero) plus output text the /goal evaluator can read in the transcript.
2. **End-to-end CI matrix** — Linux + macOS + `windows-latest`, all building & testing. Configured in a separate PR after this port lands.

For the duration of porting work, the Linux/macOS jobs must stay green at every commit. The Windows job starts as `cargo check` only (phase 0–3), then `cargo build` (phase 4), then `cargo test` (phase 5+).

---

## 7. Commit hygiene

Following `AGENTS.md`:
- Conventional commits, lowercase.
- One commit per sub-goal where reasonable.
- Reference issue number (TBD, file an issue once this plan is approved) on every commit via `refs #<n>`.
- Stay on a single task branch under `../herdr-worktrees/windows-port` (or local equivalent `C:/AI/Claude/herdr/worktrees/windows-port` since this repo is on Windows).
- Bump `PROTOCOL_VERSION` exactly once across the whole port (likely Phase 4) — no breaking-protocol changes; the bump is to flag that older clients haven't been tested with named-pipe transport.

---

## 8. Reading order for the implementer

If you (Claude or human) pick this up cold:
1. Read this file front to back.
2. Read `GOALS.md` — pick the next pending goal.
3. Read the files listed under that goal's "files touched" before changing anything.
4. Implement → check → commit → mark goal achieved.

Skip back to §3 (Architectural Decisions) whenever a sub-goal feels ambiguous; that section is the single source of truth for design choices.
