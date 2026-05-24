# CLAUDE.md — Windows port of herdr

This file is loaded automatically by Claude Code. It supplements `AGENTS.md`
(which holds the general project conventions) with context that is specific
to the in-progress **Windows port** on branch `windows-port`.

## Where the work lives

- Roadmap: `docs/plans/windows-port/GOALS.md` (10 numbered goals, each with
  acceptance criteria). `PLAN.md` next to it has the per-goal sub-tasks.
- Known blocker: `docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md`
  (Bitdefender quarantines a wuffs test fixture during the Zig build).
- All work happens on branch `windows-port`. Commits are sequential and
  follow the `AGENTS.md` conventions: lowercase conventional commits,
  goal reference in the body, never `--no-verify`.

## Running cargo on Windows

Always invoke cargo through the wrapper:

```powershell
pwsh scripts/cargo_msvc.ps1 check --target x86_64-pc-windows-msvc
pwsh scripts/cargo_msvc.ps1 test  --target x86_64-pc-windows-msvc --bin herdr transport::
```

The wrapper loads `vcvars64.bat` and appends the Windows SDK `Lib\<ver>\um\x64`
path that vcvars omits. Plain `cargo` will pick up Git Bash's
`/usr/bin/link.exe` and fail.

If `build.rs` needs to skip the vendored Zig build (because of the AV
blocker), set `LIBGHOSTTY_VT_SKIP_BUILD=1`. `build.rs` only emits link
directives when `ghostty-vt-static.lib` actually exists on disk.

## Architectural shape of the port

- **Progressive un-gating.** Modules start wrapped in `#[cfg(unix)]` and are
  un-gated one goal at a time as their dependencies become cross-platform.
  After Goal 3, the un-gated set is: `config`, `detect`, `input`, `ipc`,
  `platform`, `session`, `sound`, `transport`.
- **Local sockets / named pipes** live behind `src/transport/{mod,unix,windows}.rs`.
  Use `crate::transport::{LocalStream, LocalListener, pair, prepare_socket_path,
  restrict_socket_permissions}` — not `std::os::unix::net::*` — in any code
  that needs to compile on Windows.
- **Process / clipboard / notification primitives** live behind
  `src/platform/{mod,unix,windows}.rs`. The Windows side uses `windows-sys 0.59`
  (selected features declared in the per-target `[target.'cfg(windows)'.dependencies]`
  table in `Cargo.toml`).
- The Windows backend uses **named pipes with `OVERLAPPED` IO**. Two important
  invariants:
  1. `ERROR_BROKEN_PIPE` / `ERROR_PIPE_NOT_CONNECTED` / `ERROR_NO_DATA` in a
     read context must be mapped to `Ok(0)` so EOF semantics match
     `std::os::unix::net::UnixStream`.
  2. `pair()` uses two **unidirectional** pipes (`PIPE_ACCESS_INBOUND` /
     `PIPE_ACCESS_OUTBOUND`) so `shutdown(Write)` tears down one direction
     without breaking the other.

## Toolchain pins

- Rust 1.95.0, MSVC toolchain (`x86_64-pc-windows-msvc`).
- **Zig 0.15.2** — pinned. `libghostty-vt/build.zig` calls
  `requireZig(0.15.2)`. Winget's 0.16.0 fails this check. Manual install at
  `%LOCALAPPDATA%\zig-x86_64-windows-0.15.2`.
- Visual Studio Build Tools 2022 at `C:\BuildTools` (VC++ 14.44 + Windows
  SDK 10.0.26100).

## Working style

`/goal` is the primary driver. When the user says "weiter", advance to the
next pending row in `GOALS.md`, work through `PLAN.md`'s sub-tasks, commit
the goal, and stop. Do not pause for clarifying questions inside a `/goal`
loop — make the reasonable call and continue; the user will redirect.
