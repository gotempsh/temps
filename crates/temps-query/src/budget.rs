// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{DataError, DataRow, QueryBudget, Result};

/// Incremental result collector shared by every query backend.
///
/// Backends must feed rows into this collector as their driver yields them;
/// collecting driver rows into an intermediate `Vec` defeats the memory bound.
pub struct BoundedRows {
    budget: QueryBudget,
    rows: Vec<DataRow>,
    used_bytes: usize,
    used_cells: usize,
    truncated: bool,
}

impl BoundedRows {
    pub fn new(budget: QueryBudget) -> Self {
        Self {
            budget,
            rows: Vec::new(),
            used_bytes: 0,
            used_cells: 0,
            truncated: false,
        }
    }

    /// Add one decoded row. Returns `false` when the page-level byte/cell
    /// budget is full and the backend should stop reading its cursor.
    /// Oversized individual rows/cells are rejected, including the first row.
    pub fn push(&mut self, entity: &str, row: DataRow) -> Result<bool> {
        if row.len() > self.budget.max_cells_per_row {
            return Err(limit_error(
                entity,
                "cells_per_row",
                self.budget.max_cells_per_row,
                row.len(),
            ));
        }

        let mut row_bytes = 2usize;
        let mut row_elements = 0usize;
        for (name, value) in &row {
            let stats = value_stats(value, 0, self.budget.max_nesting_depth).ok_or_else(|| {
                limit_error(
                    entity,
                    "nesting_depth",
                    self.budget.max_nesting_depth,
                    self.budget.max_nesting_depth.saturating_add(1),
                )
            })?;
            if stats.bytes > self.budget.max_cell_bytes {
                return Err(limit_error(
                    entity,
                    "cell_bytes",
                    self.budget.max_cell_bytes,
                    stats.bytes,
                ));
            }
            row_elements = row_elements.saturating_add(stats.elements);
            row_bytes = row_bytes
                .saturating_add(json_string_bytes(name))
                .saturating_add(stats.bytes)
                .saturating_add(2);
        }
        if row_elements > self.budget.max_value_elements_per_row {
            return Err(limit_error(
                entity,
                "value_elements_per_row",
                self.budget.max_value_elements_per_row,
                row_elements,
            ));
        }

        let next_bytes = self.used_bytes.saturating_add(row_bytes);
        let next_cells = self.used_cells.saturating_add(row.len());
        if next_bytes > self.budget.max_bytes || next_cells > self.budget.max_cells {
            if self.rows.is_empty() {
                let (kind, limit, observed) = if next_bytes > self.budget.max_bytes {
                    ("response_bytes", self.budget.max_bytes, next_bytes)
                } else {
                    ("response_cells", self.budget.max_cells, next_cells)
                };
                return Err(limit_error(entity, kind, limit, observed));
            }
            self.truncated = true;
            return Ok(false);
        }

        self.used_bytes = next_bytes;
        self.used_cells = next_cells;
        self.rows.push(row);
        Ok(true)
    }

    pub fn into_parts(self) -> (Vec<DataRow>, bool) {
        (self.rows, self.truncated)
    }
}

fn limit_error(entity: &str, limit_kind: &'static str, limit: usize, observed: usize) -> DataError {
    DataError::ResultLimitExceeded {
        entity: entity.to_string(),
        limit_kind,
        limit,
        observed,
    }
}

#[derive(Clone, Copy)]
struct ValueStats {
    bytes: usize,
    elements: usize,
}

fn value_stats(value: &serde_json::Value, depth: usize, max_depth: usize) -> Option<ValueStats> {
    if depth > max_depth {
        return None;
    }
    let stats = match value {
        serde_json::Value::Null => ValueStats {
            bytes: 4,
            elements: 1,
        },
        serde_json::Value::Bool(_) => ValueStats {
            bytes: 5,
            elements: 1,
        },
        serde_json::Value::Number(number) => ValueStats {
            bytes: number.to_string().len(),
            elements: 1,
        },
        serde_json::Value::String(value) => ValueStats {
            bytes: json_string_bytes(value),
            elements: 1,
        },
        serde_json::Value::Array(values) => {
            let mut bytes = 2usize;
            let mut elements = values.len();
            for value in values {
                let child = value_stats(value, depth + 1, max_depth)?;
                bytes = bytes.saturating_add(child.bytes).saturating_add(1);
                elements = elements.saturating_add(child.elements);
            }
            ValueStats { bytes, elements }
        }
        serde_json::Value::Object(values) => {
            let mut bytes = 2usize;
            let mut elements = values.len();
            for (name, value) in values {
                let child = value_stats(value, depth + 1, max_depth)?;
                bytes = bytes
                    .saturating_add(json_string_bytes(name))
                    .saturating_add(child.bytes)
                    .saturating_add(2);
                elements = elements.saturating_add(child.elements);
            }
            ValueStats { bytes, elements }
        }
    };
    Some(stats)
}

/// Exact UTF-8 byte length of serde_json's compact string representation,
/// without allocating the encoded string. This accounts for control-character,
/// quote and backslash expansion so the response budget cannot be bypassed by
/// strings whose encoded form is much larger than their source bytes.
fn json_string_bytes(value: &str) -> usize {
    value.chars().fold(2usize, |bytes, character| {
        let encoded = match character {
            '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        };
        bytes.saturating_add(encoded)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(max_bytes: usize) -> QueryBudget {
        QueryBudget {
            max_bytes,
            max_cells: 16,
            max_cells_per_row: 8,
            max_cell_bytes: max_bytes,
            max_value_elements_per_row: 16,
            max_nesting_depth: 8,
        }
    }

    fn row(value: serde_json::Value) -> DataRow {
        [("value".to_string(), value)].into_iter().collect()
    }

    #[test]
    fn oversized_first_row_is_rejected() {
        let mut rows = BoundedRows::new(budget(64));
        let error = rows
            .push("users", row(serde_json::json!("x".repeat(1_024))))
            .expect_err("oversized first row must never be emitted");
        assert!(matches!(
            error,
            DataError::ResultLimitExceeded {
                entity,
                limit_kind: "cell_bytes",
                ..
            } if entity == "users"
        ));
    }

    #[test]
    fn later_row_over_page_budget_truncates_without_adding_it() {
        let mut rows = BoundedRows::new(budget(80));
        assert!(rows.push("users", row(serde_json::json!("small"))).unwrap());
        assert!(!rows
            .push("users", row(serde_json::json!("y".repeat(70))))
            .unwrap());
        let (rows, truncated) = rows.into_parts();
        assert_eq!(rows.len(), 1);
        assert!(truncated);
    }

    #[test]
    fn aggregate_element_limit_is_enforced() {
        let mut rows = BoundedRows::new(budget(4_096));
        let error = rows
            .push("cache", row(serde_json::json!(vec![1; 32])))
            .expect_err("aggregate element count must be bounded");
        assert!(matches!(
            error,
            DataError::ResultLimitExceeded {
                limit_kind: "value_elements_per_row",
                ..
            }
        ));
    }

    #[test]
    fn escaped_json_bytes_count_toward_the_budget() {
        let mut rows = BoundedRows::new(budget(40));
        let error = rows
            .push("events", row(serde_json::json!("\u{0000}".repeat(10))))
            .expect_err("encoded control characters must count as six bytes each");
        assert!(matches!(
            error,
            DataError::ResultLimitExceeded {
                limit_kind: "cell_bytes",
                ..
            }
        ));
    }
}
