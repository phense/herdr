//! Cross-platform unique endpoint paths for integration tests.
//!
//! Integration tests live in their own compilation unit (the `herdr` crate is
//! `bin`-only), so they can't reach `crate::transport`. This helper mirrors
//! `src/transport/mod.rs::unique_endpoint_path` so a single function builds a
//! collision-free local-socket / named-pipe path on every host.
//!
//! * Unix: a file in `std::env::temp_dir()` with a short hashed basename so
//!   the resulting path stays under the macOS `SUN_LEN` limit (104 bytes).
//! * Windows: a named-pipe path of the form `\\.\pipe\<prefix>-<hex>`.

#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build a unique endpoint path suitable for binding a `LocalListener` /
/// `UnixListener` (unix) or `NamedPipeServer` (windows).
///
/// `prefix` is hashed into the basename — it shows up only in the hash, not
/// verbatim, so callers don't need to worry about path-length budgets on
/// macOS.
pub fn unique_socket_path(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);

    let mut hasher = DefaultHasher::new();
    prefix.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    nanos.hash(&mut hasher);
    counter.hash(&mut hasher);
    let hash = hasher.finish();

    #[cfg(unix)]
    {
        std::env::temp_dir().join(format!("ht-{hash:016x}.sock"))
    }

    #[cfg(windows)]
    {
        PathBuf::from(format!(r"\\.\pipe\herdr-test-{hash:016x}"))
    }
}

/// Build a unique base directory under `std::env::temp_dir()` for tests that
/// need a private working tree (config home, runtime dir, ...). Replaces the
/// `/tmp/<test-name>-<pid>-<nanos>` literals that used to live in every test
/// file.
///
/// The basename is hashed down to a fixed-width 16-hex token so the resulting
/// `<TMPDIR>/<basename>/runtime/herdr.sock` path stays under the macOS
/// `SUN_LEN` budget — `$TMPDIR` on macOS is `/var/folders/<2>/<28>/T/`
/// (~49 chars, canonicalizing to `/private/var/...` = +8) which leaves only
/// ~30 chars for the basename + `/runtime/herdr.sock`. The label survives
/// only through the hash, so callers can still pass a human-readable
/// prefix.
pub fn unique_test_base(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);

    let mut hasher = DefaultHasher::new();
    label.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    nanos.hash(&mut hasher);
    counter.hash(&mut hasher);
    let hash = hasher.finish();

    std::env::temp_dir().join(format!("ht-{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_socket_path_is_unique_per_call() {
        let a = unique_socket_path("test");
        let b = unique_socket_path("test");
        assert_ne!(a, b);
    }

    #[test]
    fn unique_test_base_is_unique_per_call() {
        let a = unique_test_base("foo");
        let b = unique_test_base("foo");
        assert_ne!(a, b);
        assert!(a.starts_with(std::env::temp_dir()));
    }

    #[cfg(unix)]
    #[test]
    fn unique_test_base_socket_path_fits_sun_len_budget() {
        // Tests append `runtime/herdr.sock` (19 bytes) and the macOS
        // `/private/` prefix is added during canonicalization (+8 bytes), so
        // keep the result under 96 to leave headroom against the 104-byte
        // `sun_path` ceiling.
        let base = unique_test_base("integration-test");
        let socket = base.join("runtime").join("herdr.sock");
        let bytes = socket.as_os_str().len() + "/private".len();
        assert!(
            bytes < 104,
            "base + runtime/herdr.sock too long: {bytes} bytes ({socket:?})"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_path_fits_sun_len_budget() {
        // macOS sun_path is 104 bytes including NUL; linux is 108. Use the
        // tighter budget to catch regressions either way.
        let path = unique_socket_path("integration-test");
        let bytes = path.as_os_str().len();
        assert!(
            bytes < 104,
            "socket path too long: {bytes} bytes ({path:?})"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_uses_pipe_namespace() {
        let path = unique_socket_path("test");
        let s = path.to_string_lossy();
        assert!(s.starts_with(r"\\.\pipe\"), "expected pipe path, got {s}");
    }
}
