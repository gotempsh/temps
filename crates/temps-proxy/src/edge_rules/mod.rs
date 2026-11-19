// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project edge rules (ADR-089) in the proxy: the gate that keeps every
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
/// be able to break certificate renewal), and the three paths the proxy
/// answers itself for a challenge -- the verification POST and the two
/// solver files the challenge page loads -- so a challenge rule cannot lock
/// visitors out. Everything else under `/api/_temps/` and `/api/__temps/` is
/// evaluated: it may be the project's own (a project that serves its own
/// Temps API receives `/api/_temps/`), and otherwise it is still traffic sent
/// to the project's hostname, such as its analytics beacons.
///
/// The check runs on the raw path, while rules match the normalized one, so
/// only a path that cannot normalize to anything else is exempt: no dot
/// segments, percent-encoding, backslashes, semicolons or empty segments.
/// Otherwise `/.well-known/acme-challenge/../../admin` would skip a rule on
/// `/admin`. Real ACME tokens never contain them.
pub fn is_exempt_path(path: &str) -> bool {
    temps_shield::assets::is_shield_path(path)
        || (path.starts_with("/.well-known/acme-challenge/") && is_plain_path(path))
}

fn is_plain_path(path: &str) -> bool {
    !path.contains(['%', '\\', ';'])
        && !path.contains("//")
        && path
            .split('/')
            .all(|segment| segment != "." && segment != "..")
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
    fn exempt_paths_cover_acme_and_the_challenge_endpoints() {
        assert!(is_exempt_path("/.well-known/acme-challenge/token"));
        assert!(is_exempt_path(temps_shield::assets::VERIFY_PATH));
        assert!(is_exempt_path(temps_shield::assets::SOLVER_JS_PATH));
        assert!(is_exempt_path(temps_shield::assets::SOLVER_WASM_PATH));
        assert!(!is_exempt_path("/.well-known/security.txt"));
        assert!(!is_exempt_path("/admin"));
        // `/api/__temps/` only serves the solver; anything else there is the
        // app's, and a rule on it must apply.
        assert!(!is_exempt_path("/api/__temps/private"));
        assert!(!is_exempt_path("/api/__temps/temps_captcha_wasm.js.map"));
        // The rest of `/api/_temps/` is evaluated: a project that serves its
        // own Temps API receives it, and a rule there must apply.
        assert!(!is_exempt_path("/api/_temps/event"));
        assert!(!is_exempt_path("/api/_temps/captcha/verify/x"));
        assert!(!is_exempt_path("/api/_temps/"));
    }

    #[test]
    fn exempt_prefixes_cannot_smuggle_another_path() {
        for path in [
            "/.well-known/acme-challenge/../../admin",
            "/.well-known/acme-challenge/./x",
            "/.well-known/acme-challenge/%2e%2e/admin",
            "/.well-known/acme-challenge/..%2fadmin",
            "/.well-known/acme-challenge//admin",
            "/.well-known/acme-challenge/x;/../admin",
            "/api/_temps/..\\admin",
            "/api/__temps/../admin",
        ] {
            assert!(!is_exempt_path(path), "{path} must be evaluated");
        }
        assert!(is_exempt_path("/.well-known/acme-challenge/a-Zz_09.token"));
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
