// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network policy for workloads (ADR-052 §4, ADR-055 §9 and §17).
//!
//! - [`rule`]: the rule model. Rules are data, keyed by workload identity
//!   ([`temps_workload::WorkloadRef`]), never by IP literals: an address
//!   changes on every restart, an identity does not.
//! - [`identity`]: the current addresses of each workload, the only place
//!   addresses appear.
//! - [`nft`]: compiles policies and identities into one nftables ruleset,
//!   replaced atomically. Each workload's addresses become a set, so an
//!   address change is a set update, not a rule rewrite.
//! - [`executor`]: applies a ruleset where the Docker bridges are: on the
//!   host (`HostNft`, Linux) or inside the Docker VM (`DockerVmNft`,
//!   macOS). The same ruleset everywhere.
//! - [`microsandbox`]: the same policy as a microsandbox SDK network
//!   policy, for the microVM driver.
//!
//! Nothing here touches the database or calls another concern crate.

pub mod executor;
pub mod identity;
pub mod microsandbox;
pub mod nft;
pub mod rule;

pub use executor::{choose_executor, DockerVmNft, HostNft, NftError, NftExecutor};
pub use identity::IdentityMap;
pub use microsandbox::{to_sdk_policy, SdkNetworkPolicy, Translated};
pub use nft::{compile, removal_ruleset, CompileError, Compiled, TableName};
pub use rule::{
    Action, Cidr, Direction, InvalidCidr, L4Protocol, Origin, Peer, PortRange, Profile, Rule,
    WorkloadPolicy,
};
