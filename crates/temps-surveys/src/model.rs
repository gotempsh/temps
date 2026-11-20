// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Survey definitions and their validation (ADR 110 D1).
//!
//! A survey is an ordered list of questions plus display conditions. The
//! definition is stored as JSONB and validated here on every write, so the
//! public endpoint and the browser never see a shape they cannot render.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const MAX_NAME_LEN: usize = 200;
pub const MAX_DESCRIPTION_LEN: usize = 1_000;
pub const MAX_QUESTIONS: usize = 20;
pub const MAX_QUESTION_ID_LEN: usize = 32;
pub const MAX_PROMPT_LEN: usize = 500;
pub const MAX_CHOICES: usize = 20;
pub const MAX_CHOICE_LEN: usize = 200;
pub const MAX_LABEL_LEN: usize = 100;
pub const MAX_PATH_PATTERN_LEN: usize = 500;
pub const MAX_EVENT_NAME_LEN: usize = 128;
/// Longest open-text answer the browser SDK sends. Results treat anything
/// longer as the SDK would have: cut at this length.
pub const MAX_OPEN_TEXT_LEN: usize = 2_000;
/// Rating scales a question may use.
pub const RATING_SCALES: [u8; 4] = [3, 5, 7, 10];
pub const DEFAULT_RATING_SCALE: u8 = 5;

/// Where a survey is in its life. Only `active` surveys reach browsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurveyStatus {
    /// Being written; never shown.
    Draft,
    /// Shown to visitors who match its conditions.
    Active,
    /// Temporarily not shown; results are kept.
    Paused,
    /// Closed for good; results are kept.
    Completed,
}

impl SurveyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SurveyStatus::Draft => "draft",
            SurveyStatus::Active => "active",
            SurveyStatus::Paused => "paused",
            SurveyStatus::Completed => "completed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(SurveyStatus::Draft),
            "active" => Some(SurveyStatus::Active),
            "paused" => Some(SurveyStatus::Paused),
            "completed" => Some(SurveyStatus::Completed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Free text, at most 2,000 characters.
    OpenText,
    /// A number from 1 to `scale`.
    Rating,
    /// Net Promoter Score: 0 to 10.
    Nps,
    /// Exactly one of `choices`.
    SingleChoice,
    /// Any number of `choices`.
    MultipleChoice,
}

impl QuestionKind {
    pub fn has_choices(self) -> bool {
        matches!(
            self,
            QuestionKind::SingleChoice | QuestionKind::MultipleChoice
        )
    }

    pub fn is_scale(self) -> bool {
        matches!(self, QuestionKind::Rating | QuestionKind::Nps)
    }
}

/// One question of a survey.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SurveyQuestion {
    /// Stable id the answer is recorded under: `[a-z0-9_]`, 1-32 characters,
    /// unique within the survey. Keep it when rewording a question so its
    /// answers stay together.
    pub id: String,
    pub kind: QuestionKind,
    pub prompt: String,
    /// When true the visitor may skip it.
    #[serde(default)]
    pub optional: bool,
    /// Options for `single_choice` and `multiple_choice`: 2-20, unique.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Top of a `rating` scale: 3, 5, 7 or 10. Defaults to 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u8>,
    /// Label under the low end of a `rating` or `nps` scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lower_label: Option<String>,
    /// Label under the high end of a `rating` or `nps` scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper_label: Option<String>,
}

impl SurveyQuestion {
    /// The highest answer a scale question accepts.
    pub fn scale_max(&self) -> u8 {
        match self.kind {
            QuestionKind::Nps => 10,
            _ => self.scale.unwrap_or(DEFAULT_RATING_SCALE),
        }
    }

    /// The lowest answer a scale question accepts.
    pub fn scale_min(&self) -> u8 {
        match self.kind {
            QuestionKind::Nps => 0,
            _ => 1,
        }
    }
}

/// Show the survey only to visitors for whom a feature flag serves a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SurveyFlagCondition {
    /// Flag key in the same project.
    pub key: String,
    /// The value the flag must serve. Defaults to `true`.
    #[serde(default = "default_flag_value")]
    pub value: serde_json::Value,
}

fn default_flag_value() -> serde_json::Value {
    serde_json::Value::Bool(true)
}

fn default_sample_rate() -> f64 {
    1.0
}

fn default_true() -> bool {
    true
}

/// When a browser shows the survey. Every condition set must hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SurveyTargeting {
    /// Page path the survey appears on, e.g. `/checkout/*`. `*` matches any
    /// run of characters; the pattern must match the whole path. Omit for
    /// every page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_pattern: Option<String>,
    /// Show only after the app tracks an analytics event with this name
    /// (`trackEvent("checkout_completed")`). Omit to show on page load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_event: Option<String>,
    /// Share of visitors, `0..=1`, who may see it. Below 1 the visitor must be
    /// identifiable (the Temps visitor cookie or an explicit key), so the
    /// decision is the same on every page.
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,
    /// Never show the survey again to a visitor who saw it. Defaults to true.
    #[serde(default = "default_true")]
    pub once_per_visitor: bool,
    /// Show only where a feature flag serves a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<SurveyFlagCondition>,
}

impl Default for SurveyTargeting {
    fn default() -> Self {
        Self {
            path_pattern: None,
            trigger_event: None,
            sample_rate: 1.0,
            once_per_visitor: true,
            flag: None,
        }
    }
}

/// A complete definition, as validated before it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct SurveyDefinition {
    pub name: String,
    pub description: Option<String>,
    pub status: SurveyStatus,
    pub questions: Vec<SurveyQuestion>,
    pub targeting: SurveyTargeting,
}

/// Every reason a definition is refused, so one save reports them all.
pub fn validate(definition: &SurveyDefinition) -> Result<(), String> {
    let mut problems = Vec::new();
    let name = definition.name.trim();
    if name.is_empty() {
        problems.push("name must not be empty".to_string());
    } else if definition.name.chars().count() > MAX_NAME_LEN {
        problems.push(format!("name is longer than {MAX_NAME_LEN} characters"));
    }
    if let Some(description) = &definition.description {
        if description.chars().count() > MAX_DESCRIPTION_LEN {
            problems.push(format!(
                "description is longer than {MAX_DESCRIPTION_LEN} characters"
            ));
        }
    }

    if definition.questions.len() > MAX_QUESTIONS {
        problems.push(format!(
            "{} questions, maximum is {MAX_QUESTIONS}",
            definition.questions.len()
        ));
    }
    if definition.status == SurveyStatus::Active && definition.questions.is_empty() {
        problems.push("an active survey needs at least one question".to_string());
    }
    let mut ids = HashSet::new();
    for (index, question) in definition.questions.iter().enumerate() {
        let at = format!("question {} ('{}')", index + 1, question.id);
        validate_question(question, &at, &mut problems);
        if !ids.insert(question.id.as_str()) {
            problems.push(format!("{at}: id is used by an earlier question"));
        }
    }

    validate_targeting(&definition.targeting, &mut problems);

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

fn validate_question(question: &SurveyQuestion, at: &str, problems: &mut Vec<String>) {
    let id = &question.id;
    if id.is_empty()
        || id.len() > MAX_QUESTION_ID_LEN
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        problems.push(format!(
            "{at}: id must be 1-{MAX_QUESTION_ID_LEN} characters of a-z, 0-9 and _"
        ));
    }
    let prompt = question.prompt.trim();
    if prompt.is_empty() {
        problems.push(format!("{at}: prompt must not be empty"));
    } else if question.prompt.chars().count() > MAX_PROMPT_LEN {
        problems.push(format!(
            "{at}: prompt is longer than {MAX_PROMPT_LEN} characters"
        ));
    }

    if question.kind.has_choices() {
        if question.choices.len() < 2 || question.choices.len() > MAX_CHOICES {
            problems.push(format!(
                "{at}: needs 2-{MAX_CHOICES} choices, has {}",
                question.choices.len()
            ));
        }
        let mut seen = HashSet::new();
        for choice in &question.choices {
            if choice.trim().is_empty() || choice.chars().count() > MAX_CHOICE_LEN {
                problems.push(format!(
                    "{at}: every choice must be 1-{MAX_CHOICE_LEN} characters"
                ));
            }
            if !seen.insert(choice.as_str()) {
                problems.push(format!("{at}: choice '{choice}' is listed twice"));
            }
        }
    } else if !question.choices.is_empty() {
        problems.push(format!(
            "{at}: only single_choice and multiple_choice questions take choices"
        ));
    }

    match (question.kind, question.scale) {
        (QuestionKind::Rating, Some(scale)) if !RATING_SCALES.contains(&scale) => {
            problems.push(format!(
                "{at}: scale {scale} is not one of {RATING_SCALES:?}"
            ));
        }
        (QuestionKind::Rating, _) => {}
        (_, Some(_)) => problems.push(format!("{at}: only rating questions take a scale")),
        (_, None) => {}
    }

    for (label, name) in [
        (&question.lower_label, "lower_label"),
        (&question.upper_label, "upper_label"),
    ] {
        if let Some(label) = label {
            if !question.kind.is_scale() {
                problems.push(format!("{at}: only rating and nps questions take a {name}"));
            } else if label.chars().count() > MAX_LABEL_LEN {
                problems.push(format!(
                    "{at}: {name} is longer than {MAX_LABEL_LEN} characters"
                ));
            }
        }
    }
}

fn validate_targeting(targeting: &SurveyTargeting, problems: &mut Vec<String>) {
    if let Some(pattern) = &targeting.path_pattern {
        if !pattern.starts_with('/') {
            problems.push(format!(
                "targeting: path_pattern '{pattern}' must start with '/'"
            ));
        }
        if pattern.len() > MAX_PATH_PATTERN_LEN {
            problems.push(format!(
                "targeting: path_pattern is longer than {MAX_PATH_PATTERN_LEN} characters"
            ));
        }
    }
    if let Some(event) = &targeting.trigger_event {
        if event.is_empty()
            || event.len() > MAX_EVENT_NAME_LEN
            || !event
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        {
            problems.push(format!(
                "targeting: trigger_event '{event}' must be 1-{MAX_EVENT_NAME_LEN} characters of \
                 letters, digits, '_', '-', '.' and ':'"
            ));
        }
    }
    let rate = targeting.sample_rate;
    if !(0.0..=1.0).contains(&rate) {
        problems.push(format!(
            "targeting: sample_rate {rate} must be between 0 and 1"
        ));
    }
    if let Some(flag) = &targeting.flag {
        if flag.key.trim().is_empty() || flag.key.len() > 128 {
            problems.push("targeting: flag.key must be 1-128 characters".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: &str, kind: QuestionKind) -> SurveyQuestion {
        SurveyQuestion {
            id: id.into(),
            kind,
            prompt: "How was it?".into(),
            optional: false,
            choices: if kind.has_choices() {
                vec!["Yes".into(), "No".into()]
            } else {
                Vec::new()
            },
            scale: None,
            lower_label: None,
            upper_label: None,
        }
    }

    fn definition(questions: Vec<SurveyQuestion>) -> SurveyDefinition {
        SurveyDefinition {
            name: "Checkout feedback".into(),
            description: None,
            status: SurveyStatus::Active,
            questions,
            targeting: SurveyTargeting::default(),
        }
    }

    #[test]
    fn every_question_kind_validates() {
        let questions = vec![
            question("open", QuestionKind::OpenText),
            question("rating", QuestionKind::Rating),
            question("nps", QuestionKind::Nps),
            question("single", QuestionKind::SingleChoice),
            question("multi", QuestionKind::MultipleChoice),
        ];
        assert_eq!(validate(&definition(questions)), Ok(()));
    }

    #[test]
    fn an_active_survey_without_questions_is_refused_but_a_draft_is_not() {
        let mut survey = definition(Vec::new());
        let error = validate(&survey).unwrap_err();
        assert!(error.contains("at least one question"), "{error}");
        survey.status = SurveyStatus::Draft;
        assert_eq!(validate(&survey), Ok(()));
    }

    #[test]
    fn duplicate_ids_bad_ids_and_bad_choices_are_all_reported_at_once() {
        let mut single = question("pick", QuestionKind::SingleChoice);
        single.choices = vec!["A".into(), "A".into()];
        let mut bad_id = question("Bad Id", QuestionKind::OpenText);
        bad_id.prompt = "  ".into();
        let error = validate(&definition(vec![
            question("same", QuestionKind::OpenText),
            question("same", QuestionKind::Nps),
            single,
            bad_id,
        ]))
        .unwrap_err();
        assert!(error.contains("question 2 ('same'): id is used"), "{error}");
        assert!(error.contains("choice 'A' is listed twice"), "{error}");
        assert!(
            error.contains("question 4 ('Bad Id'): id must be"),
            "{error}"
        );
        assert!(error.contains("prompt must not be empty"), "{error}");
    }

    #[test]
    fn scales_and_labels_belong_to_scale_questions_only() {
        let mut rating = question("r", QuestionKind::Rating);
        rating.scale = Some(4);
        let mut open = question("o", QuestionKind::OpenText);
        open.scale = Some(5);
        open.lower_label = Some("low".into());
        let error = validate(&definition(vec![rating, open])).unwrap_err();
        assert!(error.contains("scale 4 is not one of"), "{error}");
        assert!(
            error.contains("only rating questions take a scale"),
            "{error}"
        );
        assert!(error.contains("take a lower_label"), "{error}");
    }

    #[test]
    fn choices_on_a_scale_question_are_refused() {
        let mut nps = question("n", QuestionKind::Nps);
        nps.choices = vec!["x".into(), "y".into()];
        let error = validate(&definition(vec![nps])).unwrap_err();
        assert!(error.contains("take choices"), "{error}");
    }

    #[test]
    fn targeting_is_validated() {
        let mut survey = definition(vec![question("q", QuestionKind::Nps)]);
        survey.targeting = SurveyTargeting {
            path_pattern: Some("checkout".into()),
            trigger_event: Some("has space".into()),
            sample_rate: 1.5,
            once_per_visitor: true,
            flag: Some(SurveyFlagCondition {
                key: " ".into(),
                value: serde_json::Value::Bool(true),
            }),
        };
        let error = validate(&survey).unwrap_err();
        assert!(error.contains("must start with '/'"), "{error}");
        assert!(error.contains("trigger_event 'has space'"), "{error}");
        assert!(error.contains("sample_rate 1.5"), "{error}");
        assert!(error.contains("flag.key"), "{error}");
    }

    #[test]
    fn a_nan_sample_rate_is_refused() {
        let mut survey = definition(vec![question("q", QuestionKind::Nps)]);
        survey.targeting.sample_rate = f64::NAN;
        assert!(validate(&survey).unwrap_err().contains("sample_rate"));
    }

    #[test]
    fn targeting_defaults_show_everywhere_once_per_visitor() {
        let parsed: SurveyTargeting = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, SurveyTargeting::default());
        assert!(parsed.once_per_visitor);
        assert_eq!(parsed.sample_rate, 1.0);
    }

    #[test]
    fn scale_bounds() {
        let nps = question("n", QuestionKind::Nps);
        assert_eq!((nps.scale_min(), nps.scale_max()), (0, 10));
        let mut rating = question("r", QuestionKind::Rating);
        assert_eq!((rating.scale_min(), rating.scale_max()), (1, 5));
        rating.scale = Some(10);
        assert_eq!(rating.scale_max(), 10);
    }

    #[test]
    fn status_round_trips() {
        for status in [
            SurveyStatus::Draft,
            SurveyStatus::Active,
            SurveyStatus::Paused,
            SurveyStatus::Completed,
        ] {
            assert_eq!(SurveyStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(SurveyStatus::parse("archived"), None);
    }
}
