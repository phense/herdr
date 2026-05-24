use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn zig_target(target: &str) -> &str {
    match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "x86_64-pc-windows-gnu" => "x86_64-windows-gnu",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("unsupported target for libghostty-vt build: {other}"),
    }
}

fn env_bool(name: &str) -> Option<bool> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            other => panic!("invalid boolean value for {name}: {other}"),
        },
        Err(env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read {name}: {err}"),
    }
}

fn emit_link_directives(target: &str, lib_dir: &Path) {
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    if target.contains("apple-darwin") {
        let static_lib = lib_dir.join("libghostty-vt.a");
        println!("cargo:rustc-link-arg={}", static_lib.display());
    } else if target.contains("windows") {
        // The Rust-side ghostty FFI module is gated `cfg(unix)` until the
        // Windows port (Goals 1–7) finishes; nothing on Windows resolves a
        // libghostty-vt symbol yet. We therefore *only* emit the link
        // directives when a real `ghostty-vt-static.lib` exists on disk —
        // that way `cargo build` / `cargo test` succeed on dev boxes where
        // the vendored zig build is blocked by host antivirus (see
        // docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md). When the
        // artifact *is* there, builds against future Windows ghostty
        // bindings link cleanly.
        if lib_dir.join("ghostty-vt-static.lib").exists() {
            println!("cargo:rustc-link-lib=static=ghostty-vt-static");
            for sys_lib in [
                "advapi32", "userenv", "ws2_32", "ntdll", "iphlpapi", "bcrypt", "crypt32",
                "secur32", "ole32", "shell32", "user32", "kernel32", "dbghelp",
            ] {
                println!("cargo:rustc-link-lib=dylib={sys_lib}");
            }
        } else {
            println!(
                "cargo:warning=skipping link directives for libghostty-vt; \
                 expected {} to exist. See docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md.",
                lib_dir.join("ghostty-vt-static.lib").display()
            );
        }
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig.zon");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/include");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/pkg");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/src");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/VERSION");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SIMD");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SKIP_BUILD");
    println!("cargo:rerun-if-env-changed=ZIG");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored_dir = manifest_dir.join("vendor/libghostty-vt");
    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET");

    // Allow callers to skip the vendored build (useful on Windows where the
    // wuffs package's deliberately malformed JPEG test fixtures trip antivirus
    // scanners — see docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md for
    // the workaround). When skipped, link directives are still emitted so
    // `cargo check` / `cargo metadata` succeed even though final linking
    // requires the real artifact.
    let skip_build = env_bool("LIBGHOSTTY_VT_SKIP_BUILD").unwrap_or(false);

    let zig_target = zig_target(&target);
    let version_string = fs::read_to_string(vendored_dir.join("VERSION"))
        .expect("failed to read vendored libghostty-vt VERSION")
        .trim()
        .to_string();

    let lib_dir = vendored_dir.join("zig-out/lib");

    if skip_build {
        println!(
            "cargo:warning=LIBGHOSTTY_VT_SKIP_BUILD=1 — skipping vendored libghostty-vt build for target {target}. Final linking will fail until a real artifact is placed in {}.",
            lib_dir.display()
        );
        emit_link_directives(&target, &lib_dir);
        return;
    }

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let mut command = Command::new(&zig);
    command
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dsimd={simd}"))
        .arg(format!("-Dtarget={zig_target}"))
        .arg(format!("-Dversion-string={version_string}"));
    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {
        command.arg("--system").arg(system_dir);
    }

    let status = command.current_dir(&vendored_dir).status();

    match status {
        Ok(s) if s.success() => {
            emit_link_directives(&target, &lib_dir);
        }
        other => {
            // Emit link directives so `cargo check`/`cargo metadata` succeed
            // — they don't link. A real `cargo build` will fail at the link
            // step with a clear "library not found" diagnostic if the
            // artifact never made it to disk, which is the desired UX.
            let reason = match other {
                Ok(s) => format!("zig build exited with {s}"),
                Err(e) => format!("failed to execute zig build: {e} (ZIG={zig})"),
            };
            println!(
                "cargo:warning=libghostty-vt build failed ({reason}); see docs/plans/windows-port/LIBGHOSTTY-WINDOWS-NOTES.md. Set LIBGHOSTTY_VT_SKIP_BUILD=1 to suppress the build attempt."
            );
            emit_link_directives(&target, &lib_dir);
        }
    }
}
