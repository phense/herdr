# Windows manual smoke test

Goal 6 of the [windows port](PLAN.md) asks for a 15-step manual checklist
that an operator can follow from inside Windows Terminal to convince
themselves that the windows build is feature-complete-ish for local
single-user use. The list intentionally exercises every code path that
goals 0–5 added on the windows side: named-pipe transport (goal 3),
local socket IPC consumers (goal 4), pwsh-backed `platform::shell` (goal 5),
Job-Object kill-on-close (goals 2 + 5), `%APPDATA%`/`%LOCALAPPDATA%`
config dirs (goal 1), and the platform clipboard + notification helpers
(goal 2).

> **Scope.** This file is the operator-facing companion to `windows-smoke`
> in `.github/workflows/ci.yml`. CI verifies that the binary builds and
> the unit tests pass on `windows-latest`; SMOKE.md verifies that the
> actual UX is intact. Some steps (steps 11–15) only work after goal 7
> lands the BurntToast / clipboard wiring; they are flagged inline.

## Prerequisites

1. Windows 10 22H2 or Windows 11 with Windows Terminal installed
   (`winget install Microsoft.WindowsTerminal` or via the Microsoft Store).
2. PowerShell 7 (`pwsh.exe`) on PATH. The `platform::shell::default_shell`
   resolver falls back to `powershell.exe` and then `cmd.exe`, but step 3
   below specifically checks the preferred pwsh path.
3. A release build of herdr from the windows-port branch:

   ```pwsh
   git switch windows-port
   pwsh scripts\cargo_msvc.ps1 build --release --target x86_64-pc-windows-msvc
   ```

   The binary lands at `target\x86_64-pc-windows-msvc\release\herdr.exe`.

4. (Recommended) Install the `BurntToast` PowerShell module so step 14
   shows a real toast instead of returning `Ok(false)`:

   ```pwsh
   Install-Module BurntToast -Scope CurrentUser
   ```

## 15-step checklist

Run each step in order from a fresh Windows Terminal window. Use the
release binary built above (`herdr.exe` for short below). If a step
fails, stop and capture the herdr log
(`%LOCALAPPDATA%\herdr\herdr.log`) plus a screenshot before moving on.

### 1. Launch

```pwsh
herdr.exe
```

Expected: the TUI fills the terminal, an initial workspace `default`
appears in the sidebar, and a single pane shows a pwsh prompt. No
error toasts in the status bar.

### 2. Default shell is pwsh

In the focused pane, run:

```pwsh
$PSVersionTable.PSVersion
```

Expected: PowerShell 7.x prints. If you see "Windows PowerShell" (5.x),
the resolver fell through to `powershell.exe` — that is also a passing
result but note it on the smoke report.

### 3. Workspace dir landed in `%LOCALAPPDATA%`

In a separate Windows Terminal tab (do not close herdr), check:

```pwsh
Get-Content $env:LOCALAPPDATA\herdr\herdr.log -Tail 5
```

Expected: log lines reference the named pipe path
`\\.\pipe\herdr-<sid>-<session>` and the data dir under
`%APPDATA%\herdr` or `%LOCALAPPDATA%\herdr`.

### 4. Spawn a pwsh one-liner inside a new pane

In the herdr TUI, press the prefix (default `Ctrl+B`) then `v` to split
the focused pane vertically. In the new pane run:

```pwsh
Write-Output "herdr-ok"
```

Expected: `herdr-ok` is rendered in the new pane within ~100 ms.

### 5. Tab creation

Press prefix + `c` to create a new tab. Name it `tab-2` when prompted.
Expected: the tab strip in the sidebar shows both `tab-1` and `tab-2`,
focused on `tab-2`.

### 6. Tab switching

Press prefix + `1` then prefix + `2`. Expected: focus moves between the
two tabs and the visible pane contents update accordingly.

### 7. Horizontal split + pane focus

In `tab-2`, press prefix + `-` to split horizontally. Press
prefix + `h` / `l` to move focus between left and right panes. The
focused pane border should highlight in the accent color.

### 8. Pane resize

Press prefix + `r` to enter resize mode, then use the arrow keys to
grow the focused pane by ~5 columns. Press `Esc` to leave resize mode.
Expected: the resize survives a redraw.

### 9. Workspace creation

Press prefix + `Shift+N` and create a workspace called `smoke-ws`.
Expected: `smoke-ws` appears in the sidebar and becomes focused with
an empty pane.

### 10. Run a longer-lived command + Ctrl-C

In `smoke-ws` run:

```pwsh
1..30 | ForEach-Object { Start-Sleep -Milliseconds 500 ; Write-Output "tick $_" }
```

Expected: the pane prints ticks. After ~3 ticks, hit `Ctrl+C` inside
the pane — the loop stops, prompt returns, and herdr does not crash.
This validates that `GenerateConsoleCtrlEvent(CTRL_C_EVENT, …)` reaches
the pwsh child.

### 11. Clipboard write *(post-goal 7)*

Highlight some pane text and copy it through herdr's selection action
(default keybind: prefix + `y` over a selection). Switch to another
Windows Terminal window and paste with `Ctrl+V`. Expected: the
selected herdr text appears.

> Until goal 7 wires `crate::platform::write_clipboard` into
> `src/server/clipboard_image.rs`, this step is expected to fail
> silently — skip it.

### 12. Clipboard image paste round-trip *(post-goal 7)*

Take a screenshot (`Win+Shift+S`) so the image is on the clipboard.
In a herdr pane press `Ctrl+V`. Expected: a PNG is staged under
`%TEMP%\herdr-clipboard-images-<sid>\` and the pane receives a
`@<path>.png` paste marker.

> Skip until goal 7 lands the clipboard staging port.

### 13. Toast notification *(post-goal 7)*

From the smoke pane run:

```pwsh
herdr.exe pane notify --kind system-toast --message "smoke title: smoke body"
```

Expected: a Windows toast appears in the bottom-right notification
area. If `BurntToast` is not installed, the command returns success
with no toast — that is the documented fallback (`Ok(false)`).

### 14. Detach + reattach

Press prefix + `q` to detach. Expected: the TUI exits cleanly and the
operator returns to the launching pwsh prompt. Then run:

```pwsh
herdr.exe
```

Expected: the TUI reattaches to the same server — `smoke-ws` is still
present along with its panes and the `tick` history from step 10.

### 15. Server stop + Job-Object teardown

While detached, run:

```pwsh
herdr.exe server stop
```

Expected: command exits 0 within ~1 s. Verify no orphan pwsh / cmd
children remain:

```pwsh
Get-Process pwsh, cmd -ErrorAction SilentlyContinue |
    Where-Object { $_.Parent.Name -eq 'herdr' }
```

Expected: no output. The kill-on-close Job Object set up in goal 2
should have brought every pane child down with the server.

## Reporting

Open a PR (or comment on the existing windows-port PR) titled
`smoke: <date> windows`, attaching:

- Which build sha was tested (`git rev-parse HEAD`).
- The output of `herdr.exe --version`.
- Pass/fail per step, with the herdr log excerpt for any failure.
- Whether step 2 returned pwsh 7 or fell through to PowerShell 5.

A green smoke pass is required before the windows port lands on
`master`.
