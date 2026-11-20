// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Save-time validation of variants and targeting rules.
//!
//! The evaluator is total and degrades a malformed rule to `ERROR` rather than
//! failing. That is the safety net, not the plan: everything it would degrade
//! on is rejected here, with a message that names the rule and the field, so
//! a bad rule is a 400 in the console instead of a silent default in
//! production.

use std::collections::HashSet;

use crate::error::FlagError;
use crate::eval::{
    compile_pattern, AttributeValue, ConditionValue, FlagRule, FlagValueType, FlagVariant,
    OperandShape, RuleServe,
};

/// Most variants one flag may declare.
pub const MAX_VARIANTS: usize = 20;
/// Most rules one flag may carry in one environment.
pub const MAX_RULES: usize = 50;
/// Most conditions in one rule.
pub const MAX_CONDITIONS: usize = 20;
/// Most values in an `in` / `not_in` list.
pub const MAX_LIST_VALUES: usize = 100;
/// Longest string operand, and longest variant key.
pub const MAX_STRING_OPERAND: usize = 512;
pub const MAX_VARIANT_KEY_LEN: usize = 64;
/// Longest attribute name. Same bound the public endpoint applies to the
/// attributes a browser sends.
pub const MAX_ATTRIBUTE_NAME_LEN: usize = 64;
const MAX_RULE_DESCRIPTION_LEN: usize = 256;

fn invalid(key: &str, message: String) -> FlagError {
    FlagError::Validation {
        key: key.to_string(),
        message,
    }
}

/// Whether a JSON document contains U+0000 in any string, key or value.
/// PostgreSQL's `jsonb` cannot store it, so such a write would fail at the
/// database with an error that names no field.
fn contains_nul(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains('\0'),
        serde_json::Value::Array(items) => items.iter().any(contains_nul),
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(key, value)| key.contains('\0') || contains_nul(value)),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

/// Refuse `item` when its stored JSON would contain U+0000.
fn reject_nul<T: serde::Serialize>(flag_key: &str, item: &T, what: &str) -> Result<(), FlagError> {
    match serde_json::to_value(item) {
        Ok(value) if !contains_nul(&value) => Ok(()),
        Ok(_) => Err(invalid(
            flag_key,
            format!("{what} contains a NUL character (U+0000), which cannot be stored"),
        )),
        Err(e) => Err(invalid(
            flag_key,
            format!("{what} cannot be serialized for storage: {e}"),
        )),
    }
}

/// Variant keys follow the flag-key alphabet so they are safe as analytics
/// dimension values and in URLs.
fn validate_variant_key(flag_key: &str, variant_key: &str) -> Result<(), FlagError> {
    let valid_alphabet = variant_key.chars().enumerate().all(|(i, c)| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || (i > 0 && matches!(c, '.' | '_' | '-'))
    });
    if variant_key.is_empty() || variant_key.len() > MAX_VARIANT_KEY_LEN || !valid_alphabet {
        return Err(invalid(
            flag_key,
            format!(
                "variant key '{variant_key}' must match [a-z0-9][a-z0-9._-]* and be at most \
                 {MAX_VARIANT_KEY_LEN} characters"
            ),
        ));
    }
    Ok(())
}

pub fn validate_variants(
    flag_key: &str,
    value_type: FlagValueType,
    variants: &[FlagVariant],
) -> Result<(), FlagError> {
    if variants.len() > MAX_VARIANTS {
        return Err(invalid(
            flag_key,
            format!(
                "{} variants declared, maximum is {MAX_VARIANTS}",
                variants.len()
            ),
        ));
    }
    let mut seen = HashSet::new();
    for variant in variants {
        validate_variant_key(flag_key, &variant.key)?;
        reject_nul(flag_key, variant, &format!("variant '{}'", variant.key))?;
        if !seen.insert(variant.key.as_str()) {
            return Err(invalid(
                flag_key,
                format!("variant '{}' is declared twice", variant.key),
            ));
        }
        if !value_type.matches(&variant.value) {
            return Err(FlagError::ValueTypeMismatch {
                key: flag_key.to_string(),
                value_type: value_type.as_str().to_string(),
                value: variant.value.to_string(),
                field: format!("variants.{}.value", variant.key),
            });
        }
        if let Some(description) = &variant.description {
            if description.len() > MAX_RULE_DESCRIPTION_LEN {
                return Err(invalid(
                    flag_key,
                    format!(
                        "description of variant '{}' is {} characters, maximum is \
                         {MAX_RULE_DESCRIPTION_LEN}",
                        variant.key,
                        description.len()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Attribute names: letters, digits and `_ . - $`, at most 64 characters.
/// Bounded and plain so they are safe to show in the console and to use as
/// keys in the evaluator's attribute map.
fn validate_attribute_name(flag_key: &str, rule: usize, name: &str) -> Result<(), FlagError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_ATTRIBUTE_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '$'));
    if ok {
        return Ok(());
    }
    Err(invalid(
        flag_key,
        format!(
            "rule {rule}: attribute '{name}' must be 1-{MAX_ATTRIBUTE_NAME_LEN} characters of \
             letters, digits, '_', '.', '-' or '$'"
        ),
    ))
}

fn validate_scalar(flag_key: &str, rule: usize, value: &AttributeValue) -> Result<(), FlagError> {
    match value {
        AttributeValue::String(s) if s.len() > MAX_STRING_OPERAND => Err(invalid(
            flag_key,
            format!(
                "rule {rule}: a condition value is {} characters, maximum is {MAX_STRING_OPERAND}",
                s.len()
            ),
        )),
        AttributeValue::Number(n) if !n.is_finite() => Err(invalid(
            flag_key,
            format!("rule {rule}: condition values must be finite numbers"),
        )),
        _ => Ok(()),
    }
}

fn find_declared<'a>(
    flag_key: &str,
    rule: usize,
    variants: &'a [FlagVariant],
    name: &str,
) -> Result<&'a FlagVariant, FlagError> {
    variants
        .iter()
        .find(|variant| variant.key == name)
        .ok_or_else(|| {
            invalid(
                flag_key,
                format!("rule {rule}: variant '{name}' is not declared on the flag"),
            )
        })
}

/// Validate an ordered rule list for one environment of a flag.
pub fn validate_rules(
    flag_key: &str,
    value_type: FlagValueType,
    variants: &[FlagVariant],
    rules: &[FlagRule],
) -> Result<(), FlagError> {
    if rules.len() > MAX_RULES {
        return Err(invalid(
            flag_key,
            format!("{} rules, maximum is {MAX_RULES}", rules.len()),
        ));
    }

    for (index, rule) in rules.iter().enumerate() {
        reject_nul(flag_key, rule, &format!("rule {index}"))?;
        if let Some(description) = &rule.description {
            if description.len() > MAX_RULE_DESCRIPTION_LEN {
                return Err(invalid(
                    flag_key,
                    format!(
                        "rule {index}: description is {} characters, maximum is \
                         {MAX_RULE_DESCRIPTION_LEN}",
                        description.len()
                    ),
                ));
            }
        }

        if rule.conditions.len() > MAX_CONDITIONS {
            return Err(invalid(
                flag_key,
                format!(
                    "rule {index}: {} conditions, maximum is {MAX_CONDITIONS}",
                    rule.conditions.len()
                ),
            ));
        }

        for condition in &rule.conditions {
            validate_attribute_name(flag_key, index, &condition.attribute)?;
            let operator = condition.operator.as_str();
            match (condition.operator.operand(), &condition.value) {
                (OperandShape::None, None) => {}
                (OperandShape::None, Some(_)) => {
                    return Err(invalid(
                        flag_key,
                        format!("rule {index}: '{operator}' takes no value"),
                    ))
                }
                (_, None) => {
                    return Err(invalid(
                        flag_key,
                        format!("rule {index}: '{operator}' needs a value"),
                    ))
                }
                (OperandShape::List, Some(ConditionValue::List(values))) => {
                    if values.is_empty() || values.len() > MAX_LIST_VALUES {
                        return Err(invalid(
                            flag_key,
                            format!(
                                "rule {index}: '{operator}' needs 1-{MAX_LIST_VALUES} values, got {}",
                                values.len()
                            ),
                        ));
                    }
                    for value in values {
                        validate_scalar(flag_key, index, value)?;
                    }
                }
                (OperandShape::List, Some(ConditionValue::Single(_))) => {
                    return Err(invalid(
                        flag_key,
                        format!("rule {index}: '{operator}' needs a list of values"),
                    ))
                }
                (_, Some(ConditionValue::List(_))) => {
                    return Err(invalid(
                        flag_key,
                        format!("rule {index}: '{operator}' takes one value, not a list"),
                    ))
                }
                (OperandShape::Scalar, Some(ConditionValue::Single(value))) => {
                    validate_scalar(flag_key, index, value)?;
                }
                (OperandShape::String, Some(ConditionValue::Single(value))) => {
                    let AttributeValue::String(text) = value else {
                        return Err(invalid(
                            flag_key,
                            format!("rule {index}: '{operator}' needs a string value"),
                        ));
                    };
                    validate_scalar(flag_key, index, value)?;
                    if condition.operator == crate::eval::ConditionOperator::Matches {
                        compile_pattern(text).map_err(|reason| {
                            invalid(
                                flag_key,
                                format!("rule {index}: pattern '{text}' is not valid: {reason}"),
                            )
                        })?;
                    }
                }
                (OperandShape::Number, Some(ConditionValue::Single(value))) => {
                    let finite_number = matches!(value, AttributeValue::Number(n) if n.is_finite());
                    if !finite_number {
                        return Err(invalid(
                            flag_key,
                            format!("rule {index}: '{operator}' needs a number"),
                        ));
                    }
                }
            }
        }

        if let Some(percentage) = rule.rollout_percentage {
            if !percentage.is_finite() || !(0.0..=100.0).contains(&percentage) {
                return Err(invalid(
                    flag_key,
                    format!("rule {index}: rollout_percentage must be between 0 and 100"),
                ));
            }
        }

        match &rule.serve {
            RuleServe::Value { value } => {
                if !value_type.matches(value) {
                    return Err(FlagError::ValueTypeMismatch {
                        key: flag_key.to_string(),
                        value_type: value_type.as_str().to_string(),
                        value: value.to_string(),
                        field: format!("rules.{index}.serve.value"),
                    });
                }
            }
            RuleServe::Variant { variant } => {
                find_declared(flag_key, index, variants, variant)?;
            }
            RuleServe::Split { split } => {
                if split.len() < 2 {
                    return Err(invalid(
                        flag_key,
                        format!("rule {index}: a split needs at least two variants"),
                    ));
                }
                let mut seen = HashSet::new();
                let mut total: u32 = 0;
                for arm in split {
                    find_declared(flag_key, index, variants, &arm.variant)?;
                    if !seen.insert(arm.variant.as_str()) {
                        return Err(invalid(
                            flag_key,
                            format!(
                                "rule {index}: variant '{}' appears twice in the split",
                                arm.variant
                            ),
                        ));
                    }
                    total = total.saturating_add(arm.weight);
                }
                if total != 100 {
                    return Err(invalid(
                        flag_key,
                        format!("rule {index}: split weights sum to {total}, they must sum to 100"),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Variant keys a rule list refers to.
pub fn referenced_variants(rules: &[FlagRule]) -> HashSet<&str> {
    let mut names = HashSet::new();
    for rule in rules {
        match &rule.serve {
            RuleServe::Value { .. } => {}
            RuleServe::Variant { variant } => {
                names.insert(variant.as_str());
            }
            RuleServe::Split { split } => {
                names.extend(split.iter().map(|arm| arm.variant.as_str()));
            }
        }
    }
    names
}

pub fn validate_exposure_sample_rate(flag_key: &str, rate: f64) -> Result<(), FlagError> {
    if rate.is_finite() && (0.0..=1.0).contains(&rate) {
        return Ok(());
    }
    Err(invalid(
        flag_key,
        format!("exposure_sample_rate is {rate}, it must be between 0 and 1"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{ConditionOperator, RuleCondition, VariantWeight};

    fn variants() -> Vec<FlagVariant> {
        vec![
            FlagVariant {
                key: "control".into(),
                value: serde_json::json!("old"),
                description: None,
            },
            FlagVariant {
                key: "test".into(),
                value: serde_json::json!("new"),
                description: None,
            },
        ]
    }

    fn rule(conditions: Vec<RuleCondition>, serve: RuleServe) -> FlagRule {
        FlagRule {
            description: None,
            conditions,
            rollout_percentage: None,
            serve,
        }
    }

    fn cond(
        attribute: &str,
        operator: ConditionOperator,
        value: Option<ConditionValue>,
    ) -> RuleCondition {
        RuleCondition {
            attribute: attribute.into(),
            operator,
            value,
        }
    }

    fn check(rules: Vec<FlagRule>) -> Result<(), FlagError> {
        validate_rules("f", FlagValueType::String, &variants(), &rules)
    }

    fn message(result: Result<(), FlagError>) -> String {
        match result {
            Err(error) => error.to_string(),
            Ok(()) => panic!("expected a validation error"),
        }
    }

    #[test]
    fn nul_characters_are_refused_before_they_reach_jsonb() {
        let mut declared = variants();
        declared[0].description = Some("beta\0".into());
        let error = validate_variants("f", FlagValueType::String, &declared).unwrap_err();
        assert!(error.to_string().contains("variant 'control'"), "{error}");

        let mut declared = variants();
        declared[1].value = serde_json::json!("ne\0w");
        assert!(validate_variants("f", FlagValueType::String, &declared).is_err());
        assert!(validate_variants("f", FlagValueType::String, &variants()).is_ok());

        let operand = rule(
            vec![cond(
                "plan",
                ConditionOperator::Eq,
                Some(ConditionValue::Single(AttributeValue::String("p\0".into()))),
            )],
            RuleServe::Variant {
                variant: "test".into(),
            },
        );
        assert!(message(check(vec![operand])).contains("rule 0"));
    }

    #[test]
    fn a_well_formed_rule_list_passes() {
        let rules = vec![
            rule(
                vec![
                    cond(
                        "plan",
                        ConditionOperator::In,
                        Some(ConditionValue::List(vec![AttributeValue::String(
                            "pro".into(),
                        )])),
                    ),
                    cond("email", ConditionOperator::Exists, None),
                    cond(
                        "email",
                        ConditionOperator::Matches,
                        Some(ConditionValue::Single(AttributeValue::String(
                            r".+@acme\.test".into(),
                        ))),
                    ),
                ],
                RuleServe::Split {
                    split: vec![
                        VariantWeight {
                            variant: "control".into(),
                            weight: 50,
                        },
                        VariantWeight {
                            variant: "test".into(),
                            weight: 50,
                        },
                    ],
                },
            ),
            rule(
                Vec::new(),
                RuleServe::Value {
                    value: serde_json::json!("x"),
                },
            ),
        ];
        assert!(check(rules).is_ok());
    }

    #[test]
    fn rejects_an_undeclared_variant_and_names_the_rule() {
        let text = message(check(vec![
            rule(
                Vec::new(),
                RuleServe::Value {
                    value: serde_json::json!("x"),
                },
            ),
            rule(
                Vec::new(),
                RuleServe::Variant {
                    variant: "ghost".into(),
                },
            ),
        ]));
        assert!(text.contains("rule 1") && text.contains("ghost"), "{text}");
    }

    #[test]
    fn rejects_split_weights_that_do_not_sum_to_100() {
        let text = message(check(vec![rule(
            Vec::new(),
            RuleServe::Split {
                split: vec![
                    VariantWeight {
                        variant: "control".into(),
                        weight: 50,
                    },
                    VariantWeight {
                        variant: "test".into(),
                        weight: 40,
                    },
                ],
            },
        )]));
        assert!(text.contains("sum to 90"), "{text}");
    }

    #[test]
    fn rejects_a_served_value_of_the_wrong_type() {
        assert!(matches!(
            check(vec![rule(
                Vec::new(),
                RuleServe::Value {
                    value: serde_json::json!(1)
                }
            )]),
            Err(FlagError::ValueTypeMismatch { .. })
        ));
    }

    #[test]
    fn rejects_operand_shapes_that_do_not_fit_the_operator() {
        let single = |v: &str| Some(ConditionValue::Single(AttributeValue::String(v.into())));
        for condition in [
            cond("plan", ConditionOperator::In, single("pro")),
            cond("plan", ConditionOperator::Eq, None),
            cond("plan", ConditionOperator::Exists, single("x")),
            cond("seats", ConditionOperator::Gt, single("five")),
            cond(
                "plan",
                ConditionOperator::Contains,
                Some(ConditionValue::Single(AttributeValue::Number(1.0))),
            ),
            cond(
                "plan",
                ConditionOperator::In,
                Some(ConditionValue::List(Vec::new())),
            ),
        ] {
            assert!(
                check(vec![rule(
                    vec![condition.clone()],
                    RuleServe::Value {
                        value: serde_json::json!("x")
                    }
                )])
                .is_err(),
                "{condition:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_an_uncompilable_pattern() {
        let text = message(check(vec![rule(
            vec![cond(
                "email",
                ConditionOperator::Matches,
                Some(ConditionValue::Single(AttributeValue::String(
                    "(open".into(),
                ))),
            )],
            RuleServe::Value {
                value: serde_json::json!("x"),
            },
        )]));
        assert!(text.contains("pattern"), "{text}");
    }

    #[test]
    fn rejects_bad_attribute_names_and_out_of_range_rollouts() {
        let bad_name = rule(
            vec![cond("has space", ConditionOperator::Exists, None)],
            RuleServe::Value {
                value: serde_json::json!("x"),
            },
        );
        assert!(check(vec![bad_name]).is_err());

        let mut over = rule(
            Vec::new(),
            RuleServe::Value {
                value: serde_json::json!("x"),
            },
        );
        over.rollout_percentage = Some(100.5);
        assert!(check(vec![over]).is_err());
    }

    #[test]
    fn rejects_too_many_rules() {
        let rules = (0..=MAX_RULES)
            .map(|_| {
                rule(
                    Vec::new(),
                    RuleServe::Value {
                        value: serde_json::json!("x"),
                    },
                )
            })
            .collect();
        assert!(check(rules).is_err());
    }

    #[test]
    fn variant_validation() {
        assert!(validate_variants("f", FlagValueType::String, &variants()).is_ok());

        let mut duplicate = variants();
        duplicate[1].key = "control".into();
        assert!(validate_variants("f", FlagValueType::String, &duplicate).is_err());

        let mut wrong_type = variants();
        wrong_type[0].value = serde_json::json!(true);
        assert!(matches!(
            validate_variants("f", FlagValueType::String, &wrong_type),
            Err(FlagError::ValueTypeMismatch { field, .. }) if field == "variants.control.value"
        ));

        let mut bad_key = variants();
        bad_key[0].key = "Control".into();
        assert!(validate_variants("f", FlagValueType::String, &bad_key).is_err());
    }

    #[test]
    fn referenced_variants_covers_variant_and_split_serves() {
        let rules = vec![
            rule(
                Vec::new(),
                RuleServe::Variant {
                    variant: "a".into(),
                },
            ),
            rule(
                Vec::new(),
                RuleServe::Split {
                    split: vec![
                        VariantWeight {
                            variant: "b".into(),
                            weight: 50,
                        },
                        VariantWeight {
                            variant: "c".into(),
                            weight: 50,
                        },
                    ],
                },
            ),
        ];
        let names = referenced_variants(&rules);
        assert_eq!(names.len(), 3);
        assert!(names.contains("a") && names.contains("b") && names.contains("c"));
    }

    #[test]
    fn exposure_rate_must_be_a_fraction() {
        assert!(validate_exposure_sample_rate("f", 0.0).is_ok());
        assert!(validate_exposure_sample_rate("f", 1.0).is_ok());
        assert!(validate_exposure_sample_rate("f", 1.5).is_err());
        assert!(validate_exposure_sample_rate("f", f64::NAN).is_err());
    }
}
