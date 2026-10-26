// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The lists every ingress has before it loads a snapshot (ADR 066, I3).
//!
//! Only ranges IANA reserves that can never be a real source address on the
//! public internet. They do not change, so they never go stale, and they
//! are only logged: a match means a spoofed or misrouted packet, or a test.
//! Loopback, private, link-local, shared (CGNAT) and unique-local ranges are
//! left out, because those are where a proxy's own neighbours connect from.

use crate::net::Network;
use crate::snapshot::SourceList;
use crate::verdict::ListVerdict;

pub const IANA_RESERVED: &str = "iana_reserved";

/// RFC 6890 and the IANA special-purpose registries.
const RESERVED: &[&str] = &[
    "0.0.0.0/8",       // "this network" (RFC 791)
    "192.0.2.0/24",    // TEST-NET-1 (RFC 5737)
    "198.51.100.0/24", // TEST-NET-2
    "203.0.113.0/24",  // TEST-NET-3
    "240.0.0.0/4",     // reserved, and the limited broadcast address (RFC 1112)
    "::/128",          // unspecified (RFC 4291)
    "100::/64",        // discard-only (RFC 6666)
    "2001:db8::/32",   // documentation (RFC 3849)
    "3fff::/20",       // documentation (RFC 9637)
];

pub(crate) fn lists() -> Vec<SourceList> {
    let networks = RESERVED.iter().filter_map(|text| Network::parse(text));
    vec![SourceList::new(
        IANA_RESERVED,
        ListVerdict::Log,
        0,
        None,
        networks,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reserved_range_parses() {
        assert_eq!(lists()[0].len(), RESERVED.len());
    }
}
