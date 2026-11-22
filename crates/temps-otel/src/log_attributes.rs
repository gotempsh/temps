// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire-independent log attribute types and query predicates. Values remain in
//! the existing string map, retaining exact integer/decimal text. The type map
//! distinguishes a string "7" from the integer 7 without duplicating values.

use crate::error::OtelError;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogAttributeType {
    String,
    Boolean,
    Integer,
    Double,
    Array,
    Object,
    Bytes,
}
impl LogAttributeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Double => "double",
            Self::Array => "array",
            Self::Object => "object",
            Self::Bytes => "bytes",
        }
    }
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Integer | Self::Double)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogAttributeOperator {
    #[default]
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}
impl LogAttributeOperator {
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::Gt => ">",
            Self::Gte => ">=",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogAttributeFilter {
    pub key: String,
    /// Omit to compare the existing string representation, including old rows
    /// whose producer did not supply native type metadata.
    pub ty: Option<LogAttributeType>,
    #[serde(default)]
    pub operator: LogAttributeOperator,
    pub value: String,
}

impl LogAttributeFilter {
    pub fn validate(&self) -> Result<(), OtelError> {
        let invalid = || {
            OtelError::Validation { message:"Log attribute filters require a key of 1–128 bytes, a value of at most 16 KiB, and a compatible type/operator/value".into() }
        };
        if self.key.is_empty()
            || self.key.len() > 128
            || self.key.contains('\0')
            || self.value.len() > 16384
            || self.value.contains('\0')
        {
            return Err(invalid());
        }
        let equality = matches!(
            self.operator,
            LogAttributeOperator::Eq | LogAttributeOperator::Ne
        );
        if !equality && !self.ty.is_some_and(LogAttributeType::is_numeric) {
            return Err(invalid());
        }
        match self.ty {
            Some(LogAttributeType::Integer) if self.value.parse::<i64>().is_err() => Err(invalid()),
            Some(LogAttributeType::Double)
                if self
                    .value
                    .parse::<f64>()
                    .map_or(true, |value| !value.is_finite()) =>
            {
                Err(invalid())
            }
            Some(LogAttributeType::Boolean) if !matches!(self.value.as_str(), "true" | "false") => {
                Err(invalid())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_filters_keep_int64_precision_and_reject_invalid_comparisons() {
        let filter = LogAttributeFilter {
            key: "count".into(),
            ty: Some(LogAttributeType::Integer),
            operator: LogAttributeOperator::Gt,
            value: "9007199254740993".into(),
        };
        assert!(filter.validate().is_ok());
        assert_eq!(filter.value, "9007199254740993");
        for (ty, value, op) in [
            (
                LogAttributeType::Integer,
                "9223372036854775808",
                LogAttributeOperator::Eq,
            ),
            (LogAttributeType::Double, "NaN", LogAttributeOperator::Gt),
            (LogAttributeType::Boolean, "1", LogAttributeOperator::Eq),
            (LogAttributeType::String, "7", LogAttributeOperator::Gt),
        ] {
            assert!(LogAttributeFilter {
                ty: Some(ty),
                value: value.into(),
                operator: op,
                ..filter.clone()
            }
            .validate()
            .is_err());
        }
    }
}
