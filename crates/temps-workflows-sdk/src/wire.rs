// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub sequence: i32,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub attempts: i32,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub wake_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub protocol_version: u32,
    pub run_id: String,
    pub workflow: String,
    pub attempt: i32,
    pub lease: i64,
    pub now: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    pub input: Option<Value>,
    pub journal: Vec<Entry>,
    pub journal_next_sequence: Option<i32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub name: String,
    pub key: Option<String>,
    pub payload: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Child {
    pub workflow: String,
    pub input: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Write {
    #[serde(flatten)]
    pub entry: Entry,
    pub event: Option<Event>,
    pub child: Option<Child>,
    pub duration_ms: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Completed {
        output: Value,
    },
    Failed {
        error: String,
        sequence: Option<i32>,
    },
    Sleep {
        sequence: i32,
        until: DateTime<Utc>,
    },
    Retry {
        sequence: i32,
        at: DateTime<Utc>,
    },
    Wait {
        sequence: i32,
        event: String,
        key: Option<String>,
        timeout_at: Option<DateTime<Utc>>,
    },
    Incompatible {
        sequence: i32,
        expected: String,
        found: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub workflow: String,
    pub status: String,
    pub output: Option<Value>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartedRun {
    pub run: Run,
    pub created: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub struct InvocationKey {
    pub algorithm: String,
    pub public_key: String,
    pub project_id: i32,
    pub environment_id: i32,
}
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub project_id: i32,
    pub environment_id: i32,
}
