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
    #[schema(example = 60)]
    pub interval_secs: Option<u32>,
    /// Seconds between checks while starting (1–60).
    #[schema(example = 2)]
    pub start_interval_secs: Option<u32>,
    /// Seconds after start during which failures do not count (0–600).
    #[schema(example = 90)]
    pub start_period_secs: Option<u32>,
    /// Seconds one check may run (1–60, and shorter than the interval).
    #[schema(example = 5)]
    pub timeout_secs: Option<u32>,
    /// Consecutive failures that make the container unhealthy (1–10).
    #[schema(example = 3)]
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
}

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
    /// Check the combination: the timeout must be shorter than the interval.
    /// Each value is already within its bounds when it came from a validated
    /// override or a built-in default.
    pub fn validate(&self) -> Result<(), HealthcheckValidationError> {
        let t = &self.timings;
        if t.timeout_secs >= t.interval_secs {
            return Err(HealthcheckValidationError::TimeoutNotShorterThanInterval {
                timeout_secs: t.timeout_secs,
                timeout_source: self.sources.timeout_secs,
                interval_secs: t.interval_secs,
                interval_source: self.sources.interval_secs,
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
                .validate()
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
        assert_eq!(resolve(&service, &instance, &builtin()).validate(), Ok(()));
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
                resolve(&Default::default(), &Default::default(), &builtin).validate(),
                Ok(()),
                "{engine}"
            );
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
