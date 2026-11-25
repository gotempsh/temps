// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-place credential rotation for a git provider: which supplied fields
//! apply to the provider's auth method, and the checks a new basic-auth or
//! SSH-key credential must pass before it is encrypted and stored.
//!
//! Nothing here formats a credential value into an error or a log line; the
//! errors name the field and the provider only.

use super::*;

/// Longest basic-auth username accepted (the common provider limit).
const MAX_USERNAME_LEN: usize = 255;
/// Longest password, app password or token accepted.
const MAX_PASSWORD_LEN: usize = 4096;
/// Longest SSH key accepted, private or public (an RSA-16384 PEM fits).
const MAX_SSH_KEY_LEN: usize = 16 * 1024;

/// Public key algorithms an OpenSSH `authorized_keys` line may start with.
const SSH_PUBLIC_KEY_PREFIXES: &[&str] = &[
    "ssh-ed25519 ",
    "ssh-rsa ",
    "ssh-dss ",
    "ecdsa-sha2-nistp256 ",
    "ecdsa-sha2-nistp384 ",
    "ecdsa-sha2-nistp521 ",
    "sk-ssh-ed25519@openssh.com ",
    "sk-ecdsa-sha2-nistp256@openssh.com ",
];

/// A partial credential update. `Some` replaces the field, `None` keeps the
/// stored value. Deliberately not `Debug`: every field may be a secret.
#[derive(Default, Clone)]
pub struct ProviderCredentialsUpdate {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    /// GitHub App private key (PEM), or the SSH private key of an SSH-key provider.
    pub private_key: Option<String>,
    pub webhook_secret: Option<String>,
    pub redirect_uri: Option<String>,
    /// Token of a personal-access-token provider.
    pub token: Option<String>,
    /// Username of a basic-auth provider.
    pub username: Option<String>,
    /// Password or app password of a basic-auth provider.
    pub password: Option<String>,
    /// SSH public key of an SSH-key provider (`authorized_keys` format).
    pub public_key: Option<String>,
}

impl ProviderCredentialsUpdate {
    /// Names of the supplied fields, in a fixed order.
    fn supplied(&self) -> Vec<&'static str> {
        [
            ("client_id", self.client_id.is_some()),
            ("client_secret", self.client_secret.is_some()),
            ("app_id", self.app_id.is_some()),
            ("app_secret", self.app_secret.is_some()),
            ("private_key", self.private_key.is_some()),
            ("webhook_secret", self.webhook_secret.is_some()),
            ("redirect_uri", self.redirect_uri.is_some()),
            ("token", self.token.is_some()),
            ("username", self.username.is_some()),
            ("password", self.password.is_some()),
            ("public_key", self.public_key.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
    }
}

/// The stored provider after an update, and the names (never the values) of
/// the credential fields that were replaced.
pub struct CredentialsUpdated {
    pub provider: git_providers::Model,
    pub updated_fields: Vec<&'static str>,
}

fn invalid(
    provider_id: i32,
    auth_method: &'static str,
    reason: impl Into<String>,
) -> GitProviderManagerError {
    GitProviderManagerError::InvalidCredentials {
        provider_id,
        auth_method,
        reason: reason.into(),
    }
}

/// Refuse fields that do not belong to `auth_method`, so a rotation that
/// would silently change nothing is an error instead.
fn only_fields(
    provider_id: i32,
    auth_method: &'static str,
    update: &ProviderCredentialsUpdate,
    allowed: &[&'static str],
) -> Result<(), GitProviderManagerError> {
    let foreign: Vec<&str> = update
        .supplied()
        .into_iter()
        .filter(|name| !allowed.contains(name))
        .collect();
    if !foreign.is_empty() {
        return Err(invalid(
            provider_id,
            auth_method,
            format!(
                "field(s) {} do not apply to a {auth_method} provider; send {}",
                foreign.join(", "),
                allowed.join(" and/or ")
            ),
        ));
    }
    Ok(())
}

fn validate_username(provider_id: i32, username: &str) -> Result<(), GitProviderManagerError> {
    let reason = if username.is_empty() {
        Some("username must not be empty".to_string())
    } else if username.chars().count() > MAX_USERNAME_LEN {
        Some(format!(
            "username is longer than {MAX_USERNAME_LEN} characters"
        ))
    } else if username
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == ':')
    {
        // `:` separates username and password in HTTP basic auth.
        Some("username must not contain whitespace, control characters or ':'".to_string())
    } else {
        None
    };
    match reason {
        Some(reason) => Err(invalid(provider_id, "basic", reason)),
        None => Ok(()),
    }
}

fn validate_password(provider_id: i32, password: &str) -> Result<(), GitProviderManagerError> {
    let reason = if password.is_empty() {
        Some("password must not be empty".to_string())
    } else if password.len() > MAX_PASSWORD_LEN {
        Some(format!("password is longer than {MAX_PASSWORD_LEN} bytes"))
    } else if password.chars().any(char::is_control) {
        // A newline would split the Authorization header or the credential
        // helper's input.
        Some("password must not contain control characters (newlines included)".to_string())
    } else {
        None
    };
    match reason {
        Some(reason) => Err(invalid(provider_id, "basic", reason)),
        None => Ok(()),
    }
}

fn validate_ssh_private_key(provider_id: i32, key: &str) -> Result<(), GitProviderManagerError> {
    let key = key.trim();
    if key.len() > MAX_SSH_KEY_LEN {
        return Err(invalid(
            provider_id,
            "ssh",
            format!("private_key is longer than {MAX_SSH_KEY_LEN} bytes"),
        ));
    }
    let first = key.lines().next().unwrap_or_default().trim();
    let last = key.lines().last().unwrap_or_default().trim();
    let armored = first.starts_with("-----BEGIN ")
        && first.ends_with("PRIVATE KEY-----")
        && last.starts_with("-----END ")
        && last.ends_with("PRIVATE KEY-----")
        && key.lines().count() > 2;
    if !armored {
        return Err(invalid(
            provider_id,
            "ssh",
            "private_key must be a PEM or OpenSSH private key \
             ('-----BEGIN ... PRIVATE KEY-----' ... '-----END ... PRIVATE KEY-----')",
        ));
    }
    Ok(())
}

fn validate_ssh_public_key(provider_id: i32, key: &str) -> Result<(), GitProviderManagerError> {
    let key = key.trim();
    let well_formed = key.len() <= MAX_SSH_KEY_LEN
        && !key.contains('\n')
        && SSH_PUBLIC_KEY_PREFIXES
            .iter()
            .find_map(|prefix| key.strip_prefix(prefix))
            .and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|blob| {
                !blob.is_empty()
                    && blob
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            });
    if !well_formed {
        return Err(invalid(
            provider_id,
            "ssh",
            "public_key must be one OpenSSH public key line, such as 'ssh-ed25519 AAAA... comment'",
        ));
    }
    Ok(())
}

/// Merge `update` into the provider's current auth method, validating what
/// basic-auth and SSH-key providers are given. Returns the new auth method
/// and the names of the fields that were replaced.
pub(super) fn merge_credentials(
    provider_id: i32,
    current: AuthMethod,
    update: ProviderCredentialsUpdate,
) -> Result<(AuthMethod, Vec<&'static str>), GitProviderManagerError> {
    let supplied = update.supplied();
    let applied = |allowed: &[&'static str]| -> Vec<&'static str> {
        supplied
            .iter()
            .copied()
            .filter(|name| allowed.contains(name))
            .collect()
    };
    let merged = match current {
        AuthMethod::GitHubApp {
            app_id: existing_app_id,
            client_id: existing_client_id,
            client_secret: existing_client_secret,
            private_key: existing_private_key,
            webhook_secret: existing_webhook_secret,
        } => {
            let new_app_id = match update.app_id {
                Some(v) => v.parse::<i32>().map_err(|_| {
                    GitProviderManagerError::InvalidConfiguration(
                        "app_id must be a valid integer for GitHub App".to_string(),
                    )
                })?,
                None => existing_app_id,
            };
            (
                AuthMethod::GitHubApp {
                    app_id: new_app_id,
                    client_id: update.client_id.unwrap_or(existing_client_id),
                    client_secret: update.client_secret.unwrap_or(existing_client_secret),
                    private_key: update.private_key.unwrap_or(existing_private_key),
                    webhook_secret: update.webhook_secret.unwrap_or(existing_webhook_secret),
                },
                applied(&[
                    "app_id",
                    "client_id",
                    "client_secret",
                    "private_key",
                    "webhook_secret",
                ]),
            )
        }
        AuthMethod::GitLabApp {
            app_id: existing_app_id,
            app_secret: existing_app_secret,
            redirect_uri: existing_redirect_uri,
        } => (
            AuthMethod::GitLabApp {
                app_id: update.app_id.unwrap_or(existing_app_id),
                app_secret: update.app_secret.unwrap_or(existing_app_secret),
                redirect_uri: update.redirect_uri.unwrap_or(existing_redirect_uri),
            },
            applied(&["app_id", "app_secret", "redirect_uri"]),
        ),
        AuthMethod::OAuth {
            client_id: existing_client_id,
            client_secret: existing_client_secret,
            redirect_uri: existing_redirect_uri,
        } => (
            AuthMethod::OAuth {
                client_id: update.client_id.unwrap_or(existing_client_id),
                client_secret: update.client_secret.unwrap_or(existing_client_secret),
                redirect_uri: update.redirect_uri.unwrap_or(existing_redirect_uri),
            },
            applied(&["client_id", "client_secret", "redirect_uri"]),
        ),
        AuthMethod::PersonalAccessToken {
            token: existing_token,
        } => (
            AuthMethod::PersonalAccessToken {
                token: update.token.unwrap_or(existing_token),
            },
            applied(&["token"]),
        ),
        AuthMethod::BasicAuth {
            username: existing_username,
            password: existing_password,
        } => {
            const ALLOWED: &[&str] = &["username", "password"];
            only_fields(provider_id, "basic", &update, ALLOWED)?;
            let username = update.username.map(|u| u.trim().to_string());
            if let Some(username) = &username {
                validate_username(provider_id, username)?;
            }
            if let Some(password) = &update.password {
                validate_password(provider_id, password)?;
            }
            (
                AuthMethod::BasicAuth {
                    username: username.unwrap_or(existing_username),
                    password: update.password.unwrap_or(existing_password),
                },
                applied(ALLOWED),
            )
        }
        AuthMethod::SSHKey {
            private_key: existing_private_key,
            public_key: existing_public_key,
        } => {
            const ALLOWED: &[&str] = &["private_key", "public_key"];
            only_fields(provider_id, "ssh", &update, ALLOWED)?;
            // The pair is replaced together: a new private key with the old
            // public key (or the reverse) would leave the provider showing a
            // public key that does not match the key it authenticates with.
            match (&update.private_key, &update.public_key) {
                (Some(private_key), Some(public_key)) => {
                    validate_ssh_private_key(provider_id, private_key)?;
                    validate_ssh_public_key(provider_id, public_key)?;
                }
                (None, None) => {}
                _ => {
                    return Err(invalid(
                        provider_id,
                        "ssh",
                        "private_key and public_key are replaced together; send both",
                    ))
                }
            }
            (
                AuthMethod::SSHKey {
                    private_key: update
                        .private_key
                        .map(|k| format!("{}\n", k.trim()))
                        .unwrap_or(existing_private_key),
                    public_key: update
                        .public_key
                        .map(|k| k.trim().to_string())
                        .unwrap_or(existing_public_key),
                },
                applied(ALLOWED),
            )
        }
    };
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
        b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n\
        -----END OPENSSH PRIVATE KEY-----\n";
    const PUBLIC_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleExampleExampleExampleExampleExam deploy@example";

    fn basic() -> AuthMethod {
        AuthMethod::BasicAuth {
            username: "old-user".to_string(),
            password: "old-password".to_string(),
        }
    }

    fn ssh() -> AuthMethod {
        AuthMethod::SSHKey {
            private_key: "old-private".to_string(),
            public_key: "old-public".to_string(),
        }
    }

    fn reason(err: GitProviderManagerError) -> String {
        match err {
            GitProviderManagerError::InvalidCredentials {
                provider_id,
                reason,
                ..
            } => {
                assert_eq!(provider_id, 7, "the error names the provider");
                reason
            }
            other => panic!("expected InvalidCredentials, got {other}"),
        }
    }

    #[test]
    fn a_basic_auth_password_rotates_in_place_and_keeps_the_username() {
        let (merged, fields) = merge_credentials(
            7,
            basic(),
            ProviderCredentialsUpdate {
                password: Some("new-app-password".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(fields, vec!["password"]);
        match merged {
            AuthMethod::BasicAuth { username, password } => {
                assert_eq!(username, "old-user");
                assert_eq!(password, "new-app-password");
            }
            _ => panic!("the auth method stays basic"),
        }
    }

    #[test]
    fn a_basic_auth_username_and_password_rotate_together() {
        let (merged, fields) = merge_credentials(
            7,
            basic(),
            ProviderCredentialsUpdate {
                username: Some("  new-user ".to_string()),
                password: Some("new-password".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(fields, vec!["username", "password"]);
        assert!(matches!(
            merged,
            AuthMethod::BasicAuth { username, password }
                if username == "new-user" && password == "new-password"
        ));
    }

    #[test]
    fn invalid_basic_auth_credentials_are_refused_without_echoing_them() {
        for update in [
            ProviderCredentialsUpdate {
                username: Some("".to_string()),
                ..Default::default()
            },
            ProviderCredentialsUpdate {
                username: Some("user:name".to_string()),
                ..Default::default()
            },
            ProviderCredentialsUpdate {
                password: Some(String::new()),
                ..Default::default()
            },
            ProviderCredentialsUpdate {
                password: Some("secret-value\nInjected: header".to_string()),
                ..Default::default()
            },
            ProviderCredentialsUpdate {
                password: Some("x".repeat(MAX_PASSWORD_LEN + 1)),
                ..Default::default()
            },
        ] {
            let reason = reason(merge_credentials(7, basic(), update).unwrap_err());
            assert!(!reason.contains("secret-value"), "{reason}");
        }
    }

    #[test]
    fn fields_of_another_auth_method_are_refused_for_basic_and_ssh() {
        let reason = reason(
            merge_credentials(
                7,
                basic(),
                ProviderCredentialsUpdate {
                    token: Some("a-token".to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err(),
        );
        assert!(reason.contains("token"), "{reason}");
        assert!(!reason.contains("a-token"), "{reason}");
        assert!(merge_credentials(
            7,
            ssh(),
            ProviderCredentialsUpdate {
                password: Some("pw".to_string()),
                ..Default::default()
            },
        )
        .is_err());
    }

    #[test]
    fn an_ssh_key_pair_is_replaced_together() {
        let (merged, fields) = merge_credentials(
            7,
            ssh(),
            ProviderCredentialsUpdate {
                private_key: Some(PRIVATE_KEY.to_string()),
                public_key: Some(format!("{PUBLIC_KEY}\n")),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(fields, vec!["private_key", "public_key"]);
        match merged {
            AuthMethod::SSHKey {
                private_key,
                public_key,
            } => {
                assert_eq!(private_key, PRIVATE_KEY);
                assert_eq!(public_key, PUBLIC_KEY);
            }
            _ => panic!("the auth method stays ssh"),
        }

        let half = reason(
            merge_credentials(
                7,
                ssh(),
                ProviderCredentialsUpdate {
                    private_key: Some(PRIVATE_KEY.to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err(),
        );
        assert!(half.contains("together"), "{half}");
    }

    #[test]
    fn malformed_ssh_keys_are_refused() {
        for (private_key, public_key) in [
            ("not a key", PUBLIC_KEY),
            (
                "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----",
                PUBLIC_KEY,
            ),
            (PRIVATE_KEY, "ssh-ed25519"),
            (PRIVATE_KEY, "ssh-ed25519 not*base64"),
            (PRIVATE_KEY, "rsa AAAA"),
        ] {
            let result = merge_credentials(
                7,
                ssh(),
                ProviderCredentialsUpdate {
                    private_key: Some(private_key.to_string()),
                    public_key: Some(public_key.to_string()),
                    ..Default::default()
                },
            );
            assert!(
                matches!(
                    result,
                    Err(GitProviderManagerError::InvalidCredentials { .. })
                ),
                "{public_key}"
            );
        }
    }

    #[test]
    fn a_pat_update_reports_only_the_token() {
        let (_, fields) = merge_credentials(
            7,
            AuthMethod::PersonalAccessToken {
                token: "old".to_string(),
            },
            ProviderCredentialsUpdate {
                token: Some("new".to_string()),
                redirect_uri: Some("https://ignored.example".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            fields,
            vec!["token"],
            "fields that do not apply are not reported"
        );
    }
}
