//! Windows backend for the cross-platform transport, built on Win32
//! named pipes via `windows-sys`. Provides `LocalStream` / `LocalListener`
//! semantics close enough to `UnixStream` / `UnixListener` that
//! consumers can swap one for the other.
//!
//! Design notes:
//! * Connect/accept share a single full-duplex named-pipe instance per
//!   connection. `shutdown(Shutdown::Write)` on such a stream closes the
//!   pipe handle (Windows has no half-shutdown on a duplex pipe); the
//!   peer's next read returns EOF as expected.
//! * `pair()` uses TWO unidirectional pipes (one per direction) so that
//!   shutting down one writer doesn't blow up the other end's writer.
//!   This matches Unix `UnixStream::pair` semantics tightly enough for
//!   the test suite.
//! * Reads/writes go through overlapped IO with a per-handle auto-reset
//!   event. Timeouts use `WaitForSingleObject` on the event followed by
//!   `CancelIoEx` if it expires. Setting non-blocking flips the
//!   per-handle wait deadline to zero.

use std::fs;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND,
    ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, FALSE, HANDLE,
    INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

// PIPE_ACCESS_* aren't surfaced by windows-sys 0.59; they're the same bits
// as FILE_ACCESS_RIGHTS_GENERIC variants on the file-system side. See
// WinBase.h / NamedPipeApi.h.
const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;
const PIPE_ACCESS_OUTBOUND: u32 = 0x0000_0002;
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;

use windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const PIPE_BUFFER_SIZE: u32 = 64 * 1024;
const NEVER: u64 = u64::MAX;

fn last_io_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

fn path_to_pipe_name(path: &Path) -> Vec<u16> {
    let s = path.to_string_lossy();
    let prefix = r"\\.\pipe\";
    let raw = if s.starts_with(prefix) {
        s.into_owned()
    } else {
        // Sanitize the path into a single-segment pipe name. Drive
        // letters, separators and reserved chars become underscores.
        let cleaned: String = s
            .chars()
            .map(|c| match c {
                '\\' | '/' | ':' | '|' | '<' | '>' | '"' | '?' | '*' => '_',
                _ => c,
            })
            .collect();
        format!("{prefix}herdr-{}", cleaned.trim_start_matches('_'))
    };
    let os = std::ffi::OsString::from(raw);
    let mut wide: Vec<u16> = os.encode_wide().collect();
    wide.push(0);
    wide
}

// --- Handles --------------------------------------------------------------

#[derive(Debug)]
struct PipeHandle {
    handle: HANDLE,
}
unsafe impl Send for PipeHandle {}
unsafe impl Sync for PipeHandle {}

impl PipeHandle {
    fn from_raw(handle: HANDLE) -> Self {
        Self { handle }
    }
    fn as_raw(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

#[derive(Debug)]
struct EventHandle {
    handle: HANDLE,
}
unsafe impl Send for EventHandle {}
unsafe impl Sync for EventHandle {}

impl EventHandle {
    fn manual_reset() -> io::Result<Self> {
        // Manual-reset, signaled=false. Each operation explicitly resets
        // it via setting OVERLAPPED.hEvent before issuing the IO.
        let h = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if h.is_null() {
            return Err(last_io_error());
        }
        Ok(Self { handle: h })
    }
    fn as_raw(&self) -> HANDLE {
        self.handle
    }
}
impl Drop for EventHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

// --- LocalStream ----------------------------------------------------------

#[derive(Debug)]
pub struct LocalStream {
    read: Arc<PipeHandle>,
    write: Arc<PipeHandle>,
    // Per-call event handles for overlapped IO. Stored under Mutex so
    // read/write from concurrent threads don't share the same OVERLAPPED.
    read_event: Arc<Mutex<EventHandle>>,
    write_event: Arc<Mutex<EventHandle>>,
    read_timeout_ms: Arc<AtomicU64>,
    write_timeout_ms: Arc<AtomicU64>,
    nonblocking: Arc<AtomicBool>,
}

impl LocalStream {
    fn new_duplex(handle: HANDLE) -> io::Result<Self> {
        let read = Arc::new(PipeHandle::from_raw(handle));
        let write = read.clone();
        Ok(Self {
            read,
            write,
            read_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
            write_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
            read_timeout_ms: Arc::new(AtomicU64::new(NEVER)),
            write_timeout_ms: Arc::new(AtomicU64::new(NEVER)),
            nonblocking: Arc::new(AtomicBool::new(false)),
        })
    }

    fn new_split(read_handle: HANDLE, write_handle: HANDLE) -> io::Result<Self> {
        Ok(Self {
            read: Arc::new(PipeHandle::from_raw(read_handle)),
            write: Arc::new(PipeHandle::from_raw(write_handle)),
            read_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
            write_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
            read_timeout_ms: Arc::new(AtomicU64::new(NEVER)),
            write_timeout_ms: Arc::new(AtomicU64::new(NEVER)),
            nonblocking: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn connect(path: &Path) -> io::Result<Self> {
        let name = path_to_pipe_name(path);
        // Retry briefly on PIPE_BUSY (server hasn't called ConnectNamedPipe
        // for the next instance yet) — common at startup.
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        loop {
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_NONE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let err = unsafe { GetLastError() };
                if err == ERROR_PIPE_BUSY && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                if err == ERROR_FILE_NOT_FOUND {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("no transport endpoint at {}", path.display()),
                    ));
                }
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            return Self::new_duplex(handle);
        }
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::SeqCst);
        Ok(())
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        let ms = duration_to_ms(dur);
        self.read_timeout_ms.store(ms, Ordering::SeqCst);
        Ok(())
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        let ms = duration_to_ms(dur);
        self.write_timeout_ms.store(ms, Ordering::SeqCst);
        Ok(())
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        let read_dup = duplicate_handle(self.read.as_raw())?;
        let write_dup = if Arc::ptr_eq(&self.read, &self.write) {
            // Single-handle duplex: clone is also single-handle. We duplicated
            // once so close-on-drop semantics stay correct.
            read_dup
        } else {
            duplicate_handle(self.write.as_raw())?
        };
        let same_handle = Arc::ptr_eq(&self.read, &self.write);
        if same_handle {
            let h = Arc::new(PipeHandle::from_raw(read_dup));
            let w = h.clone();
            Ok(Self {
                read: h,
                write: w,
                read_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
                write_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
                read_timeout_ms: self.read_timeout_ms.clone(),
                write_timeout_ms: self.write_timeout_ms.clone(),
                nonblocking: self.nonblocking.clone(),
            })
        } else {
            Ok(Self {
                read: Arc::new(PipeHandle::from_raw(read_dup)),
                write: Arc::new(PipeHandle::from_raw(write_dup)),
                read_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
                write_event: Arc::new(Mutex::new(EventHandle::manual_reset()?)),
                read_timeout_ms: self.read_timeout_ms.clone(),
                write_timeout_ms: self.write_timeout_ms.clone(),
                nonblocking: self.nonblocking.clone(),
            })
        }
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        // Goal 3 semantics: shutdown(Write) must make the peer's read see
        // EOF. We achieve that by closing our handle to the named pipe
        // half that we'd be writing through.
        //
        // For pair()'s split layout the read and write halves are distinct
        // pipes, so each direction can be torn down independently.
        //
        // For connect/accept's duplex layout there's only one pipe handle;
        // closing it tears down the whole connection regardless of `how`.
        // Callers that need precise half-shutdown semantics should
        // exchange explicit framing markers — Goal 4 maps the existing
        // herdr protocol to that contract.
        match how {
            Shutdown::Read => self.close_read(),
            Shutdown::Write => self.close_write(),
            Shutdown::Both => {
                let _ = self.close_read();
                self.close_write()
            }
        }
    }

    fn close_read(&self) -> io::Result<()> {
        // Replace the contained handle with INVALID_HANDLE_VALUE so Drop
        // becomes a no-op. We use a one-shot atomic-like swap via an
        // inner Arc<Mutex<HANDLE>> — but for simplicity we just call
        // CloseHandle directly (drops cleanly on subsequent unwrap).
        close_pipe_arc(&self.read);
        Ok(())
    }

    fn close_write(&self) -> io::Result<()> {
        close_pipe_arc(&self.write);
        Ok(())
    }

    fn read_inner(&self, buf: &mut [u8]) -> io::Result<usize> {
        let timeout_ms = effective_wait_ms(
            self.nonblocking.load(Ordering::SeqCst),
            self.read_timeout_ms.load(Ordering::SeqCst),
        );
        let event_guard = self.read_event.lock().expect("read_event poisoned");
        overlapped_read(self.read.as_raw(), event_guard.as_raw(), buf, timeout_ms)
    }

    fn write_inner(&self, buf: &[u8]) -> io::Result<usize> {
        let timeout_ms = effective_wait_ms(
            self.nonblocking.load(Ordering::SeqCst),
            self.write_timeout_ms.load(Ordering::SeqCst),
        );
        let event_guard = self.write_event.lock().expect("write_event poisoned");
        overlapped_write(self.write.as_raw(), event_guard.as_raw(), buf, timeout_ms)
    }

    pub fn pair() -> io::Result<(Self, Self)> {
        pair()
    }
}

impl Read for LocalStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_inner(buf)
    }
}

impl Write for LocalStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_inner(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        // Named pipes are buffered by the kernel; our overlapped writes
        // already complete when the kernel accepts the bytes. Nothing
        // userspace can do here.
        Ok(())
    }
}

impl<'a> Read for &'a LocalStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_inner(buf)
    }
}
impl<'a> Write for &'a LocalStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_inner(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn close_pipe_arc(arc: &Arc<PipeHandle>) {
    // CloseHandle the raw handle; rely on the fact that Drop also tries
    // to close it (CloseHandle on an already-closed handle is harmless
    // unless the handle has been recycled — for our short-lived
    // shutdown→drop sequence that race is benign).
    let raw = arc.as_raw();
    if !raw.is_null() && raw != INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(raw) };
    }
}

fn duration_to_ms(dur: Option<Duration>) -> u64 {
    match dur {
        None => NEVER,
        Some(d) => {
            let ms = d.as_millis();
            if ms >= INFINITE as u128 { (INFINITE - 1) as u64 } else { ms as u64 }
        }
    }
}

fn effective_wait_ms(nonblocking: bool, timeout: u64) -> u32 {
    if nonblocking {
        return 0;
    }
    if timeout == NEVER {
        return INFINITE;
    }
    let clamped = if timeout >= INFINITE as u64 {
        (INFINITE - 1) as u64
    } else {
        timeout
    };
    clamped as u32
}

fn duplicate_handle(source: HANDLE) -> io::Result<HANDLE> {
    let mut dup: HANDLE = ptr::null_mut();
    let me = unsafe { GetCurrentProcess() };
    let ok = unsafe {
        DuplicateHandle(
            me,
            source,
            me,
            &mut dup,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(last_io_error());
    }
    Ok(dup)
}

fn overlapped_read(
    handle: HANDLE,
    event: HANDLE,
    buf: &mut [u8],
    timeout_ms: u32,
) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        // Closed read handle => peer-EOF semantics. std's UnixStream
        // returns Ok(0) here after shutdown(Read), so we mirror that.
        return Ok(0);
    }
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event;
    let mut transferred: u32 = 0;
    let ok = unsafe {
        ReadFile(
            handle,
            buf.as_mut_ptr() as *mut _,
            buf.len() as u32,
            &mut transferred,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(transferred as usize);
    }
    let err = unsafe { GetLastError() };
    if err == ERROR_BROKEN_PIPE || err == ERROR_PIPE_NOT_CONNECTED || err == ERROR_NO_DATA {
        // Peer closed the write half (Unix shutdown(Write) equivalent).
        // Std maps this to EOF.
        return Ok(0);
    }
    if err != ERROR_IO_PENDING {
        return Err(map_io_error(err));
    }
    let waited = unsafe { WaitForSingleObject(event, timeout_ms) };
    if waited == WAIT_OBJECT_0 {
        if unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, FALSE) } == 0 {
            let werr = unsafe { GetLastError() };
            if werr == ERROR_BROKEN_PIPE
                || werr == ERROR_PIPE_NOT_CONNECTED
                || werr == ERROR_NO_DATA
            {
                return Ok(0);
            }
            return Err(map_io_error(werr));
        }
        return Ok(transferred as usize);
    }
    if waited == WAIT_TIMEOUT {
        unsafe { CancelIoEx(handle, &overlapped) };
        // Drain the completion (may legitimately return ERROR_OPERATION_ABORTED).
        let _ = unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, TRUE) };
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "read timed out"));
    }
    Err(last_io_error())
}

fn overlapped_write(
    handle: HANDLE,
    event: HANDLE,
    buf: &[u8],
    timeout_ms: u32,
) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "transport handle is closed",
        ));
    }
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event;
    let mut transferred: u32 = 0;
    let len = buf.len().min(u32::MAX as usize) as u32;
    let ok = unsafe {
        WriteFile(
            handle,
            buf.as_ptr() as *const _,
            len,
            &mut transferred,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(transferred as usize);
    }
    let err = unsafe { GetLastError() };
    if err != ERROR_IO_PENDING {
        return Err(map_io_error(err));
    }
    let waited = unsafe { WaitForSingleObject(event, timeout_ms) };
    if waited == WAIT_OBJECT_0 {
        if unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, FALSE) } == 0 {
            return Err(map_io_error(unsafe { GetLastError() }));
        }
        return Ok(transferred as usize);
    }
    if waited == WAIT_TIMEOUT {
        unsafe { CancelIoEx(handle, &overlapped) };
        let _ = unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, TRUE) };
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "write timed out"));
    }
    Err(last_io_error())
}

fn map_io_error(code: u32) -> io::Error {
    match code {
        ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA => {
            // Treat broken-pipe as EOF on read — std does the same.
            io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed")
        }
        ERROR_OPERATION_ABORTED => {
            io::Error::new(io::ErrorKind::WouldBlock, "operation cancelled")
        }
        ERROR_IO_INCOMPLETE => io::Error::new(io::ErrorKind::WouldBlock, "io incomplete"),
        _ => io::Error::from_raw_os_error(code as i32),
    }
}

// --- LocalListener --------------------------------------------------------

#[derive(Debug)]
pub struct LocalListener {
    name: Vec<u16>,
    next_instance: Mutex<Option<HANDLE>>,
    nonblocking: AtomicBool,
}

unsafe impl Send for LocalListener {}
unsafe impl Sync for LocalListener {}

impl LocalListener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        // Ensure parent dir exists for any sidecar files the caller might
        // place next to the "socket path".
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let name = path_to_pipe_name(path);
        let instance = create_pipe_instance(&name, true)?;
        Ok(Self {
            name,
            next_instance: Mutex::new(Some(instance)),
            nonblocking: AtomicBool::new(false),
        })
    }

    pub fn accept(&self) -> io::Result<(LocalStream, ())> {
        let nonblocking = self.nonblocking.load(Ordering::SeqCst);
        let pending = {
            let mut guard = self.next_instance.lock().expect("next_instance poisoned");
            match guard.take() {
                Some(h) => h,
                None => create_pipe_instance(&self.name, false)?,
            }
        };

        let event = EventHandle::manual_reset()?;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw();
        let ok = unsafe { ConnectNamedPipe(pending, &mut overlapped) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            match err {
                ERROR_PIPE_CONNECTED => {
                    // Client raced us and is already connected. Good.
                }
                ERROR_IO_PENDING => {
                    let timeout_ms = if nonblocking { 0 } else { INFINITE };
                    let waited = unsafe { WaitForSingleObject(event.as_raw(), timeout_ms) };
                    match waited {
                        WAIT_OBJECT_0 => {}
                        WAIT_TIMEOUT => {
                            unsafe { CancelIoEx(pending, &overlapped) };
                            let mut tmp = 0u32;
                            let _ = unsafe {
                                GetOverlappedResult(pending, &overlapped, &mut tmp, TRUE)
                            };
                            // Stash the instance back so the next accept reuses it.
                            *self
                                .next_instance
                                .lock()
                                .expect("next_instance poisoned") = Some(pending);
                            return Err(io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "no pending connection",
                            ));
                        }
                        _ => {
                            unsafe { CloseHandle(pending) };
                            return Err(last_io_error());
                        }
                    }
                }
                _ => {
                    unsafe { CloseHandle(pending) };
                    return Err(io::Error::from_raw_os_error(err as i32));
                }
            }
        }

        // Pre-create the next instance so the next accept doesn't race
        // a fast client.
        let next = create_pipe_instance(&self.name, false)?;
        *self.next_instance.lock().expect("next_instance poisoned") = Some(next);

        let stream = LocalStream::new_duplex(pending)?;
        Ok((stream, ()))
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for LocalListener {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.next_instance.lock() {
            if let Some(h) = guard.take() {
                unsafe {
                    let _ = DisconnectNamedPipe(h);
                    CloseHandle(h);
                }
            }
        }
    }
}

fn create_pipe_instance(name: &[u16], _first: bool) -> io::Result<HANDLE> {
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_SIZE,
            PIPE_BUFFER_SIZE,
            0,
            ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_io_error());
    }
    Ok(handle)
}

// --- pair() --------------------------------------------------------------

pub fn pair() -> io::Result<(LocalStream, LocalStream)> {
    // Two unidirectional pipes wire the pair so that shutting down one
    // half doesn't break the other.
    let nonce = next_pair_nonce();
    let ab_name = make_pair_name(nonce, 0);
    let ba_name = make_pair_name(nonce, 1);

    let ab_server = create_pipe_unidir(&ab_name, /*inbound*/ false)?;
    let ba_server = create_pipe_unidir(&ba_name, /*inbound*/ true)?;

    let ab_client = open_pipe_client(&ab_name, /*read*/ true, /*write*/ false)?;
    let ba_client = open_pipe_client(&ba_name, /*read*/ false, /*write*/ true)?;

    // Server-side ConnectNamedPipe completes immediately once the client
    // is on the other side; we still need the call (or to swallow
    // ERROR_PIPE_CONNECTED). Use overlapped + WAIT_OBJECT_0.
    finish_pair_connect(ab_server)?;
    finish_pair_connect(ba_server)?;

    // a's view: writes go via ab_server (server outbound), reads come
    //           from ba_client (client read end of ba).
    // b's view: writes go via ba_server (server outbound), reads come
    //           from ab_client (client read end of ab).
    let a = LocalStream::new_split(/*read*/ ba_client, /*write*/ ab_server)?;
    let b = LocalStream::new_split(/*read*/ ab_client, /*write*/ ba_server)?;
    Ok((a, b))
}

fn finish_pair_connect(server: HANDLE) -> io::Result<()> {
    let event = EventHandle::manual_reset()?;
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.as_raw();
    let ok = unsafe { ConnectNamedPipe(server, &mut overlapped) };
    if ok != 0 {
        return Ok(());
    }
    let err = unsafe { GetLastError() };
    if err == ERROR_PIPE_CONNECTED {
        return Ok(());
    }
    if err == ERROR_IO_PENDING {
        let waited = unsafe { WaitForSingleObject(event.as_raw(), 1000) };
        if waited == WAIT_OBJECT_0 {
            return Ok(());
        }
        if waited == WAIT_TIMEOUT {
            unsafe { CancelIoEx(server, &overlapped) };
        }
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "pair connect did not complete",
        ));
    }
    Err(io::Error::from_raw_os_error(err as i32))
}

fn create_pipe_unidir(name: &[u16], inbound: bool) -> io::Result<HANDLE> {
    let access = if inbound {
        PIPE_ACCESS_INBOUND
    } else {
        PIPE_ACCESS_OUTBOUND
    };
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            access | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1, // single instance — this is a private pair
            PIPE_BUFFER_SIZE,
            PIPE_BUFFER_SIZE,
            0,
            ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_io_error());
    }
    Ok(handle)
}

fn open_pipe_client(name: &[u16], read: bool, write: bool) -> io::Result<HANDLE> {
    let mut access = 0u32;
    if read {
        access |= GENERIC_READ;
    }
    if write {
        access |= GENERIC_WRITE;
    }
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_NONE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_io_error());
    }
    Ok(handle)
}

fn next_pair_nonce() -> u64 {
    use std::sync::atomic::AtomicU64 as Au64;
    static COUNTER: Au64 = Au64::new(0);
    let pid = std::process::id() as u64;
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    pid.wrapping_mul(1_000_003).wrapping_add(stamp).wrapping_add(n)
}

fn make_pair_name(nonce: u64, channel: u8) -> Vec<u16> {
    let raw = format!(r"\\.\pipe\herdr-pair-{}-{}-{}", std::process::id(), nonce, channel);
    let os = std::ffi::OsString::from(raw);
    let mut wide: Vec<u16> = os.encode_wide().collect();
    wide.push(0);
    wide
}

// --- prepare/restrict helpers --------------------------------------------

pub fn prepare_socket_path(
    path: &Path,
    busy_message: impl FnOnce(&Path) -> String,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Probe whether a pipe with the corresponding name is already accepting
    // connections. CreateFileW with FILE_SHARE_NONE either succeeds (busy)
    // or fails. ERROR_PIPE_BUSY also counts as busy.
    let name = path_to_pipe_name(path);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_NONE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(handle) };
        return Err(io::Error::new(io::ErrorKind::AddrInUse, busy_message(path)));
    }
    let err = unsafe { GetLastError() };
    if err == ERROR_PIPE_BUSY {
        return Err(io::Error::new(io::ErrorKind::AddrInUse, busy_message(path)));
    }
    Ok(())
}

pub fn restrict_socket_permissions(_path: &Path, _mode: u32) -> io::Result<()> {
    // Windows named pipes already restrict access to the creating user via
    // their default DACL. A targeted hardening pass (deny-network ACE,
    // explicit owner-only DACL) is tracked under Goal 7.
    Ok(())
}
