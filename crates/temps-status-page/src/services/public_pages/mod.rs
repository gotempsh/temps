// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Publishable status pages: admin management plus the cached, read-only
//! public view (#1412).

pub mod domains;
pub mod render;
pub mod service;
pub mod snapshot;
pub mod types;

pub use domains::{
    normalize_status_page_hostname, AddStatusPageDomainRequest, DnsRecordInstruction,
    DomainServiceCertificates, StatusPageCertificates, StatusPageDomainResponse,
    StatusPageDomainService, StatusPageDomainStatus, VerificationMethod,
    MAX_DOMAINS_PER_STATUS_PAGE,
};
pub use service::{is_well_formed_slug, PublicStatusPageService, StatusPageListFilter, SLUG_LEN};
pub use snapshot::{PublishedSnapshot, StatusSnapshotCache, REFRESH_INTERVAL};
pub use types::*;
