use crate::public_hostname::{base_domain, PublicHostnameStrategy};
use async_trait::async_trait;
use std::collections::HashMap;

/// Resolves the [`PublicHostnameStrategy`] that applies to a given preview/base
/// domain.
///
/// The strategy is stored per managed domain (`dns_managed_domains`), so the
/// implementation (in `temps-dns`) maps a normalized base domain to the mode the
/// operator selected for it. Consumers that only need the layout — primarily the
/// route table and the deployments handler — depend on this trait rather than on
/// `temps-dns` directly, avoiding a crate-dependency cycle.
///
/// When no managed domain matches, implementations MUST return
/// [`PublicHostnameStrategy::Standard`] so behaviour is unchanged for instances
/// without configured DNS providers.
#[async_trait]
pub trait PublicHostnameResolver: Send + Sync {
    /// Resolve the strategy for a single preview/base domain.
    async fn strategy_for(&self, preview_domain: &str) -> PublicHostnameStrategy;

    /// Load every managed domain's strategy keyed by its normalized base domain.
    /// Bulk callers (e.g. route-table rebuilds) use this once per pass and then
    /// match locally via [`match_strategy`].
    async fn strategy_map(&self) -> HashMap<String, PublicHostnameStrategy>;
}

/// Fallback resolver that always reports [`PublicHostnameStrategy::Standard`].
///
/// Used when no DNS provider plugin registered a real resolver, so hostname
/// generation behaves exactly as it did before per-domain modes existed.
pub struct StandardHostnameResolver;

#[async_trait]
impl PublicHostnameResolver for StandardHostnameResolver {
    async fn strategy_for(&self, _preview_domain: &str) -> PublicHostnameStrategy {
        PublicHostnameStrategy::Standard
    }

    async fn strategy_map(&self) -> HashMap<String, PublicHostnameStrategy> {
        HashMap::new()
    }
}

/// Pick the strategy for `preview_domain` from a pre-loaded map keyed by base
/// domain, using a longest-suffix match. Defaults to `Standard` when nothing
/// matches.
pub fn match_strategy(
    map: &HashMap<String, PublicHostnameStrategy>,
    preview_domain: &str,
) -> PublicHostnameStrategy {
    let base = base_domain(preview_domain);
    let mut best: Option<(usize, PublicHostnameStrategy)> = None;
    for (domain, strategy) in map {
        let domain = domain.to_ascii_lowercase();
        let matches = base == domain || base.ends_with(&format!(".{domain}"));
        if matches {
            let len = domain.len();
            if best.map(|(best_len, _)| len > best_len).unwrap_or(true) {
                best = Some((len, *strategy));
            }
        }
    }
    best.map(|(_, s)| s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_standard_when_empty() {
        let map = HashMap::new();
        assert_eq!(
            match_strategy(&map, "*.example.com"),
            PublicHostnameStrategy::Standard
        );
    }

    #[test]
    fn matches_exact_base_domain() {
        let mut map = HashMap::new();
        map.insert("example.com".to_string(), PublicHostnameStrategy::Flat);
        assert_eq!(
            match_strategy(&map, "*.example.com"),
            PublicHostnameStrategy::Flat
        );
    }

    #[test]
    fn matches_subdomain_suffix_and_prefers_longest() {
        let mut map = HashMap::new();
        map.insert("example.com".to_string(), PublicHostnameStrategy::Standard);
        map.insert(
            "staging.example.com".to_string(),
            PublicHostnameStrategy::Flat,
        );
        // preview_domain under the more specific managed domain wins.
        assert_eq!(
            match_strategy(&map, "*.staging.example.com"),
            PublicHostnameStrategy::Flat
        );
        // a different subdomain falls back to the apex managed domain.
        assert_eq!(
            match_strategy(&map, "*.prod.example.com"),
            PublicHostnameStrategy::Standard
        );
    }
}
