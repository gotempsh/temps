// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which active surveys a visitor may see.
//!
//! Two conditions are decided on the server because the browser cannot be
//! trusted with them: sampling (stable per visitor, so reloading does not
//! re-roll it) and the feature-flag condition (evaluated with the same
//! context and rules as `/api/_temps/flags`, including server-only flags).
//! The page path, triggering event and once-per-visitor conditions depend on
//! browser state, so the SDK applies them.

use temps_flags::services::EnvironmentFlags;
use temps_flags::{bucket, evaluate_compiled, EvalContext, EvalReason};

use crate::model::{SurveyQuestion, SurveyTargeting};

/// An active survey as the public endpoint holds it in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveredSurvey {
    pub id: i32,
    pub name: String,
    pub questions: Vec<SurveyQuestion>,
    pub targeting: SurveyTargeting,
}

/// Whether `subject` falls inside a survey's sample. Deterministic per
/// survey and subject; independent of every flag bucket because the survey
/// id is the hashed key.
pub fn in_sample(survey_id: i32, sample_rate: f64, subject: Option<&str>) -> bool {
    if sample_rate.is_nan() || sample_rate <= 0.0 {
        return false;
    }
    if sample_rate >= 1.0 {
        return true;
    }
    // Below 100% without a stable subject the decision would change on every
    // page, so the visitor would eventually be shown a survey meant for a
    // fraction of visitors. Skip it instead, as a partial flag rollout does.
    let Some(subject) = subject.filter(|s| !s.is_empty()) else {
        return false;
    };
    bucket(&format!("survey:{survey_id}"), "", subject) < sample_rate * 100.0
}

/// Whether the survey's flag condition holds for this context. A survey
/// without one always passes. A flag that does not exist (or was archived),
/// or whose evaluation failed, fails closed: the survey was meant for the
/// flag's audience only, and a failed evaluation serves the default, which
/// says nothing about that audience. Numbers compare by value, so `1` and
/// `1.0` are the same condition.
pub fn flag_condition_holds(
    targeting: &SurveyTargeting,
    flags: Option<&EnvironmentFlags>,
    ctx: &EvalContext,
) -> bool {
    let Some(condition) = &targeting.flag else {
        return true;
    };
    let Some(flag) = flags.and_then(|flags| flags.get(&condition.key)) else {
        return false;
    };
    let evaluation = evaluate_compiled(&flag.snapshot, &flag.patterns, ctx);
    evaluation.reason != EvalReason::Error && same_value(&evaluation.value, &condition.value)
}

/// JSON equality, except that numbers are equal when their values are:
/// `serde_json` keeps `1` and `1.0` apart, and a console round trip through
/// JavaScript turns one into the other.
fn same_value(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (a, b) {
        // Two integers compare exactly (as f64, 2^53 + 1 would equal 2^53);
        // only a fractional operand compares as a float, so `1 == 1.0`.
        (Value::Number(x), Value::Number(y)) if !x.is_f64() && !y.is_f64() => {
            x.as_i64() == y.as_i64() && x.as_u64() == y.as_u64()
        }
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_value(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, x)| y.get(key).is_some_and(|y| same_value(x, y)))
        }
        _ => a == b,
    }
}

/// The surveys this visitor may see, in the order the project created them.
pub fn select<'a>(
    surveys: &'a [DeliveredSurvey],
    flags: Option<&EnvironmentFlags>,
    ctx: &EvalContext,
) -> Vec<&'a DeliveredSurvey> {
    surveys
        .iter()
        .filter(|survey| in_sample(survey.id, survey.targeting.sample_rate, ctx.key.as_deref()))
        .filter(|survey| flag_condition_holds(&survey.targeting, flags, ctx))
        .collect()
}

/// Whether any survey needs the environment's flags, so a project without
/// flag conditions never loads them.
pub fn needs_flags(surveys: &[DeliveredSurvey]) -> bool {
    surveys.iter().any(|survey| survey.targeting.flag.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SurveyFlagCondition;
    use temps_flags::services::DeliveredFlag;
    use temps_flags::{CompiledPatterns, FlagSnapshot, FlagValueType};

    fn survey(id: i32, targeting: SurveyTargeting) -> DeliveredSurvey {
        DeliveredSurvey {
            id,
            name: format!("survey {id}"),
            questions: Vec::new(),
            targeting,
        }
    }

    fn flags(key: &str, enabled: bool) -> EnvironmentFlags {
        flag_serving(
            key,
            enabled,
            FlagValueType::Bool,
            serde_json::json!(true),
            serde_json::json!(false),
        )
    }

    fn flag_serving(
        key: &str,
        enabled: bool,
        value_type: FlagValueType,
        value: serde_json::Value,
        default_value: serde_json::Value,
    ) -> EnvironmentFlags {
        let snapshot = FlagSnapshot {
            key: key.into(),
            value_type,
            enabled,
            environment_value: Some(value),
            default_value,
            salt: "salt".into(),
            variants: Vec::new(),
            rules: Vec::new(),
        };
        EnvironmentFlags::new(
            1,
            2,
            vec![DeliveredFlag {
                patterns: CompiledPatterns::default(),
                snapshot,
                client_visible: false,
                exposure_sample_rate: 0.0,
            }],
        )
    }

    fn ctx(key: Option<&str>) -> EvalContext {
        EvalContext {
            key: key.map(str::to_string),
            ..EvalContext::default()
        }
    }

    #[test]
    fn full_sample_needs_no_subject_and_zero_shows_nobody() {
        assert!(in_sample(1, 1.0, None));
        assert!(!in_sample(1, 0.0, Some("visitor")));
        assert!(!in_sample(1, f64::NAN, Some("visitor")));
    }

    #[test]
    fn a_partial_sample_skips_an_anonymous_visitor() {
        assert!(!in_sample(1, 0.5, None));
        assert!(!in_sample(1, 0.5, Some("")));
    }

    #[test]
    fn sampling_is_stable_and_close_to_the_rate() {
        let picked = (0..10_000)
            .filter(|i| in_sample(7, 0.3, Some(&format!("visitor-{i}"))))
            .count();
        assert!((2_700..=3_300).contains(&picked), "{picked} of 10000");
        for i in 0..100 {
            let subject = format!("visitor-{i}");
            assert_eq!(
                in_sample(7, 0.3, Some(&subject)),
                in_sample(7, 0.3, Some(&subject))
            );
        }
    }

    #[test]
    fn surveys_sample_independently_of_each_other() {
        let both = (0..10_000)
            .filter(|i| {
                let subject = format!("visitor-{i}");
                in_sample(1, 0.5, Some(&subject)) && in_sample(2, 0.5, Some(&subject))
            })
            .count();
        // Independent 50% samples overlap on about a quarter of visitors; the
        // same bucket for both would overlap on half.
        assert!((2_200..=2_800).contains(&both), "{both} of 10000");
    }

    fn on(key: &str, value: serde_json::Value) -> SurveyTargeting {
        SurveyTargeting {
            flag: Some(SurveyFlagCondition {
                key: key.into(),
                value,
            }),
            ..SurveyTargeting::default()
        }
    }

    #[test]
    fn a_failed_evaluation_fails_closed_even_when_the_default_matches() {
        // The stored value is not a boolean, so the flag serves its default
        // (false) with reason ERROR; a condition on `false` must not hold.
        let broken = flag_serving(
            "beta",
            true,
            FlagValueType::Bool,
            serde_json::json!("yes"),
            serde_json::json!(false),
        );
        assert!(!flag_condition_holds(
            &on("beta", serde_json::json!(false)),
            Some(&broken),
            &ctx(None)
        ));
    }

    #[test]
    fn numeric_conditions_compare_by_value() {
        let served = flag_serving(
            "limit",
            true,
            FlagValueType::Number,
            serde_json::json!(1.0),
            serde_json::json!(0),
        );
        assert!(flag_condition_holds(
            &on("limit", serde_json::json!(1)),
            Some(&served),
            &ctx(None)
        ));
        assert!(!flag_condition_holds(
            &on("limit", serde_json::json!(2)),
            Some(&served),
            &ctx(None)
        ));
        assert!(same_value(
            &serde_json::json!({"a": [1, 2.0]}),
            &serde_json::json!({"a": [1.0, 2]})
        ));
        assert!(!same_value(&serde_json::json!("1"), &serde_json::json!(1)));
        assert!(!same_value(
            &serde_json::json!(9_007_199_254_740_992_u64),
            &serde_json::json!(9_007_199_254_740_993_u64)
        ));
        assert!(!same_value(
            &serde_json::json!(u64::MAX),
            &serde_json::json!(u64::MAX - 1)
        ));
        assert!(!same_value(&serde_json::json!(-1), &serde_json::json!(1)));
        assert!(same_value(&serde_json::json!(-3), &serde_json::json!(-3.0)));
    }

    #[test]
    fn a_flag_condition_follows_the_flag() {
        let condition = SurveyTargeting {
            flag: Some(SurveyFlagCondition {
                key: "beta".into(),
                value: serde_json::json!(true),
            }),
            ..SurveyTargeting::default()
        };
        assert!(flag_condition_holds(
            &condition,
            Some(&flags("beta", true)),
            &ctx(None)
        ));
        // Killed: the flag serves its default, false.
        assert!(!flag_condition_holds(
            &condition,
            Some(&flags("beta", false)),
            &ctx(None)
        ));
    }

    #[test]
    fn a_missing_flag_fails_closed() {
        let condition = SurveyTargeting {
            flag: Some(SurveyFlagCondition {
                key: "gone".into(),
                value: serde_json::json!(true),
            }),
            ..SurveyTargeting::default()
        };
        assert!(!flag_condition_holds(
            &condition,
            Some(&flags("beta", true)),
            &ctx(None)
        ));
        assert!(!flag_condition_holds(&condition, None, &ctx(None)));
    }

    #[test]
    fn select_applies_both_conditions_in_creation_order() {
        let surveys = vec![
            survey(1, SurveyTargeting::default()),
            survey(
                2,
                SurveyTargeting {
                    sample_rate: 0.0,
                    ..SurveyTargeting::default()
                },
            ),
            survey(
                3,
                SurveyTargeting {
                    flag: Some(SurveyFlagCondition {
                        key: "beta".into(),
                        value: serde_json::json!(true),
                    }),
                    ..SurveyTargeting::default()
                },
            ),
        ];
        let environment = flags("beta", true);
        let ids: Vec<i32> = select(&surveys, Some(&environment), &ctx(Some("v")))
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec![1, 3]);
        assert!(needs_flags(&surveys));
        assert!(!needs_flags(&surveys[..2]));
    }
}
