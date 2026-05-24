//! Cross-platform shell-spawn smoke test for goal 5.
//!
//! `tests/*` integration files can't reach private `herdr::platform::shell::*`
//! items (the crate is a binary, not a library), so this test duplicates the
//! minimal shell-selection logic. It exercises the same contract: the platform
//! default shell can be launched, run a one-liner, and emit `herdr-ok` on
//! stdout within a few seconds.
//!
//! The full pane-runtime / portable-pty smoke test that the goals doc
//! described still lives behind libghostty-vt + ConPTY work that lands in
//! goals 6+. Until then this stand-in keeps the contract covered on both
//! unix and windows.

use std::process::Command;
use std::time::{Duration, Instant};

fn windows_shell_on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(name).is_file())
}

fn smoke_command() -> (String, Vec<String>) {
    if cfg!(windows) {
        for candidate in ["pwsh.exe", "powershell.exe"] {
            if windows_shell_on_path(candidate) {
                return (
                    candidate.to_string(),
                    vec![
                        "-NoProfile".to_string(),
                        "-Command".to_string(),
                        "Write-Output herdr-ok".to_string(),
                    ],
                );
            }
        }
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        (comspec, vec!["/C".to_string(), "echo herdr-ok".to_string()])
    } else {
        (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), "printf 'herdr-ok\\n'".to_string()],
        )
    }
}

#[test]
fn spawns_shell_runs_command() {
    let (program, args) = smoke_command();
    let started = Instant::now();
    let output = Command::new(&program)
        .args(&args)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn {program}: {err}"));
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "default shell took {elapsed:?} (>5s) to print herdr-ok via {program}"
    );
    assert!(
        output.status.success(),
        "default shell {program} exited with status {:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("herdr-ok"),
        "expected 'herdr-ok' in stdout from {program}, got: {stdout:?}"
    );
}
