# libghostty-vt on Windows — current state and next steps

Captured on 2026-05-24 during Goal 0 (toolchain baseline) execution.

## Status

`zig build -Demit-lib-vt -Dtarget=x86_64-windows-msvc -Doptimize=ReleaseFast`
inside `vendor/libghostty-vt/` currently **fails** on the development host with
the following error chain:

```
error: unable to hash 'wuffs-0.4.0-alpha.9\test\data\artificial-jpeg\hippopotamus-bad-comment-length.jpeg': AccessDenied
…
pkg/wuffs/build.zig.zon:9:21: error: hash mismatch:
  manifest declares 'N-V-__8AAAzZywE3s51XfsLbP9eyEw57ae9swYB9aGB6fCMs'
  but the fetched package has 'N-V-__8AAEXUywEb8JCSytwiCVUsFb2CwHjOB59jhyRhOhsj'
```

## Root cause

The libghostty-vt build pulls the [wuffs](https://github.com/google/wuffs)
codec library (via `pkg/wuffs/build.zig.zon`). The wuffs source tarball
includes deliberately-malformed JPEG fixtures under
`test/data/artificial-jpeg/`. Bitdefender (and Microsoft Defender, when
real-time protection is on) flag those fixtures as suspicious and quarantine
them during archive extraction. Zig's package manager then computes the hash
of the partial extraction and fails the integrity check.

Verified by inspecting the cache:

- `%LOCALAPPDATA%\zig\tmp\<id>\wuffs-0.4.0-alpha.9\test\data\artificial-jpeg\`
  contains `hippopotamus-sof-dht-swap.jpeg` (sibling fixture) and
  `make-hippopotamus-bad-comment-length.go` (generator script) but the
  flagged `hippopotamus-bad-comment-length.jpeg` is missing.
- `%LOCALAPPDATA%\zig\p\` contains a wuffs entry under the **observed** hash
  (`N-V-__8AAEXUywE…`) but not under the **expected** hash
  (`N-V-__8AAAzZyw…`).

## Workarounds (in order of preference)

### 1. Add an antivirus exclusion (recommended for any real Windows build)

Run elevated PowerShell on the build machine:

```powershell
Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\zig"
```

For Bitdefender: GUI → *Protection* → *Antivirus* → *Settings* →
*Manage exceptions* → add the `zig` cache path.

After excluding, delete the corrupted cache and re-run:

```powershell
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\zig\p"
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\zig\tmp"
pwsh -File scripts/build_vendored_libghostty_vt.ps1 -- -Dtarget=x86_64-windows-msvc
```

### 2. Use `LIBGHOSTTY_VT_SKIP_BUILD=1` for development without the lib

`build.rs` now honors `LIBGHOSTTY_VT_SKIP_BUILD=1`. With this env var set,
the vendored Zig build is skipped entirely. Link directives are still
emitted, so `cargo check`, `cargo metadata`, and `rust-analyzer` all
succeed. A real `cargo build` will fail at the link step with a clean
"library not found" message until a real `ghostty-vt-static.lib` is placed
in `vendor/libghostty-vt/zig-out/lib/`. Use this for iterating on Rust-side
code that doesn't actually call into the C library (the transport,
platform, and JSON-API modules being the primary targets of the port).

```powershell
$env:LIBGHOSTTY_VT_SKIP_BUILD = '1'
cargo check --target x86_64-pc-windows-msvc
```

### 3. Build libghostty-vt on a CI machine without AV interference

GitHub Actions' `windows-latest` runner does not have Bitdefender; Microsoft
Defender's tamper protection there is permissive of build-tool extraction.
Once Goal 6 lands a Windows CI job, we can publish the built
`ghostty-vt-static.lib` as a CI artifact and consume it from local dev
machines via `LIBGHOSTTY_VT_SKIP_BUILD=1` plus a copy step.

## Next steps

- [ ] Verify the wuffs hash matches on a clean (AV-excluded) Windows VM.
- [ ] Once verified, document the AV exclusion as a prerequisite in
      `docs/next/CONTRIBUTING.md`'s Windows section (Goal 10).
- [ ] In Goal 6 (CI), make the `windows-latest` job upload the built static
      lib so local dev machines can use it without rebuilding.
- [ ] Long-term: open a patch to ghostty-vt to either vendor wuffs locally
      (no archive re-fetch on each build) or to allow a `-Dskip-wuffs=true`
      build option for ports where JPEG decoding is not required.

## Build attempt log excerpt

```
$ env ZIG=$LOCALAPPDATA/zig-x86_64-windows-0.15.2/zig.exe \
      zig build -Demit-lib-vt -Dtarget=x86_64-windows-msvc -Doptimize=ReleaseFast
error: unable to hash 'wuffs-0.4.0-alpha.9\test\data\artificial-jpeg\hippopotamus-bad-comment-length.jpeg': AccessDenied
C:\AI\Claude\herdr\repo\vendor\libghostty-vt\pkg/wuffs\build.zig.zon:9:21: error:
  hash mismatch: manifest declares 'N-V-__8AAAzZywE3s51XfsLbP9eyEw57ae9swYB9aGB6fCMs'
  but the fetched package has 'N-V-__8AAEXUywEb8JCSytwiCVUsFb2CwHjOB59jhyRhOhsj'
            .hash = "N-V-__8AAAzZywE3s51XfsLbP9eyEw57ae9swYB9aGB6fCMs",
                    ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
```

Tools verified:
- `rustc 1.95.0 (59807616e 2026-04-14)` — installed via `rustup update stable`.
- `zig 0.15.2` — manually downloaded from `https://ziglang.org/download/0.15.2/`
  (winget's `zig.zig` package installs 0.16.0, which is rejected by
  `requireZig(0.15.2)` in `vendor/libghostty-vt/build.zig`).
