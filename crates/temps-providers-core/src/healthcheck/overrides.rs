// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Operator overrides of the managed-service healthcheck, their bounds, and
//! how they combine with Temps' built-in timings.
//!
//! Two levels can override any of the five values: the instance default
//! (platform settings) and the service itself (its `external_services` row).
//! A value that is not set inherits from the next level: service, then
//! instance default, then built-in ([`resolve`]).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

use super::HealthcheckTimings;

/// Overrides of some or all of the five healthcheck values. `None` inherits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct HealthcheckOverrides {
    /// Seconds between checks once healthy (5–3600).
    #[schema(example = 60, minimum = 5, maximum = 3600)]
    pub interval_secs: Option<u32>,
    /// Seconds between checks while starting (1–60).
    #[schema(example = 2, minimum = 1, maximum = 60)]
    pub start_interval_secs: Option<u32>,
    /// Seconds after start during which failures do not count (0–600).
    #[schema(example = 90, minimum = 0, maximum = 600)]
    pub start_period_secs: Option<u32>,
    /// Seconds one check may run (1–60, and shorter than the interval).
    #[schema(example = 5, minimum = 1, maximum = 60)]
    pub timeout_secs: Option<u32>,
    /// Consecutive failures that make the container unhealthy (1–10).
    #[schema(example = 3, minimum = 1, maximum = 10)]
    pub retries: Option<u32>,
}

impl HealthcheckOverrides {
    /// Whether nothing is overridden.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn value(&self, field: HealthcheckField) -> Option<u32> {
        match field {
            HealthcheckField::Interval => self.interval_secs,
            HealthcheckField::StartInterval => self.start_interval_secs,
            HealthcheckField::StartPeriod => self.start_period_secs,
            HealthcheckField::Timeout => self.timeout_secs,
            HealthcheckField::Retries => self.retries,
        }
    }

    /// Check every set value against its bounds. Says which field is wrong.
    pub fn validate(&self) -> Result<(), HealthcheckValidationError> {
        for field in HealthcheckField::ALL {
            if let Some(value) = self.value(field) {
                field.check(value)?;
            }
        }
        Ok(())
    }
}

/// One of the five healthcheck values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthcheckField {
    Interval,
    StartInterval,
    StartPeriod,
    Timeout,
    Retries,
}

impl HealthcheckField {
    pub const ALL: [Self; 5] = [
        Self::Interval,
        Self::StartInterval,
        Self::StartPeriod,
        Self::Timeout,
        Self::Retries,
    ];

    /// The field's name in the API.
    pub fn name(self) -> &'static str {
        match self {
            Self::Interval => "interval_secs",
            Self::StartInterval => "start_interval_secs",
            Self::StartPeriod => "start_period_secs",
            Self::Timeout => "timeout_secs",
            Self::Retries => "retries",
        }
    }

    /// The accepted range, inclusive.
    pub fn bounds(self) -> (u32, u32) {
        match self {
            Self::Interval => (5, 3600),
            Self::StartInterval => (1, 60),
            Self::StartPeriod => (0, 600),
            Self::Timeout => (1, 60),
            Self::Retries => (1, 10),
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Retries => "checks",
            _ => "seconds",
        }
    }

    fn check(self, value: u32) -> Result<(), HealthcheckValidationError> {
        let (min, max) = self.bounds();
        if (min..=max).contains(&value) {
            Ok(())
        } else {
            Err(HealthcheckValidationError::OutOfRange {
                field: self.name(),
                value,
                min,
                max,
                unit: self.unit(),
            })
        }
    }
}

/// Why a healthcheck setting was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HealthcheckValidationError {
    #[error("healthcheck {field} must be between {min} and {max} {unit}, got {value}")]
    OutOfRange {
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
        unit: &'static str,
    },
    #[error(
        "healthcheck timeout_secs ({timeout_secs}s, from {timeout_source}) must be shorter than \
         interval_secs ({interval_secs}s, from {interval_source}): a check that may run longer \
         than the time between checks overlaps the next one"
    )]
    TimeoutNotShorterThanInterval {
        timeout_secs: u32,
        timeout_source: HealthcheckSource,
        interval_secs: u32,
        interval_source: HealthcheckSource,
    },
    #[error(
        "healthcheck's first check comes {first_check_secs}s after the container starts \
         ({first_check_field}, from {first_check_source}) and may take timeout_secs \
         ({timeout_secs}s, from {timeout_source}); with the up to {READINESS_POLL_SECS}s Temps \
         takes to read the result, it ends after the {readiness_wait_secs}s Temps waits for this \
         engine's container to become healthy, so every create, upgrade and restore would time out"
    )]
    FirstCheckTooLate {
        first_check_secs: u32,
        /// `start_interval_secs` with a start period, else `interval_secs`.
        first_check_field: &'static str,
        first_check_source: HealthcheckSource,
        timeout_secs: u32,
        timeout_source: HealthcheckSource,
        readiness_wait_secs: u32,
    },
    #[error(
        "healthcheck start_period_secs ({start_period_secs}s, from {start_period_source}) plus \
         interval_secs ({interval_secs}s, from {interval_source}) plus timeout_secs \
         ({timeout_secs}s, from {timeout_source}), with a start period also start_interval_secs \
         ({start_interval_secs}s, from {start_interval_source}) and the timeout again (the last \
         check Docker schedules inside it may start one start interval before it ends and still \
         be running after it), and the up to {READINESS_POLL_SECS}s Temps takes to read the \
         result must not exceed {readiness_wait_secs}s, how long Temps waits for this engine's \
         container to become healthy, unless the start period lasts that long: after the start \
         period Docker checks only once per interval, so a container still starting would be \
         seen too late and every create, upgrade and restore would time out"
    )]
    StartPeriodTooShortForInterval {
        start_period_secs: u32,
        start_period_source: HealthcheckSource,
        start_interval_secs: u32,
        start_interval_source: HealthcheckSource,
        interval_secs: u32,
        interval_source: HealthcheckSource,
        timeout_secs: u32,
        timeout_source: HealthcheckSource,
        readiness_wait_secs: u32,
    },
}

/// The longest Temps' readiness waits take to read a container's health once
/// Docker has it: they poll with a backoff capped at this.
pub const READINESS_POLL_SECS: u32 = 2;

/// Where an effective healthcheck value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthcheckSource {
    /// Temps' built-in value for the engine.
    Builtin,
    /// The instance default in platform settings.
    InstanceDefault,
    /// This service's own override.
    Service,
}

impl std::fmt::Display for HealthcheckSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Builtin => "the built-in default",
            Self::InstanceDefault => "the instance default",
            Self::Service => "this service",
        })
    }
}

/// The source of each of the five values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct HealthcheckSources {
    pub interval_secs: HealthcheckSource,
    pub start_interval_secs: HealthcheckSource,
    pub start_period_secs: HealthcheckSource,
    pub timeout_secs: HealthcheckSource,
    pub retries: HealthcheckSource,
}

/// The values a container would be created with, and where each comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ResolvedHealthcheck {
    pub timings: HealthcheckTimings,
    pub sources: HealthcheckSources,
}

impl ResolvedHealthcheck {
    /// Check the combination, for an engine whose readiness waits give a new
    /// container `readiness_wait_secs` to become healthy. Each value is
    /// already within its bounds when it came from a validated override or a
    /// built-in default.
    ///
    /// * The timeout must be shorter than the interval, or checks overlap.
    /// * A check must be seen inside the readiness wait. A check started
    ///   later than `readiness_wait_secs - timeout_secs - READINESS_POLL_SECS`
    ///   may finish, and be read, only after the wait gave up. So:
    ///   * the first check must start by then. Docker runs it one start
    ///     interval after the container starts when there is a start period,
    ///     else one full interval after;
    ///   * and checks must keep coming until then. Docker checks every start
    ///     interval inside the start period and once per interval after it,
    ///     each wait counted from when the previous check finished, so the
    ///     start period lasts until then, or the first check after it fits
    ///     before it, or the interval is no longer than the start interval
    ///     (checks never slow down). Docker picks the next wait as a check
    ///     finishes, so the last check at the start interval is scheduled
    ///     just before the start period ends, starts up to one start interval
    ///     after it and may run its timeout; the next one comes an interval
    ///     after that. Otherwise
    ///     a container that takes longer to start than the start period is
    ///     checked next only after the wait gave up.
    pub fn validate(&self, readiness_wait_secs: u32) -> Result<(), HealthcheckValidationError> {
        let t = &self.timings;
        if t.timeout_secs >= t.interval_secs {
            return Err(HealthcheckValidationError::TimeoutNotShorterThanInterval {
                timeout_secs: t.timeout_secs,
                timeout_source: self.sources.timeout_secs,
                interval_secs: t.interval_secs,
                interval_source: self.sources.interval_secs,
            });
        }
        // The latest a check can start and still be seen by the wait.
        let last_start =
            readiness_wait_secs.saturating_sub(t.timeout_secs.saturating_add(READINESS_POLL_SECS));
        let (first_check_secs, first_check_field, first_check_source) = if t.start_period_secs > 0 {
            (
                t.start_interval_secs,
                "start_interval_secs",
                self.sources.start_interval_secs,
            )
        } else {
            (t.interval_secs, "interval_secs", self.sources.interval_secs)
        };
        if first_check_secs > last_start {
            return Err(HealthcheckValidationError::FirstCheckTooLate {
                first_check_secs,
                first_check_field,
                first_check_source,
                timeout_secs: t.timeout_secs,
                timeout_source: self.sources.timeout_secs,
                readiness_wait_secs,
            });
        }
        // The last check scheduled inside the start period starts up to one
        // start interval after it ends and may run its full timeout; Docker
        // waits one interval after it finishes.
        let boundary_check = if t.start_period_secs > 0 {
            t.start_interval_secs.saturating_add(t.timeout_secs)
        } else {
            0
        };
        let first_after_start_period = t
            .start_period_secs
            .saturating_add(boundary_check)
            .saturating_add(t.interval_secs);
        let covered = t.start_period_secs >= last_start
            || first_after_start_period <= last_start
            || t.interval_secs <= t.start_interval_secs;
        if !covered {
            return Err(HealthcheckValidationError::StartPeriodTooShortForInterval {
                start_period_secs: t.start_period_secs,
                start_period_source: self.sources.start_period_secs,
                start_interval_secs: t.start_interval_secs,
                start_interval_source: self.sources.start_interval_secs,
                interval_secs: t.interval_secs,
                interval_source: self.sources.interval_secs,
                timeout_secs: t.timeout_secs,
                timeout_source: self.sources.timeout_secs,
                readiness_wait_secs,
            });
        }
        Ok(())
    }
}

/// Combine the levels: each value is the service's override, else the
/// instance default, else the built-in.
pub fn resolve(
    service: &HealthcheckOverrides,
    instance: &HealthcheckOverrides,
    builtin: &HealthcheckTimings,
) -> ResolvedHealthcheck {
    fn pick(service: Option<u32>, instance: Option<u32>, builtin: u32) -> (u32, HealthcheckSource) {
        match (service, instance) {
            (Some(value), _) => (value, HealthcheckSource::Service),
            (None, Some(value)) => (value, HealthcheckSource::InstanceDefault),
            (None, None) => (builtin, HealthcheckSource::Builtin),
        }
    }
    let (interval_secs, interval_source) = pick(
        service.interval_secs,
        instance.interval_secs,
        builtin.interval_secs,
    );
    let (start_interval_secs, start_interval_source) = pick(
        service.start_interval_secs,
        instance.start_interval_secs,
        builtin.start_interval_secs,
    );
    let (start_period_secs, start_period_source) = pick(
        service.start_period_secs,
        instance.start_period_secs,
        builtin.start_period_secs,
    );
    let (timeout_secs, timeout_source) = pick(
        service.timeout_secs,
        instance.timeout_secs,
        builtin.timeout_secs,
    );
    let (retries, retries_source) = pick(service.retries, instance.retries, builtin.retries);
    ResolvedHealthcheck {
        timings: HealthcheckTimings {
            interval_secs,
            start_interval_secs,
            start_period_secs,
            timeout_secs,
            retries,
        },
        sources: HealthcheckSources {
            interval_secs: interval_source,
            start_interval_secs: start_interval_source,
            start_period_secs: start_period_source,
            timeout_secs: timeout_source,
            retries: retries_source,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::recreate_guard::ManagedEngine;
    use HealthcheckSource::*;

    fn builtin() -> HealthcheckTimings {
        HealthcheckTimings::builtin(ManagedEngine::Rustfs)
    }

    /// How long RustFS's readiness wait gives a new container.
    const RUSTFS_WAIT: u32 = 60;

    fn all(value: u32) -> HealthcheckOverrides {
        HealthcheckOverrides {
            interval_secs: Some(value),
            start_interval_secs: Some(value),
            start_period_secs: Some(value),
            timeout_secs: Some(value),
            retries: Some(value),
        }
    }

    #[test]
    fn nothing_set_is_the_builtin() {
        let resolved = resolve(&Default::default(), &Default::default(), &builtin());
        assert_eq!(resolved.timings, builtin());
        assert_eq!(
            resolved.sources,
            HealthcheckSources {
                interval_secs: Builtin,
                start_interval_secs: Builtin,
                start_period_secs: Builtin,
                timeout_secs: Builtin,
                retries: Builtin,
            }
        );
    }

    #[test]
    fn the_instance_default_overrides_the_builtin() {
        let resolved = resolve(&Default::default(), &all(7), &builtin());
        assert_eq!(resolved.timings.interval_secs, 7);
        assert_eq!(resolved.timings.retries, 7);
        assert_eq!(resolved.sources.start_period_secs, InstanceDefault);
        assert_eq!(resolved.sources.retries, InstanceDefault);
    }

    #[test]
    fn the_service_overrides_the_instance_default() {
        let resolved = resolve(&all(9), &all(7), &builtin());
        assert_eq!(resolved.timings.interval_secs, 9);
        assert_eq!(resolved.sources.interval_secs, Service);
        assert_eq!(resolved.sources.timeout_secs, Service);
    }

    #[test]
    fn each_value_is_resolved_on_its_own() {
        // The case from the issue: a 60 s steady interval everywhere, one
        // service with a 10 s start period.
        let instance = HealthcheckOverrides {
            interval_secs: Some(60),
            ..Default::default()
        };
        let service = HealthcheckOverrides {
            start_period_secs: Some(10),
            ..Default::default()
        };
        let resolved = resolve(&service, &instance, &builtin());
        assert_eq!(
            resolved.timings,
            HealthcheckTimings {
                interval_secs: 60,
                start_interval_secs: 2,
                start_period_secs: 10,
                timeout_secs: 5,
                retries: 3,
            }
        );
        assert_eq!(
            resolved.sources,
            HealthcheckSources {
                interval_secs: InstanceDefault,
                start_interval_secs: Builtin,
                start_period_secs: Service,
                timeout_secs: Builtin,
                retries: Builtin,
            }
        );
        // A service override on top of the instance default for the same
        // field wins, the others still inherit.
        let service = HealthcheckOverrides {
            interval_secs: Some(15),
            ..service
        };
        let resolved = resolve(&service, &instance, &builtin());
        assert_eq!(resolved.timings.interval_secs, 15);
        assert_eq!(resolved.sources.interval_secs, Service);
        assert_eq!(resolved.sources.start_interval_secs, Builtin);
    }

    #[test]
    fn every_bound_is_inclusive_and_names_its_field() {
        for field in HealthcheckField::ALL {
            let (min, max) = field.bounds();
            let with = |value: u32| {
                let mut o = HealthcheckOverrides::default();
                match field {
                    HealthcheckField::Interval => o.interval_secs = Some(value),
                    HealthcheckField::StartInterval => o.start_interval_secs = Some(value),
                    HealthcheckField::StartPeriod => o.start_period_secs = Some(value),
                    HealthcheckField::Timeout => o.timeout_secs = Some(value),
                    HealthcheckField::Retries => o.retries = Some(value),
                }
                o
            };
            assert_eq!(with(min).validate(), Ok(()), "{} min", field.name());
            assert_eq!(with(max).validate(), Ok(()), "{} max", field.name());
            let above = with(max + 1).validate().unwrap_err();
            assert!(
                matches!(above, HealthcheckValidationError::OutOfRange { field: f, value, .. } if f == field.name() && value == max + 1),
                "{above}"
            );
            assert!(above.to_string().contains(field.name()), "{above}");
            if min > 0 {
                let below = with(min - 1).validate().unwrap_err();
                assert!(
                    matches!(below, HealthcheckValidationError::OutOfRange { field: f, .. } if f == field.name()),
                    "{below}"
                );
            }
        }
    }

    #[test]
    fn the_bounds_are_the_documented_ones() {
        assert_eq!(HealthcheckField::Interval.bounds(), (5, 3600));
        assert_eq!(HealthcheckField::StartInterval.bounds(), (1, 60));
        assert_eq!(HealthcheckField::StartPeriod.bounds(), (0, 600));
        assert_eq!(HealthcheckField::Timeout.bounds(), (1, 60));
        assert_eq!(HealthcheckField::Retries.bounds(), (1, 10));
        assert_eq!(HealthcheckOverrides::default().validate(), Ok(()));
        let error = all(0).validate().unwrap_err();
        assert_eq!(
            error.to_string(),
            "healthcheck interval_secs must be between 5 and 3600 seconds, got 0"
        );
    }

    #[test]
    fn a_timeout_at_or_above_the_interval_is_refused_with_both_sources() {
        let instance = HealthcheckOverrides {
            interval_secs: Some(10),
            ..Default::default()
        };
        for timeout in [10, 11] {
            let service = HealthcheckOverrides {
                timeout_secs: Some(timeout),
                ..Default::default()
            };
            let error = resolve(&service, &instance, &builtin())
                .validate(RUSTFS_WAIT)
                .unwrap_err();
            assert_eq!(
                error,
                HealthcheckValidationError::TimeoutNotShorterThanInterval {
                    timeout_secs: timeout,
                    timeout_source: Service,
                    interval_secs: 10,
                    interval_source: InstanceDefault,
                }
            );
            assert!(error.to_string().contains("this service"), "{error}");
            assert!(
                error.to_string().contains("the instance default"),
                "{error}"
            );
        }
        let service = HealthcheckOverrides {
            timeout_secs: Some(9),
            ..Default::default()
        };
        assert_eq!(
            resolve(&service, &instance, &builtin()).validate(RUSTFS_WAIT),
            Ok(())
        );
    }

    #[test]
    fn every_builtin_is_valid() {
        for engine in [
            ManagedEngine::Postgres,
            ManagedEngine::Redis,
            ManagedEngine::MongoDb,
            ManagedEngine::MariaDb,
            ManagedEngine::Rustfs,
            ManagedEngine::Minio,
        ] {
            let builtin = HealthcheckTimings::builtin(engine);
            let as_overrides = HealthcheckOverrides {
                interval_secs: Some(builtin.interval_secs),
                start_interval_secs: Some(builtin.start_interval_secs),
                start_period_secs: Some(builtin.start_period_secs),
                timeout_secs: Some(builtin.timeout_secs),
                retries: Some(builtin.retries),
            };
            assert_eq!(as_overrides.validate(), Ok(()), "{engine}");
            assert_eq!(
                resolve(&Default::default(), &Default::default(), &builtin)
                    .validate(crate::healthcheck::readiness_wait_secs(engine)),
                Ok(()),
                "{engine}"
            );
        }
    }

    #[test]
    fn a_check_must_be_seen_inside_the_readiness_wait() {
        // RustFS: a 60 s wait and a 5 s timeout, so a check has to start by
        // 60 - 5 - READINESS_POLL_SECS = 53 s to be read in time.
        let check = |start_period: u32, interval: u32| {
            let instance = HealthcheckOverrides {
                start_period_secs: Some(start_period),
                interval_secs: Some(interval),
                ..Default::default()
            };
            resolve(&Default::default(), &instance, &builtin()).validate(RUSTFS_WAIT)
        };
        // The start period lasts until the last useful check: any interval.
        assert_eq!(check(60, 3600), Ok(()));
        assert_eq!(check(600, 600), Ok(()));
        assert_eq!(check(53, 3600), Ok(()));
        assert_eq!(check(55, 10), Ok(()));
        // A shorter start period plus one interval still leaves the check
        // time to finish and be read: 20 s, the 2 s start interval of the
        // last check scheduled inside it, that check's 5 s, then 26 s, so the
        // next check starts at 53 s.
        assert_eq!(check(0, 53), Ok(()));
        assert_eq!(check(20, 26), Ok(()));
        // Checking as often as while starting needs no start period.
        let never_slower = HealthcheckOverrides {
            start_period_secs: Some(0),
            start_interval_secs: Some(10),
            interval_secs: Some(10),
            ..Default::default()
        };
        assert_eq!(
            resolve(&never_slower, &Default::default(), &builtin()).validate(RUSTFS_WAIT),
            Ok(())
        );

        // The next check after the start period starts too late to be seen.
        // (20, 33) would start it at 53 s if the check running when the start
        // period ends took no time; it may take 5 s. (20, 27) would if that
        // check started as the start period ended; it may start 2 s after.
        for (start_period, interval) in [(10, 120), (30, 30), (20, 33), (20, 29), (20, 27)] {
            let error = check(start_period, interval).unwrap_err();
            assert_eq!(
                error,
                HealthcheckValidationError::StartPeriodTooShortForInterval {
                    start_period_secs: start_period,
                    start_period_source: InstanceDefault,
                    start_interval_secs: 2,
                    start_interval_source: Builtin,
                    interval_secs: interval,
                    interval_source: InstanceDefault,
                    timeout_secs: 5,
                    timeout_source: Builtin,
                    readiness_wait_secs: RUSTFS_WAIT,
                },
                "{start_period}+{interval}"
            );
            let message = error.to_string();
            assert!(
                message.contains("start_period_secs") && message.contains("60s"),
                "{message}"
            );
        }

        // Without a start period the first check comes one interval in: one
        // starting exactly as the wait ends would never be read.
        for interval in [54, 60, 61, 120] {
            assert_eq!(
                check(0, interval).unwrap_err(),
                HealthcheckValidationError::FirstCheckTooLate {
                    first_check_secs: interval,
                    first_check_field: "interval_secs",
                    first_check_source: InstanceDefault,
                    timeout_secs: 5,
                    timeout_source: Builtin,
                    readiness_wait_secs: RUSTFS_WAIT,
                },
                "0+{interval}"
            );
        }
        // With one, it comes one start interval in.
        let slow_start = HealthcheckOverrides {
            start_period_secs: Some(30),
            start_interval_secs: Some(60),
            interval_secs: Some(60),
            ..Default::default()
        };
        let error = resolve(&slow_start, &Default::default(), &builtin())
            .validate(RUSTFS_WAIT)
            .unwrap_err();
        assert!(
            matches!(
                error,
                HealthcheckValidationError::FirstCheckTooLate {
                    first_check_secs: 60,
                    first_check_field: "start_interval_secs",
                    first_check_source: Service,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("start_interval_secs"), "{error}");
    }

    #[test]
    fn a_start_interval_longer_than_the_start_period_delays_the_next_check() {
        // PostgreSQL: a 90 s wait and a 5 s timeout, so a check has to start
        // by 83 s. The first check comes 30 s in, after the 10 s start period
        // ended, so Docker waits 60 s after it: the next one starts at 95 s.
        let postgres = HealthcheckTimings::builtin(ManagedEngine::Postgres);
        let wait = crate::healthcheck::readiness_wait_secs(ManagedEngine::Postgres);
        let check = |start_interval: u32| {
            let service = HealthcheckOverrides {
                start_period_secs: Some(10),
                start_interval_secs: Some(start_interval),
                interval_secs: Some(60),
                ..Default::default()
            };
            resolve(&service, &Default::default(), &postgres).validate(wait)
        };
        assert_eq!(
            check(30),
            Err(HealthcheckValidationError::StartPeriodTooShortForInterval {
                start_period_secs: 10,
                start_period_source: Service,
                start_interval_secs: 30,
                start_interval_source: Service,
                interval_secs: 60,
                interval_source: Service,
                timeout_secs: 5,
                timeout_source: Builtin,
                readiness_wait_secs: wait,
            })
        );
        // 10 + 8 + 5 + 60 = 83: the next check starts just in time.
        assert_eq!(check(8), Ok(()));
        assert!(check(9).is_err());
    }

    #[test]
    fn a_slow_check_needs_its_timeout_inside_the_wait() {
        // MongoDB: a 90 s wait and a 10 s timeout. Its first check, 90 s in,
        // would still be running when the wait ends.
        let mongo = HealthcheckTimings::builtin(ManagedEngine::MongoDb);
        let check = |interval: u32| {
            let instance = HealthcheckOverrides {
                start_period_secs: Some(0),
                interval_secs: Some(interval),
                ..Default::default()
            };
            resolve(&Default::default(), &instance, &mongo).validate(90)
        };
        assert!(matches!(
            check(90),
            Err(HealthcheckValidationError::FirstCheckTooLate {
                first_check_secs: 90,
                timeout_secs: 10,
                ..
            })
        ));
        assert!(check(79).is_err());
        assert_eq!(check(78), Ok(()));
    }

    /// The published schema says what validation accepts, so a client that
    /// validates against it is not refused with a 400.
    #[test]
    fn the_schema_publishes_the_accepted_bounds() {
        use utoipa::PartialSchema;
        let schema = serde_json::to_value(<HealthcheckOverrides as PartialSchema>::schema())
            .expect("serialize the schema");
        for field in HealthcheckField::ALL {
            let property = &schema["properties"][field.name()];
            let (min, max) = field.bounds();
            assert_eq!(property["minimum"], min, "{}: {property}", field.name());
            assert_eq!(property["maximum"], max, "{}: {property}", field.name());
        }
    }

    #[test]
    fn overrides_deserialize_with_missing_fields_as_inherit() {
        let parsed: HealthcheckOverrides =
            serde_json::from_str(r#"{"interval_secs":60,"retries":null}"#).unwrap();
        assert_eq!(
            parsed,
            HealthcheckOverrides {
                interval_secs: Some(60),
                ..Default::default()
            }
        );
        assert!(HealthcheckOverrides::default().is_empty());
        assert!(!parsed.is_empty());
    }
}
