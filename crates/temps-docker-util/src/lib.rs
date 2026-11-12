// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker helpers that several crates need and none should own.
//!
//! - [`exec`]: run a command in a container and capture its output under an
//!   explicit limit.
//! - [`pull`]: pull an image, optionally reporting progress and bounding the
//!   wait.
//! - [`network`]: create a named bridge network, tolerating a concurrent
//!   creator.
//! - [`image_removal`]: remove images only when no container, running or
//!   stopped, was created from them.
//! - [`upload`]: stream a tar archive into a container, bounded by a stall
//!   timeout rather than a total deadline.
//!
//! This crate sits at the bottom of the dependency graph: bollard and the
//! little it strictly needs, no `temps-*` crate. It does not log, because a
//! log line belongs to the caller that knows why the operation ran: helpers
//! hand back what happened (see [`exec::ExecOutput::stream_error`]) and accept
//! a progress callback instead.
//!
//! Every behaviour on which existing callers differ is a parameter with no
//! default a caller could pick up by accident. In particular there is no
//! default output limit: a caller that keeps unbounded output in memory says
//! [`exec::OutputLimit::Unbounded`], so those callers can be found with grep.

pub mod error;
pub mod exec;
pub mod image_removal;
pub mod network;
pub mod pull;
pub mod upload;

#[cfg(test)]
mod test_support;

pub use error::DockerUtilError;
pub use exec::{
    exec_capture, ExecCaptureOptions, ExecOutput, ExitCodePolicy, OtherFrames, OutputLimit,
    RingBuffer, StreamErrorPolicy,
};
pub use image_removal::{
    list_labelled_images, remove_unused_images, ImageRemovalOutcome, ImageRemovalRequest,
    ImageRemovalStatus, LabelledImage, LabelledImages,
};
pub use network::{ensure_bridge_network, NetworkEnsured};
pub use pull::{pull_image, pull_image_with_progress, PullSpec, PullTimeouts};
