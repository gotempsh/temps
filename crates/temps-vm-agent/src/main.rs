// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-guest agent for Firecracker sandboxes (ADR-029 §5).
//!
//! Runs as PID 1 inside the microVM: mounts the pseudo-filesystems, then
//! serves exec/fs RPCs over vsock. Injected into every rootfs by the
//! host-side conversion pipeline as a static musl binary.
//!
//! The implementation is Linux-only (AF_VSOCK, `reboot(2)`, devtmpfs, …),
//! so the guts live in a `#[cfg(target_os = "linux")]` module. On other
//! platforms the binary still compiles — so `cargo check --workspace` is
//! green for macOS developers — but refuses to run.

#[cfg(target_os = "linux")]
mod agent;

#[cfg(target_os = "linux")]
fn main() {
    std::hint::black_box(&temps_vm_agent::BUILD_MARKER);
    // PID 1 re-executes itself as the sandbox user for a write into that
    // user's files (`agent::as_sandbox_user`); that child does one
    // operation and exits.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(temps_vm_agent::sandbox_user::AS_USER_FLAG) {
        std::process::exit(agent::run_as_user(&args[1..]));
    }
    agent::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("temps-vm-agent is a Linux guest binary and cannot run on this platform");
    std::process::exit(1);
}
