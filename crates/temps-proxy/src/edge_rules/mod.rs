// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project edge rules (ADR-071) in the proxy: the gate that keeps every
//! project's compiled rules in memory, and the request adapter the engine
//! reads.
//!
//! Management (validation, persistence, the dry run) is
//! [`crate::service::edge_rule_service`]; it never talks to the gate. The gate
//! notices writes through the change feed in `project_edge_rule_versions`.

mod gate;
mod request;

pub use gate::{EdgeRuleGate, EdgeRuleGateError, ProjectRules, HIT_FLUSH_INTERVAL, POLL_INTERVAL};
pub use request::{classify_bot, GeoSnapshot, ProxyRequestView};

use temps_edge_rules::{Action, EdgeRuleError, Group, Mode, RuleDefinition};
use temps_entities::project_edge_rules;

/// Paths the edge rules never see: ACME HTTP-01 validation (a rule must not
/// be able to break certificate renewal) and Temps' own endpoints under
/// `/api/_temps/` and `/api/__temps/` (challenge verification and the solver
/// a challenge page loads, so a challenge rule cannot lock visitors out).
pub fn is_exempt_path(path: &str) -> bool {
    path.starts_with("/.well-known/acme-challenge/")
        || path.starts_with("/api/_temps/")
        || path.starts_with("/api/__temps/")
}

/// Decodes a stored row into the engine's model.
pub fn definition_from_model(
    model: &project_edge_rules::Model,
) -> Result<RuleDefinition, EdgeRuleError> {
    let expression: Group =
        serde_json::from_value(model.expression.clone()).map_err(|e| EdgeRuleError::Decode {
            rule_id: model.id,
            part: "expression",
            reason: e.to_string(),
        })?;
    let action: Action =
        serde_json::from_value(model.action.clone()).map_err(|e| EdgeRuleError::Decode {
            rule_id: model.id,
            part: "action",
            reason: e.to_string(),
        })?;
    let mode = Mode::parse(&model.mode).ok_or_else(|| EdgeRuleError::Decode {
        rule_id: model.id,
        part: "mode",
        reason: format!("unknown mode '{}'", model.mode),
    })?;
    Ok(RuleDefinition {
        id: model.id,
        revision: model.revision,
        name: model.name.clone(),
        enabled: model.enabled,
        mode,
        environment_id: model.environment_id,
        expression,
        action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(
        id: i32,
        expression: serde_json::Value,
        action: serde_json::Value,
    ) -> project_edge_rules::Model {
        project_edge_rules::Model {
            id,
            project_id: 1,
            environment_id: None,
            position: id,
            name: format!("rule {id}"),
            enabled: true,
            mode: "enforce".into(),
            expression,
            action,
            revision: 1,
            created_by: None,
            updated_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn exempt_paths_cover_acme_and_temps_endpoints() {
        assert!(is_exempt_path("/.well-known/acme-challenge/token"));
        assert!(is_exempt_path(temps_shield::assets::VERIFY_PATH));
        assert!(is_exempt_path(temps_shield::assets::SOLVER_JS_PATH));
        assert!(is_exempt_path(temps_shield::assets::SOLVER_WASM_PATH));
        assert!(!is_exempt_path("/.well-known/security.txt"));
        assert!(!is_exempt_path("/admin"));
    }

    #[test]
    fn decodes_a_stored_rule_and_names_the_broken_part() {
        let ok = model(
            3,
            serde_json::json!({"combinator": "all", "children": []}),
            serde_json::json!({"type": "block", "status": 403}),
        );
        let def = definition_from_model(&ok).expect("decodes");
        assert_eq!(def.mode, Mode::Enforce);

        let bad = model(
            4,
            serde_json::json!({"combinator": "sometimes"}),
            serde_json::json!({"type": "allow"}),
        );
        match definition_from_model(&bad) {
            Err(EdgeRuleError::Decode {
                rule_id: 4,
                part: "expression",
                ..
            }) => {}
            other => panic!("expected an expression decode error, got {other:?}"),
        }

        let mut bad_mode = ok.clone();
        bad_mode.mode = "maybe".into();
        assert!(matches!(
            definition_from_model(&bad_mode),
            Err(EdgeRuleError::Decode { part: "mode", .. })
        ));
    }
}
