// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Survey results from analytics events.
//!
//! Answers are `survey_sent` events whose properties carry `survey_id` and a
//! `responses` object keyed by question id. The database does the counting
//! (`GROUP BY`, bounded by a row limit); this module turns those counts into
//! per-question results. Events come from browsers, so nothing here trusts
//! their shape: answers that are not a declared choice or an in-range score
//! are counted as `other` rather than shown.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::model::{QuestionKind, SurveyQuestion, MAX_OPEN_TEXT_LEN};

pub const SHOWN_EVENT: &str = "survey_shown";
pub const DISMISSED_EVENT: &str = "survey_dismissed";
pub const SENT_EVENT: &str = "survey_sent";

/// Distinct (question, answer) pairs the distribution query returns at most.
/// Declared choices and scale values are far fewer; the cap only bounds what
/// junk answers sent by a misbehaving client can cost.
pub const MAX_DISTRIBUTION_ROWS: u64 = 5_000;

/// One `(question, answer) -> count` row from the database.
#[derive(Debug, Clone, PartialEq)]
pub struct AnswerRow {
    pub question_id: String,
    pub answer: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AnswerCount {
    /// The choice, or the score as text.
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NpsSummary {
    /// Answers of 9 or 10.
    pub promoters: i64,
    /// Answers of 7 or 8.
    pub passives: i64,
    /// Answers of 0 to 6.
    pub detractors: i64,
    /// `(promoters - detractors) / answers * 100`, from -100 to 100.
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct QuestionResults {
    pub question_id: String,
    pub kind: QuestionKind,
    pub prompt: String,
    /// Responses that answered this question.
    pub answered: i64,
    /// Count per choice (declared order) or per score (ascending), zeros
    /// included. Empty for open-text questions: read those in the responses
    /// list.
    pub distribution: Vec<AnswerCount>,
    /// Answers that are not a declared choice or an in-range score.
    pub other: i64,
    /// Mean score of `rating` and `nps` answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nps: Option<NpsSummary>,
}

/// Fold the database's answer counts into per-question results, in the
/// survey's question order.
pub fn summarize(
    questions: &[SurveyQuestion],
    answered: &HashMap<String, i64>,
    rows: &[AnswerRow],
) -> Vec<QuestionResults> {
    let mut by_question: HashMap<&str, Vec<&AnswerRow>> = HashMap::new();
    for row in rows {
        by_question
            .entry(row.question_id.as_str())
            .or_default()
            .push(row);
    }
    questions
        .iter()
        .map(|question| {
            let rows = by_question
                .get(question.id.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            summarize_question(
                question,
                answered.get(&question.id).copied().unwrap_or(0),
                rows,
            )
        })
        .collect()
}

fn summarize_question(
    question: &SurveyQuestion,
    answered: i64,
    rows: &[&AnswerRow],
) -> QuestionResults {
    let mut result = QuestionResults {
        question_id: question.id.clone(),
        kind: question.kind,
        prompt: question.prompt.clone(),
        answered,
        distribution: Vec::new(),
        other: 0,
        average: None,
        nps: None,
    };
    match question.kind {
        QuestionKind::OpenText => {}
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice => {
            let mut counts: Vec<AnswerCount> = question
                .choices
                .iter()
                .map(|choice| AnswerCount {
                    value: choice.clone(),
                    count: 0,
                })
                .collect();
            for row in rows {
                match counts.iter_mut().find(|c| c.value == row.answer) {
                    Some(slot) => slot.count += row.count,
                    None => result.other += row.count,
                }
            }
            result.distribution = counts;
        }
        QuestionKind::Rating | QuestionKind::Nps => {
            let (min, max) = (question.scale_min(), question.scale_max());
            let mut counts: Vec<AnswerCount> = (min..=max)
                .map(|score| AnswerCount {
                    value: score.to_string(),
                    count: 0,
                })
                .collect();
            let (mut total, mut sum) = (0i64, 0f64);
            for row in rows {
                match parse_score(&row.answer, min, max) {
                    Some(score) => {
                        counts[usize::from(score - min)].count += row.count;
                        total += row.count;
                        sum += f64::from(score) * row.count as f64;
                    }
                    None => result.other += row.count,
                }
            }
            if total > 0 {
                result.average = Some(sum / total as f64);
            }
            if question.kind == QuestionKind::Nps {
                result.nps = Some(nps_summary(&counts, total));
            }
            result.distribution = counts;
        }
    }
    result
}

/// An integer score in `min..=max`. `"7"` and `"7.0"` are 7; `"7.5"` and
/// `"eleven"` are not scores.
fn parse_score(raw: &str, min: u8, max: u8) -> Option<u8> {
    let value: f64 = raw.trim().parse().ok()?;
    if value.fract() != 0.0 || value < f64::from(min) || value > f64::from(max) {
        return None;
    }
    // In range of a u8 by the check above.
    Some(value as u8)
}

fn nps_summary(counts: &[AnswerCount], total: i64) -> NpsSummary {
    let mut summary = NpsSummary {
        promoters: 0,
        passives: 0,
        detractors: 0,
        score: 0.0,
    };
    for (score, slot) in counts.iter().enumerate() {
        match score {
            9 | 10 => summary.promoters += slot.count,
            7 | 8 => summary.passives += slot.count,
            _ => summary.detractors += slot.count,
        }
    }
    if total > 0 {
        summary.score = (summary.promoters - summary.detractors) as f64 / total as f64 * 100.0;
    }
    summary
}

/// An answer as the responses list shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum AnswerValue {
    /// Open text or a single choice.
    Text(String),
    /// A rating or NPS score.
    Number(f64),
    /// Multiple-choice selections.
    Choices(Vec<String>),
}

/// Keep only answers to declared questions, in the shape their kind allows.
/// Open text is cut to the length the SDK sends.
pub fn sanitize_answers(
    questions: &[SurveyQuestion],
    raw: Option<&serde_json::Value>,
) -> HashMap<String, AnswerValue> {
    let mut answers = HashMap::new();
    let Some(serde_json::Value::Object(raw)) = raw else {
        return answers;
    };
    for question in questions {
        let Some(value) = raw.get(&question.id) else {
            continue;
        };
        let answer = match (question.kind, value) {
            (
                QuestionKind::OpenText | QuestionKind::SingleChoice,
                serde_json::Value::String(text),
            ) => {
                if text.is_empty() {
                    continue;
                }
                AnswerValue::Text(text.chars().take(MAX_OPEN_TEXT_LEN).collect())
            }
            (QuestionKind::Rating | QuestionKind::Nps, serde_json::Value::Number(number)) => {
                match number.as_f64() {
                    Some(score) => AnswerValue::Number(score),
                    None => continue,
                }
            }
            (QuestionKind::MultipleChoice, serde_json::Value::Array(items)) => {
                let picked: Vec<String> = items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .take(question.choices.len().max(1))
                    .map(|item| item.chars().take(MAX_OPEN_TEXT_LEN).collect())
                    .collect();
                if picked.is_empty() {
                    continue;
                }
                AnswerValue::Choices(picked)
            }
            _ => continue,
        };
        answers.insert(question.id.clone(), answer);
    }
    answers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: &str, kind: QuestionKind, choices: &[&str]) -> SurveyQuestion {
        SurveyQuestion {
            id: id.into(),
            kind,
            prompt: format!("prompt {id}"),
            optional: false,
            choices: choices.iter().map(|c| c.to_string()).collect(),
            scale: None,
            lower_label: None,
            upper_label: None,
        }
    }

    fn row(question_id: &str, answer: &str, count: i64) -> AnswerRow {
        AnswerRow {
            question_id: question_id.into(),
            answer: answer.into(),
            count,
        }
    }

    #[test]
    fn choices_keep_declared_order_and_zeroes_and_count_junk_as_other() {
        let questions = vec![question(
            "plan",
            QuestionKind::SingleChoice,
            &["Free", "Pro", "Team"],
        )];
        let results = summarize(
            &questions,
            &HashMap::from([("plan".to_string(), 6)]),
            &[
                row("plan", "Pro", 4),
                row("plan", "Free", 1),
                row("plan", "<script>", 1),
            ],
        );
        let plan = &results[0];
        assert_eq!(plan.answered, 6);
        assert_eq!(
            plan.distribution,
            vec![
                AnswerCount {
                    value: "Free".into(),
                    count: 1
                },
                AnswerCount {
                    value: "Pro".into(),
                    count: 4
                },
                AnswerCount {
                    value: "Team".into(),
                    count: 0
                },
            ]
        );
        assert_eq!(plan.other, 1);
    }

    #[test]
    fn nps_score_and_average() {
        let questions = vec![question("nps", QuestionKind::Nps, &[])];
        // 3 promoters, 1 passive, 1 detractor, 1 out of range.
        let rows = [
            row("nps", "10", 2),
            row("nps", "9", 1),
            row("nps", "8", 1),
            row("nps", "3", 1),
            row("nps", "11", 1),
        ];
        let results = summarize(&questions, &HashMap::new(), &rows);
        let nps = results[0].nps.as_ref().unwrap();
        assert_eq!((nps.promoters, nps.passives, nps.detractors), (3, 1, 1));
        assert!((nps.score - 40.0).abs() < 1e-9, "{}", nps.score);
        assert_eq!(results[0].other, 1);
        assert_eq!(results[0].distribution.len(), 11);
        let average = results[0].average.unwrap();
        assert!((average - 8.0).abs() < 1e-9, "{average}");
    }

    #[test]
    fn a_rating_counts_only_whole_scores_in_range() {
        let mut rating = question("stars", QuestionKind::Rating, &[]);
        rating.scale = Some(3);
        let results = summarize(
            &[rating],
            &HashMap::new(),
            &[
                row("stars", "3", 2),
                row("stars", "2.0", 1),
                row("stars", "2.5", 1),
                row("stars", "0", 1),
            ],
        );
        let values: Vec<i64> = results[0].distribution.iter().map(|c| c.count).collect();
        assert_eq!(values, vec![0, 1, 2]);
        assert_eq!(results[0].other, 2);
        assert!(results[0].nps.is_none());
    }

    #[test]
    fn open_text_has_no_distribution_and_unanswered_questions_are_zero() {
        let questions = vec![
            question("why", QuestionKind::OpenText, &[]),
            question("plan", QuestionKind::SingleChoice, &["A", "B"]),
        ];
        let results = summarize(&questions, &HashMap::from([("why".to_string(), 3)]), &[]);
        assert_eq!(results[0].answered, 3);
        assert!(results[0].distribution.is_empty());
        assert_eq!(results[1].answered, 0);
        assert_eq!(
            results[1].distribution.iter().map(|c| c.count).sum::<i64>(),
            0
        );
        assert!(results[1].average.is_none());
    }

    #[test]
    fn rows_for_undeclared_questions_are_ignored() {
        let questions = vec![question("plan", QuestionKind::SingleChoice, &["A", "B"])];
        let results = summarize(&questions, &HashMap::new(), &[row("ghost", "A", 9)]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].other, 0);
    }

    #[test]
    fn answers_are_sanitized_to_declared_questions_and_shapes() {
        let questions = vec![
            question("why", QuestionKind::OpenText, &[]),
            question("nps", QuestionKind::Nps, &[]),
            question("tags", QuestionKind::MultipleChoice, &["a", "b"]),
            question("plan", QuestionKind::SingleChoice, &["A", "B"]),
        ];
        let long = "x".repeat(MAX_OPEN_TEXT_LEN + 50);
        let raw = serde_json::json!({
            "why": long,
            "nps": 9,
            "tags": ["a", 7, "b", "c"],
            "plan": 3,
            "ghost": "boo",
        });
        let answers = sanitize_answers(&questions, Some(&raw));
        assert_eq!(answers.len(), 3);
        match &answers["why"] {
            AnswerValue::Text(text) => assert_eq!(text.chars().count(), MAX_OPEN_TEXT_LEN),
            other => panic!("{other:?}"),
        }
        assert_eq!(answers["nps"], AnswerValue::Number(9.0));
        // At most as many picks as there are choices.
        assert_eq!(
            answers["tags"],
            AnswerValue::Choices(vec!["a".into(), "b".into()])
        );
        assert!(!answers.contains_key("plan"));
        assert!(sanitize_answers(&questions, Some(&serde_json::json!("nope"))).is_empty());
        assert!(sanitize_answers(&questions, None).is_empty());
    }
}
