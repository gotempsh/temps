// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps_ask_user`: the assistant asks the user a question and waits for the
//! answer inside the same turn.
//!
//! The question is shown with the same question card the native harnesses use
//! (`PermissionKind::Question`), so a gateway model can ask a multiple choice
//! or free-text question instead of guessing. Questions are never answered
//! automatically, whatever the chat's permission mode: only a person answers.
//!
//! The answers come back as the tool result, which is saved with the tool call,
//! so the questions and what the user chose stay in the transcript after a
//! reload and in the model's context on later turns.

use serde::Serialize;
use serde_json::Value;
use temps_ai::streaming::ChatTool;

/// Tool name the model calls.
pub const ASK_USER_TOOL: &str = "temps_ask_user";

/// Questions in one call. The card shows them together with one Submit.
const MAX_QUESTIONS: usize = 4;
/// Options per question. More than this belongs in a free-text answer.
const MAX_OPTIONS: usize = 6;
const MAX_QUESTION_CHARS: usize = 300;
const MAX_HEADER_CHARS: usize = 40;
const MAX_LABEL_CHARS: usize = 80;
const MAX_DESCRIPTION_CHARS: usize = 200;
/// Longest answer returned to the model. The resolve endpoint already caps the
/// whole answer set at 4 KiB; this keeps any single answer readable.
const MAX_ANSWER_CHARS: usize = 2_000;

pub fn tool() -> ChatTool {
    ChatTool {
        name: ASK_USER_TOOL.to_string(),
        description: "Ask the user and wait for the answer. Use it when you cannot \
            continue correctly without their decision: which of several matching projects, \
            environments or services they mean, which option to pick when the choice \
            changes the result (a database type, a region, a plan), or a value only they \
            know. Offer the likely choices as `options` (the user can always type their \
            own answer instead), and ask up to 4 related questions in one call rather than \
            one at a time. Do not use it for confirming a change (`temps_write` already \
            asks for approval), for credentials (use `request_secret`), or when the answer \
            can be looked up with the `temps` tool. The answers are returned to you; carry \
            on with them in this same turn."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The full question, ending with a question mark."
                            },
                            "header": {
                                "type": "string",
                                "description": "A 1-3 word label shown above it, e.g. \"Database\"."
                            },
                            "options": {
                                "type": "array",
                                "maxItems": MAX_OPTIONS,
                                "description": "Likely answers. Omit for a free-text question.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "1-5 words." },
                                        "description": { "type": "string", "description": "What choosing it means." }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multiSelect": {
                                "type": "boolean",
                                "description": "Let the user pick more than one option."
                            }
                        },
                        "required": ["question"]
                    }
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AskOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One question, in the shape the console's question card renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AskQuestion {
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub options: Vec<AskOption>,
    #[serde(rename = "multiSelect")]
    pub multi_select: bool,
}

/// One answered question, as returned to the model and saved in the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AskAnswer {
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub answer: String,
}

/// Validate the model's arguments. The error is a sentence for the model, so
/// it can correct the call.
pub fn parse(arguments: &str) -> Result<Vec<AskQuestion>, String> {
    let args: Value = serde_json::from_str(arguments)
        .map_err(|error| format!("Invalid `{ASK_USER_TOOL}` arguments (not JSON): {error}"))?;
    let Some(items) = args.get("questions").and_then(Value::as_array) else {
        return Err(format!(
            "`{ASK_USER_TOOL}` needs a `questions` array with at least one question."
        ));
    };
    if items.is_empty() || items.len() > MAX_QUESTIONS {
        return Err(format!(
            "`{ASK_USER_TOOL}` takes 1 to {MAX_QUESTIONS} questions; {} were given.",
            items.len()
        ));
    }
    let mut questions: Vec<AskQuestion> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let number = index + 1;
        let question = text(item.get("question"))
            .ok_or_else(|| format!("Question {number} has no `question` text."))?;
        if question.chars().count() > MAX_QUESTION_CHARS {
            return Err(format!(
                "Question {number} is longer than {MAX_QUESTION_CHARS} characters; shorten it."
            ));
        }
        // Answers come back keyed by the question text, so two identical
        // questions would share one answer.
        if questions
            .iter()
            .any(|existing| existing.question == question)
        {
            return Err(format!(
                "Question {number} repeats an earlier question; each must be different."
            ));
        }
        let header = text(item.get("header")).map(|header| clip(&header, MAX_HEADER_CHARS));
        let raw_options = match item.get("options") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(options)) => options.clone(),
            Some(_) => {
                return Err(format!(
                "Question {number} has `options` that is not an array of {{label, description}}."
            ))
            }
        };
        if raw_options.len() > MAX_OPTIONS {
            return Err(format!(
                "Question {number} has {} options; offer at most {MAX_OPTIONS}.",
                raw_options.len()
            ));
        }
        let mut options: Vec<AskOption> = Vec::with_capacity(raw_options.len());
        for option in &raw_options {
            // A bare string is a common shorthand for a label.
            let (label, description) = match option {
                Value::String(label) => (Some(label.trim().to_string()), None),
                _ => (text(option.get("label")), text(option.get("description"))),
            };
            let Some(label) = label.filter(|label| !label.is_empty()) else {
                return Err(format!(
                    "Question {number} has an option without a `label`."
                ));
            };
            let label = clip(&label, MAX_LABEL_CHARS);
            if options.iter().any(|existing| existing.label == label) {
                continue;
            }
            options.push(AskOption {
                label,
                description: description.map(|d| clip(&d, MAX_DESCRIPTION_CHARS)),
            });
        }
        let multi_select = item
            .get("multiSelect")
            .or_else(|| item.get("multi_select"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && !options.is_empty();
        questions.push(AskQuestion {
            question,
            header,
            options,
            multi_select,
        });
    }
    Ok(questions)
}

/// The question card's input.
pub fn card_input(questions: &[AskQuestion]) -> Value {
    serde_json::json!({ "questions": questions })
}

/// Pair each question with the user's answer. The card sends answers keyed by
/// question text: a string, or a list for some multi-select clients.
pub fn answers(questions: &[AskQuestion], answers: &Value) -> Vec<AskAnswer> {
    questions
        .iter()
        .map(|question| {
            let answer = match answers.get(&question.question) {
                Some(Value::String(answer)) => answer.trim().to_string(),
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => String::new(),
            };
            AskAnswer {
                question: question.question.clone(),
                header: question.header.clone(),
                answer: clip(&answer, MAX_ANSWER_CHARS),
            }
        })
        .collect()
}

/// Tool result for answered questions. An empty answer means the user skipped
/// that question; the note tells the model not to invent one.
pub fn answered_result(answers: &[AskAnswer]) -> String {
    let skipped = answers.iter().any(|answer| answer.answer.is_empty());
    let mut result = serde_json::json!({
        "status": "answered",
        "answers": answers,
    });
    if skipped {
        result["note"] = Value::String(
            "An empty answer means the user left that question blank. Do not assume one; \
             continue without it or ask in your reply."
                .to_string(),
        );
    }
    result.to_string()
}

/// Tool result when no answer came back (the turn was stopped, the question
/// expired, or the user dismissed it).
pub fn unanswered_result(reason: &str) -> String {
    serde_json::json!({
        "status": "not_answered",
        "reason": reason,
        "note": "The user did not answer. Do not assume an answer: say what you need and \
                 end your reply so they can answer in the chat.",
    })
    .to_string()
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn clip(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_questions_into_the_card_shape() {
        let questions = parse(
            r#"{"questions":[{"question":"Which database?","header":"Database",
                "options":[{"label":"PostgreSQL","description":"Relational"},"Redis"],
                "multiSelect":false}]}"#,
        )
        .expect("valid questions");
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].options.len(), 2);
        assert_eq!(questions[0].options[1].label, "Redis");
        let input = card_input(&questions);
        assert_eq!(input["questions"][0]["question"], "Which database?");
        assert_eq!(input["questions"][0]["header"], "Database");
        assert_eq!(input["questions"][0]["multiSelect"], false);
        assert_eq!(input["questions"][0]["options"][0]["label"], "PostgreSQL");
        assert!(input["questions"][0]["options"][1]
            .get("description")
            .is_none());
    }

    #[test]
    fn a_free_text_question_has_no_options_and_cannot_be_multi_select() {
        let questions = parse(
            r#"{"questions":[{"question":"What should the app be called?","multiSelect":true}]}"#,
        )
        .expect("valid question");
        assert!(questions[0].options.is_empty());
        assert!(!questions[0].multi_select);
        // The card maps over `options`, so it is always present.
        assert_eq!(
            card_input(&questions)["questions"][0]["options"],
            serde_json::json!([])
        );
    }

    #[test]
    fn rejects_calls_the_card_cannot_show() {
        assert!(parse("not json").unwrap_err().contains("not JSON"));
        assert!(parse(r#"{}"#).unwrap_err().contains("`questions` array"));
        assert!(parse(r#"{"questions":[]}"#).unwrap_err().contains("1 to 4"));
        let five = serde_json::json!({
            "questions": (0..5).map(|i| serde_json::json!({"question": format!("Q{i}?")})).collect::<Vec<_>>()
        });
        assert!(parse(&five.to_string())
            .unwrap_err()
            .contains("5 were given"));
        assert!(parse(r#"{"questions":[{"question":"  "}]}"#)
            .unwrap_err()
            .contains("Question 1 has no `question`"));
        assert!(
            parse(r#"{"questions":[{"question":"A?"},{"question":"A?"}]}"#)
                .unwrap_err()
                .contains("Question 2 repeats")
        );
        assert!(
            parse(r#"{"questions":[{"question":"A?","options":[{"description":"x"}]}]}"#)
                .unwrap_err()
                .contains("without a `label`")
        );
        assert!(
            parse(r#"{"questions":[{"question":"A?","options":"yes,no"}]}"#)
                .unwrap_err()
                .contains("not an array")
        );
        let many = serde_json::json!({"questions":[{"question":"A?","options":(0..7).map(|i| format!("o{i}")).collect::<Vec<_>>()}]});
        assert!(parse(&many.to_string()).unwrap_err().contains("at most 6"));
        let long = serde_json::json!({"questions":[{"question":"x".repeat(301)}]});
        assert!(parse(&long.to_string())
            .unwrap_err()
            .contains("300 characters"));
    }

    #[test]
    fn duplicate_option_labels_collapse() {
        let questions = parse(r#"{"questions":[{"question":"Pick?","options":["A","A","B"]}]}"#)
            .expect("valid");
        assert_eq!(questions[0].options.len(), 2);
    }

    #[test]
    fn pairs_each_question_with_its_answer() {
        let questions = parse(
            r#"{"questions":[{"question":"Which database?","header":"Database","options":["PostgreSQL","Redis"]},
                {"question":"Which features?","options":["Backups","Metrics"],"multiSelect":true},
                {"question":"Name?"}]}"#,
        )
        .expect("valid");
        let paired = answers(
            &questions,
            &serde_json::json!({
                "Which database?": "PostgreSQL",
                "Which features?": ["Backups", "Metrics"],
                "Name?": "  shop-db  ",
            }),
        );
        assert_eq!(paired[0].answer, "PostgreSQL");
        assert_eq!(paired[0].header.as_deref(), Some("Database"));
        assert_eq!(paired[1].answer, "Backups, Metrics");
        assert_eq!(paired[2].answer, "shop-db");

        let result: Value = serde_json::from_str(&answered_result(&paired)).expect("json");
        assert_eq!(result["status"], "answered");
        assert_eq!(result["answers"][2]["question"], "Name?");
        assert_eq!(result["answers"][2]["answer"], "shop-db");
        assert!(result.get("note").is_none());
    }

    #[test]
    fn a_missing_answer_is_reported_as_blank_not_invented() {
        let questions =
            parse(r#"{"questions":[{"question":"A?"},{"question":"B?"}]}"#).expect("valid");
        let paired = answers(
            &questions,
            &serde_json::json!({ "A?": "yes", "C?": "stray" }),
        );
        assert_eq!(paired[1].answer, "");
        let result: Value = serde_json::from_str(&answered_result(&paired)).expect("json");
        assert!(result["note"]
            .as_str()
            .unwrap_or_default()
            .contains("left that question blank"));
        // Answers to questions that were never asked are dropped.
        assert!(!result.to_string().contains("stray"));
    }

    #[test]
    fn long_answers_are_clipped_on_a_char_boundary() {
        let questions = parse(r#"{"questions":[{"question":"A?"}]}"#).expect("valid");
        let paired = answers(&questions, &serde_json::json!({ "A?": "é".repeat(3000) }));
        assert_eq!(paired[0].answer.chars().count(), MAX_ANSWER_CHARS + 1);
        assert!(paired[0].answer.ends_with('…'));
    }

    #[test]
    fn unanswered_tells_the_model_not_to_guess() {
        let result: Value =
            serde_json::from_str(&unanswered_result("the turn was stopped")).expect("json");
        assert_eq!(result["status"], "not_answered");
        assert_eq!(result["reason"], "the turn was stopped");
        assert!(result["note"]
            .as_str()
            .unwrap_or_default()
            .contains("Do not assume"));
    }
}
