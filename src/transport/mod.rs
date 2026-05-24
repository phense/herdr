//! Cross-platform local-IPC primitives used by every herdr daemon/client
//! interaction (control socket, render channel, JSON API, headless
//! handshake, session list, update probe).
//!
//! On unix we delegate to `std::os::unix::net::{UnixStream, UnixListener}`;
//! on windows we wrap Win32 named pipes via `windows-sys`. The surface
//! intentionally matches the existing usage of `UnixStream`/`UnixListener`
//! so Goal 4 can migrate call sites mechanically.
//!
//! Path semantics:
//! * On unix the path is a filesystem path that becomes the socket file.
//! * On windows the path is mapped to `\\.\pipe\<sanitized>` unless the
//!   caller already supplied a `\\.\pipe\…` literal. The pipe is a kernel
//!   object — there's no file on disk to remove. `prepare_socket_path`
//!   still creates the parent dir for callers that store auxiliary files
//!   next to the "socket" (e.g. lockfiles).

use std::io;
use std::net::Shutdown;
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as backend;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as backend;

#[cfg(not(any(unix, windows)))]
compile_error!("transport module supports only unix and windows hosts");

pub use backend::{LocalListener, LocalStream};

/// Create a connected pair of `LocalStream`s for tests / in-process pipes.
/// On unix this is `UnixStream::pair`; on windows we open a unique private
/// named pipe and connect to it from the same process.
pub fn pair() -> io::Result<(LocalStream, LocalStream)> {
    backend::pair()
}

/// Prepare a path for use as a local-IPC endpoint:
/// * Ensure the parent directory exists.
/// * On unix: probe an existing socket file; remove it if no server is
///   listening, return [`io::ErrorKind::AddrInUse`] (with `busy_message`)
///   if one is.
/// * On windows: named pipes don't leave a file on disk, so this is a
///   no-op past the directory check.
pub fn prepare_socket_path(
    path: &Path,
    busy_message: impl FnOnce(&Path) -> String,
) -> io::Result<()> {
    backend::prepare_socket_path(path, busy_message)
}

/// Restrict the endpoint's permissions to owner-only.
///
/// * On unix: chmod to `mode` (typically `0o600`).
/// * On windows: named pipes inherit a default DACL that already restricts
///   access to the creating user; we return `Ok(())` so cross-platform
///   callers can call this unconditionally. Tightening the DACL further
///   is tracked under Goal 7 (clipboard / notifications hardening).
pub fn restrict_socket_permissions(path: &Path, mode: u32) -> io::Result<()> {
    backend::restrict_socket_permissions(path, mode)
}

/// Common error envelope used internally; not exposed publicly.
#[allow(dead_code)]
pub(crate) fn make_io_err(kind: io::ErrorKind, msg: impl Into<String>) -> io::Error {
    io::Error::new(kind, msg.into())
}

/// Re-export of `std::net::Shutdown` for ergonomics — callers shouldn't
/// have to import it separately when they already pulled in the transport
/// module.
pub use std::net::Shutdown as ShutdownHow;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::thread;

    fn unique_endpoint_path(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-transport-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir.join(format!("{label}.sock"))
    }

    #[test]
    fn pair_round_trips_small_payload() {
        let (mut a, mut b) = pair().expect("pair");
        let msg = b"hello";
        a.write_all(msg).unwrap();
        let mut buf = [0u8; 5];
        b.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, msg);
    }

    #[test]
    fn pair_round_trips_64k() {
        let (mut a, mut b) = pair().expect("pair");
        let payload = vec![0x55u8; 64 * 1024];
        let send = payload.clone();
        let writer = thread::spawn(move || {
            a.write_all(&send).expect("write");
        });
        let mut got = vec![0u8; 64 * 1024];
        b.read_exact(&mut got).expect("read");
        writer.join().expect("writer");
        assert_eq!(got, payload);
    }

    #[test]
    fn shutdown_write_makes_peer_see_eof() {
        let (a, mut b) = pair().expect("pair");
        a.shutdown(Shutdown::Write).expect("shutdown");
        let mut buf = [0u8; 8];
        let n = b.read(&mut buf).expect("read");
        assert_eq!(n, 0, "read after peer-write-shutdown should hit EOF");
    }

    #[test]
    fn set_read_timeout_fires_on_idle() {
        let (a, mut b) = pair().expect("pair");
        b.set_read_timeout(Some(Duration::from_millis(50)))
            .expect("set_read_timeout");
        let start = std::time::Instant::now();
        let mut buf = [0u8; 1];
        let err = b.read(&mut buf).expect_err("expected timeout");
        let elapsed = start.elapsed();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "unexpected kind: {:?}",
            err.kind()
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "timeout too slow: {:?}",
            elapsed
        );
        drop(a);
    }

    #[test]
    fn set_nonblocking_makes_read_return_would_block() {
        let (a, b) = pair().expect("pair");
        b.set_nonblocking(true).expect("set_nonblocking");
        let mut reader = b.try_clone().expect("try_clone");
        let mut buf = [0u8; 1];
        let err = reader.read(&mut buf).expect_err("expected wouldblock");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(a);
    }

    #[test]
    fn try_clone_yields_independently_usable_handle() {
        let (mut a, b) = pair().expect("pair");
        let cloned = b.try_clone().expect("clone");
        a.write_all(b"two").expect("write");
        a.shutdown(Shutdown::Write).ok();

        let mut original = b;
        let mut buf = [0u8; 3];
        original.read_exact(&mut buf).expect("orig read");
        assert_eq!(&buf, b"two");
        // Cloned handle reads from the same underlying stream — should now
        // observe EOF (the data was consumed once).
        let mut clone_buf = [0u8; 4];
        let mut cloned = cloned;
        let n = cloned.read(&mut clone_buf).expect("clone read");
        assert_eq!(n, 0, "clone should see EOF after data was consumed");
    }

    #[test]
    fn oversized_write_does_not_panic_when_peer_drops() {
        let (a, b) = pair().expect("pair");
        drop(b);
        let mut a = a;
        // 256 KiB write to a closed peer. Implementations may return any
        // of: BrokenPipe, ConnectionAborted, ConnectionReset, partial Ok,
        // even Ok(_) on systems with large kernel buffers — what matters
        // is that we don't panic.
        let payload = vec![0xAAu8; 256 * 1024];
        let result = a.write_all(&payload);
        // Either an error of an expected kind, or success (kernel
        // buffered) — both are fine.
        if let Err(err) = result {
            assert!(
                matches!(
                    err.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::Other
                        | io::ErrorKind::WriteZero
                ),
                "unexpected kind: {:?}",
                err.kind()
            );
        }
    }

    #[test]
    fn bind_accept_connect_round_trip() {
        let path = unique_endpoint_path("bac");
        let listener = LocalListener::bind(&path).expect("bind");
        let path_for_client = path.clone();

        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).expect("read");
            s.write_all(b"pong").expect("write");
            buf
        });

        let mut client = LocalStream::connect(&path_for_client).expect("connect");
        client.write_all(b"ping").expect("client write");
        let mut resp = [0u8; 4];
        client.read_exact(&mut resp).expect("client read");
        let server_recv = server.join().expect("server thread");

        assert_eq!(&server_recv, b"ping");
        assert_eq!(&resp, b"pong");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn listener_set_nonblocking_returns_would_block_when_idle() {
        let path = unique_endpoint_path("lsnb");
        let listener = LocalListener::bind(&path).expect("bind");
        listener.set_nonblocking(true).expect("nb");
        let err = listener.accept().expect_err("accept should fail");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn prepare_socket_path_creates_parent_dir() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let base = std::env::temp_dir().join(format!(
            "herdr-prep-{}-{nanos}/nested/dir",
            std::process::id()
        ));
        let path = base.join("herdr.sock");

        prepare_socket_path(&path, |p| format!("busy {}", p.display())).expect("prepare");
        assert!(base.exists());
        let _ = std::fs::remove_dir_all(base.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn prepare_socket_path_rejects_live_endpoint() {
        let path = unique_endpoint_path("live");
        let _listener = LocalListener::bind(&path).expect("bind");
        let err = prepare_socket_path(&path, |p| format!("busy at {}", p.display()))
            .expect_err("should be busy");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn restrict_socket_permissions_is_ok_after_bind() {
        let path = unique_endpoint_path("perm");
        let _listener = LocalListener::bind(&path).expect("bind");
        // On windows this is a no-op (pipe DACL inherits user-only).
        // On unix it chmods the socket file to 0o600.
        let res = restrict_socket_permissions(&path, 0o600);
        assert!(res.is_ok(), "restrict_socket_permissions: {res:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn connect_returns_not_found_for_missing_endpoint() {
        let path = unique_endpoint_path("missing");
        // Don't bind. Connect should fail.
        let err = LocalStream::connect(&path).expect_err("connect should fail");
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::TimedOut
            ),
            "unexpected kind: {:?}",
            err.kind()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn shutdown_read_then_write_succeeds_then_fails() {
        let (mut a, _b) = pair().expect("pair");
        a.shutdown(Shutdown::Both).expect("shutdown both");
        // After Both, writes should fail (or return 0).
        let res = a.write(b"after shutdown");
        if let Ok(n) = res {
            assert_eq!(n, 0);
        }
    }

    #[test]
    fn pair_two_writers_concurrent_does_not_corrupt() {
        let (a, mut b) = pair().expect("pair");
        let mut a2 = a.try_clone().expect("clone");
        let mut a = a;
        let h1 = thread::spawn(move || {
            for _ in 0..200 {
                let _ = a.write_all(b"AAAA");
            }
            a.shutdown(Shutdown::Write).ok();
        });
        let h2 = thread::spawn(move || {
            for _ in 0..200 {
                let _ = a2.write_all(b"BBBB");
            }
            a2.shutdown(Shutdown::Write).ok();
        });
        let mut sink = Vec::new();
        b.read_to_end(&mut sink).expect("read_to_end");
        h1.join().ok();
        h2.join().ok();
        // We only verify length here: per-byte ordering between two
        // writers is intentionally unspecified.
        assert!(sink.len() >= 4 * 200, "got {} bytes", sink.len());
    }
}
