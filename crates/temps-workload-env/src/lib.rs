// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workload variables (ADR-052 §3).
//!
//! One assembly, two deliveries:
//!
//! 1. [`assemble`] merges the layers in precedence order (later wins):
//!    platform, service access, project, environment, workload override.
//!    Every name records which layer won and which it shadowed, so the
//!    console can explain where a value came from.
//! 2. [`Assembled::to_spec`] turns the result into a
//!    [`temps_workload::EnvSpec`]. Secrets stay **references** there: a
//!    spec, a stored revision or a node snapshot never holds a secret
//!    value.
//! 3. [`materialize`] resolves the references at the last moment, for the
//!    kind's delivery:
//!    - `Baked` (deployments): plain variables become the environment,
//!      secrets become files under `/run/secrets`, never in `docker
//!      inspect`;
//!    - `PerExec` (sandboxes): everything is resolved for one command and
//!      comes with a [`Redactor`] for its output, never stored in the
//!      instance.
//!
//! [`is_secret_variable`] classifies plain variables whose name says they
//! hold a credential (`DATABASE_URL`, `API_TOKEN`), for redaction and
//! masking. It does not move them into files: an application reading
//! `DATABASE_URL` from its environment keeps working.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use async_trait::async_trait;
use temps_workload::driver::SecretFile;
use temps_workload::{Delivery, EnvSpec, SecretRef};

/// Where a variable comes from, in precedence order: a later layer wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// Set by Temps itself (`PORT`, `TEMPS_*`).
    Platform,
    /// Produced by a linked service's access engine (`POSTGRES_URL`).
    ServiceAccess,
    Project,
    Environment,
    /// Set on this one workload.
    Workload,
}

impl Layer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Layer::Platform => "platform",
            Layer::ServiceAccess => "service_access",
            Layer::Project => "project",
            Layer::Environment => "environment",
            Layer::Workload => "workload",
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Value {
    Plain(String),
    /// Stored elsewhere (an encrypted column); resolved by [`materialize`].
    Secret {
        source: String,
    },
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // A plain value can still be a credential by name; Debug output
            // ends up in logs, so it never prints values at all.
            Value::Plain(_) => f.write_str("Plain(..)"),
            Value::Secret { source } => f.debug_struct("Secret").field("source", source).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: Value,
}

impl Variable {
    pub fn plain(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: Value::Plain(value.into()),
        }
    }

    pub fn secret(name: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: Value::Secret {
                source: source.into(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvError {
    #[error(
        "variable name '{name}' from the {layer} layer is invalid: expected [A-Za-z_][A-Za-z0-9_]*"
    )]
    InvalidName { layer: Layer, name: String },
    #[error("variable '{name}' is set twice in the {layer} layer; a layer sets each name once")]
    DuplicateInLayer { layer: Layer, name: String },
    #[error("variable '{name}' from the {layer} layer contains a NUL byte, which no process environment can hold")]
    NulInValue { layer: Layer, name: String },
    #[error("secret '{name}' (source {source_ref}) could not be resolved: {reason}")]
    Unresolved {
        name: String,
        source_ref: String,
        reason: String,
    },
    #[error("secret '{name}' resolved to a value with a NUL byte (source {source_ref})")]
    NulInSecret { name: String, source_ref: String },
}

/// Whether a variable's name says it holds a credential. Moved here from
/// `temps-ai-chat` (ADR-052 §3) so deployments and sandboxes classify the
/// same way.
pub fn is_secret_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    // A key prefix names where a sandbox may write; it is not a credential.
    if name.ends_with("KEY_PREFIX") {
        return false;
    }
    ["PASSWORD", "SECRET", "TOKEN", "KEY", "URL"]
        .iter()
        .any(|marker| name.contains(marker))
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The layers to merge. Order of insertion does not matter; [`Layer`]'s
/// order does.
#[derive(Debug, Clone, Default)]
pub struct Layers {
    layers: BTreeMap<Layer, Vec<Variable>>,
}

impl Layers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, layer: Layer, vars: impl IntoIterator<Item = Variable>) -> Self {
        self.layers.entry(layer).or_default().extend(vars);
        self
    }
}

/// One assembled variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value: Value,
    /// The layer whose value won.
    pub layer: Layer,
    /// Lower layers that also set it, lowest first.
    pub shadows: Vec<Layer>,
}

impl Resolved {
    /// A secret reference, or a plain value whose name marks a credential.
    pub fn is_sensitive(&self, name: &str) -> bool {
        matches!(self.value, Value::Secret { .. }) || is_secret_variable(name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assembled {
    pub vars: BTreeMap<String, Resolved>,
}

pub fn assemble(layers: &Layers) -> Result<Assembled, EnvError> {
    let mut vars: BTreeMap<String, Resolved> = BTreeMap::new();
    for (layer, list) in &layers.layers {
        let mut seen = BTreeSet::new();
        for var in list {
            if !valid_name(&var.name) {
                return Err(EnvError::InvalidName {
                    layer: *layer,
                    name: var.name.clone(),
                });
            }
            if !seen.insert(var.name.as_str()) {
                return Err(EnvError::DuplicateInLayer {
                    layer: *layer,
                    name: var.name.clone(),
                });
            }
            if let Value::Plain(value) = &var.value {
                if value.contains('\0') {
                    return Err(EnvError::NulInValue {
                        layer: *layer,
                        name: var.name.clone(),
                    });
                }
            }
            match vars.get_mut(&var.name) {
                Some(existing) => {
                    existing.shadows.push(existing.layer);
                    existing.layer = *layer;
                    existing.value = var.value.clone();
                }
                None => {
                    vars.insert(
                        var.name.clone(),
                        Resolved {
                            value: var.value.clone(),
                            layer: *layer,
                            shadows: Vec::new(),
                        },
                    );
                }
            }
        }
    }
    Ok(Assembled { vars })
}

impl Assembled {
    /// The spec form: plain values inline, secrets as references.
    pub fn to_spec(&self, delivery: Delivery) -> EnvSpec {
        let mut spec = EnvSpec {
            delivery,
            ..Default::default()
        };
        for (name, resolved) in &self.vars {
            match &resolved.value {
                Value::Plain(value) => {
                    spec.vars.insert(name.clone(), value.clone());
                }
                Value::Secret { source } => spec.secrets.push(SecretRef {
                    name: name.clone(),
                    source: source.clone(),
                }),
            }
        }
        spec
    }

    /// For display: every sensitive value replaced by `***`.
    pub fn masked(&self) -> BTreeMap<String, String> {
        self.vars
            .iter()
            .map(|(name, resolved)| {
                let shown = match (&resolved.value, resolved.is_sensitive(name)) {
                    (Value::Plain(value), false) => value.clone(),
                    _ => "***".to_string(),
                };
                (name.clone(), shown)
            })
            .collect()
    }
}

/// Reads a secret's value from where it is stored.
#[async_trait]
pub trait SecretResolver: Send + Sync {
    async fn resolve(&self, secret: &SecretRef) -> Result<String, String>;
}

/// What a driver receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Materialized {
    Baked {
        /// `NAME=value`, sorted by name.
        env: Vec<String>,
        /// Delivered at `/run/secrets/<name>`, mode 0400.
        secret_files: Vec<SecretFile>,
    },
    PerExec {
        env: BTreeMap<String, String>,
        redactor: Redactor,
    },
}

pub async fn materialize(
    spec: &EnvSpec,
    resolver: &dyn SecretResolver,
) -> Result<Materialized, EnvError> {
    let mut secrets = Vec::with_capacity(spec.secrets.len());
    for secret in &spec.secrets {
        let value = resolver
            .resolve(secret)
            .await
            .map_err(|reason| EnvError::Unresolved {
                name: secret.name.clone(),
                source_ref: secret.source.clone(),
                reason,
            })?;
        if value.contains('\0') {
            return Err(EnvError::NulInSecret {
                name: secret.name.clone(),
                source_ref: secret.source.clone(),
            });
        }
        secrets.push((secret.name.clone(), value));
    }
    Ok(match spec.delivery {
        Delivery::Baked => Materialized::Baked {
            env: spec.vars.iter().map(|(k, v)| format!("{k}={v}")).collect(),
            secret_files: secrets
                .into_iter()
                .map(|(name, value)| SecretFile { name, value })
                .collect(),
        },
        Delivery::PerExec => {
            let mut sensitive: Vec<String> = spec
                .vars
                .iter()
                .filter(|(name, _)| is_secret_variable(name))
                .map(|(_, value)| value.clone())
                .collect();
            sensitive.extend(secrets.iter().map(|(_, value)| value.clone()));
            let mut env = spec.vars.clone();
            env.extend(secrets);
            Materialized::PerExec {
                env,
                redactor: Redactor::new(sensitive),
            }
        }
    })
}

/// Replaces known secret values in command output.
///
/// Hygiene, not the boundary: it keeps a password out of a transcript when
/// a command prints its environment. Values shorter than
/// [`Redactor::MIN_LEN`] are skipped, since replacing `on` or `1` would
/// mangle every line.
#[derive(Clone, PartialEq, Eq)]
pub struct Redactor {
    /// Longest first, so a URL holding a password is replaced whole.
    values: Vec<String>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("values", &self.values.len())
            .finish()
    }
}

impl Redactor {
    pub const MIN_LEN: usize = 6;
    pub const MARKER: &'static str = "[redacted]";

    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        let mut values: Vec<String> = values
            .into_iter()
            .filter(|v| v.len() >= Self::MIN_LEN)
            .collect();
        values.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        values.dedup();
        Self { values }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for value in &self.values {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), Self::MARKER);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Store;

    #[async_trait]
    impl SecretResolver for Store {
        async fn resolve(&self, secret: &SecretRef) -> Result<String, String> {
            match secret.source.as_str() {
                "env_var:1" => Ok("s3cr3t-value".to_string()),
                "env_var:2" => Ok("bad\0value".to_string()),
                other => Err(format!("no row for {other}")),
            }
        }
    }

    fn layers() -> Layers {
        Layers::new()
            .with(
                Layer::Environment,
                [
                    Variable::plain("LOG_LEVEL", "debug"),
                    Variable::secret("API_TOKEN", "env_var:1"),
                ],
            )
            .with(
                Layer::Platform,
                [
                    Variable::plain("PORT", "3000"),
                    Variable::plain("LOG_LEVEL", "info"),
                ],
            )
            .with(
                Layer::ServiceAccess,
                [Variable::plain(
                    "POSTGRES_URL",
                    "postgres://app:pw-123456@db:5432/app",
                )],
            )
            .with(Layer::Project, [Variable::plain("LOG_LEVEL", "warn")])
    }

    #[test]
    fn later_layers_win_and_record_what_they_shadow() {
        let assembled = assemble(&layers()).unwrap();
        let level = &assembled.vars["LOG_LEVEL"];
        assert_eq!(level.value, Value::Plain("debug".into()));
        assert_eq!(level.layer, Layer::Environment);
        assert_eq!(level.shadows, [Layer::Platform, Layer::Project]);
        assert_eq!(assembled.vars["PORT"].layer, Layer::Platform);
        assert!(assembled.vars["PORT"].shadows.is_empty());
    }

    #[test]
    fn a_workload_override_beats_every_layer() {
        let assembled =
            assemble(&layers().with(Layer::Workload, [Variable::plain("PORT", "8080")])).unwrap();
        assert_eq!(assembled.vars["PORT"].value, Value::Plain("8080".into()));
        assert_eq!(assembled.vars["PORT"].shadows, [Layer::Platform]);
    }

    #[test]
    fn the_spec_holds_secret_references_never_values() {
        let spec = assemble(&layers()).unwrap().to_spec(Delivery::Baked);
        assert_eq!(
            spec.secrets,
            [SecretRef {
                name: "API_TOKEN".into(),
                source: "env_var:1".into()
            }]
        );
        assert!(!spec.vars.contains_key("API_TOKEN"));
        let printed = format!("{spec:?}");
        assert!(!printed.contains("s3cr3t"), "{printed}");
    }

    #[test]
    fn masking_hides_secrets_and_credential_names() {
        let masked = assemble(&layers()).unwrap().masked();
        assert_eq!(masked["API_TOKEN"], "***");
        assert_eq!(masked["POSTGRES_URL"], "***");
        assert_eq!(masked["PORT"], "3000");
    }

    #[tokio::test]
    async fn baked_delivery_puts_secrets_in_files_and_the_rest_in_env() {
        let spec = assemble(&layers()).unwrap().to_spec(Delivery::Baked);
        let Materialized::Baked { env, secret_files } = materialize(&spec, &Store).await.unwrap()
        else {
            panic!("expected baked");
        };
        assert_eq!(
            env,
            [
                "LOG_LEVEL=debug",
                "PORT=3000",
                "POSTGRES_URL=postgres://app:pw-123456@db:5432/app"
            ]
        );
        assert_eq!(
            secret_files,
            [SecretFile {
                name: "API_TOKEN".into(),
                value: "s3cr3t-value".into()
            }]
        );
        assert!(!env.iter().any(|e| e.contains("s3cr3t")));
    }

    #[tokio::test]
    async fn per_exec_delivery_redacts_secrets_and_credential_values() {
        let spec = assemble(&layers()).unwrap().to_spec(Delivery::PerExec);
        let Materialized::PerExec { env, redactor } = materialize(&spec, &Store).await.unwrap()
        else {
            panic!("expected per-exec");
        };
        assert_eq!(env["API_TOKEN"], "s3cr3t-value");
        let output = "token=s3cr3t-value url=postgres://app:pw-123456@db:5432/app port=3000";
        assert_eq!(
            redactor.redact(output),
            "token=[redacted] url=[redacted] port=3000"
        );
        assert!(!format!("{redactor:?}").contains("s3cr3t"));
    }

    #[tokio::test]
    async fn unresolvable_secrets_name_the_variable_and_source() {
        let spec = assemble(&Layers::new().with(
            Layer::Workload,
            [Variable::secret("DB_PASSWORD", "env_var:99")],
        ))
        .unwrap()
        .to_spec(Delivery::Baked);
        let error = materialize(&spec, &Store).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "secret 'DB_PASSWORD' (source env_var:99) could not be resolved: no row for env_var:99"
        );
        let spec = assemble(
            &Layers::new().with(Layer::Workload, [Variable::secret("X_KEY", "env_var:2")]),
        )
        .unwrap()
        .to_spec(Delivery::Baked);
        assert!(matches!(
            materialize(&spec, &Store).await,
            Err(EnvError::NulInSecret { .. })
        ));
    }

    #[test]
    fn invalid_input_is_refused_with_its_layer() {
        let bad =
            |vars: Vec<Variable>| assemble(&Layers::new().with(Layer::Project, vars)).unwrap_err();
        assert_eq!(
            bad(vec![Variable::plain("1ST", "x")]),
            EnvError::InvalidName {
                layer: Layer::Project,
                name: "1ST".into()
            }
        );
        assert!(matches!(
            bad(vec![Variable::plain("A-B", "x")]),
            EnvError::InvalidName { .. }
        ));
        assert!(matches!(
            bad(vec![Variable::plain("", "x")]),
            EnvError::InvalidName { .. }
        ));
        assert_eq!(
            bad(vec![Variable::plain("A", "1"), Variable::plain("A", "2")]),
            EnvError::DuplicateInLayer {
                layer: Layer::Project,
                name: "A".into()
            }
        );
        assert!(matches!(
            bad(vec![Variable::plain("A", "x\0y")]),
            EnvError::NulInValue { .. }
        ));
    }

    #[test]
    fn classification_matches_the_sandbox_rule() {
        for name in [
            "DATABASE_URL",
            "api_token",
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "STRIPE_KEY",
        ] {
            assert!(is_secret_variable(name), "{name}");
        }
        for name in ["PORT", "S3_KEY_PREFIX", "LOG_LEVEL", "NODE_ENV"] {
            assert!(!is_secret_variable(name), "{name}");
        }
    }

    #[test]
    fn the_redactor_skips_short_values_and_prefers_the_longest() {
        let redactor = Redactor::new([
            "on".to_string(),
            "pw-123456".to_string(),
            "postgres://u:pw-123456@h".to_string(),
        ]);
        assert_eq!(
            redactor.redact("x=on postgres://u:pw-123456@h pw-123456"),
            "x=on [redacted] [redacted]"
        );
    }

    #[test]
    fn debug_never_prints_values() {
        let printed = format!("{:?}", Variable::plain("DB_PASSWORD", "hunter2-long"));
        assert!(!printed.contains("hunter2"), "{printed}");
    }
}
