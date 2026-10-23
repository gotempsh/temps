// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a workload's environment comes from at create time.
//!
//! A stored spec never holds a secret value (`EnvSpec` holds references and
//! imports). The plane resolves them through an [`EnvSource`] only into the
//! transient instance spec a driver receives.

use std::collections::BTreeMap;

use async_trait::async_trait;
use temps_workload::{EnvSpec, SecretRef};
use temps_workload_env::{is_secret_variable, Redactor, SecretResolver};

/// Resolves secret references and imports. Implemented by the product that
/// knows where its variables are stored.
#[async_trait]
pub trait EnvSource: SecretResolver {
    /// Every variable of `source` (`deployment_container:<id>`), in order.
    async fn import(&self, source: &str) -> Result<Vec<(String, String)>, String>;
}

/// Resolves nothing. For planes whose workloads carry only plain variables.
pub struct NoEnvSource;

#[async_trait]
impl SecretResolver for NoEnvSource {
    async fn resolve(&self, secret: &SecretRef) -> Result<String, String> {
        Err(format!(
            "no secret source is configured for '{}' ({})",
            secret.name, secret.source
        ))
    }
}

#[async_trait]
impl EnvSource for NoEnvSource {
    async fn import(&self, source: &str) -> Result<Vec<(String, String)>, String> {
        Err(format!("no import source is configured for '{source}'"))
    }
}

/// The variables an instance gets, and a redactor for its output.
pub(crate) struct ResolvedEnv {
    pub env: Vec<(String, String)>,
    pub redactor: Redactor,
}

/// Imports first, then the spec's own variables, which win. Secret
/// references are refused at admission until secret-file delivery exists.
#[cfg(test)]
pub(crate) async fn resolve(spec: &EnvSpec, source: &dyn EnvSource) -> Result<ResolvedEnv, String> {
    resolve_with_staged(spec, source, &|_| None).await
}

/// Answers a `staged:` import from this process's staged environments.
pub(crate) type StagedLookup<'a> = dyn Fn(&str) -> Option<Vec<(String, String)>> + Sync + 'a;

/// [`resolve`], with environments staged in this process
/// (`WorkloadPlane::stage_env`) answering their imports first.
pub(crate) async fn resolve_with_staged(
    spec: &EnvSpec,
    source: &dyn EnvSource,
    staged: &StagedLookup<'_>,
) -> Result<ResolvedEnv, String> {
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    for import in &spec.imports {
        if import.starts_with(crate::plane::STAGED_IMPORT_PREFIX) {
            let imported = staged(import).ok_or_else(|| {
                format!(
                    "import '{import}' was staged by a process that no longer holds it (staged \
                     environments live only in memory until the instance is created)"
                )
            })?;
            vars.extend(imported);
            continue;
        }
        let imported = source
            .import(import)
            .await
            .map_err(|reason| format!("import '{import}' failed: {reason}"))?;
        vars.extend(imported);
    }
    vars.extend(spec.vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    if let Some((name, _)) = vars.iter().find(|(_, v)| v.contains('\0')) {
        return Err(format!("variable '{name}' contains a NUL byte"));
    }
    let redactor = Redactor::new(
        vars.iter()
            .filter(|(name, _)| is_secret_variable(name))
            .map(|(_, value)| value.clone()),
    );
    Ok(ResolvedEnv {
        env: vars.into_iter().collect(),
        redactor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Imports;

    #[async_trait]
    impl SecretResolver for Imports {
        async fn resolve(&self, _: &SecretRef) -> Result<String, String> {
            Err("unused".into())
        }
    }

    #[async_trait]
    impl EnvSource for Imports {
        async fn import(&self, source: &str) -> Result<Vec<(String, String)>, String> {
            match source {
                "deployment_container:1" => Ok(vec![
                    ("MODE".into(), "web".into()),
                    ("API_TOKEN".into(), "tok-abcdef123".into()),
                ]),
                other => Err(format!("unknown source {other}")),
            }
        }
    }

    #[tokio::test]
    async fn spec_variables_override_imports_and_secret_values_are_redacted() {
        let spec = EnvSpec {
            vars: BTreeMap::from([("MODE".into(), "batch".into())]),
            imports: vec!["deployment_container:1".into()],
            ..Default::default()
        };
        let resolved = resolve(&spec, &Imports).await.expect("resolved");
        assert_eq!(
            resolved.env,
            vec![
                ("API_TOKEN".to_string(), "tok-abcdef123".to_string()),
                ("MODE".to_string(), "batch".to_string()),
            ]
        );
        assert_eq!(
            resolved.redactor.redact("token=tok-abcdef123"),
            "token=[redacted]"
        );
    }

    #[tokio::test]
    async fn a_failed_import_names_the_source() {
        let spec = EnvSpec {
            imports: vec!["deployment_container:9".into()],
            ..Default::default()
        };
        let error = resolve(&spec, &Imports).await.err().expect("refused");
        assert!(error.contains("deployment_container:9"), "{error}");
    }
}
