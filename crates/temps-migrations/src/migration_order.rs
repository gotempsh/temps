// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Works out the order `Migrator::migrations()` runs migrations in.
//!
//! Shared by `build.rs`, which generates the module declarations and the
//! migration list from it, and by this crate's unit tests.
//!
//! The order is the migration file names sorted, except for the migrations
//! named in `src/migration/order_exceptions.txt`. Each block there reads
//!
//! ```text
//! after m20260907_000001_add_mfa_pending_origin_to_sessions:
//!     m20260831_000001_ai_first_applications
//!     m20260901_000001_persist_ai_turn_state
//! ```
//!
//! and places the listed migrations, in that order, directly after the anchor.
//! It records history that a sorted list cannot express: two branches that
//! shipped migrations with overlapping stamps, where the one that landed
//! second still had to run second on databases that already applied the
//! first. Blocks are applied top to bottom, so an anchor can be a migration
//! an earlier block placed.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

/// The file in the migration directory that is the module itself.
pub const MODULE_FILE: &str = "mod.rs";
/// The file in the migration directory that lists the order exceptions.
pub const EXCEPTIONS_FILE: &str = "order_exceptions.txt";

/// Why the migration list could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderError {
    /// A file in the migration directory is not `mYYYYMMDD_NNNNNN_name.rs`.
    BadFileName { file_name: String },
    /// Two entries in the migration directory produce the same migration name.
    DuplicateMigration { name: String },
    /// A line of the exceptions file is neither a block header, a migration
    /// name inside a block, a comment nor blank.
    BadExceptionLine { line: usize, text: String },
    /// A migration name in the exceptions file has no migration file.
    UnknownMigration { line: usize, name: String },
    /// A migration is placed by more than one line of the exceptions file.
    PlacedTwice {
        name: String,
        first_line: usize,
        second_line: usize,
    },
    /// Two blocks of the exceptions file share an anchor.
    AnchorReused {
        anchor: String,
        first_line: usize,
        second_line: usize,
    },
    /// A block's anchor is placed by a later block, so it is not in the list
    /// yet when this block is applied.
    AnchorNotYetPlaced { line: usize, anchor: String },
    /// A block header lists no migrations.
    EmptyBlock { line: usize, anchor: String },
}

impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadFileName { file_name } => write!(
                f,
                "'{file_name}' in src/migration/ is not a migration file: name migrations \
                 mYYYYMMDD_NNNNNN_description.rs (lowercase letters, digits and '_'), and keep \
                 every other file out of that directory"
            ),
            Self::DuplicateMigration { name } => write!(
                f,
                "migration '{name}' appears more than once in src/migration/; every migration \
                 name is recorded in seaql_migrations and must be unique"
            ),
            Self::BadExceptionLine { line, text } => write!(
                f,
                "{EXCEPTIONS_FILE}:{line}: expected 'after <migration>:', an indented migration \
                 name inside a block, a '#' comment or a blank line, found '{text}'"
            ),
            Self::UnknownMigration { line, name } => write!(
                f,
                "{EXCEPTIONS_FILE}:{line}: no migration file src/migration/{name}.rs"
            ),
            Self::PlacedTwice {
                name,
                first_line,
                second_line,
            } => write!(
                f,
                "{EXCEPTIONS_FILE}:{second_line}: '{name}' is already placed on line {first_line}"
            ),
            Self::AnchorReused {
                anchor,
                first_line,
                second_line,
            } => write!(
                f,
                "{EXCEPTIONS_FILE}:{second_line}: the block on line {first_line} already follows \
                 '{anchor}'; merge the two blocks"
            ),
            Self::AnchorNotYetPlaced { line, anchor } => write!(
                f,
                "{EXCEPTIONS_FILE}:{line}: anchor '{anchor}' is placed by a later block; move \
                 this block below that one"
            ),
            Self::EmptyBlock { line, anchor } => write!(
                f,
                "{EXCEPTIONS_FILE}:{line}: the block after '{anchor}' lists no migrations"
            ),
        }
    }
}

impl std::error::Error for OrderError {}

/// One `after <anchor>:` block of the exceptions file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub anchor: String,
    /// Line of the `after` header, 1-based.
    pub line: usize,
    /// The migrations placed after the anchor, in order, with their lines.
    pub migrations: Vec<(String, usize)>,
}

fn is_migration_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 17
        && bytes[0] == b'm'
        && bytes[1..9].iter().all(u8::is_ascii_digit)
        && bytes[9] == b'_'
        && bytes[10..16].iter().all(u8::is_ascii_digit)
        && bytes[16] == b'_'
        && bytes[17..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// The migration name of a migration directory entry, or `None` for the
/// module file, the exceptions file and hidden files (editor and file manager
/// metadata). `is_file` is false for directories.
pub fn migration_name(file_name: &str, is_file: bool) -> Result<Option<String>, OrderError> {
    if file_name == MODULE_FILE || file_name == EXCEPTIONS_FILE || file_name.starts_with('.') {
        return Ok(None);
    }
    match file_name.strip_suffix(".rs") {
        Some(name) if is_file && is_migration_name(name) => Ok(Some(name.to_string())),
        _ => Err(OrderError::BadFileName {
            file_name: file_name.to_string(),
        }),
    }
}

/// Parses the exceptions file into its blocks, in file order.
pub fn parse_exceptions(text: &str) -> Result<Vec<Placement>, OrderError> {
    let mut placements: Vec<Placement> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let bad_line = || OrderError::BadExceptionLine {
            line,
            text: trimmed.to_string(),
        };
        if let Some(header) = trimmed.strip_prefix("after ") {
            if raw.starts_with(char::is_whitespace) {
                return Err(bad_line());
            }
            let anchor = header.strip_suffix(':').ok_or_else(bad_line)?.trim();
            if !is_migration_name(anchor) {
                return Err(bad_line());
            }
            placements.push(Placement {
                anchor: anchor.to_string(),
                line,
                migrations: Vec::new(),
            });
        } else if raw.starts_with(char::is_whitespace) && is_migration_name(trimmed) {
            placements
                .last_mut()
                .ok_or_else(bad_line)?
                .migrations
                .push((trimmed.to_string(), line));
        } else {
            return Err(bad_line());
        }
    }
    if let Some(empty) = placements.iter().find(|p| p.migrations.is_empty()) {
        return Err(OrderError::EmptyBlock {
            line: empty.line,
            anchor: empty.anchor.clone(),
        });
    }
    Ok(placements)
}

/// The migrations in the order they run: `migrations` sorted, then every
/// block of `placements` applied top to bottom.
pub fn order(migrations: Vec<String>, placements: &[Placement]) -> Result<Vec<String>, OrderError> {
    let mut known = BTreeSet::new();
    for name in migrations {
        if !known.insert(name.clone()) {
            return Err(OrderError::DuplicateMigration { name });
        }
    }

    let mut placed_on = HashMap::new();
    let mut anchored_on = HashMap::new();
    for placement in placements {
        if !known.contains(&placement.anchor) {
            return Err(OrderError::UnknownMigration {
                line: placement.line,
                name: placement.anchor.clone(),
            });
        }
        if let Some(first_line) = anchored_on.insert(&placement.anchor, placement.line) {
            return Err(OrderError::AnchorReused {
                anchor: placement.anchor.clone(),
                first_line,
                second_line: placement.line,
            });
        }
        for (name, line) in &placement.migrations {
            if !known.contains(name) {
                return Err(OrderError::UnknownMigration {
                    line: *line,
                    name: name.clone(),
                });
            }
            if let Some(first_line) = placed_on.insert(name, *line) {
                return Err(OrderError::PlacedTwice {
                    name: name.clone(),
                    first_line,
                    second_line: *line,
                });
            }
        }
    }

    let mut ordered: Vec<String> = known
        .iter()
        .filter(|name| !placed_on.contains_key(name))
        .cloned()
        .collect();
    for placement in placements {
        let position = ordered
            .iter()
            .position(|name| *name == placement.anchor)
            .ok_or_else(|| OrderError::AnchorNotYetPlaced {
                line: placement.line,
                anchor: placement.anchor.clone(),
            })?;
        let names = placement.migrations.iter().map(|(name, _)| name.clone());
        ordered.splice(position + 1..position + 1, names);
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    const A: &str = "m20260101_000001_a";
    const B: &str = "m20260102_000001_b";
    const C: &str = "m20260103_000001_c";
    const D: &str = "m20260104_000001_d";

    #[test]
    fn without_exceptions_the_order_is_sorted_file_names() {
        let ordered = order(names(&[C, A, D, B]), &[]).unwrap();
        assert_eq!(ordered, names(&[A, B, C, D]));
    }

    #[test]
    fn a_block_places_its_migrations_right_after_the_anchor() {
        let placements =
            parse_exceptions(&format!("# why\nafter {C}:\n    {A}\n    {B}\n")).unwrap();
        let ordered = order(names(&[A, B, C, D]), &placements).unwrap();
        assert_eq!(ordered, names(&[C, A, B, D]));
    }

    #[test]
    fn a_later_block_can_anchor_on_a_migration_an_earlier_block_placed() {
        let text = format!("after {D}:\n    {A}\n\nafter {A}:\n    {B}\n");
        let ordered = order(names(&[A, B, C, D]), &parse_exceptions(&text).unwrap()).unwrap();
        assert_eq!(ordered, names(&[C, D, A, B]));
    }

    #[test]
    fn an_anchor_placed_by_a_later_block_is_rejected() {
        let text = format!("after {A}:\n    {B}\nafter {D}:\n    {A}\n");
        let error = order(names(&[A, B, C, D]), &parse_exceptions(&text).unwrap()).unwrap_err();
        assert_eq!(
            error,
            OrderError::AnchorNotYetPlaced {
                line: 1,
                anchor: A.to_string()
            }
        );
        assert!(error.to_string().contains("order_exceptions.txt:1"));
    }

    #[test]
    fn duplicate_migration_names_are_rejected() {
        let error = order(names(&[A, B, A]), &[]).unwrap_err();
        assert_eq!(
            error,
            OrderError::DuplicateMigration {
                name: A.to_string()
            }
        );
        assert!(error.to_string().contains(A));
    }

    #[test]
    fn placing_a_migration_twice_is_rejected() {
        let text = format!("after {C}:\n    {A}\nafter {D}:\n    {A}\n");
        let error = order(names(&[A, C, D]), &parse_exceptions(&text).unwrap()).unwrap_err();
        assert_eq!(
            error,
            OrderError::PlacedTwice {
                name: A.to_string(),
                first_line: 2,
                second_line: 4
            }
        );
    }

    #[test]
    fn reusing_an_anchor_is_rejected() {
        let text = format!("after {D}:\n    {A}\nafter {D}:\n    {B}\n");
        let error = order(names(&[A, B, D]), &parse_exceptions(&text).unwrap()).unwrap_err();
        assert_eq!(
            error,
            OrderError::AnchorReused {
                anchor: D.to_string(),
                first_line: 1,
                second_line: 3
            }
        );
    }

    #[test]
    fn an_exception_naming_a_missing_file_is_rejected() {
        let text = format!("after {D}:\n    {A}\n");
        let error = order(names(&[A, B]), &parse_exceptions(&text).unwrap()).unwrap_err();
        assert_eq!(
            error,
            OrderError::UnknownMigration {
                line: 1,
                name: D.to_string()
            }
        );
        let text = format!("after {A}:\n    {D}\n");
        let error = order(names(&[A, B]), &parse_exceptions(&text).unwrap()).unwrap_err();
        assert_eq!(
            error,
            OrderError::UnknownMigration {
                line: 2,
                name: D.to_string()
            }
        );
    }

    #[test]
    fn malformed_exception_lines_are_rejected_with_their_line() {
        for (text, line) in [
            (format!("    {A}\n"), 1),
            (format!("after {A}\n    {B}\n"), 1),
            (format!("after {A}:\n{B}\n"), 2),
            (format!("after {A}:\n    not_a_migration\n"), 2),
            (format!("  after {A}:\n    {B}\n"), 1),
        ] {
            let error = parse_exceptions(&text).unwrap_err();
            assert!(
                matches!(error, OrderError::BadExceptionLine { line: l, .. } if l == line),
                "{text:?} gave {error:?}"
            );
        }
        assert_eq!(
            parse_exceptions(&format!("after {A}:\n\n# nothing\n")).unwrap_err(),
            OrderError::EmptyBlock {
                line: 1,
                anchor: A.to_string()
            }
        );
    }

    #[test]
    fn migration_names_come_from_well_formed_rs_files_only() {
        assert_eq!(
            migration_name("m20260101_000001_add_x2.rs", true).unwrap(),
            Some("m20260101_000001_add_x2".to_string())
        );
        assert_eq!(migration_name(MODULE_FILE, true).unwrap(), None);
        assert_eq!(migration_name(EXCEPTIONS_FILE, true).unwrap(), None);
        assert_eq!(migration_name(".DS_Store", true).unwrap(), None);
        for bad in [
            "m20260101_000001_add_x.txt",
            "m2026010_000001_add_x.rs",
            "m20260101_00001_add_x.rs",
            "m20260101_000001_.rs",
            "m20260101_000001_Add_x.rs",
            "m20260101_000001_add-x.rs",
            "helpers.rs",
        ] {
            assert_eq!(
                migration_name(bad, true).unwrap_err(),
                OrderError::BadFileName {
                    file_name: bad.to_string()
                },
                "{bad}"
            );
        }
        assert!(migration_name("m20260101_000001_add_x.rs", false).is_err());
    }
}
