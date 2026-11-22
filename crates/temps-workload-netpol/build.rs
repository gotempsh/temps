// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Builds the network policy eBPF programs (`crates/temps-netpol-ebpf`) when
//! the `ebpf` feature is on, and leaves the object in `OUT_DIR` for
//! `include_bytes_aligned!`.
//!
//! The programs need nightly Rust with `rust-src` and `bpf-linker`. With the
//! feature off nothing is built and the datapath reports that it is not in
//! this build, so macOS and CI builds need neither.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "build script: panicking is how a build script reports failure, and it reads the variables Cargo sets for it"
)]
use std::env;
use std::path::PathBuf;
use std::process::Command;

const OBJECT: &str = "temps-netpol-ebpf.o";

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let programs = manifest.join("../temps-netpol-ebpf");
    let common = manifest.join("../temps-netpol-ebpf-common");
    for path in [
        programs.join("src"),
        programs.join("Cargo.toml"),
        common.join("src"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if env::var_os("CARGO_FEATURE_EBPF").is_none() {
        return;
    }
    // The object is only embedded on Linux (`ebpf/mod.rs`), so other targets
    // have nothing to build even with the feature on.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default());
    // Clippy (`--all-features` in CI and the pre-commit hook) lints the Rust
    // that embeds the object, not the object itself, so an empty placeholder
    // stands in rather than requiring nightly and bpf-linker for a lint run.
    if is_clippy() {
        if let Err(error) = std::fs::write(out.join(OBJECT), []) {
            panic!(
                "writing the placeholder eBPF object for clippy to {}: {error}",
                out.display()
            );
        }
        return;
    }
    let target_dir = out.join("ebpf-target");

    // Run the programs' own workspace with its own toolchain file
    // (nightly). Everything the outer build sets for itself is removed, or
    // it would leak into a build for a different target.
    let mut cargo = Command::new("rustup");
    cargo
        .args([
            "run",
            "nightly",
            "cargo",
            "build",
            "--release",
            "--target-dir",
        ])
        .arg(&target_dir)
        .current_dir(&programs);
    for var in [
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTUP_TOOLCHAIN",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "CARGO_MAKEFLAGS",
        "CARGO_PROFILE_RELEASE_DEBUG",
    ] {
        cargo.env_remove(var);
    }
    let status = match cargo.status() {
        Ok(status) => status,
        Err(error) => panic!(
            "the `ebpf` feature builds the network policy programs with `rustup run nightly cargo`, \
             which could not start: {error}. Install rustup, the nightly toolchain with rust-src \
             (`rustup toolchain install nightly --component rust-src`) and `cargo install bpf-linker`, \
             or build without the `ebpf` feature"
        ),
    };
    if !status.success() {
        panic!(
            "building the network policy eBPF programs in {} failed ({status}); see the output above. \
             They need nightly Rust with rust-src and bpf-linker",
            programs.display()
        );
    }
    let built = target_dir.join("bpfel-unknown-none/release/temps-netpol-ebpf");
    if let Err(error) = std::fs::copy(&built, out.join(OBJECT)) {
        panic!(
            "copying the built eBPF object {} to {}: {error}",
            built.display(),
            out.display()
        );
    }
}

/// Whether this build script runs for `cargo clippy`, which sets the
/// workspace wrapper to `clippy-driver`.
fn is_clippy() -> bool {
    env::var_os("RUSTC_WORKSPACE_WRAPPER")
        .map(PathBuf::from)
        .and_then(|path| path.file_stem().map(|stem| stem == "clippy-driver"))
        .unwrap_or(false)
}
