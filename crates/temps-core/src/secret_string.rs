// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A string a request carries that must never be shown, logged or echoed.
//!
//! [`SecretString`] marks a request field as holding a credential (an access
//! key, a password, a token). It does three things:
//!
//! * Its [`Debug`] and [`Serialize`] output is a fixed mask, so a request DTO
//!   that is logged, audited or returned in a response cannot leak the value
//!   by accident. Reading the value takes an explicit [`SecretString::expose`].
//! * It deserializes exactly like a [`String`], so a direct API caller sees no
//!   difference.
//! * Its OpenAPI schema is `type: string, format: password` with the
//!   [`SECRET_SCHEMA_EXTENSION`] extension, which is how tooling that sits in
//!   front of the API (the AI assistant's tool layer) learns which fields take
//!   a credential without a hand-kept list.
//!
//! # Serialization is lossy on purpose
//!
//! `Serialize` writes [`SECRET_MASK`], not the value, matching the rule that a
//! stored credential is masked whenever it is read back. A DTO that must carry
//! the real value across a boundary (a queue message, a database row) has to
//! call [`SecretString::expose`] and handle it deliberately; it cannot do so by
//! deriving `Serialize` and forgetting.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::{
    extensions::ExtensionsBuilder,
    schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type},
    RefOr,
};
use utoipa::{PartialSchema, ToSchema};

/// What [`SecretString`] serializes and debug-prints as.
pub const SECRET_MASK: &str = "***";

/// OpenAPI extension set on the schema of every [`SecretString`] field.
pub const SECRET_SCHEMA_EXTENSION: &str = "x-temps-secret";

/// A credential carried in a request body. See the [module docs](self).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Every call site is a place the secret can escape, so
    /// keep the borrow short and never pass it to a logger or an error.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the raw value.
    pub fn into_exposed(self) -> String {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Not even the length: it narrows a guess.
        f.write_str("SecretString(***)")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(SECRET_MASK)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl PartialSchema for SecretString {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Password)))
            .description(Some(
                "A credential. Write-only: it is never returned, and the AI assistant supplies it \
                 through a secret slot instead of typing it into the conversation.",
            ))
            .extensions(Some(
                ExtensionsBuilder::new()
                    .add(SECRET_SCHEMA_EXTENSION, true)
                    .build(),
            ))
            .into()
    }
}

impl ToSchema for SecretString {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const VALUE: &str = "AKIA-example-not-a-real-key";

    #[test]
    fn deserializes_like_a_plain_string() {
        let secret: SecretString = serde_json::from_value(json!(VALUE)).expect("a string parses");
        assert_eq!(secret.expose(), VALUE);
    }

    #[test]
    fn rejects_non_strings_instead_of_coercing_them() {
        assert!(serde_json::from_value::<SecretString>(json!(12345)).is_err());
        assert!(serde_json::from_value::<SecretString>(json!(null)).is_err());
        assert!(serde_json::from_value::<SecretString>(json!({"a": 1})).is_err());
    }

    #[test]
    fn serializes_as_the_mask_never_the_value() {
        let rendered = serde_json::to_string(&SecretString::new(VALUE)).expect("serializes");
        assert_eq!(rendered, format!("\"{SECRET_MASK}\""));
        assert!(!rendered.contains(VALUE));
    }

    #[test]
    fn debug_hides_the_value_and_its_length() {
        let rendered = format!("{:?}", SecretString::new(VALUE));
        assert!(!rendered.contains(VALUE));
        assert_eq!(rendered, format!("{:?}", SecretString::new("x")));
    }

    #[test]
    fn a_dto_holding_one_cannot_leak_it_through_debug_or_serialize() {
        #[derive(Debug, Serialize, Deserialize)]
        struct CreateThing {
            name: String,
            secret_key: SecretString,
        }

        let dto: CreateThing =
            serde_json::from_value(json!({"name": "n", "secret_key": VALUE})).expect("parses");
        assert_eq!(dto.secret_key.expose(), VALUE);
        assert!(!format!("{dto:?}").contains(VALUE));
        assert!(!serde_json::to_string(&dto)
            .expect("serializes")
            .contains(VALUE));
    }

    #[test]
    fn schema_is_a_password_string_flagged_for_tooling() {
        let schema = serde_json::to_value(SecretString::schema()).expect("schema serializes");
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["format"], "password");
        assert_eq!(schema[SECRET_SCHEMA_EXTENSION], true);
    }

    #[test]
    fn a_field_of_this_type_lands_in_the_spec_as_a_flagged_reference() {
        #[derive(Serialize, Deserialize, ToSchema)]
        struct Body {
            secret_key: SecretString,
        }

        let mut schemas = Vec::new();
        <Body as ToSchema>::schemas(&mut schemas);
        let body = serde_json::to_value(<Body as PartialSchema>::schema()).expect("serializes");
        assert_eq!(
            body["properties"]["secret_key"]["$ref"], "#/components/schemas/SecretString",
            "the field must reference the shared flagged schema"
        );
        let collected: Vec<_> = schemas.iter().map(|(name, _)| name.as_str()).collect();
        assert!(
            collected.contains(&"SecretString"),
            "the shared schema must be registered so the reference resolves: {collected:?}"
        );
        let (_, shared) = schemas
            .iter()
            .find(|(name, _)| name == "SecretString")
            .expect("checked above");
        let shared = serde_json::to_value(shared).expect("serializes");
        assert_eq!(shared[SECRET_SCHEMA_EXTENSION], true);
    }
}
