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
    agent::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("temps-vm-agent is a Linux guest binary and cannot run on this platform");
    std::process::exit(1);
}
