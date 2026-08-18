use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

const DEFAULT_BASE_DOMAIN: &str = "localho.st";
const DNS_LABEL_MAX_LEN: usize = 63;
const SHORT_HASH_LEN: usize = 8;

/// Public hostname generation mode for Temps-managed preview routes.
///
/// The mode is stored per managed domain (`dns_managed_domains.generated_hostname_mode`)
/// rather than globally, so a provider such as Cloudflare can offer the flat layout
/// required by its Universal SSL wildcard cert without changing every domain's behaviour.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PublicHostnameStrategy {
    /// Preserve Temps' existing generated hostname layout (`{service}--{env}.base`).
    #[default]
    Standard,
    /// Force generated service hostnames to one label below `preview_domain`
    /// (`{env}--{service}.base`) so a single-label wildcard cert covers them.
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
    /// strategies: Standard yields `{service}--{environment}.base`, Flat
    /// yields `{environment}--{service}.base`.
    ///
    /// The two halves are joined by a **double** hyphen, and that is a
    /// security property rather than cosmetics. Docker Compose service names
    /// come from the tenant's compose file, so with a single hyphen a service
    /// named `foo` in an environment slugged `bar-prod` generates exactly the
    /// hostname another tenant's environment `foo-bar-prod` owns — and route
    /// insertion is first-come/vacancy-based, so whichever tenant is loaded
    /// first captures the other's preview traffic.
    ///
    /// Every slug generator in the codebase collapses runs of hyphens
    /// (`generate_slug`, `slugify_branch_name`), so no project, environment or
    /// deployment slug can ever contain `--`. Using it as the separator makes
    /// the collision structurally impossible instead of order-dependent.
    pub fn service_hostname(
        self,
        preview_domain: &str,
        environment: &str,
        service: &str,
    ) -> String {
        let base = normalize_base_domain(preview_domain);
        let label = match self {
            PublicHostnameStrategy::Standard => namespaced_service_label(service, environment),
            PublicHostnameStrategy::Flat => namespaced_service_label(environment, service),
        };
        format!("{label}.{base}")
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
    let sanitized = sanitize_label(label);
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

/// Join two already-sanitized name parts into one DNS label using the
/// double-hyphen namespace separator. Built here rather than via
/// [`dns_label`] because that path collapses hyphen runs, which would erase
/// the separator and reintroduce the ambiguity it exists to prevent.
fn namespaced_service_label(first: &str, second: &str) -> String {
    let combined = format!("{}--{}", sanitize_label(first), sanitize_label(second));
    if combined.len() <= DNS_LABEL_MAX_LEN {
        return combined;
    }

    // Over-long labels are truncated with a hash of the full name, so two
    // different services that share a 55-character prefix still get distinct
    // hostnames.
    //
    // The hash is joined with `--`, not `-`, and that is load-bearing. With a
    // single hyphen the truncated form `first--second-<hash>` is a string an
    // *untruncated* label can also produce, by picking a `second` that ends in
    // `-<hash>` — and `short_hash` is plain SHA-256, so an attacker computes
    // the target offline and squats another tenant's hostname, which is the
    // exact collision this function exists to prevent. `sanitize_label`
    // collapses hyphen runs, so neither part can itself contain `--`: an
    // untruncated label therefore holds exactly one `--` and a truncated one
    // holds two, making the two forms structurally impossible to confuse.
    let suffix = format!("--{}", short_hash(&combined));
    let max_prefix_len = DNS_LABEL_MAX_LEN.saturating_sub(suffix.len());
    let prefix = combined
        .chars()
        .take(max_prefix_len)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string();

    if prefix.is_empty() {
        short_hash(&combined)
    } else {
        format!("{prefix}{suffix}")
    }
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
            "files--staging.example.com"
        );
    }

    #[test]
    fn flat_service_hostname_uses_environment_first() {
        assert_eq!(
            PublicHostnameStrategy::Flat.service_hostname("example.com", "staging", "files"),
            "staging--files.example.com"
        );
    }

    /// A tenant-chosen Compose service name must not be able to generate the
    /// hostname another tenant's environment owns. With a single-hyphen join,
    /// service `foo` in environment `bar-prod` produced exactly
    /// `foo-bar-prod.example.com` — the preview host of environment
    /// `foo-bar-prod` — and whichever route was inserted first won.
    #[test]
    fn service_hostname_cannot_collide_with_an_environment_hostname() {
        let squatted =
            PublicHostnameStrategy::Standard.service_hostname("example.com", "bar-prod", "foo");
        let victim =
            PublicHostnameStrategy::Standard.environment_hostname("example.com", "foo-bar-prod");
        assert_ne!(squatted, victim);
        assert_eq!(squatted, "foo--bar-prod.example.com");
        assert_eq!(victim, "foo-bar-prod.example.com");
    }

    /// The separator survives sanitization: a service name containing runs of
    /// punctuation must not be able to smuggle its own `--` boundary in and
    /// re-create the ambiguity from the other direction.
    #[test]
    fn service_names_cannot_forge_the_namespace_separator() {
        let forged =
            PublicHostnameStrategy::Standard.service_hostname("example.com", "prod", "foo--bar");
        let genuine =
            PublicHostnameStrategy::Standard.service_hostname("example.com", "bar-prod", "foo");
        assert_ne!(forged, genuine);
    }

    /// Regression: a *truncated* label must not be reproducible by an
    /// untruncated one.
    ///
    /// When the hash was joined with a single hyphen, `first--second-<hash>`
    /// was a string an attacker could also produce untruncated, by choosing an
    /// environment slug ending in `-<hash>` — and `short_hash` is plain
    /// SHA-256, so the target is computable offline. Route insertion is
    /// vacancy-based and cert-eligible, so whoever loads first captures the
    /// other tenant's traffic and its on-demand certificate.
    ///
    /// The invariant that closes it: an untruncated label contains exactly one
    /// `--` (sanitize_label collapses hyphen runs, so neither part can hold
    /// one), a truncated label contains two.
    #[test]
    fn a_truncated_label_cannot_be_forged_by_an_untruncated_one() {
        // Long enough to force truncation.
        let victim_env = format!("prod-{}", "x".repeat(55));
        let victim =
            PublicHostnameStrategy::Standard.service_hostname("example.com", &victim_env, "app");
        let victim_label = victim.split('.').next().unwrap();
        assert!(victim_label.len() <= 63);
        assert_eq!(
            victim_label.matches("--").count(),
            2,
            "a truncated label must carry both separators: {victim_label}"
        );

        // Replay the truncated label back as an attacker-chosen environment
        // slug. Whatever it produces, it must not be the victim's hostname.
        let stolen = victim_label
            .strip_prefix("app--")
            .expect("victim label starts with the service namespace");
        let attacker =
            PublicHostnameStrategy::Standard.service_hostname("example.com", stolen, "app");
        assert_ne!(
            attacker, victim,
            "an untruncated label reproduced a truncated one"
        );
    }

    /// An over-long service+environment pair is truncated with a hash rather
    /// than silently colliding on the shared prefix.
    #[test]
    fn overlong_service_labels_stay_distinct_and_within_dns_limits() {
        let long_a = "a".repeat(60);
        let long_b = format!("{}b", "a".repeat(59));
        let host_a =
            PublicHostnameStrategy::Standard.service_hostname("example.com", "prod", &long_a);
        let host_b =
            PublicHostnameStrategy::Standard.service_hostname("example.com", "prod", &long_b);
        assert_ne!(host_a, host_b);
        for host in [&host_a, &host_b] {
            let label = host.split('.').next().unwrap();
            assert!(label.len() <= 63, "label {label} exceeds the DNS limit");
        }
    }

    #[test]
    fn environment_hostname_is_strategy_independent() {
        let env = "preview-123";
        assert_eq!(
            PublicHostnameStrategy::Standard.environment_hostname("example.com", env),
            PublicHostnameStrategy::Flat.environment_hostname("example.com", env),
        );
        assert_eq!(
            PublicHostnameStrategy::Flat.environment_hostname("*.example.com", env),
            "preview-123.example.com"
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
