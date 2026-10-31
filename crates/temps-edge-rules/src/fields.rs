// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-field rules: which operators a field accepts, how its values are
//! written, and how it reads in plain English. Mirrors
//! `web/src/components/project/settings/edge-rules/fields.ts`.

use crate::model::{Field, Operator};

/// How a field's values are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Text,
    Cidr,
    Number,
    Options,
    Boolean,
}

/// Fields that test a named item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Header,
    QueryParameter,
    Cookie,
}

impl KeyKind {
    pub fn label(self) -> &'static str {
        match self {
            KeyKind::Header => "header",
            KeyKind::QueryParameter => "query parameter",
            KeyKind::Cookie => "cookie",
        }
    }
}

pub struct FieldSpec {
    pub label: &'static str,
    pub operators: &'static [Operator],
    pub kind: ValueKind,
    pub keyed: Option<KeyKind>,
    /// Accepts the per-condition `case_insensitive` flag.
    pub case_flag: bool,
    /// Allowed values and their labels, for `Options` fields.
    pub options: &'static [(&'static str, &'static str)],
    /// Needs the GeoIP database.
    pub geo: bool,
}

use Operator::*;

const STRING_OPS: &[Operator] = &[StartsWith, Eq, In, EndsWith, Contains, Glob, Regex];
const KEYED_OPS: &[Operator] = &[Exists, NotExists, Eq, In, Contains, Glob, Regex];

pub const METHODS: &[(&str, &str)] = &[
    ("GET", "GET"),
    ("HEAD", "HEAD"),
    ("POST", "POST"),
    ("PUT", "PUT"),
    ("PATCH", "PATCH"),
    ("DELETE", "DELETE"),
    ("OPTIONS", "OPTIONS"),
];

pub const CONTINENTS: &[(&str, &str)] = &[
    ("AF", "Africa"),
    ("AN", "Antarctica"),
    ("AS", "Asia"),
    ("EU", "Europe"),
    ("NA", "North America"),
    ("OC", "Oceania"),
    ("SA", "South America"),
];

pub const BOT_CLASSES: &[(&str, &str)] = &[
    ("human", "Human"),
    ("search_engine", "Search engine"),
    ("ai_crawler", "AI crawler"),
    ("ai_agent", "AI agent"),
    ("unknown_bot", "Unknown bot"),
];

pub fn spec(field: Field) -> FieldSpec {
    let base = FieldSpec {
        label: "",
        operators: STRING_OPS,
        kind: ValueKind::Text,
        keyed: None,
        case_flag: false,
        options: &[],
        geo: false,
    };
    match field {
        Field::Path => FieldSpec {
            label: "Path",
            case_flag: true,
            ..base
        },
        Field::RawPath => FieldSpec {
            label: "Raw path",
            operators: &[Contains, Glob, Regex],
            case_flag: true,
            ..base
        },
        Field::Host => FieldSpec {
            label: "Host",
            operators: &[Eq, In, EndsWith, Glob],
            ..base
        },
        Field::Method => FieldSpec {
            label: "Method",
            operators: &[Eq, In],
            kind: ValueKind::Options,
            options: METHODS,
            ..base
        },
        Field::Ip => FieldSpec {
            label: "Client IP",
            operators: &[InCidr],
            kind: ValueKind::Cidr,
            ..base
        },
        Field::Country => FieldSpec {
            label: "Country",
            operators: &[Eq, In],
            geo: true,
            ..base
        },
        Field::Continent => FieldSpec {
            label: "Continent",
            operators: &[Eq, In],
            kind: ValueKind::Options,
            options: CONTINENTS,
            geo: true,
            ..base
        },
        Field::Asn => FieldSpec {
            label: "Network (ASN)",
            operators: &[Eq, In],
            kind: ValueKind::Number,
            geo: true,
            ..base
        },
        Field::IsHostingProvider => FieldSpec {
            label: "Hosting provider network",
            operators: &[Eq],
            kind: ValueKind::Boolean,
            geo: true,
            ..base
        },
        Field::Header => FieldSpec {
            label: "Header",
            operators: KEYED_OPS,
            keyed: Some(KeyKind::Header),
            case_flag: true,
            ..base
        },
        Field::Query => FieldSpec {
            label: "Query parameter",
            operators: &[Exists, NotExists, Eq, Contains, Glob, Regex],
            keyed: Some(KeyKind::QueryParameter),
            case_flag: true,
            ..base
        },
        Field::Cookie => FieldSpec {
            label: "Cookie",
            operators: &[Exists, NotExists, Eq],
            keyed: Some(KeyKind::Cookie),
            ..base
        },
        Field::UserAgent => FieldSpec {
            label: "User agent",
            operators: &[Contains, Glob, Regex],
            case_flag: true,
            ..base
        },
        Field::Ja4 => FieldSpec {
            label: "TLS fingerprint (JA4)",
            operators: &[Eq, In],
            ..base
        },
        Field::BotClass => FieldSpec {
            label: "Bot class",
            operators: &[Eq, In],
            kind: ValueKind::Options,
            options: BOT_CLASSES,
            ..base
        },
    }
}

/// Hosts, methods, countries and enumerated fields always compare without
/// case; free-text fields honour the condition's flag.
pub fn always_case_insensitive(field: Field) -> bool {
    matches!(field, Field::Host | Field::Method | Field::Country)
        || spec(field).kind == ValueKind::Options
}

pub fn operator_label(op: Operator) -> &'static str {
    match op {
        Eq => "is",
        In => "is any of",
        InCidr => "is in",
        StartsWith => "starts with",
        EndsWith => "ends with",
        Contains => "contains",
        Glob => "matches pattern",
        Regex => "matches regex",
        Exists => "exists",
        NotExists => "does not exist",
    }
}

/// Headers a client can set to anything unless a trusted proxy rewrites them.
pub const SPOOFABLE_HEADERS: &[&str] = &[
    "x-forwarded-for",
    "x-real-ip",
    "forwarded",
    "cf-connecting-ip",
    "true-client-ip",
    "x-client-ip",
];

/// Limits from ADR-071 §1. The console enforces the same values.
pub mod limits {
    pub const RULES_PER_PROJECT: usize = 100;
    pub const CONDITIONS_PER_RULE: usize = 32;
    pub const DEPTH: usize = 4;
    pub const LIST_VALUES: usize = 64;
    pub const CIDR_VALUES: usize = 1000;
    pub const VALUE_BYTES: usize = 256;
    pub const GLOB_WILDCARDS: usize = 8;
    pub const NAME_LENGTH: usize = 80;
    pub const BLOCK_BODY_BYTES: usize = 1024;
    pub const NODE_ID_LENGTH: usize = 64;
    /// Compiled size limit for one regex (and for its lazy DFA cache).
    pub const REGEX_SIZE: usize = 256 * 1024;
    /// `regex` conditions across all of a project's rules, each up to
    /// [`REGEX_SIZE`] compiled: 8 MiB in the worst case.
    pub const REGEX_CONDITIONS_PER_PROJECT: usize = 32;
    /// Conditions that compile to an automaton across all of a project's
    /// rules: every `regex` and `glob`, and `starts_with`, `ends_with` or
    /// `contains` ignoring case (see `eval::compiles_to_pattern`). Those
    /// built from a literal compile to at most about 64 KiB (a test pins
    /// it), so with the regex cap a project's patterns stay under about
    /// 22 MiB in the worst case. A path condition like `/wp-admin` compiles
    /// to a few KiB, so real rule sets use well under 1 MiB.
    pub const PATTERN_CONDITIONS_PER_PROJECT: usize = 256;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_has_operators_and_a_label() {
        for field in [
            Field::Ip,
            Field::Country,
            Field::Continent,
            Field::Asn,
            Field::IsHostingProvider,
            Field::Host,
            Field::Path,
            Field::RawPath,
            Field::Method,
            Field::Query,
            Field::Header,
            Field::Cookie,
            Field::UserAgent,
            Field::Ja4,
            Field::BotClass,
        ] {
            let s = spec(field);
            assert!(!s.label.is_empty(), "{field:?} has no label");
            assert!(!s.operators.is_empty(), "{field:?} has no operators");
            if s.kind == ValueKind::Options {
                assert!(!s.options.is_empty(), "{field:?} has no options");
            }
        }
    }

    #[test]
    fn enumerated_fields_ignore_case() {
        assert!(always_case_insensitive(Field::Method));
        assert!(always_case_insensitive(Field::BotClass));
        assert!(always_case_insensitive(Field::Host));
        assert!(!always_case_insensitive(Field::Path));
        assert!(!always_case_insensitive(Field::Header));
    }
}
