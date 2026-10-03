// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kubeconfigs: the embedded cluster CA and client certificates, and bearer
//! tokens that are JWTs. Only inline `*-data` and `token` fields are read.
//! File-path fields (`certificate-authority`, `client-certificate`,
//! `tokenFile`) and `exec` / `auth-provider` entries are never deserialized,
//! so a stored value can never cause a host read or a process spawn.

use super::{jwt, label_text, x509, Found};
use base64::Engine;
use serde::Deserialize;

/// kubectl writes `null` for empty lists and entries, so every field tolerates it.
#[derive(Deserialize)]
struct Kubeconfig {
    kind: Option<String>,
    clusters: Option<Vec<NamedCluster>>,
    users: Option<Vec<NamedUser>>,
}
#[derive(Deserialize)]
struct NamedCluster {
    name: Option<String>,
    cluster: Option<Cluster>,
}
#[derive(Deserialize)]
struct Cluster {
    #[serde(rename = "certificate-authority-data")]
    certificate_authority_data: Option<String>,
}
#[derive(Deserialize)]
struct NamedUser {
    name: Option<String>,
    user: Option<User>,
}
#[derive(Deserialize)]
struct User {
    #[serde(rename = "client-certificate-data")]
    client_certificate_data: Option<String>,
    token: Option<String>,
}

pub(super) fn from_text(text: &str) -> Found {
    if !looks_like_kubeconfig(text) {
        return Found::default();
    }
    // serde_yaml bounds recursion depth and alias expansion.
    let config: Kubeconfig = match serde_yaml::from_str(text) {
        Ok(config) => config,
        Err(_) => {
            return Found::problem(
                "kubeconfig_invalid",
                "The kubeconfig could not be parsed as YAML.".into(),
            )
        }
    };
    let mut found = Found::default();
    if config.kind.as_deref() != Some("Config") {
        return found;
    }
    for entry in config.clusters.iter().flatten() {
        let Some(cluster) = &entry.cluster else {
            continue;
        };
        if let Some(data) = &cluster.certificate_authority_data {
            let context = format!("Kubeconfig cluster '{}' CA certificate", name(&entry.name));
            found.extend(certificates(data, &context));
        }
    }
    for entry in config.users.iter().flatten() {
        let Some(credentials) = &entry.user else {
            continue;
        };
        let user = name(&entry.name);
        if let Some(data) = &credentials.client_certificate_data {
            found.extend(certificates(
                data,
                &format!("Kubeconfig user '{user}' client certificate"),
            ));
        }
        if let Some(token) = &credentials.token {
            found.extend(jwt::from_value(
                token,
                &format!("Kubeconfig user '{user}' token"),
            ));
        }
    }
    found
}

/// A cheap gate so arbitrary values are never handed to the YAML parser:
/// `kind: Config` exactly, not `kind: ConfigMap`.
fn looks_like_kubeconfig(text: &str) -> bool {
    fn skip(s: &str) -> &str {
        s.trim_start_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'')
    }
    text.match_indices("kind").any(|(at, word)| {
        let rest = skip(&text[at + word.len()..]);
        let Some(rest) = rest.strip_prefix(':') else {
            return false;
        };
        let Some(rest) = skip(rest).strip_prefix("Config") else {
            return false;
        };
        !rest.starts_with(|c: char| c.is_ascii_alphanumeric())
    })
}

fn name(raw: &Option<String>) -> String {
    let name = label_text(raw.as_deref().unwrap_or_default());
    if name.is_empty() {
        "unnamed".into()
    } else {
        name
    }
}

/// `*-data` fields hold base64 of a PEM bundle (or, rarely, DER).
fn certificates(data: &str, context: &str) -> Found {
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(compact) else {
        return Found::problem(
            "kubeconfig_invalid",
            format!("{context} data is not valid base64."),
        );
    };
    match std::str::from_utf8(&bytes) {
        Ok(text) if text.contains("-----BEGIN") => x509::from_pem_text(text, Some(context)),
        _ => match x509::from_der(&bytes, Some(context)) {
            Ok(artifacts) => Found {
                artifacts,
                findings: vec![],
            },
            Err(()) => Found::problem(
                "kubeconfig_invalid",
                format!("{context} data is not a certificate."),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::jwt::tests::token;
    use super::super::tests::{at, verify};
    use super::super::x509::tests::certificate_pem;
    use super::super::*;
    use crate::verification::CheckStatus;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    fn kubeconfig(ca_until: (i32, u8, u8), client_until: (i32, u8, u8), token: &str) -> String {
        let ca = STANDARD.encode(certificate_pem("cluster-ca", (2025, 1, 1), ca_until));
        let client = STANDARD.encode(certificate_pem("ci-runner", (2026, 1, 1), client_until));
        format!(
            "apiVersion: v1
kind: Config
clusters:
- name: staging
  cluster:
    server: https://cluster.example.test:6443
    certificate-authority-data: {ca}
users:
- name: ci
  user:
    client-certificate-data: {client}
    client-key-data: bm90LWEta2V5
- name: deployer
  user:
    token: {token}
- name: plugin
  user:
    client-certificate: /etc/hostname
    tokenFile: /etc/hostname
    exec:
      apiVersion: client.authentication.k8s.io/v1
      command: /bin/false
contexts:
- name: staging
  context:
    cluster: staging
    user: ci
current-context: staging
"
        )
    }

    #[test]
    fn reports_every_certificate_and_token_earliest_first() {
        let jwt = token(serde_json::json!({"exp": at("2026-12-01").timestamp()}));
        let value = kubeconfig((2030, 1, 1), (2026, 9, 26), &jwt);
        let inspection = inspect(&value);
        let labels: Vec<&str> = inspection
            .artifacts
            .iter()
            .map(|a| a.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Kubeconfig user 'ci' client certificate 'ci-runner'",
                "Kubeconfig user 'deployer' token",
                "Kubeconfig cluster 'staging' CA certificate 'cluster-ca'",
            ]
        );
        assert!(inspection.findings.is_empty(), "{:?}", inspection.findings);
        let result = verify(&value);
        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(
            result.findings[0].message,
            "Found 2 certificates and 1 JWT."
        );
        assert_eq!(result.findings[1].code, "expires_within_7_days");
    }

    #[test]
    fn base64_wrapped_and_json_kubeconfigs_are_recognized() {
        let value = kubeconfig((2030, 1, 1), (2027, 1, 1), "opaque-token");
        assert_eq!(inspect(&STANDARD.encode(&value)).artifacts.len(), 2);
        let ca = STANDARD.encode(certificate_pem("cluster-ca", (2025, 1, 1), (2030, 1, 1)));
        let json = format!(
            r#"{{"apiVersion":"v1","kind":"Config","clusters":[{{"name":"prod","cluster":{{"certificate-authority-data":"{ca}"}}}}],"users":[]}}"#
        );
        assert_eq!(inspect(&json).artifacts.len(), 1);
    }

    #[test]
    fn exec_only_users_and_path_fields_yield_nothing() {
        let value = "apiVersion: v1\nkind: Config\nusers:\n- name: cloud\n  user:\n    client-certificate: /etc/hostname\n    exec:\n      command: /bin/false\n";
        let inspection = inspect(value);
        assert!(inspection.artifacts.is_empty());
        assert!(inspection.findings.is_empty());
        assert_eq!(verify(value).findings[0].code, "credential_not_inspectable");
    }

    #[test]
    fn malformed_kubeconfigs_are_reported() {
        let bad_yaml = "apiVersion: v1\nkind: Config\nusers: [unclosed\n";
        assert_eq!(inspect(bad_yaml).findings[0].code, "kubeconfig_invalid");
        let bad_data = "apiVersion: v1\nkind: Config\nusers:\n- name: ci\n  user:\n    client-certificate-data: '!!!'\n";
        let inspection = inspect(bad_data);
        assert_eq!(inspection.findings[0].code, "kubeconfig_invalid");
        assert_eq!(
            inspection.findings[0].message,
            "Kubeconfig user 'ci' client certificate data is not valid base64."
        );
        assert_eq!(verify(bad_data).status, CheckStatus::Error);
    }

    #[test]
    fn alias_expansion_and_deep_nesting_are_bounded() {
        let mut bomb = String::from("apiVersion: v1\nkind: Config\na: &a [\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\"]\n");
        for (name, previous) in ["b", "c", "d", "e", "f", "g", "h", "i"]
            .iter()
            .zip(["a", "b", "c", "d", "e", "f", "g", "h"])
        {
            bomb.push_str(&format!(
                "{name}: &{name} [*{previous},*{previous},*{previous},*{previous},*{previous},*{previous},*{previous},*{previous},*{previous}]\n"
            ));
        }
        bomb.push_str("users: *i\n");
        let started = std::time::Instant::now();
        let inspection = inspect(&bomb);
        assert!(inspection.artifacts.is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let nested = format!(
            "apiVersion: v1\nkind: Config\nusers: {}{}\n",
            "[".repeat(5000),
            "]".repeat(5000)
        );
        assert_eq!(inspect(&nested).findings[0].code, "kubeconfig_invalid");
    }
    #[test]
    fn null_lists_and_entries_written_by_kubectl_are_accepted() {
        let ca = STANDARD.encode(certificate_pem("cluster-ca", (2025, 1, 1), (2030, 1, 1)));
        let value = format!("apiVersion: v1\nkind: Config\nclusters:\n- name: prod\n  cluster:\n    certificate-authority-data: {ca}\n- name: empty\n  cluster: null\nusers: null\ncontexts: null\n");
        let inspection = inspect(&value);
        assert_eq!(inspection.artifacts.len(), 1, "{:?}", inspection.findings);
        assert!(inspection.findings.is_empty());
    }

    #[test]
    fn config_maps_are_not_kubeconfigs() {
        let pem = certificate_pem("svc.example.test", (2026, 1, 1), (2030, 1, 1));
        let indented: String = pem.lines().map(|line| format!("    {line}\n")).collect();
        let value = format!("apiVersion: v1\nkind: ConfigMap\ndata:\n  ca.crt: |\n{indented}---\napiVersion: v1\nkind: Secret\n");
        let inspection = inspect(&value);
        assert_eq!(inspection.artifacts.len(), 1);
        assert!(inspection.findings.is_empty(), "{:?}", inspection.findings);
    }
}
