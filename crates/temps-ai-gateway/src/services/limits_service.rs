// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project rate limit and monthly spend cap for deployment-token callers.
//!
//! Every deployed container receives a non-expiring token that can call the
//! AI gateway, and all projects share the operator's provider keys. Without a
//! limit, one runaway or compromised app can exhaust the operator's provider
//! credits. This service is the guard in front of the gateway handlers:
//!
//! * **Rate limit** — a sliding-window counter per project, held in memory.
//!   Each request does one short mutex-protected update on a tiny struct; no
//!   I/O. State is per process, so a multi-node deployment allows up to
//!   `nodes × limit` in aggregate, which is still a hard ceiling.
//! * **Monthly budget** — the project's operator-funded spend since the start
//!   of the UTC month, read from `ai_usage_logs` (via the per-project index)
//!   and cached for a few seconds. It is a soft cap: spend is only known when a
//!   request finishes, so concurrent in-flight requests can overshoot it by up
//!   to the cache lifetime's worth of traffic, which the rate limit bounds.
//!
//! Limits are stored on `ai_gateway_config` rows: `project:{id}` overrides the
//! `instance` row, which overrides the built-in default. A NULL column
//! inherits; `0` means unlimited.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::Serialize;
use temps_entities::ai_gateway_config::{self, Column};
use tracing::{error, warn};
use utoipa::ToSchema;

use crate::error::AiGatewayError;
use crate::services::UsageService;

/// Requests per minute each project may make when neither the project nor the
/// instance configures a limit. Generous for interactive use, small enough to
/// stop a runaway loop within the first minute.
pub const DEFAULT_MAX_REQUESTS_PER_MINUTE: u32 = 300;

/// Upper bound accepted when configuring a limit, to catch unit mistakes.
const MAX_CONFIGURABLE_REQUESTS_PER_MINUTE: i64 = 1_000_000;

const WINDOW_MS: u64 = 60_000;
/// Projects tracked at once; idle entries expire so memory stays bounded.
const MAX_TRACKED_PROJECTS: u64 = 50_000;
const LIMITS_CACHE_TTL: Duration = Duration::from_secs(30);
const SPEND_CACHE_TTL: Duration = Duration::from_secs(5);
const INSTANCE_SCOPE: &str = "instance";

// ============================================================================
// Sliding window
// ============================================================================

/// Sliding-window counter: the current fixed window's count plus the previous
/// window's count weighted by how much of it still overlaps the last 60s.
/// O(1) memory per key, no per-request allocation.
#[derive(Debug, Default)]
struct Window {
    index: u64,
    current: u32,
    previous: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WindowDecision {
    Allowed,
    Limited { retry_after_secs: u64 },
}

pub(crate) struct SlidingWindowLimiter {
    windows: moka::sync::Cache<i32, Arc<Mutex<Window>>>,
}

impl SlidingWindowLimiter {
    pub(crate) fn new() -> Self {
        Self {
            windows: moka::sync::Cache::builder()
                .max_capacity(MAX_TRACKED_PROJECTS)
                .time_to_idle(Duration::from_millis(WINDOW_MS * 2))
                .build(),
        }
    }

    /// Admit one request for `key` unless it already used `limit` in the
    /// trailing minute. `now_ms` is any monotonic-enough millisecond clock;
    /// the caller supplies it so tests are deterministic.
    pub(crate) fn check(&self, key: i32, limit: u32, now_ms: u64) -> WindowDecision {
        let window = self
            .windows
            .get_with(key, || Arc::new(Mutex::new(Window::default())));
        // A poisoned lock only means another thread panicked mid-update of
        // three integers; the counters are still usable.
        let mut window = window.lock().unwrap_or_else(PoisonError::into_inner);

        let index = now_ms / WINDOW_MS;
        match index.saturating_sub(window.index) {
            0 => {}
            1 => {
                window.previous = window.current;
                window.current = 0;
                window.index = index;
            }
            _ => {
                window.previous = 0;
                window.current = 0;
                window.index = index;
            }
        }

        let elapsed = now_ms % WINDOW_MS;
        if Self::admits(window.previous, window.current, elapsed, limit) {
            window.current = window.current.saturating_add(1);
            return WindowDecision::Allowed;
        }
        WindowDecision::Limited {
            retry_after_secs: Self::retry_after_secs(
                window.previous,
                window.current,
                elapsed,
                limit,
            ),
        }
    }

    /// Would one more request fit, given `elapsed` ms into the current window?
    fn admits(previous: u32, current: u32, elapsed: u64, limit: u32) -> bool {
        let remaining_overlap = (WINDOW_MS - elapsed) as f64 / WINDOW_MS as f64;
        f64::from(previous) * remaining_overlap + f64::from(current) + 1.0 <= f64::from(limit)
    }

    /// Whole seconds (at least 1) until a request would be admitted.
    fn retry_after_secs(previous: u32, current: u32, elapsed: u64, limit: u32) -> u64 {
        let window = WINDOW_MS as f64;
        // Within this window the previous count decays linearly to zero, so
        // solve prev * (W - e') / W + cur + 1 <= limit for e'.
        let headroom = f64::from(limit) - f64::from(current) - 1.0;
        let wait_ms = if headroom >= 0.0 && previous > 0 {
            let admit_at = window - headroom * window / f64::from(previous);
            if admit_at < window {
                (admit_at - elapsed as f64).max(0.0)
            } else {
                Self::wait_for_next_window(current, elapsed, limit)
            }
        } else if headroom >= 0.0 {
            0.0
        } else {
            Self::wait_for_next_window(current, elapsed, limit)
        };
        ((wait_ms / 1000.0).ceil() as u64).clamp(1, WINDOW_MS * 2 / 1000)
    }

    /// The current window rolls over: it becomes `previous` and decays.
    fn wait_for_next_window(current: u32, elapsed: u64, limit: u32) -> f64 {
        let window = WINDOW_MS as f64;
        let until_rollover = window - elapsed as f64;
        if current == 0 {
            return until_rollover;
        }
        let decay_needed =
            (window - (f64::from(limit) - 1.0) * window / f64::from(current)).max(0.0);
        until_rollover + decay_needed
    }
}

// ============================================================================
// Types
// ============================================================================

/// Where an effective limit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitSource {
    /// The project's own `project:{id}` configuration
    Project,
    /// The instance-wide default configuration
    Instance,
    /// Nothing configured; the built-in default applies
    BuiltIn,
}

/// The limits that apply to one project after inheritance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EffectiveLimits {
    /// Requests per rolling minute; `null` means unlimited
    pub max_requests_per_minute: Option<u32>,
    pub max_requests_per_minute_source: LimitSource,
    /// Operator-funded spend per UTC calendar month, in microcents
    /// (1/10000 of a cent); `null` means unlimited
    pub max_cost_per_month_microcents: Option<i64>,
    pub max_cost_per_month_source: LimitSource,
}

/// Limits as stored. `None` inherits from the next level; `0` is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LimitSettings {
    pub max_requests_per_minute: Option<i64>,
    pub max_cost_per_month_microcents: Option<i64>,
}

impl From<&ai_gateway_config::Model> for LimitSettings {
    fn from(row: &ai_gateway_config::Model) -> Self {
        Self {
            max_requests_per_minute: row.max_requests_per_minute,
            max_cost_per_month_microcents: row.max_cost_per_month_microcents,
        }
    }
}

/// Which configuration row a limit write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitScope {
    Instance,
    Project(i32),
}

impl LimitScope {
    fn key(self) -> String {
        match self {
            LimitScope::Instance => INSTANCE_SCOPE.to_string(),
            LimitScope::Project(project_id) => project_scope(project_id),
        }
    }
}

fn project_scope(project_id: i32) -> String {
    format!("project:{project_id}")
}

fn resolve(project: Option<LimitSettings>, instance: Option<LimitSettings>) -> EffectiveLimits {
    let pick =
        |from_project: Option<i64>, from_instance: Option<i64>| match (from_project, from_instance)
        {
            (Some(value), _) => (Some(value), LimitSource::Project),
            (None, Some(value)) => (Some(value), LimitSource::Instance),
            (None, None) => (None, LimitSource::BuiltIn),
        };
    let (rpm, rpm_source) = pick(
        project.and_then(|p| p.max_requests_per_minute),
        instance.and_then(|i| i.max_requests_per_minute),
    );
    let (cost, cost_source) = pick(
        project.and_then(|p| p.max_cost_per_month_microcents),
        instance.and_then(|i| i.max_cost_per_month_microcents),
    );
    EffectiveLimits {
        max_requests_per_minute: match rpm {
            Some(0) => None,
            Some(value) => Some(u32::try_from(value).unwrap_or(u32::MAX)),
            None => Some(DEFAULT_MAX_REQUESTS_PER_MINUTE),
        },
        max_requests_per_minute_source: rpm_source,
        max_cost_per_month_microcents: match cost {
            Some(0) | None => None,
            Some(value) => Some(value),
        },
        max_cost_per_month_source: cost_source,
    }
}

/// Start of the UTC calendar month containing `now`.
pub(crate) fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

/// Whole seconds from `now` until the next UTC month begins (at least 1).
pub(crate) fn secs_until_next_month(now: DateTime<Utc>) -> u64 {
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .map(|next| (next - now).num_seconds().max(1) as u64)
        .unwrap_or(3600)
}

// ============================================================================
// Service
// ============================================================================

pub struct AiLimitsService {
    db: Arc<DatabaseConnection>,
    usage_service: Arc<UsageService>,
    limiter: SlidingWindowLimiter,
    limits_cache: moka::future::Cache<i32, EffectiveLimits>,
    spend_cache: moka::future::Cache<i32, i64>,
    started: std::time::Instant,
}

impl AiLimitsService {
    pub fn new(db: Arc<DatabaseConnection>, usage_service: Arc<UsageService>) -> Self {
        Self {
            db,
            usage_service,
            limiter: SlidingWindowLimiter::new(),
            limits_cache: moka::future::Cache::builder()
                .max_capacity(MAX_TRACKED_PROJECTS)
                .time_to_live(LIMITS_CACHE_TTL)
                .build(),
            spend_cache: moka::future::Cache::builder()
                .max_capacity(MAX_TRACKED_PROJECTS)
                .time_to_live(SPEND_CACHE_TTL)
                .build(),
            started: std::time::Instant::now(),
        }
    }

    /// Admit or refuse one gateway request from `project_id`'s deployment
    /// token. Refusals carry the seconds the caller should wait.
    ///
    /// A failure to read the configured limits or the spend total never takes
    /// the gateway down: it is logged and the rate limit (which needs no I/O)
    /// still applies with the built-in default.
    pub async fn enforce(&self, project_id: i32) -> Result<(), AiGatewayError> {
        let limits = match self.effective_limits(project_id).await {
            Ok(limits) => limits,
            Err(limits_error) => {
                error!(
                    project_id,
                    error = %limits_error,
                    "Could not load AI gateway limits; applying the built-in default"
                );
                resolve(None, None)
            }
        };

        if let Some(budget) = limits.max_cost_per_month_microcents {
            match self.month_spend(project_id).await {
                Ok(used) if used >= budget => {
                    return Err(AiGatewayError::BudgetExceeded {
                        project_id,
                        limit_microcents: budget,
                        used_microcents: used,
                        retry_after_secs: secs_until_next_month(Utc::now()),
                    });
                }
                Ok(_) => {}
                Err(spend_error) => warn!(
                    project_id,
                    error = %spend_error,
                    "Could not read AI spend for budget check; allowing the request"
                ),
            }
        }

        if let Some(limit) = limits.max_requests_per_minute {
            let now_ms = self.started.elapsed().as_millis() as u64;
            if let WindowDecision::Limited { retry_after_secs } =
                self.limiter.check(project_id, limit, now_ms)
            {
                return Err(AiGatewayError::RateLimited {
                    project_id,
                    limit,
                    retry_after_secs,
                });
            }
        }
        Ok(())
    }

    /// Limits that apply to `project_id` after inheritance.
    pub async fn effective_limits(
        &self,
        project_id: i32,
    ) -> Result<EffectiveLimits, AiGatewayError> {
        if let Some(cached) = self.limits_cache.get(&project_id).await {
            return Ok(cached);
        }
        let rows = ai_gateway_config::Entity::find()
            .filter(Column::Scope.is_in([INSTANCE_SCOPE.to_string(), project_scope(project_id)]))
            .all(self.db.as_ref())
            .await?;
        let settings_of = |scope: &str| {
            rows.iter()
                .find(|row| row.scope == scope)
                .map(LimitSettings::from)
        };
        let limits = resolve(
            settings_of(&project_scope(project_id)),
            settings_of(INSTANCE_SCOPE),
        );
        self.limits_cache.insert(project_id, limits.clone()).await;
        Ok(limits)
    }

    /// Operator-funded spend of `project_id` this UTC month, in microcents.
    pub async fn month_spend(&self, project_id: i32) -> Result<i64, AiGatewayError> {
        if let Some(cached) = self.spend_cache.get(&project_id).await {
            return Ok(cached);
        }
        let used = self
            .usage_service
            .project_spend_since(project_id, month_start(Utc::now()))
            .await?;
        self.spend_cache.insert(project_id, used).await;
        Ok(used)
    }

    /// Stored (un-inherited) limits for a scope, if the scope has a row.
    pub async fn get_settings(
        &self,
        scope: LimitScope,
    ) -> Result<Option<LimitSettings>, AiGatewayError> {
        Ok(ai_gateway_config::Entity::find()
            .filter(Column::Scope.eq(scope.key()))
            .one(self.db.as_ref())
            .await?
            .as_ref()
            .map(LimitSettings::from))
    }

    /// Page of projects that have their own limits configured, newest
    /// configuration first. Returns the page and the total count.
    pub async fn list_project_overrides(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<(i32, LimitSettings)>, u64), AiGatewayError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let paginator = ai_gateway_config::Entity::find()
            .filter(Column::Scope.like("project:%"))
            .order_by_desc(Column::UpdatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;
        let overrides = rows
            .iter()
            .filter_map(|row| {
                let project_id = row.scope.strip_prefix("project:")?.parse::<i32>().ok()?;
                Some((project_id, LimitSettings::from(row)))
            })
            .collect();
        Ok((overrides, total))
    }

    /// Replace the stored limits of a scope. `None` fields inherit; `0` means
    /// unlimited.
    pub async fn set_settings(
        &self,
        scope: LimitScope,
        settings: LimitSettings,
    ) -> Result<LimitSettings, AiGatewayError> {
        validate_settings(scope, settings)?;
        if let LimitScope::Project(project_id) = scope {
            let exists = temps_entities::projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .is_some();
            if !exists {
                return Err(AiGatewayError::ProjectNotFound { project_id });
            }
        }

        let existing = ai_gateway_config::Entity::find()
            .filter(Column::Scope.eq(scope.key()))
            .one(self.db.as_ref())
            .await?;
        let saved = match existing {
            Some(row) => {
                let mut active = ai_gateway_config::ActiveModel::from(row);
                active.max_requests_per_minute = Set(settings.max_requests_per_minute);
                active.max_cost_per_month_microcents = Set(settings.max_cost_per_month_microcents);
                active.update(self.db.as_ref()).await?
            }
            None => {
                ai_gateway_config::ActiveModel {
                    scope: Set(scope.key()),
                    max_requests_per_minute: Set(settings.max_requests_per_minute),
                    max_cost_per_month_microcents: Set(settings.max_cost_per_month_microcents),
                    provider_type: Set("gateway".to_string()),
                    agent_cli_provider_id: Set(None),
                    interactive_bridge_enabled: Set(false),
                    ..Default::default()
                }
                .insert(self.db.as_ref())
                .await?
            }
        };

        // Limits are cached per project and an instance change affects all of
        // them. Spend is unaffected by a limit change.
        self.limits_cache.invalidate_all();
        Ok(LimitSettings::from(&saved))
    }
}

fn validate_settings(scope: LimitScope, settings: LimitSettings) -> Result<(), AiGatewayError> {
    let scope_name = scope.key();
    if let Some(rpm) = settings.max_requests_per_minute {
        if !(0..=MAX_CONFIGURABLE_REQUESTS_PER_MINUTE).contains(&rpm) {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "max_requests_per_minute for scope '{scope_name}' must be between 0 \
                     (unlimited) and {MAX_CONFIGURABLE_REQUESTS_PER_MINUTE}, got {rpm}"
                ),
            });
        }
    }
    if let Some(cost) = settings.max_cost_per_month_microcents {
        if cost < 0 {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "max_cost_per_month_microcents for scope '{scope_name}' must be 0 \
                     (unlimited) or positive, got {cost}"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn config_row(scope: &str, rpm: Option<i64>, cost: Option<i64>) -> ai_gateway_config::Model {
        ai_gateway_config::Model {
            id: 1,
            scope: scope.to_string(),
            allowed_models: None,
            max_requests_per_minute: rpm,
            max_cost_per_month_microcents: cost,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            interactive_bridge_enabled: false,
            summary_provider_id: None,
            summary_model: None,
            summary_thinking_level: None,
        }
    }

    fn service_with(db: DatabaseConnection) -> AiLimitsService {
        let db = Arc::new(db);
        AiLimitsService::new(db.clone(), Arc::new(UsageService::new(db)))
    }

    // ---- sliding window ----------------------------------------------------

    #[test]
    fn window_admits_up_to_the_limit_then_refuses() {
        let limiter = SlidingWindowLimiter::new();
        for _ in 0..3 {
            assert_eq!(limiter.check(1, 3, 1_000), WindowDecision::Allowed);
        }
        assert!(matches!(
            limiter.check(1, 3, 1_000),
            WindowDecision::Limited { .. }
        ));
    }

    #[test]
    fn window_keeps_projects_independent() {
        let limiter = SlidingWindowLimiter::new();
        assert_eq!(limiter.check(1, 1, 0), WindowDecision::Allowed);
        assert!(matches!(
            limiter.check(1, 1, 0),
            WindowDecision::Limited { .. }
        ));
        assert_eq!(limiter.check(2, 1, 0), WindowDecision::Allowed);
    }

    #[test]
    fn window_refusal_does_not_consume_quota() {
        let limiter = SlidingWindowLimiter::new();
        assert_eq!(limiter.check(1, 2, 0), WindowDecision::Allowed);
        assert_eq!(limiter.check(1, 2, 0), WindowDecision::Allowed);
        for _ in 0..50 {
            assert!(matches!(
                limiter.check(1, 2, 0),
                WindowDecision::Limited { .. }
            ));
        }
        // Two windows later the hammering left no trace.
        assert_eq!(limiter.check(1, 2, 2 * WINDOW_MS), WindowDecision::Allowed);
        assert_eq!(limiter.check(1, 2, 2 * WINDOW_MS), WindowDecision::Allowed);
    }

    #[test]
    fn window_is_sliding_not_fixed() {
        let limiter = SlidingWindowLimiter::new();
        // Burst at the very end of window 0.
        let burst_at = WINDOW_MS - 1;
        for _ in 0..10 {
            assert_eq!(limiter.check(1, 10, burst_at), WindowDecision::Allowed);
        }
        // A fixed-window counter would grant a fresh 10 immediately after the
        // boundary; the sliding window still sees the burst.
        assert!(matches!(
            limiter.check(1, 10, WINDOW_MS + 1),
            WindowDecision::Limited { .. }
        ));
        // Half a window later roughly half the burst has aged out.
        let mut admitted = 0;
        while limiter.check(1, 10, WINDOW_MS + WINDOW_MS / 2) == WindowDecision::Allowed {
            admitted += 1;
            assert!(admitted <= 10, "limiter never refused");
        }
        assert!((4..=6).contains(&admitted), "admitted {admitted}");
    }

    #[test]
    fn window_fully_recovers_after_two_idle_windows() {
        let limiter = SlidingWindowLimiter::new();
        for _ in 0..5 {
            limiter.check(1, 5, 0);
        }
        assert!(matches!(
            limiter.check(1, 5, 0),
            WindowDecision::Limited { .. }
        ));
        for _ in 0..5 {
            assert_eq!(limiter.check(1, 5, 3 * WINDOW_MS), WindowDecision::Allowed);
        }
    }

    #[test]
    fn retry_after_is_honoured_when_the_client_waits_that_long() {
        let limiter = SlidingWindowLimiter::new();
        let start = 5 * WINDOW_MS + 10_000;
        for _ in 0..30 {
            assert_eq!(limiter.check(1, 30, start), WindowDecision::Allowed);
        }
        let WindowDecision::Limited { retry_after_secs } = limiter.check(1, 30, start) else {
            panic!("expected the 31st request to be limited");
        };
        assert!(retry_after_secs >= 1);
        assert!(retry_after_secs <= 120);
        assert_eq!(
            limiter.check(1, 30, start + retry_after_secs * 1000),
            WindowDecision::Allowed,
            "waiting the advertised Retry-After ({retry_after_secs}s) must be enough"
        );
    }

    #[test]
    fn retry_after_waits_for_decay_of_the_previous_window() {
        let limiter = SlidingWindowLimiter::new();
        // Fill window 0 completely, then be refused early in window 1.
        for _ in 0..60 {
            limiter.check(1, 60, 1_000);
        }
        let now = WINDOW_MS + 1_000;
        // The first request fits in the sliver the previous window has
        // already released; the next one does not.
        assert_eq!(limiter.check(1, 60, now), WindowDecision::Allowed);
        let WindowDecision::Limited { retry_after_secs } = limiter.check(1, 60, now) else {
            panic!("expected to be limited right after the boundary");
        };
        // Roughly one request's worth of decay: about a second, never the
        // whole minute.
        assert!(retry_after_secs <= 3, "retry_after_secs={retry_after_secs}");
        assert_eq!(
            limiter.check(1, 60, now + retry_after_secs * 1000),
            WindowDecision::Allowed
        );
    }

    #[test]
    fn retry_after_is_clamped_to_a_sane_range() {
        let limiter = SlidingWindowLimiter::new();
        assert_eq!(limiter.check(1, 1, 0), WindowDecision::Allowed);
        let WindowDecision::Limited { retry_after_secs } = limiter.check(1, 1, 0) else {
            panic!("expected limit");
        };
        assert!((1..=120).contains(&retry_after_secs));
    }

    // ---- limit resolution --------------------------------------------------

    #[test]
    fn resolve_falls_back_to_the_built_in_default() {
        let limits = resolve(None, None);
        assert_eq!(
            limits.max_requests_per_minute,
            Some(DEFAULT_MAX_REQUESTS_PER_MINUTE)
        );
        assert_eq!(limits.max_requests_per_minute_source, LimitSource::BuiltIn);
        assert_eq!(limits.max_cost_per_month_microcents, None);
    }

    #[test]
    fn resolve_prefers_project_over_instance_per_field() {
        let project = LimitSettings {
            max_requests_per_minute: Some(10),
            max_cost_per_month_microcents: None,
        };
        let instance = LimitSettings {
            max_requests_per_minute: Some(500),
            max_cost_per_month_microcents: Some(9_000),
        };
        let limits = resolve(Some(project), Some(instance));
        assert_eq!(limits.max_requests_per_minute, Some(10));
        assert_eq!(limits.max_requests_per_minute_source, LimitSource::Project);
        assert_eq!(limits.max_cost_per_month_microcents, Some(9_000));
        assert_eq!(limits.max_cost_per_month_source, LimitSource::Instance);
    }

    #[test]
    fn resolve_treats_zero_as_unlimited_and_overrides_inherited_limits() {
        let project = LimitSettings {
            max_requests_per_minute: Some(0),
            max_cost_per_month_microcents: Some(0),
        };
        let instance = LimitSettings {
            max_requests_per_minute: Some(5),
            max_cost_per_month_microcents: Some(100),
        };
        let limits = resolve(Some(project), Some(instance));
        assert_eq!(limits.max_requests_per_minute, None);
        assert_eq!(limits.max_cost_per_month_microcents, None);
    }

    // ---- calendar helpers --------------------------------------------------

    #[test]
    fn month_start_truncates_to_the_first_of_the_month() {
        let now = Utc.with_ymd_and_hms(2026, 10, 16, 13, 45, 9).unwrap();
        assert_eq!(
            month_start(now),
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn secs_until_next_month_rolls_over_the_year() {
        let now = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 0).unwrap();
        assert_eq!(secs_until_next_month(now), 60);
        let mid = Utc.with_ymd_and_hms(2026, 10, 31, 0, 0, 0).unwrap();
        assert_eq!(secs_until_next_month(mid), 24 * 3600);
    }

    // ---- service -----------------------------------------------------------

    #[tokio::test]
    async fn enforce_returns_rate_limited_with_project_context() {
        let service = service_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![config_row("project:7", Some(2), None)]])
                .into_connection(),
        );
        service.enforce(7).await.unwrap();
        service.enforce(7).await.unwrap();
        match service.enforce(7).await.unwrap_err() {
            AiGatewayError::RateLimited {
                project_id,
                limit,
                retry_after_secs,
            } => {
                assert_eq!(project_id, 7);
                assert_eq!(limit, 2);
                assert!(retry_after_secs >= 1);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
        // Another project is untouched by project 7's exhaustion.
        // (Its limits are not mocked, so it falls back to the built-in default.)
        service.enforce(8).await.unwrap();
    }

    #[tokio::test]
    async fn enforce_applies_the_default_when_limits_cannot_be_loaded() {
        // An empty mock fails the config query; the limiter must still run.
        let service = service_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        for _ in 0..DEFAULT_MAX_REQUESTS_PER_MINUTE {
            service.enforce(3).await.unwrap();
        }
        assert!(matches!(
            service.enforce(3).await.unwrap_err(),
            AiGatewayError::RateLimited { project_id: 3, .. }
        ));
    }

    #[tokio::test]
    async fn enforce_refuses_once_the_monthly_budget_is_spent() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![config_row("project:4", Some(0), Some(1_000))]])
            .append_query_results([vec![std::collections::BTreeMap::from([(
                "spend".to_string(),
                sea_orm::Value::BigInt(Some(1_000)),
            )])]])
            .into_connection();
        let service = service_with(db);
        match service.enforce(4).await.unwrap_err() {
            AiGatewayError::BudgetExceeded {
                project_id,
                limit_microcents,
                used_microcents,
                retry_after_secs,
            } => {
                assert_eq!(project_id, 4);
                assert_eq!(limit_microcents, 1_000);
                assert_eq!(used_microcents, 1_000);
                assert!(retry_after_secs >= 1);
            }
            other => panic!("expected BudgetExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enforce_allows_spend_below_the_budget() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![config_row("project:4", Some(0), Some(1_000))]])
            .append_query_results([vec![std::collections::BTreeMap::from([(
                "spend".to_string(),
                sea_orm::Value::BigInt(Some(999)),
            )])]])
            .into_connection();
        let service = service_with(db);
        service.enforce(4).await.unwrap();
    }

    #[tokio::test]
    async fn set_settings_rejects_negative_and_absurd_values() {
        let service = service_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        for settings in [
            LimitSettings {
                max_requests_per_minute: Some(-1),
                ..Default::default()
            },
            LimitSettings {
                max_requests_per_minute: Some(MAX_CONFIGURABLE_REQUESTS_PER_MINUTE + 1),
                ..Default::default()
            },
            LimitSettings {
                max_cost_per_month_microcents: Some(-5),
                ..Default::default()
            },
        ] {
            let error = service
                .set_settings(LimitScope::Instance, settings)
                .await
                .unwrap_err();
            assert!(
                matches!(error, AiGatewayError::Validation { ref message } if message.contains("instance")),
                "{error:?}"
            );
        }
    }

    #[tokio::test]
    async fn set_settings_reports_the_missing_project() {
        let service = service_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::projects::Model>::new()])
                .into_connection(),
        );
        let error = service
            .set_settings(
                LimitScope::Project(404),
                LimitSettings {
                    max_requests_per_minute: Some(10),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, AiGatewayError::ProjectNotFound { project_id: 404 }),
            "{error:?}"
        );
        assert!(error.to_string().contains("404"));
    }
}
