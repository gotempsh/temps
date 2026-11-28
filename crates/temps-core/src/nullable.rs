// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A request field where "absent" and `null` mean different things.
//!
//! Serde reads both a missing field and an explicit `null` into `None` for an
//! `Option<Option<T>>`, so an update cannot tell "leave it" from "clear it"
//! and a client asking to clear a value sees it kept. Declare the field
//!
//! ```ignore
//! #[serde(default, deserialize_with = "temps_core::nullable::deserialize")]
//! #[schema(value_type = Option<String>, nullable)]
//! pub description: Option<Option<String>>,
//! ```
//!
//! and it reads as:
//!
//! | JSON              | value              | meaning        |
//! |-------------------|--------------------|----------------|
//! | field absent      | `None`             | keep it        |
//! | `"field": null`   | `Some(None)`       | clear it       |
//! | `"field": value`  | `Some(Some(value))`| set it         |

use serde::{Deserialize, Deserializer};

/// `deserialize_with` target for a nullable update field; see the module
/// documentation. Pair it with `#[serde(default)]`, which is what makes an
/// absent field `None`.
pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    #[derive(serde::Deserialize, Debug)]
    struct Update {
        #[serde(default, deserialize_with = "super::deserialize")]
        description: Option<Option<String>>,
    }

    fn parse(json: &str) -> Option<Option<String>> {
        serde_json::from_str::<Update>(json)
            .expect("valid update")
            .description
    }

    #[test]
    fn absent_null_and_a_value_are_three_different_answers() {
        assert_eq!(parse("{}"), None);
        assert_eq!(parse(r#"{"description":null}"#), Some(None));
        assert_eq!(
            parse(r#"{"description":"text"}"#),
            Some(Some("text".to_string()))
        );
    }
}
