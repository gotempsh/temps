// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Making a project reachable (ADR 064): per address, whether visitors can
//! reach it, why not, and one guided, undoable way to fix it.
//!
//! - [`compute`] judges each address from gathered facts (D3).
//! - [`addresses`] names the primary address, the preview domain's status
//!   and the deploy warning (ADR 068 D2, D4, D5).
//! - [`environment_hosts`] gives every environment a host under one
//!   tunnel's domain, when the operator turns it on (ADR 068 D3).
//! - [`plan`] picks the ways in the cluster can offer (D4).
//! - [`run`] carries one out step by step, recording what it creates so
//!   undo removes exactly that.
//! - [`token_check`] tells which Cloudflare permissions a token has (D8).
//! - [`edge_rules`] keeps a tunnel address on HTTPS and standard ports at
//!   Cloudflare's edge, and [`http_check`] flags one that answers plain
//!   HTTP (ADR 070).
//! - [`service`] gathers the facts and owns runs; [`handlers`] serves them.
//! - [`gate_handlers`] is the console's half of the signed-in gate (ADR 065),
//!   and [`gate_check`] checks that it is in front of an address (D10).

pub mod access;
pub mod access_handlers;
pub mod access_service;
pub mod address_handlers;
pub mod addresses;
pub mod compute;
pub mod edge_rules;
pub mod environment_address;
pub mod environment_hosts;
pub mod gate_check;
pub mod gate_handlers;
pub mod handlers;
pub mod http_check;
pub mod model;
pub mod plan;
pub mod principals;
pub mod run;
pub mod serve_entries;
pub mod service;
pub mod token_check;

pub use environment_address::EnvironmentAddressService;
pub use environment_hosts::EnvironmentHostService;
pub use service::{ReachabilityDeps, ReachabilityError, ReachabilityService, WaysIn};
