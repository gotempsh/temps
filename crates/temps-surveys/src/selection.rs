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
use temps_flags::{bucket, evaluate_compiled, EvalContext};

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
/// without one always passes. A flag that does not exist (or was archived)
/// fails closed: the survey was meant for the flag's audience only.
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
    evaluate_compiled(&flag.snapshot, &flag.patterns, ctx).value == condition.value
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
        let snapshot = FlagSnapshot {
            key: key.into(),
            value_type: FlagValueType::Bool,
            enabled,
            environment_value: Some(serde_json::json!(true)),
            default_value: serde_json::json!(false),
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
