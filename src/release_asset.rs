//! Asset-key helpers shared between `crate::update` and any future
//! cross-platform release tooling.
//!
//! The release manifest at `website/latest.json` keys binaries by
//! `"<os>-<arch>"` (see `AGENTS.md::Releases`). Goal 9 of the Windows port
//! added `windows-x86_64` and `windows-aarch64` alongside the existing
//! `linux-*` / `macos-*` keys. These functions live in their own module so
//! they remain reachable from `cfg(windows)` builds even though the rest of
//! `crate::update` is currently `cfg(unix)`-gated (Goal 4 deferral).

/// Resolve the current host's `(os, arch)` tuple in the form the release
/// manifest understands. Both components are `"unknown"` if the host falls
/// outside the supported matrix; callers should treat that as a no-update
/// signal rather than a crash.
pub fn platform_target() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };

    (os, arch)
}

/// Asset key advertised to the release manifest for the current host
/// (e.g. `"linux-x86_64"`, `"macos-aarch64"`, `"windows-x86_64"`).
#[allow(dead_code)]
pub fn asset_key() -> String {
    let (os, arch) = platform_target();
    format!("{os}-{arch}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_target_is_known() {
        let (os, arch) = platform_target();
        assert!(
            os == "linux" || os == "macos" || os == "windows",
            "os: {os}"
        );
        assert!(arch == "x86_64" || arch == "aarch64", "arch: {arch}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn asset_key_for_windows() {
        let key = asset_key();
        #[cfg(target_arch = "x86_64")]
        assert_eq!(key, "windows-x86_64");
        #[cfg(target_arch = "aarch64")]
        assert_eq!(key, "windows-aarch64");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn asset_key_for_linux() {
        let key = asset_key();
        #[cfg(target_arch = "x86_64")]
        assert_eq!(key, "linux-x86_64");
        #[cfg(target_arch = "aarch64")]
        assert_eq!(key, "linux-aarch64");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn asset_key_for_macos() {
        let key = asset_key();
        #[cfg(target_arch = "x86_64")]
        assert_eq!(key, "macos-x86_64");
        #[cfg(target_arch = "aarch64")]
        assert_eq!(key, "macos-aarch64");
    }
}
