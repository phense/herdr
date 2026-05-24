# Releasing a Windows build (fork)

This document covers how to ship a Windows `.exe` from the
[`windows-port`](https://github.com/phense/herdr/tree/windows-port) branch on
the **fork** (`phense/herdr`). Upstream `ogulcancelik/herdr` has a different
release workflow that this file does not touch.

The driver is `.github/workflows/build-windows-release.yml`. It has two
trigger paths:

| Trigger | What happens | Use when |
|---------|--------------|----------|
| `workflow_dispatch` (Actions UI button) | Build `herdr-windows-x86_64.exe`, upload as workflow artifact (7-day retention). No public release. | Sanity-checking that the build still works after a code change. |
| Push tag `windows-port-v*` | Same build, then publish a **prerelease** GitHub Release with the `.exe` + `BUILD_INFO.txt` attached. | Distributing a new build to beta testers. |

## Tag naming

Use `windows-port-v<cargo-version>-<iteration>`:

- `windows-port-v0.6.2-1` — first windows build of upstream 0.6.2
- `windows-port-v0.6.2-2` — second rev (e.g. bug fix on top of -1)
- `windows-port-v0.7.0-1` — first windows build after upstream bumps to 0.7.0

The `-<iteration>` suffix means we can cut multiple windows builds against
the same upstream version without colliding with upstream's `v0.6.2` tag.

## Pre-release checklist

1. CI green on `windows-port` for the head commit. Confirm with:
   ```
   gh api 'repos/phense/herdr/actions/runs?branch=windows-port&per_page=1' \
     --jq '.workflow_runs[].conclusion'
   ```
   Expect `success`.
2. Manual `SMOKE.md` checklist passes on a real Windows host.
3. `Cargo.toml` version matches the version segment of your planned tag.

## Cutting the release

```pwsh
# from C:\AI\Claude\herdr\repo, on branch windows-port
git tag windows-port-v0.6.2-1
git push origin windows-port-v0.6.2-1
```

That push fires `build-windows-release.yml`. After ~5–10 minutes the new
prerelease appears at:

<https://github.com/phense/herdr/releases/tag/windows-port-v0.6.2-1>

A direct download URL for the binary will be:

<https://github.com/phense/herdr/releases/download/windows-port-v0.6.2-1/herdr-windows-x86_64.exe>

## Verifying the release

```pwsh
# from the GitHub release page, grab the sha256 line out of BUILD_INFO.txt
# and compare against the downloaded .exe:
(Get-FileHash -Algorithm SHA256 herdr-windows-x86_64.exe).Hash.ToLower()
```

## If the build fails

The workflow uses the same `Swatinem/rust-cache@v2 / mlugg/setup-zig` setup
as `ci.yml::windows-smoke`. If `windows-smoke` is green but the release
build fails, the most likely culprits are:

- `cargo build --release --locked` mismatches on `Cargo.lock` (`ci.yml` uses
  the same flags — uncommon).
- A `libghostty-vt` build that regresses between debug and release modes
  (Zig optimize flags differ). Document in
  [`LIBGHOSTTY-WINDOWS-NOTES.md`](LIBGHOSTTY-WINDOWS-NOTES.md) if so.

To re-cut the release with a fix, delete the tag locally and on the remote
(GitHub auto-deletes the failed prerelease only if the workflow failed
before the release step ran):

```pwsh
git tag -d windows-port-v0.6.2-1
git push --delete origin windows-port-v0.6.2-1
# fix the issue, commit, then re-tag and push.
```

## Future extensions

- `aarch64-pc-windows-msvc` target: add a second matrix entry. Cross-compile
  is doable from `windows-latest` runners but requires explicit linker
  configuration; see the `aarch64-unknown-linux-musl` precedent in
  `.github/workflows/release.yml`.
- `.msi` installer: integrate `cargo-wix`. Requires a `wix/main.wxs`
  template plus `choco install wixtoolset` step on the runner.
- Code signing: an EV certificate (~USD 200/year) makes the SmartScreen
  warning go away. Out of scope for this fork.
