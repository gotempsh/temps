// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod handlers;
pub mod plugin;
pub mod providers;
pub mod sentry;
pub mod services;

// Re-export main types but not the types modules to avoid ambiguity
pub use handlers::handler;
pub use providers::*;
pub use sentry::{
    DSNService, Envelope, EnvelopeError, EnvelopeItem, SentryIngestionService,
    SENTRY_TUNNEL_ROUTE_PATH,
};
pub use services::*;

// Export plugin
pub use plugin::ErrorTrackingPlugin;

/// Lockfile guards for the `relay-event-schema` dependency this crate owns.
///
/// The workspace patches `opentelemetry-proto` with an empty local crate so
/// relay's unused dependency on it cannot pull `opentelemetry_sdk` 0.30
/// (GHSA-w9wp-h8wv-79jx) into the build; see item 2b in the workspace
/// `Cargo.toml`. A patch only applies to semver-compatible requirements, so a
/// relay bump to `opentelemetry-proto` 0.31 would make Cargo skip it with
/// nothing more than an "unused patch" warning, and the vulnerable SDK would
/// return. These tests turn that into a failure. `Cargo.lock` is embedded at
/// compile time, so they also run from prebuilt nextest archives.
#[cfg(test)]
mod dependency_guard_tests {
    const CARGO_LOCK: &str = include_str!("../../../Cargo.lock");

    /// First `opentelemetry_sdk` release with the W3C Baggage size limits.
    const FIXED_OTEL_SDK: semver::Version = semver::Version::new(0, 32, 1);

    #[derive(Debug)]
    struct LockedPackage<'a> {
        name: &'a str,
        version: &'a str,
        /// `None` for workspace and `[patch]` path crates.
        source: Option<&'a str>,
        /// Raw entries: `"name"` or, when ambiguous, `"name version"`.
        dependencies: Vec<&'a str>,
    }

    fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        line.strip_prefix(key)?
            .strip_prefix(" = \"")?
            .strip_suffix('"')
    }

    fn parse_lock(lock: &str) -> Vec<LockedPackage<'_>> {
        lock.split("[[package]]")
            .skip(1)
            .map(|block| {
                let mut package = LockedPackage {
                    name: "",
                    version: "",
                    source: None,
                    dependencies: Vec::new(),
                };
                let mut in_dependencies = false;
                for line in block.lines().map(str::trim) {
                    if in_dependencies {
                        if line == "]" {
                            in_dependencies = false;
                        } else if let Some(dep) = line
                            .strip_prefix('"')
                            .and_then(|l| l.trim_end_matches(',').strip_suffix('"'))
                        {
                            package.dependencies.push(dep);
                        }
                    } else if line == "dependencies = [" {
                        in_dependencies = true;
                    } else if let Some(name) = field(line, "name") {
                        package.name = name;
                    } else if let Some(version) = field(line, "version") {
                        package.version = version;
                    } else if let Some(source) = field(line, "source") {
                        package.source = Some(source);
                    }
                }
                package
            })
            .collect()
    }

    /// Resolve a `dependencies` entry to the package it names.
    fn resolve<'p, 'a>(
        packages: &'p [LockedPackage<'a>],
        entry: &str,
    ) -> Vec<&'p LockedPackage<'a>> {
        let mut parts = entry.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let version = parts.next();
        packages
            .iter()
            .filter(|p| p.name == name && version.is_none_or(|v| p.version == v))
            .collect()
    }

    /// `opentelemetry_sdk` versions in the lockfile that lack the fix.
    fn vulnerable_otel_sdks<'a>(packages: &[LockedPackage<'a>]) -> Vec<&'a str> {
        packages
            .iter()
            .filter(|p| p.name == "opentelemetry_sdk")
            .filter(|p| {
                semver::Version::parse(p.version)
                    .map(|v| v < FIXED_OTEL_SDK)
                    .unwrap_or(true)
            })
            .map(|p| p.version)
            .collect()
    }

    /// Why relay's `opentelemetry-proto` is not the local stub, if it isn't.
    fn relay_proto_patch_problem(packages: &[LockedPackage<'_>]) -> Option<String> {
        let Some(relay) = packages.iter().find(|p| p.name == "relay-event-schema") else {
            return Some(
                "relay-event-schema is missing from Cargo.lock; if it was removed, delete the \
                 opentelemetry-proto `[patch.crates-io]` entry, the stub crate and these guards"
                    .to_string(),
            );
        };
        let proto_entries: Vec<_> = relay
            .dependencies
            .iter()
            .filter(|d| d.split_whitespace().next() == Some("opentelemetry-proto"))
            .collect();
        if proto_entries.is_empty() {
            return Some(format!(
                "relay-event-schema {} no longer depends on opentelemetry-proto. Remove the \
                 `[patch.crates-io]` entry and crates/patches/opentelemetry-proto-unused, \
                 then these guards.",
                relay.version
            ));
        }
        for entry in proto_entries {
            let resolved = resolve(packages, entry);
            let [package] = resolved.as_slice() else {
                return Some(format!(
                    "relay-event-schema dependency {entry:?} matched {} packages in Cargo.lock: \
                     {resolved:?}",
                    resolved.len()
                ));
            };
            if let Some(source) = package.source {
                return Some(format!(
                    "relay-event-schema {} resolves opentelemetry-proto {} from {source} \
                     instead of the local stub, so the `[patch.crates-io]` entry no longer \
                     applies (relay's requirement probably moved past 0.30). If relay still \
                     never imports opentelemetry_proto, retarget the stub's version; otherwise \
                     drop the patch once relay's opentelemetry-proto pulls opentelemetry_sdk \
                     >= {FIXED_OTEL_SDK}. See item 2b in the workspace Cargo.toml.",
                    relay.version, package.version
                ));
            }
        }
        None
    }

    #[test]
    fn lockfile_parser_reads_relay_entry() {
        let packages = parse_lock(CARGO_LOCK);
        let relay = packages
            .iter()
            .find(|p| p.name == "relay-event-schema")
            .expect("relay-event-schema missing from Cargo.lock");
        assert!(!relay.version.is_empty(), "parsed no version for {relay:?}");
        assert!(
            relay.source.is_some(),
            "relay-event-schema should be a git dependency: {relay:?}"
        );
        assert!(
            !relay.dependencies.is_empty(),
            "parsed no dependencies for {relay:?}"
        );
    }

    #[test]
    fn no_vulnerable_opentelemetry_sdk_is_locked() {
        let vulnerable = vulnerable_otel_sdks(&parse_lock(CARGO_LOCK));
        assert!(
            vulnerable.is_empty(),
            "Cargo.lock contains opentelemetry_sdk {vulnerable:?}, below {FIXED_OTEL_SDK} \
             (GHSA-w9wp-h8wv-79jx, unbounded allocation in W3C Baggage extraction). Run \
             `cargo tree -i opentelemetry_sdk --target all` to find the path; see item 2b \
             in the workspace Cargo.toml."
        );
    }

    #[test]
    fn relay_opentelemetry_proto_resolves_to_local_patch() {
        if let Some(problem) = relay_proto_patch_problem(&parse_lock(CARGO_LOCK)) {
            panic!("{problem}");
        }
    }

    /// A relay bump to opentelemetry-proto 0.31 that Cargo resolves from the
    /// registry, skipping the 0.30 patch, the scenario these guards exist for.
    const BYPASSED_PATCH_LOCK: &str = r#"
[[package]]
name = "opentelemetry-proto"
version = "0.31.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "opentelemetry_sdk",
]

[[package]]
name = "opentelemetry_sdk"
version = "0.31.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "relay-event-schema"
version = "26.1.0"
source = "git+https://github.com/getsentry/relay?rev=0000000#0000000"
dependencies = [
 "opentelemetry-proto",
 "serde",
]
"#;

    #[test]
    fn guards_flag_a_relay_bump_that_bypasses_the_patch() {
        let packages = parse_lock(BYPASSED_PATCH_LOCK);
        assert_eq!(vulnerable_otel_sdks(&packages), vec!["0.31.0"]);
        let problem = relay_proto_patch_problem(&packages)
            .expect("a registry opentelemetry-proto must be reported");
        assert!(
            problem.contains("opentelemetry-proto 0.31.0 from registry+"),
            "unexpected message: {problem}"
        );
    }

    #[test]
    fn guards_resolve_versioned_entries_and_accept_the_stub() {
        // Two opentelemetry-proto copies make Cargo qualify the entry with a
        // version; the stub (no `source`) must still be accepted.
        let lock = r#"
[[package]]
name = "opentelemetry-proto"
version = "0.30.0"

[[package]]
name = "opentelemetry-proto"
version = "0.32.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "opentelemetry_sdk",
]

[[package]]
name = "opentelemetry_sdk"
version = "0.32.1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "relay-event-schema"
version = "25.9.0"
source = "git+https://github.com/getsentry/relay?rev=0000000#0000000"
dependencies = [
 "opentelemetry-proto 0.30.0",
]
"#;
        let packages = parse_lock(lock);
        assert!(vulnerable_otel_sdks(&packages).is_empty());
        assert_eq!(relay_proto_patch_problem(&packages), None);
    }

    #[test]
    fn guards_report_when_relay_drops_the_dependency() {
        let lock = r#"
[[package]]
name = "relay-event-schema"
version = "26.1.0"
source = "git+https://github.com/getsentry/relay?rev=0000000#0000000"
dependencies = [
 "serde",
]
"#;
        let problem = relay_proto_patch_problem(&parse_lock(lock))
            .expect("a relay without opentelemetry-proto must be reported");
        assert!(problem.contains("no longer depends on opentelemetry-proto"));
    }
}
