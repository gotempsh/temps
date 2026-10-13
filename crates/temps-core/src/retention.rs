// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extension point for per-project ClickHouse retention resolution.
//!
//! OSS installs [`SettingsRetentionResolver`] by default, which stamps rows
//! with the operator's `observability_retention` setting (the same values the
//! TimescaleDB retention policies use). A plugin (e.g. one implementing per-project
//! data retention policies) can supply an alternative implementation — callers
//! pass an `Arc<dyn RetentionResolver>` received at construction time and the
//! plugin overrides the default by registering its implementation via the
//! service registry before the storage backends are wired up.
//!
//! `resolve` is called on the ingest path (once per row in a batch, not once
//! per HTTP request) and must be synchronous and lock-free on the read path.
//! Any expensive lookup (Postgres, external service) must be driven by a
//! background refresh task in the implementing type; the result must be cached
//! so that individual `resolve` calls do no I/O.
//!
//! [`RetentionResolverSlot`] exists because `register_services` runs in
//! plugin-registration order: the ClickHouse storage backends are constructed
//! (and their resolver captured) before a later-registered plugin gets a
//! chance to provide one (see `OtelPlugin`/`ProxyPlugin` in their respective
//! crates — the same two-phase handoff `DeploymentGate` uses via
//! `deployment_gate_slot`, adapted to a lock-free `ArcSwap` since `resolve`
//! must stay synchronous).

/// Which ClickHouse table a retention-days resolution is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetentionTable {
    /// OTel spans table (`spans`). Default TTL: 90 days.
    Spans,
    /// OTel metric points table (`metrics`). Default TTL: 90 days.
    Metrics,
    /// Proxy / request-log table (`proxy_logs`). Default TTL: 30 days.
    ProxyLogs,
}

impl RetentionTable {
    /// The table-level TTL constant in days.
    ///
    /// Matches the `DEFAULT` in the `ADD COLUMN … retention_days` migration
    /// and the prior hardcoded `INTERVAL` in the original DDL. Used by
    /// [`FixedRetentionResolver`] and as a fallback for rows that have no
    /// project context (e.g. unrouted proxy requests).
    pub fn default_days(self) -> u16 {
        match self {
            Self::Spans | Self::Metrics => 90,
            Self::ProxyLogs => 30,
        }
    }
}

/// Extension point for resolving the effective `retention_days` to stamp onto
/// an ingested ClickHouse row.
///
/// OSS registers [`FixedRetentionResolver`] at startup, which always returns
/// [`RetentionTable::default_days`] regardless of `project_id`. A plugin (e.g.
/// one implementing per-project data retention policies) registers an
/// implementation via the service registry — `context.register_service(resolver)`
/// — only when appropriate (e.g. gated by its own licensing check). Storage
/// backends receive the resolver at construction time as
/// `Arc<dyn RetentionResolver>`, so a plugin-free binary uses the fixed
/// default unconditionally and the ClickHouse rows self-expire at the
/// table-level TTL without any configuration.
pub trait RetentionResolver: Send + Sync {
    /// Return the effective `retention_days` for `project_id` in `table`.
    ///
    /// The returned value is written into the `retention_days` column of each
    /// new row, where it drives the per-row TTL expression
    /// `toDateTime(<time_col>) + toIntervalDay(retention_days)`.
    ///
    /// Implementations must be synchronous and must not perform I/O — see the
    /// module-level note.
    fn resolve(&self, project_id: i32, table: RetentionTable) -> u16;

    /// Return the instance-wide `retention_days` for rows with no project
    /// context (e.g. unrouted proxy requests), instead of passing a
    /// fabricated project ID to [`Self::resolve`].
    ///
    /// Defaults to [`RetentionTable::default_days`]; a resolver that follows
    /// instance settings overrides it so those rows follow them too.
    fn resolve_unscoped(&self, table: RetentionTable) -> u16 {
        table.default_days()
    }
}

/// Default [`RetentionResolver`] that returns the ClickHouse table-level
/// default for every project.
///
/// Registered at startup when no overriding implementation is present.
/// Callers with no project context (e.g. unrouted proxy requests where
/// `project_id` is `NULL`) should use [`RetentionTable::default_days`]
/// directly rather than passing a fabricated project ID to this resolver.
pub struct FixedRetentionResolver;

impl RetentionResolver for FixedRetentionResolver {
    fn resolve(&self, _project_id: i32, table: RetentionTable) -> u16 {
        table.default_days()
    }
}

/// Bounds `observability_retention` values are validated against when saved.
const MIN_RETENTION_DAYS: u32 = 1;
const MAX_RETENTION_DAYS: u32 = 3650;

/// Default [`RetentionResolver`]: stamps every project's rows with the
/// instance-wide `observability_retention` setting.
///
/// Without it, ClickHouse rows were always stamped with the table default
/// (90 days for spans and metrics, 30 for proxy logs), so changing
/// `otel_spans_days`, `otel_metrics_days` or `proxy_logs_days` only affected
/// TimescaleDB: lowering it did not free ClickHouse disk, and raising it past
/// the default still deleted data at the default.
///
/// `resolve` reads atomics and does no I/O. The owner loads the settings
/// before the first insert and refreshes them in the background with
/// [`Self::apply`]. A new value applies to rows ingested afterwards; stored
/// rows keep the window they were written with, because their TTL is
/// per-row.
pub struct SettingsRetentionResolver {
    spans: std::sync::atomic::AtomicU16,
    metrics: std::sync::atomic::AtomicU16,
    proxy_logs: std::sync::atomic::AtomicU16,
}

impl SettingsRetentionResolver {
    /// Start from the table defaults, which are also the settings defaults.
    pub fn new() -> Self {
        Self {
            spans: std::sync::atomic::AtomicU16::new(RetentionTable::Spans.default_days()),
            metrics: std::sync::atomic::AtomicU16::new(RetentionTable::Metrics.default_days()),
            proxy_logs: std::sync::atomic::AtomicU16::new(RetentionTable::ProxyLogs.default_days()),
        }
    }

    /// Start from `settings`.
    pub fn from_settings(settings: &crate::ObservabilityRetentionSettings) -> Self {
        let resolver = Self::new();
        resolver.apply(settings);
        resolver
    }

    /// Load new settings. Values are clamped to the range the settings API
    /// accepts, so a row written from a hand-edited settings row still gets a
    /// sane TTL.
    pub fn apply(&self, settings: &crate::ObservabilityRetentionSettings) {
        use std::sync::atomic::Ordering::Relaxed;
        self.spans
            .store(clamp_retention_days(settings.otel_spans_days), Relaxed);
        self.metrics
            .store(clamp_retention_days(settings.otel_metrics_days), Relaxed);
        self.proxy_logs
            .store(clamp_retention_days(settings.proxy_logs_days), Relaxed);
    }
}

impl Default for SettingsRetentionResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl RetentionResolver for SettingsRetentionResolver {
    fn resolve(&self, _project_id: i32, table: RetentionTable) -> u16 {
        self.resolve_unscoped(table)
    }

    /// The setting is instance-wide, so rows without a project follow it too.
    fn resolve_unscoped(&self, table: RetentionTable) -> u16 {
        use std::sync::atomic::Ordering::Relaxed;
        match table {
            RetentionTable::Spans => self.spans.load(Relaxed),
            RetentionTable::Metrics => self.metrics.load(Relaxed),
            RetentionTable::ProxyLogs => self.proxy_logs.load(Relaxed),
        }
    }
}

fn clamp_retention_days(days: u32) -> u16 {
    // 3650 fits in a u16, the type of the ClickHouse `retention_days` column.
    days.clamp(MIN_RETENTION_DAYS, MAX_RETENTION_DAYS) as u16
}

/// Deferred-registration handle for a [`RetentionResolver`].
///
/// Constructed with [`FixedRetentionResolver`] loaded by default and handed
/// to a storage backend immediately at construction time (as
/// `Arc<dyn RetentionResolver>`, via unsized coercion — this type itself
/// implements the trait). Once every plugin has finished `register_services`,
/// whichever plugin owns the slot calls [`Self::set`] from
/// `initialize_plugin_services` if a plugin registered an alternative
/// resolver — see the module-level note for why this indirection exists.
/// `resolve` reads are lock-free (`ArcSwap::load`).
///
/// **Write-once semantics:** only the first call to [`Self::set`] takes
/// effect. A second caller cannot silently overwrite a resolver that was
/// already installed — the call is a no-op and returns `false`. This
/// prevents a buggy or late-registered plugin from replacing a correctly
/// wired resolver after the fact.
pub struct RetentionResolverSlot {
    resolver: arc_swap::ArcSwap<std::sync::Arc<dyn RetentionResolver>>,
    /// Flipped to `true` by the first successful [`Self::set`] call.
    claimed: std::sync::atomic::AtomicBool,
}

impl RetentionResolverSlot {
    /// Start with [`FixedRetentionResolver`] loaded.
    pub fn new_default() -> Self {
        Self::with_default(std::sync::Arc::new(FixedRetentionResolver))
    }

    /// Start with `resolver` loaded (normally a [`SettingsRetentionResolver`]).
    /// Unlike [`Self::set`], this does not claim the slot: a plugin-provided
    /// resolver can still replace it once.
    pub fn with_default(resolver: std::sync::Arc<dyn RetentionResolver>) -> Self {
        Self {
            resolver: arc_swap::ArcSwap::new(std::sync::Arc::new(resolver)),
            claimed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Swap in a resolver provided by a plugin, but only once.
    ///
    /// A resolver that has already been set (by a prior `set()` call) cannot
    /// be silently overwritten by a second plugin. Returns `true` if the swap
    /// was applied, `false` if a resolver was already set and this call was a
    /// no-op.
    pub fn set(&self, resolver: std::sync::Arc<dyn RetentionResolver>) -> bool {
        if self
            .claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            self.resolver.store(std::sync::Arc::new(resolver));
            true
        } else {
            false
        }
    }
}

impl RetentionResolver for RetentionResolverSlot {
    fn resolve(&self, project_id: i32, table: RetentionTable) -> u16 {
        self.resolver.load().resolve(project_id, table)
    }

    fn resolve_unscoped(&self, table: RetentionTable) -> u16 {
        self.resolver.load().resolve_unscoped(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_resolver_returns_table_defaults() {
        let r = FixedRetentionResolver;
        assert_eq!(r.resolve(1, RetentionTable::Spans), 90);
        assert_eq!(r.resolve(1, RetentionTable::Metrics), 90);
        assert_eq!(r.resolve(1, RetentionTable::ProxyLogs), 30);
        // project_id is ignored
        assert_eq!(r.resolve(99999, RetentionTable::Spans), 90);
        assert_eq!(r.resolve(0, RetentionTable::ProxyLogs), 30);
    }

    #[test]
    fn retention_table_default_days() {
        assert_eq!(RetentionTable::Spans.default_days(), 90);
        assert_eq!(RetentionTable::ProxyLogs.default_days(), 30);
    }

    struct AlwaysSeven;
    impl RetentionResolver for AlwaysSeven {
        fn resolve(&self, _project_id: i32, _table: RetentionTable) -> u16 {
            7
        }
    }

    #[test]
    fn slot_defaults_to_fixed_resolver() {
        let slot = RetentionResolverSlot::new_default();
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 90);
        assert_eq!(slot.resolve(1, RetentionTable::ProxyLogs), 30);
    }

    #[test]
    fn slot_set_overrides_the_default() {
        let slot = RetentionResolverSlot::new_default();
        let swapped = slot.set(std::sync::Arc::new(AlwaysSeven));
        assert!(swapped, "first set() must return true");
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 7);
        assert_eq!(slot.resolve(1, RetentionTable::ProxyLogs), 7);
    }

    struct AlwaysForty;
    impl RetentionResolver for AlwaysForty {
        fn resolve(&self, _project_id: i32, _table: RetentionTable) -> u16 {
            40
        }
    }

    #[test]
    fn slot_second_set_is_noop() {
        let slot = RetentionResolverSlot::new_default();

        // First set: succeeds and resolver is active.
        let first = slot.set(std::sync::Arc::new(AlwaysSeven));
        assert!(first, "first set() must return true");
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 7);

        // Second set: must be a no-op — returns false, resolver unchanged.
        let second = slot.set(std::sync::Arc::new(AlwaysForty));
        assert!(!second, "second set() must return false (write-once)");

        // The FIRST resolver is still active; AlwaysForty must not have taken effect.
        assert_eq!(
            slot.resolve(1, RetentionTable::Spans),
            7,
            "resolver must remain AlwaysSeven after rejected second set()"
        );
        assert_eq!(
            slot.resolve(1, RetentionTable::ProxyLogs),
            7,
            "resolver must remain AlwaysSeven after rejected second set()"
        );
    }

    fn retention(
        spans: u32,
        metrics: u32,
        proxy_logs: u32,
    ) -> crate::ObservabilityRetentionSettings {
        crate::ObservabilityRetentionSettings {
            otel_spans_days: spans,
            otel_metrics_days: metrics,
            proxy_logs_days: proxy_logs,
            ..Default::default()
        }
    }

    #[test]
    fn settings_resolver_defaults_match_table_defaults() {
        let r = SettingsRetentionResolver::new();
        for table in [
            RetentionTable::Spans,
            RetentionTable::Metrics,
            RetentionTable::ProxyLogs,
        ] {
            assert_eq!(r.resolve(1, table), table.default_days());
        }
        // The settings defaults and the table defaults must agree, or a fresh
        // instance would stamp rows differently from the DDL DEFAULT.
        let from_default_settings = SettingsRetentionResolver::from_settings(
            &crate::ObservabilityRetentionSettings::default(),
        );
        for table in [
            RetentionTable::Spans,
            RetentionTable::Metrics,
            RetentionTable::ProxyLogs,
        ] {
            assert_eq!(
                from_default_settings.resolve(1, table),
                table.default_days()
            );
        }
    }

    #[test]
    fn settings_resolver_follows_each_setting() {
        let r = SettingsRetentionResolver::from_settings(&retention(7, 14, 3));
        assert_eq!(r.resolve(1, RetentionTable::Spans), 7);
        assert_eq!(r.resolve(1, RetentionTable::Metrics), 14);
        assert_eq!(r.resolve(1, RetentionTable::ProxyLogs), 3);

        // Above the old fixed default too: 365 days must not be cut to 90.
        r.apply(&retention(365, 180, 60));
        assert_eq!(r.resolve(42, RetentionTable::Spans), 365);
        assert_eq!(r.resolve(42, RetentionTable::Metrics), 180);
        assert_eq!(r.resolve(42, RetentionTable::ProxyLogs), 60);
    }

    #[test]
    fn settings_resolver_clamps_out_of_range_values() {
        let r = SettingsRetentionResolver::from_settings(&retention(0, 100_000, u32::MAX));
        assert_eq!(r.resolve(1, RetentionTable::Spans), 1);
        assert_eq!(r.resolve(1, RetentionTable::Metrics), 3650);
        assert_eq!(r.resolve(1, RetentionTable::ProxyLogs), 3650);
    }

    /// Rows with no project (unrouted proxy requests) must follow the
    /// instance setting too, not the fixed table default.
    #[test]
    fn unscoped_rows_follow_settings_through_the_slot() {
        let settings = std::sync::Arc::new(SettingsRetentionResolver::from_settings(&retention(
            7, 14, 3,
        )));
        let slot = RetentionResolverSlot::with_default(settings.clone());
        assert_eq!(slot.resolve_unscoped(RetentionTable::ProxyLogs), 3);
        assert_eq!(slot.resolve_unscoped(RetentionTable::Spans), 7);

        settings.apply(&retention(7, 14, 60));
        assert_eq!(slot.resolve_unscoped(RetentionTable::ProxyLogs), 60);

        // Resolvers that do not override it keep the table default.
        assert_eq!(
            FixedRetentionResolver.resolve_unscoped(RetentionTable::ProxyLogs),
            30
        );
        assert_eq!(AlwaysSeven.resolve_unscoped(RetentionTable::ProxyLogs), 30);
    }

    #[test]
    fn slot_with_default_can_still_be_claimed_by_a_plugin() {
        let settings = std::sync::Arc::new(SettingsRetentionResolver::from_settings(&retention(
            10, 10, 10,
        )));
        let slot = RetentionResolverSlot::with_default(settings.clone());
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 10);

        // A settings refresh is visible through the slot.
        settings.apply(&retention(20, 20, 20));
        assert_eq!(slot.resolve(1, RetentionTable::Metrics), 20);

        // The default does not claim the slot: a plugin resolver still wins.
        assert!(slot.set(std::sync::Arc::new(AlwaysSeven)));
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 7);
    }
}
