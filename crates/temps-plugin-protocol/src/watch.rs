// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bounded, scalar subset of JSON Schema supported by event routing.
//! The first filterable field is the resource the plugin authorizes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishedEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub title: String,
    pub attributes_schema: AttributesSchema,
    pub filterable: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttributesSchema {
    #[serde(rename = "type")]
    pub schema_type: ObjectType,
    pub properties: BTreeMap<String, Attribute>,
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ObjectType {
    Object,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    #[serde(rename = "type")]
    pub kind: ScalarType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScalarType {
    String,
    Integer,
    Boolean,
}

impl ScalarType {
    fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::String => value.as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= 160 && !s.chars().any(char::is_control)
            }),
            Self::Integer => value.as_i64().is_some(),
            Self::Boolean => value.is_boolean(),
        }
    }

    fn parse_subject(&self, subject: &str) -> Option<Value> {
        match self {
            Self::String => Some(Value::String(subject.to_owned())),
            Self::Integer => subject.parse::<i64>().ok().map(Value::from),
            Self::Boolean => subject.parse::<bool>().ok().map(Value::from),
        }
    }
}

impl PublishedEvent {
    pub fn is_valid(&self) -> bool {
        let valid_name = |s: &str| {
            !s.is_empty()
                && s.len() <= 80
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        };
        let fields = &self.attributes_schema;
        valid_name(&self.event_type)
            && !self.title.trim().is_empty()
            && self.title.len() <= 120
            && !self.filterable.is_empty()
            && fields.properties.len() <= 24
            && fields.properties.keys().all(|k| valid_name(k))
            && fields.required.len() <= fields.properties.len()
            && fields
                .required
                .iter()
                .all(|k| fields.properties.contains_key(k))
            && self.filterable.len() <= fields.properties.len()
            && self.filterable.iter().all(|k| fields.required.contains(k))
    }

    fn valid_values(&self, data: &Value) -> bool {
        data.as_object().is_some_and(|fields| {
            fields.iter().all(|(k, v)| {
                self.attributes_schema
                    .properties
                    .get(k)
                    .is_some_and(|field| field.kind.accepts(v))
            })
        })
    }

    /// Get the resource key only after checking the complete event or filter.
    pub fn subject(&self, data: &Value, filter: bool) -> Option<String> {
        if !self.valid_values(data) {
            return None;
        }
        if filter {
            if !data
                .as_object()?
                .keys()
                .all(|key| self.filterable.contains(key))
            {
                return None;
            }
        } else if !self
            .attributes_schema
            .required
            .iter()
            .all(|key| data.get(key).is_some())
        {
            return None;
        }
        let value = data.get(self.filterable.first()?)?;
        Some(
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        )
    }

    /// Allows a UI with a single resource input to use the same typed validation.
    pub fn filter_for_subject(&self, subject: &str) -> Option<Value> {
        let key = self.filterable.first()?;
        let value = self
            .attributes_schema
            .properties
            .get(key)?
            .kind
            .parse_subject(subject)?;
        Some(serde_json::json!({(key):value}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_are_typed_and_scoped_and_can_refine_a_resource() {
        let event: PublishedEvent = serde_json::from_value(json!({
            "type":"build.finished","title":"Build finished","filterable":["project","success"],
            "attributes_schema":{"type":"object","properties":{"project":{"type":"integer"},
            "success":{"type":"boolean"},"build":{"type":"string"}},"required":["project","success","build"]}
        })).unwrap();
        assert!(event.is_valid());
        let data = json!({"project":4,"success":false,"build":"b1"});
        assert_eq!(event.subject(&data, false).as_deref(), Some("4"));
        assert_eq!(event.subject(&data, true), None);
        assert_eq!(
            event
                .subject(&json!({"project":4,"success":false}), true)
                .as_deref(),
            Some("4")
        );
        assert_eq!(event.subject(&json!({"success":true}), true), None);
        assert_eq!(event.subject(&json!({"project":"4"}), true), None);
        assert_eq!(event.subject(&json!({"project":4}), false), None);
        assert_eq!(event.filter_for_subject("4"), Some(json!({"project":4})));
    }
}
