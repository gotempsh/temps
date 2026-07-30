use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

const DEFAULT_BASE_DOMAIN: &str = "localho.st";
const DNS_LABEL_MAX_LEN: usize = 63;
const SHORT_HASH_LEN: usize = 8;
/// Appended to a generated label that would otherwise end in an IPv4 octet.
/// See [`guard_trailing_ip_octet`].
const OCTET_GUARD_SUFFIX: &str = "x";

/// Public hostname generation mode for Temps-managed preview routes.
///
/// The mode is stored per managed domain (`dns_managed_domains.generated_hostname_mode`)
/// rather than globally, so a provider such as Cloudflare can offer the flat layout
/// required by its Universal SSL wildcard cert without changing every domain's behaviour.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PublicHostnameStrategy {
    /// Preserve Temps' existing generated hostname layout (`{service}-{env}.base`).
    #[default]
    Standard,
    /// Force generated service hostnames to one label below `preview_domain`
    /// (`{env}-{service}.base`) so a single-label wildcard cert covers them.
    Flat,
}

impl PublicHostnameStrategy {
    /// Stable string used to persist the strategy in `dns_managed_domains`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            PublicHostnameStrategy::Standard => "standard",
            PublicHostnameStrategy::Flat => "flat",
        }
    }

    /// Parse the persisted strategy string. Unknown values fall back to
    /// `Standard` so an unrecognised column value never breaks hostname
    /// generation (forward-compatible).
    pub fn from_db_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "flat" => PublicHostnameStrategy::Flat,
            _ => PublicHostnameStrategy::Standard,
        }
    }

    fn force_single_label(self) -> bool {
        matches!(self, PublicHostnameStrategy::Flat)
    }

    /// Environment public host: `{environment}.{base_domain}` (identical for both
    /// strategies; already a single label below the base).
    pub fn environment_hostname(self, preview_domain: &str, environment: &str) -> String {
        let base = normalize_base_domain(preview_domain);
        let raw = format!("{environment}.{base}");
        normalize_hostname(&raw, &base, self.force_single_label())
    }

    /// Per-service public host. This is the only layout that differs between
    /// strategies: Standard yields `{service}-{environment}.base`, Flat yields
    /// `{environment}-{service}.base`.
    pub fn service_hostname(
        self,
        preview_domain: &str,
        environment: &str,
        service: &str,
    ) -> String {
        let base = normalize_base_domain(preview_domain);
        let raw = match self {
            PublicHostnameStrategy::Standard => format!("{service}-{environment}.{base}"),
            PublicHostnameStrategy::Flat => format!("{environment}-{service}.{base}"),
        };
        normalize_hostname(&raw, &base, self.force_single_label())
    }

    /// Deployment public host: `{deployment}.{base_domain}` (single label for both).
    pub fn deployment_hostname(self, preview_domain: &str, deployment: &str) -> String {
        let base = normalize_base_domain(preview_domain);
        let raw = format!("{deployment}.{base}");
        normalize_hostname(&raw, &base, self.force_single_label())
    }

    /// Calculated project/deployment host: `{project}-{environment}-{deployment}.base`
    /// (single label for both strategies).
    pub fn project_deployment_hostname(
        self,
        preview_domain: &str,
        project: &str,
        environment: &str,
        deployment: &str,
    ) -> String {
        let base = normalize_base_domain(preview_domain);
        let raw = format!("{project}-{environment}-{deployment}.{base}");
        normalize_hostname(&raw, &base, self.force_single_label())
    }
}

/// Normalize the configured preview domain into the base domain used for
/// generated public hosts. Accepts both `example.com` and `*.example.com`.
pub fn base_domain(preview_domain: &str) -> String {
    normalize_base_domain(preview_domain)
}

fn normalize_base_domain(preview_domain: &str) -> String {
    let trimmed = preview_domain
        .trim()
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();

    if trimmed.is_empty() {
        DEFAULT_BASE_DOMAIN.to_string()
    } else {
        trimmed
    }
}

fn normalize_hostname(raw: &str, base_domain: &str, force_single_label: bool) -> String {
    let host = raw
        .trim()
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let base_domain = normalize_base_domain(base_domain);
    let suffix = format!(".{base_domain}");

    let relative = if host == base_domain {
        String::new()
    } else if host.ends_with(&suffix) {
        host[..host.len() - suffix.len()].to_string()
    } else {
        host
    };

    let raw_labels: Vec<&str> = relative
        .split('.')
        .filter(|label| !label.is_empty())
        .collect();
    if raw_labels.is_empty() {
        return base_domain;
    }

    let labels = if force_single_label {
        vec![dns_label(&raw_labels.join("-"), &relative)]
    } else {
        raw_labels
            .iter()
            .map(|label| dns_label(label, label))
            .collect()
    };

    format!("{}.{}", labels.join("."), base_domain)
}

fn dns_label(label: &str, hash_seed: &str) -> String {
    let sanitized = guard_trailing_ip_octet(&sanitize_label(label));
    if sanitized.len() <= DNS_LABEL_MAX_LEN {
        return sanitized;
    }

    let suffix = format!("-{}", short_hash(hash_seed));
    let max_prefix_len = DNS_LABEL_MAX_LEN.saturating_sub(suffix.len());
    let prefix = sanitized
        .chars()
        .take(max_prefix_len)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string();

    if prefix.is_empty() {
        short_hash(hash_seed)
    } else {
        format!("{prefix}{suffix}")
    }
}

/// Append a non-numeric marker when a label would end in something a
/// wildcard-DNS service parses as an IP octet.
///
/// Wildcard-DNS services (sslip.io, nip.io) resolve `<name>.<ip>.sslip.io` by
/// scanning the hostname for four numeric components joined by a *single*
/// separator style — all dots or all dashes — and taking the leftmost match.
/// That match is not anchored to the base domain, so a generated label ending
/// in a decimal octet donates its trailing number to the front of the base
/// domain's IP and pushes the real last octet out:
///
/// ```text
/// observability-starter-1.127.0.0.1.sslip.io  ->  1.127.0.0   (unreachable)
/// pr-42.127.0.0.1.sslip.io                    ->  42.127.0.0  (unreachable)
/// ```
///
/// Temps generates exactly this shape as a matter of course — deployment slugs
/// are `{project}-{n}` and preview environments are `pr-{number}` — so on a
/// sslip.io install (the quick-start default) every generated hostname would
/// time out. Appending a letter makes the trailing component non-numeric, which
/// stops the match: `observability-starter-1x.127.0.0.1.sslip.io` resolves to
/// `127.0.0.1`.
///
/// Applied to every generated label, not just sslip.io installs, so a hostname
/// is identical across deployments regardless of the operator's base domain.
fn guard_trailing_ip_octet(label: &str) -> String {
    let trailing = label.rsplit('-').next().unwrap_or(label);
    if is_ip_octet(trailing) {
        format!("{label}{OCTET_GUARD_SUFFIX}")
    } else {
        label.to_string()
    }
}

/// Whether `token` is a decimal IPv4 octet as a wildcard-DNS service parses it.
///
/// Leading zeros (`01`) and out-of-range values (`256`) are rejected by those
/// services, so they need no guarding — verified against live sslip.io
/// resolution.
fn is_ip_octet(token: &str) -> bool {
    if token.is_empty() || token.len() > 3 || !token.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if token.len() > 1 && token.starts_with('0') {
        return false;
    }
    token.parse::<u16>().is_ok_and(|value| value <= 255)
}

fn sanitize_label(label: &str) -> String {
    let mut output = String::new();
    let mut previous_hyphen = false;

    for ch in label.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            output.push(lower);
            previous_hyphen = false;
        } else if !previous_hyphen {
            output.push('-');
            previous_hyphen = true;
        }
    }

    let trimmed = output.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed
    }
}

fn short_hash(seed: &str) -> String {
    let digest = Sha256::digest(seed.as_bytes());
    hex::encode(digest).chars().take(SHORT_HASH_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_domain_strips_wildcard_prefix() {
        assert_eq!(base_domain("*.Example.COM."), "example.com");
    }

    #[test]
    fn standard_service_hostname_preserves_existing_order() {
        assert_eq!(
            PublicHostnameStrategy::Standard.service_hostname("*.example.com", "staging", "files"),
            "files-staging.example.com"
        );
    }

    #[test]
    fn flat_service_hostname_uses_environment_first() {
        assert_eq!(
            PublicHostnameStrategy::Flat.service_hostname("example.com", "staging", "files"),
            "staging-files.example.com"
        );
    }

    #[test]
    fn environment_hostname_is_strategy_independent() {
        let env = "preview-123";
        assert_eq!(
            PublicHostnameStrategy::Standard.environment_hostname("example.com", env),
            PublicHostnameStrategy::Flat.environment_hostname("example.com", env),
        );
        // Trailing `123` is guarded to `123x` so wildcard-DNS bases can't parse
        // it as an IP octet — see `guard_trailing_ip_octet`.
        assert_eq!(
            PublicHostnameStrategy::Flat.environment_hostname("*.example.com", env),
            "preview-123x.example.com"
        );
    }

    #[test]
    fn db_str_round_trips_and_defaults() {
        assert_eq!(PublicHostnameStrategy::Standard.as_db_str(), "standard");
        assert_eq!(PublicHostnameStrategy::Flat.as_db_str(), "flat");
        assert_eq!(
            PublicHostnameStrategy::from_db_str("flat"),
            PublicHostnameStrategy::Flat
        );
        assert_eq!(
            PublicHostnameStrategy::from_db_str("FLAT"),
            PublicHostnameStrategy::Flat
        );
        // Unknown / legacy values fall back to Standard.
        assert_eq!(
            PublicHostnameStrategy::from_db_str("bogus"),
            PublicHostnameStrategy::Standard
        );
    }

    /// A deployment slug is `{project}-{n}`, so without the guard every single
    /// deployment on a sslip.io install produces an unreachable hostname:
    /// `observability-starter-1.127.0.0.1.sslip.io` resolves to `1.127.0.0`.
    #[test]
    fn deployment_slug_hostname_does_not_end_in_an_ip_octet() {
        assert_eq!(
            PublicHostnameStrategy::Standard
                .deployment_hostname("127.0.0.1.sslip.io", "observability-starter-1"),
            "observability-starter-1x.127.0.0.1.sslip.io"
        );
    }

    /// Preview environments are named `pr-{number}` / `preview-{number}`, which
    /// hit the same parse.
    #[test]
    fn preview_environment_hostname_does_not_end_in_an_ip_octet() {
        assert_eq!(
            PublicHostnameStrategy::Standard.environment_hostname("127.0.0.1.sslip.io", "pr-42"),
            "pr-42x.127.0.0.1.sslip.io"
        );
        assert_eq!(
            PublicHostnameStrategy::Flat.environment_hostname("example.com", "preview-123"),
            "preview-123x.example.com"
        );
    }

    #[test]
    fn guard_applies_to_every_label_of_a_multi_label_host() {
        // Standard keeps one label per dotted component; each is guarded, so no
        // component can donate a number to the one that follows it.
        assert_eq!(
            PublicHostnameStrategy::Standard
                .deployment_hostname("127.0.0.1.sslip.io", "api-7.edge-2"),
            "api-7x.edge-2x.127.0.0.1.sslip.io"
        );
    }

    #[test]
    fn flat_strategy_guards_the_joined_label() {
        assert_eq!(
            PublicHostnameStrategy::Flat.service_hostname("example.com", "staging", "web-2"),
            "staging-web-2x.example.com"
        );
    }

    #[test]
    fn hostnames_not_ending_in_an_octet_are_untouched() {
        // The guard must not churn the hostnames operators already depend on.
        assert_eq!(
            PublicHostnameStrategy::Standard
                .environment_hostname("127.0.0.1.sslip.io", "observability-starter-production"),
            "observability-starter-production.127.0.0.1.sslip.io"
        );
        assert_eq!(
            PublicHostnameStrategy::Standard.service_hostname("example.com", "staging", "files"),
            "files-staging.example.com"
        );
    }

    /// Values a wildcard-DNS service already refuses to read as an octet need
    /// no guard — verified against live sslip.io resolution.
    #[test]
    fn only_real_octets_are_guarded() {
        for token in ["0", "1", "42", "255"] {
            assert!(is_ip_octet(token), "{token} should be treated as an octet");
        }
        for token in ["01", "007", "256", "999", "1234", "1x", "x", ""] {
            assert!(!is_ip_octet(token), "{token} should not be an octet");
        }
    }

    #[test]
    fn guard_is_idempotent() {
        // Re-normalizing an already-guarded host must not keep appending.
        let once = PublicHostnameStrategy::Standard
            .deployment_hostname("127.0.0.1.sslip.io", "observability-starter-1");
        let twice = PublicHostnameStrategy::Standard.deployment_hostname(
            "127.0.0.1.sslip.io",
            once.trim_end_matches(".127.0.0.1.sslip.io"),
        );
        assert_eq!(once, twice);
    }

    #[test]
    fn guarded_label_stays_within_the_dns_length_limit() {
        // 63 chars ending in an octet: the guard would overflow the label, so
        // it must fall through to the hash-truncation path instead.
        let slug = format!("{}-1", "a".repeat(61));
        assert_eq!(slug.len(), DNS_LABEL_MAX_LEN);
        let host = PublicHostnameStrategy::Standard.deployment_hostname("example.com", &slug);
        let label = host.split('.').next().unwrap();
        assert!(
            label.len() <= DNS_LABEL_MAX_LEN,
            "label was {}",
            label.len()
        );
        // The truncation path ends in an 8-char hash, which is never an octet.
        assert!(!is_ip_octet(label.rsplit('-').next().unwrap()));
    }

    #[test]
    fn long_generated_label_gets_stable_hash_suffix() {
        let host = PublicHostnameStrategy::Flat.service_hostname(
            "example.com",
            "preview-this-branch-name-is-deliberately-long-and-keeps-going",
            "extremely-long-service-name-that-would-overflow-the-dns-label",
        );
        let label = host.split('.').next().unwrap();
        assert!(label.len() <= DNS_LABEL_MAX_LEN);
        assert_eq!(
            host,
            PublicHostnameStrategy::Flat.service_hostname(
                "example.com",
                "preview-this-branch-name-is-deliberately-long-and-keeps-going",
                "extremely-long-service-name-that-would-overflow-the-dns-label",
            )
        );
    }
}
