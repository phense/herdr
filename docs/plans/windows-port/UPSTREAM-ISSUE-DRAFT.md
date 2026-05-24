# Upstream issue draft — Windows port

This file is a personal scratch pad for the **issue** I plan to file against
`ogulcancelik/herdr` once the windows port is feature-complete enough to
discuss. Per [CONTRIBUTING.md](../../../CONTRIBUTING.md), first-time
contributors must open an issue first and wait for `/approve` before any PR.

Use the `change.yml` issue template at
<https://github.com/ogulcancelik/herdr/issues/new/choose>. Paste the
sections below into the matching template fields. Do **not** edit the
template fields after submission to add `/i-intend-to-pr`; either check
the contribution-intent box, or include the literal string
`/i-intend-to-pr` in the issue body when filing through the GitHub CLI.

---

## Suggested title

```
Add Windows support (named-pipe IPC, ConPTY panes, %APPDATA% paths)
```

Short, factual, mentions the three biggest moving parts. Easy for the
maintainer to triage.

---

## What is the current behavior

herdr builds on `cargo build` for `x86_64-unknown-linux-gnu` and
`x86_64-apple-darwin` / `aarch64-apple-darwin`. On Windows targets the
build panics in `build.rs::zig_target`, and even past that point the
code base hard-codes:

- `std::os::unix::net::{UnixStream, UnixListener}` for every IPC path
  (client socket, JSON API, session liveness probes, SSH bridge).
- `/bin/sh -c …` for the pane shell, the agent-restore wrapper, and
  every `spawn_shell_command` call site.
- `libc::{kill, pid_t, WNOHANG, …}` and `std::os::unix::fs::PermissionsExt`
  in `src/server/clipboard_image.rs`, `src/update.rs`, integration tests, etc.
- XDG / `$HOME/.config` path conventions in `src/config/io.rs` and
  `src/session.rs::data_dir_for`.

The net result: no path to running herdr on Windows 10/11 even inside
Windows Terminal, where ConPTY would otherwise make it natural.

## What do you want to change

Add a Windows port that preserves Linux/macOS behavior bit-for-bit and
adds a Windows host-OS surface for local single-user mode (Phase A in
the linked plan):

- `herdr.exe` builds on `x86_64-pc-windows-msvc` with stable Rust.
- Local single-process mode (`herdr --no-session`) spawns a pwsh pane in
  Windows Terminal, supports splits/tabs/workspaces/detach-reattach.
- Persistent session server uses Win32 named pipes
  (`\\.\pipe\herdr-<sid>-<session>`) instead of Unix sockets.
- ConPTY panes via the existing `portable-pty 0.9` dep — no PTY
  abstraction change needed.
- A `windows-smoke` job on `windows-latest` keeps Windows healthy from CI
  forward.

Phase B (Windows clipboard wiring, BurntToast desktop notifications,
`.ps1` agent-integration scripts, SSH bridge, updater asset keys) is
scoped but split off into follow-up issues; this issue is about Phase A
landing first.

## Why this change belongs in herdr

- **Reach.** A large fraction of coding-agent users run on Windows
  (Windows Terminal + pwsh + WSL-adjacent workflows). Today herdr is
  invisible to them.
- **Locality.** All Windows-specific code stays inside `src/platform/`
  and `src/transport/` per [AGENTS.md](../../../AGENTS.md) §Principles
  ("Platform code is isolated. Core modules don't have
  `#[cfg(target_os)]`."). The port introduces:
    - `src/platform/windows.rs` (Win32 Job Objects, console signals,
      clipboard, BurntToast hook, NtQueryInformationProcess for cwd)
    - `src/platform/shell.rs` (cross-platform default-shell resolver +
      restore wrapper)
    - `src/transport/{mod,unix,windows}.rs` (`LocalStream` /
      `LocalListener` cross-platform shim wrapping
      `std::os::unix::net::*` on unix and named pipes with overlapped
      IO on windows)
  No new `#[cfg(target_os)]` lands outside those two directories.
- **Protocol unchanged.** No `PROTOCOL_VERSION` bump. The wire format is
  byte-identical between platforms. An older Linux client could in
  principle attach to a Windows server (and vice versa) over a TCP
  forwarder; not a goal, but not blocked either.
- **No new dependencies on Linux/macOS.** `windows-sys 0.59` is added
  under `[target.'cfg(windows)'.dependencies]` so the unix tree's
  dep graph is unchanged.

## Does this change UI, interaction, or workflow expectations

No.

- The TUI, keybindings, sidebar, settings, onboarding, prefix-mode
  actions, mouse capture, integration-script flow are byte-identical.
- The CLI surface is byte-identical (`herdr`, `herdr session …`,
  `herdr workspace …`, `herdr update`, etc.).
- The config schema is unchanged. `herdr --default-config` produces the
  same text on every platform.
- The release model is unchanged; this issue does NOT ask for new
  release artifacts. The follow-up updater issue covers
  `windows-x86_64` / `windows-aarch64` asset keys.

The only operator-visible difference: on Windows the default shell is
`pwsh.exe` / `powershell.exe` / `cmd.exe` (in that fallback order),
the data dir lives under `%APPDATA%\herdr` (config) and
`%LOCALAPPDATA%\herdr` (state), and the IPC endpoint is a named-pipe
path rather than a `.sock` file.

## I intend to implement it myself

Yes — the port already exists on a fork branch and runs locally for me.
I am filing this issue first per `CONTRIBUTING.md` before opening a PR.

`/i-intend-to-pr`

## Plan + status reference

The branch lives at
`https://github.com/phense/herdr/tree/windows-port` and contains:

- [`docs/plans/windows-port/PLAN.md`](https://github.com/phense/herdr/blob/windows-port/docs/plans/windows-port/PLAN.md)
  — architectural plan (10 phases, risk register, decision log,
  reading order).
- [`docs/plans/windows-port/GOALS.md`](https://github.com/phense/herdr/blob/windows-port/docs/plans/windows-port/GOALS.md)
  — testable `/goal` conditions for each phase.
- [`docs/plans/windows-port/SMOKE.md`](https://github.com/phense/herdr/blob/windows-port/docs/plans/windows-port/SMOKE.md)
  — 15-step manual Windows Terminal smoke checklist.
- [`docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md`](https://github.com/phense/herdr/blob/windows-port/docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md)
  — blocker write-up for the vendored libghostty-vt Zig build on
  AV-protected boxes.

Branch status at filing time (regenerated before submission):

| Goal | Commit |
|---|---|
| 0 — toolchain + scaffolding | `8ebf84c` |
| 1 — `%APPDATA%` / `%LOCALAPPDATA%` paths | `ded4e94` |
| 2 — `src/platform/windows.rs` (job objects, signals, clipboard, notifications) | `d5c67e0` |
| 3 — `src/transport/{mod,unix,windows}.rs` | `caa0023` |
| 4 — migrate IPC consumers (~16 files) to `crate::transport` | `99964aa` |
| 5 — `src/platform/shell.rs` + pane.rs `/bin/sh` removal | `ad60372` |
| 6 — `windows-smoke` CI job + `SMOKE.md` | `a8a116d` |

`cargo build --release --locked --target x86_64-pc-windows-msvc`
finishes in ~3m40s on a fresh Windows runner. The full unit test suite
plus the cross-platform `pane_smoke` integration test pass under
`cargo nextest run` on Windows; the Linux/macOS host-side tests are
unchanged (existing CI matrix on `windows-port` is green for
Linux+macOS, plus the new `windows-smoke` job green).

## Two PR shapes you could pick

I am happy to land this either way; please let me know your preference
before I open the PR:

**Option A — one MVP PR (recommended)**

One PR containing all eight Phase-A commits as-is. Smaller review
surface for the project as a whole, single CI gate to look at, single
revert if needed. Total diff is ~3000 lines, of which ~2000 are the new
`src/platform/windows.rs` + `src/transport/windows.rs`.

**Option B — stacked PRs**

PR 1: toolchain + paths (goals 0 + 1) — risk-free build-only.
PR 2: platform + transport (goals 2 + 3) — the bulk of new code.
PR 3: IPC migration (goal 4) — mechanical rename + cfg-gate plumbing.
PR 4: PTY shell + CI (goals 5 + 6).

I prefer **Option A**; happy to split into Option B if you'd rather
review in stages.

## Follow-up issues I will file after this one

Held back so this issue stays scoped to Phase A:

- **Windows clipboard + notifications wiring** — connect
  `crate::platform::write_clipboard`, `read_clipboard_image`, and
  `show_desktop_notification` into the existing
  `src/server/clipboard_image.rs` and `src/server/notifications.rs`
  call sites. Adds `.ps1` siblings to every
  `src/integration/assets/*/herdr-agent-state.sh`.
- **Windows test suite portability** — undo the blanket
  `#![cfg(unix)]` on `tests/*.rs` and add
  `tests/support/local_socket.rs` so the existing api/server/session
  integration tests run on Windows.
- **Windows updater asset keys** — `windows-x86_64` and
  `windows-aarch64` keys in `src/update.rs::asset_key()` plus the
  `latest.json` schema doc update in `AGENTS.md::Releases`.
- **SSH bridge for `herdr --remote` on Windows** — replaces the
  current `cfg(windows)` "not yet supported" early return.

## Anything else

Quick acknowledgement of CONTRIBUTING.md guardrails I followed in
preparing the port: lowercase conventional commits, no `unwrap()` in
production, no edits to root `README.md` / `CHANGELOG.md`, no
`#[cfg(target_os)]` outside `src/platform/` and `src/transport/`,
`refs <plan>` lines in commit bodies (will rewrite to `refs #<this-issue>`
before the PR if approved).

I can explain any chunk of the port in detail.

---

## After-filing checklist (for me, not the maintainer)

- [ ] Issue filed, `/i-intend-to-pr` label set by GitHub Actions or
      manually
- [ ] Maintainer comment `/approve` received
- [ ] Add my username to `.github/APPROVED_CONTRIBUTORS` in the PR
- [ ] Revert the `branches: [master, "windows-port"]` workflow trigger
      addition in `.github/workflows/ci.yml` (keep the `windows-smoke`
      job, drop the branch-specific push trigger)
- [ ] Delete `CLAUDE.md` from the branch (personal orientation file,
      not project material)
- [ ] Rewrite commit bodies to use `refs #<issue-number>` instead of
      `refs docs/plans/windows-port/GOALS.md`
- [ ] `git fetch upstream && git rebase upstream/master` to pick up
      anything that landed during the issue discussion
- [ ] WSL or Linux box: `just ci` clean before opening the PR
- [ ] Open the PR
