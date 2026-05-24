//! Legacy import surface for IPC helpers. Goal 3 moved the real
//! implementations to `crate::transport`; this file is a thin
//! re-export so older call sites keep compiling while Goal 4 migrates
//! them to the new module path directly.

pub(crate) use crate::transport::{prepare_socket_path, restrict_socket_permissions};
