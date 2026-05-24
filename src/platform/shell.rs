//! Cross-platform shell + restore-wrapper helpers.
//!
//! Goal 5 of the windows port: pane.rs no longer hardcodes `/bin/sh`. The
//! default shell, `<shell> -c <cmd>` invocation, and the agent-restore wrapper
//! all live here so the pane code can stay platform-neutral. On unix nothing
//! changes; on windows we prefer pwsh.exe, then powershell.exe, then
//! `%COMSPEC%`/`cmd.exe`, and emit a PowerShell variant of the restore
//! wrapper script.

/// Returns the default shell program path for the current platform.
///
/// Honors `$SHELL` on unix and `%COMSPEC%` on windows, falling back to
/// `/bin/sh` and `cmd.exe` respectively when no environment hint is set
/// and no preferred shell is found on `PATH`.
pub fn default_shell() -> String {
    default_shell_from_env(
        std::env::var("SHELL").ok(),
        std::env::var("COMSPEC").ok(),
        windows_preferred_shell_on_path,
    )
}

/// Pure version of [`default_shell`] used by tests to inject mocked env vars
/// and a fake "is this windows shell on PATH" probe.
pub(crate) fn default_shell_from_env(
    unix_shell: Option<String>,
    windows_comspec: Option<String>,
    windows_probe: fn(&str) -> bool,
) -> String {
    if cfg!(windows) {
        for candidate in ["pwsh.exe", "powershell.exe"] {
            if windows_probe(candidate) {
                return candidate.to_string();
            }
        }
        return windows_comspec
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "cmd.exe".to_string());
    }

    unix_shell
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// Resolves the shell that should drive a pane: configured value wins, else
/// [`default_shell`].
pub fn pane_shell(configured: &str) -> String {
    pane_shell_from(configured, std::env::var("SHELL").ok())
}

/// Pure version of [`pane_shell`] used by tests to inject `$SHELL`.
pub fn pane_shell_from(configured: &str, env_shell: Option<String>) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    if cfg!(windows) {
        return default_shell();
    }
    env_shell
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

#[cfg(windows)]
fn windows_preferred_shell_on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        if dir.join(name).is_file() {
            return true;
        }
    }
    false
}

#[cfg(not(windows))]
fn windows_preferred_shell_on_path(_name: &str) -> bool {
    false
}

/// Returns `(program, args)` for "run this command line as a shell command".
///
/// - Unix: `("/bin/sh", ["-c", cmd])`.
/// - Windows: if pwsh/powershell is selected, `("pwsh.exe", ["-NoProfile", "-Command", cmd])`;
///   when cmd.exe is the chosen shell, `("cmd.exe", ["/C", cmd])`.
pub fn shell_command_args(cmd: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        let shell = default_shell();
        if is_cmd_exe(&shell) {
            return (shell, vec!["/C".to_string(), cmd.to_string()]);
        }
        return (
            shell,
            vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                cmd.to_string(),
            ],
        );
    }
    (
        "/bin/sh".to_string(),
        vec!["-c".to_string(), cmd.to_string()],
    )
}

/// Returns the body of the restore wrapper script.
///
/// On unix this is the historical POSIX `sh -c` script that runs the agent's
/// argv, prints a friendly diagnostic on early-exit failure, and then execs
/// a fallback shell. On windows it returns a PowerShell equivalent meant to
/// be invoked via `pwsh -NoProfile -Command`.
pub fn restore_wrapper_script() -> &'static str {
    if cfg!(windows) {
        RESTORE_WRAPPER_PS1
    } else {
        RESTORE_WRAPPER_SH
    }
}

const RESTORE_WRAPPER_SH: &str = r#"agent="$1"
fallback_shell="$2"
early_window="$3"
shift 3
	start="$(date +%s 2>/dev/null || printf 0)"
	"$@"
	status="$?"
	end="$(date +%s 2>/dev/null || printf 999999)"
	elapsed="$((end - start))"
	if [ "$status" -ne 0 ] && [ "$elapsed" -le "$early_window" ]; then
	  printf 'herdr: %s session restore failed; started a shell instead\n' "$agent"
	fi
	exec "$fallback_shell"
	"#;

const RESTORE_WRAPPER_PS1: &str = r#"param([Parameter(ValueFromRemainingArguments=$true)][string[]]$All)
$agent = $All[0]
$fallback_shell = $All[1]
$early_window = [int]$All[2]
$rest = $All[3..($All.Length - 1)]
$start = [int][double]::Parse((Get-Date -UFormat %s))
$exe = $rest[0]
$exe_args = if ($rest.Length -gt 1) { $rest[1..($rest.Length - 1)] } else { @() }
& $exe @exe_args
$status = $LASTEXITCODE
$end = [int][double]::Parse((Get-Date -UFormat %s))
$elapsed = $end - $start
if ($status -ne 0 -and $elapsed -le $early_window) {
    Write-Host ("herdr: {0} session restore failed; started a shell instead" -f $agent)
}
& $fallback_shell
"#;

/// Returns `(program, args)` to spawn the restore wrapper for an agent.
///
/// - Unix: `("/bin/sh", ["-c", SCRIPT, "herdr-agent-restore", agent, fallback_shell, "30", ...argv])`.
/// - Windows (pwsh/powershell): `(<shell>, ["-NoProfile", "-Command", SCRIPT, "--", agent, fallback_shell, "30", ...argv])`.
/// - Windows (cmd.exe fallback): `("cmd.exe", ["/C", <argv-chain-with-fallback>])`.
pub fn restore_command_args(
    agent: &str,
    fallback_shell: &str,
    argv: &[String],
) -> (String, Vec<String>) {
    if cfg!(windows) {
        let shell = default_shell();
        if is_cmd_exe(&shell) {
            // cmd.exe has no inline-PS escape that survives portable-pty quoting,
            // so degrade gracefully: run argv, fall back to the shell on failure.
            let mut chain = String::new();
            for (index, piece) in argv.iter().enumerate() {
                if index > 0 {
                    chain.push(' ');
                }
                chain.push_str(piece);
            }
            chain.push_str(" || ");
            chain.push_str(fallback_shell);
            return (shell, vec!["/C".to_string(), chain]);
        }

        let mut args = vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            RESTORE_WRAPPER_PS1.to_string(),
            "--".to_string(),
            agent.to_string(),
            fallback_shell.to_string(),
            "30".to_string(),
        ];
        args.extend(argv.iter().cloned());
        return (shell, args);
    }

    let mut args = vec![
        "-c".to_string(),
        RESTORE_WRAPPER_SH.to_string(),
        "herdr-agent-restore".to_string(),
        agent.to_string(),
        fallback_shell.to_string(),
        "30".to_string(),
    ];
    args.extend(argv.iter().cloned());
    ("/bin/sh".to_string(), args)
}

fn is_cmd_exe(program: &str) -> bool {
    let lowered = program.to_ascii_lowercase();
    lowered == "cmd.exe" || lowered.ends_with("\\cmd.exe") || lowered.ends_with("/cmd.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_on_path(_: &str) -> bool {
        false
    }

    fn always_finds(name: &str) -> bool {
        name == "pwsh.exe"
    }

    fn only_powershell(name: &str) -> bool {
        name == "powershell.exe"
    }

    #[cfg(unix)]
    #[test]
    fn default_shell_uses_env_shell_on_unix() {
        let shell = default_shell_from_env(Some("/usr/bin/zsh".to_string()), None, never_on_path);
        assert_eq!(shell, "/usr/bin/zsh");
    }

    #[cfg(unix)]
    #[test]
    fn default_shell_falls_back_to_bin_sh_on_unix() {
        let shell = default_shell_from_env(None, Some("C:/Windows/cmd.exe".into()), never_on_path);
        assert_eq!(shell, "/bin/sh");
    }

    #[cfg(unix)]
    #[test]
    fn default_shell_trims_whitespace_only_env_on_unix() {
        let shell = default_shell_from_env(Some("   ".to_string()), None, never_on_path);
        assert_eq!(shell, "/bin/sh");
    }

    #[cfg(windows)]
    #[test]
    fn default_shell_prefers_pwsh_on_windows() {
        let shell = default_shell_from_env(None, None, always_finds);
        assert_eq!(shell, "pwsh.exe");
    }

    #[cfg(windows)]
    #[test]
    fn default_shell_falls_back_to_powershell_when_pwsh_missing() {
        let shell = default_shell_from_env(None, None, only_powershell);
        assert_eq!(shell, "powershell.exe");
    }

    #[cfg(windows)]
    #[test]
    fn default_shell_uses_comspec_when_no_shell_on_path() {
        let shell = default_shell_from_env(
            None,
            Some(r"C:\Windows\System32\cmd.exe".into()),
            never_on_path,
        );
        assert_eq!(shell, r"C:\Windows\System32\cmd.exe");
    }

    #[cfg(windows)]
    #[test]
    fn default_shell_final_fallback_is_cmd_exe() {
        let shell = default_shell_from_env(None, None, never_on_path);
        assert_eq!(shell, "cmd.exe");
    }

    /// Cross-platform shape contract: `default_shell()` always returns a
    /// non-empty string that names a known shell binary for the host.
    #[test]
    fn default_shell_is_platform_appropriate() {
        let shell = default_shell();
        assert!(!shell.is_empty());
        if cfg!(windows) {
            let lowered = shell.to_ascii_lowercase();
            assert!(
                lowered.ends_with("pwsh.exe")
                    || lowered.ends_with("powershell.exe")
                    || lowered.ends_with("cmd.exe"),
                "windows default_shell should be a known windows shell, got: {shell}"
            );
        } else {
            assert!(
                shell.starts_with('/'),
                "unix default_shell should be an absolute path, got: {shell}"
            );
        }
    }

    #[test]
    fn pane_shell_prefers_configured_value() {
        assert_eq!(
            pane_shell_from("/usr/bin/nu", Some("/bin/bash".to_string())),
            "/usr/bin/nu"
        );
    }

    #[test]
    fn pane_shell_falls_back_to_env_shell() {
        if cfg!(unix) {
            assert_eq!(
                pane_shell_from("", Some("/bin/bash".to_string())),
                "/bin/bash"
            );
        }
    }

    #[test]
    fn pane_shell_ignores_whitespace_only_values() {
        if cfg!(unix) {
            assert_eq!(pane_shell_from("   ", Some("  ".to_string())), "/bin/sh");
            assert_eq!(pane_shell_from("", None), "/bin/sh");
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_command_args_uses_bin_sh_minus_c_on_unix() {
        let (program, args) = shell_command_args("echo hi");
        assert_eq!(program, "/bin/sh");
        assert_eq!(args, vec!["-c".to_string(), "echo hi".to_string()]);
    }

    #[cfg(windows)]
    #[test]
    fn shell_command_args_uses_pwsh_or_cmd_on_windows() {
        let (program, args) = shell_command_args("echo hi");
        let lowered = program.to_ascii_lowercase();
        if lowered.ends_with("cmd.exe") {
            assert_eq!(args, vec!["/C".to_string(), "echo hi".to_string()]);
        } else {
            assert_eq!(
                args,
                vec![
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    "echo hi".to_string(),
                ]
            );
        }
    }

    #[test]
    fn restore_wrapper_script_returns_non_empty() {
        let script = restore_wrapper_script();
        assert!(!script.is_empty());
        if cfg!(windows) {
            assert!(script.contains("param"));
        } else {
            assert!(script.contains("exec"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn restore_command_args_emits_posix_invocation_on_unix() {
        let (program, args) = restore_command_args(
            "codex",
            "/bin/zsh",
            &["/bin/sh".to_string(), "-c".to_string(), "exit 7".to_string()],
        );
        assert_eq!(program, "/bin/sh");
        assert_eq!(args[0], "-c");
        assert!(args[1].contains("agent"));
        assert_eq!(args[2], "herdr-agent-restore");
        assert_eq!(args[3], "codex");
        assert_eq!(args[4], "/bin/zsh");
        assert_eq!(args[5], "30");
        assert_eq!(args[6], "/bin/sh");
    }

    #[cfg(windows)]
    #[test]
    fn restore_command_args_emits_pwsh_invocation_on_windows() {
        let (program, args) = restore_command_args(
            "codex",
            "pwsh.exe",
            &["pwsh.exe".to_string(), "-c".to_string(), "exit 7".to_string()],
        );
        let lowered = program.to_ascii_lowercase();
        if lowered.ends_with("cmd.exe") {
            assert_eq!(args[0], "/C");
            assert!(args[1].contains("pwsh.exe"));
            assert!(args[1].contains("||"));
        } else {
            assert_eq!(args[0], "-NoProfile");
            assert_eq!(args[1], "-Command");
            assert!(args[2].contains("param"));
            assert_eq!(args[3], "--");
            assert_eq!(args[4], "codex");
            assert_eq!(args[5], "pwsh.exe");
            assert_eq!(args[6], "30");
            assert_eq!(args[7], "pwsh.exe");
        }
    }

    #[test]
    fn is_cmd_exe_recognizes_full_and_bare_paths() {
        assert!(is_cmd_exe("cmd.exe"));
        assert!(is_cmd_exe(r"C:\Windows\System32\cmd.exe"));
        assert!(is_cmd_exe("/some/wsl/path/cmd.exe"));
        assert!(!is_cmd_exe("pwsh.exe"));
        assert!(!is_cmd_exe("/bin/sh"));
    }
}
