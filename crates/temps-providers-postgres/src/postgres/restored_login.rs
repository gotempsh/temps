// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Setting the service's configured login inside restored physical data.
//!
//! A WAL-G base backup carries its origin server's roles and passwords. When
//! the backup comes from a server whose login Temps does not hold, the
//! restored server rejects the destination's configured login, and the
//! restore cannot be verified. Before verifying, Temps connects over the
//! container's local socket (which the image trusts) and sets the configured
//! user's password, creating the user when the backup has none by that name.
//! Every other role keeps the password it had.
//!
//! The password reaches PostgreSQL as a SCRAM-SHA-256 verifier computed here,
//! so the statement never carries it in plain text, even if the restored
//! configuration logs every statement. The session also turns statement
//! logging off before the change.

use super::*;
use crate::externalsvc::exec_util::run_exec_as;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tracing::info;

/// Iterations PostgreSQL uses for the verifiers it computes itself.
const SCRAM_ITERATIONS: u32 = 4096;

/// How long the restored server may take to accept local connections and
/// finish recovery before the login step gives up.
const RESTORED_LOGIN_WAIT_SECONDS: u32 = 90;

/// Prefix of the lines the login script prints, one per restored login role.
const RESTORED_ROLE_LINE: &str = "temps-restored-role ";

/// Prefix of the line that ends the login script: `set`, `no-local-login`
/// or `still-recovering`. A failed SQL statement exits non-zero instead.
const OUTCOME_LINE: &str = "temps-login-outcome ";

/// Waits for the restored server to accept a local connection (as the
/// configured user, `postgres` or a role named in the restored catalogs, a
/// superuser preferred) and leave recovery, then sets the
/// configured user's password from `$TEMPS_LOGIN_SECRET`, creating the user
/// as a superuser when the backup has none by that name. Prints every role
/// that can log in, prefixed with [`RESTORED_ROLE_LINE`], then its
/// [`OUTCOME_LINE`]. Values arrive in the environment and as psql variables,
/// never in the script text.
const SET_LOGIN_SCRIPT: &str = r#"set -u
err=$(mktemp)
deadline=$(( $(date +%s) + TEMPS_LOGIN_WAIT_SECONDS ))
# Role names to sign in as over the trusted socket, one per line: the
# configured user, postgres, then the names stored in the restored shared
# catalogs, pg_authid (global/1260) first. A backup from another server keeps
# that server's superuser, whose name nothing else records. A name is a run of
# printable (or UTF-8) bytes, kept whole so a quoted name such as
# `origin-admin` is tried as it is; its alphanumeric pieces follow, for a name
# the bytes next to it in the page ran into.
catalog_names() {
  { cat "$PGDATA/global/1260" 2>/dev/null; cat "$PGDATA"/global/* 2>/dev/null; } \
    | LC_ALL=C tr -c '\40-\176\200-\377' '\n'
}
candidates() {
  printf '%s\n' "$TEMPS_LOGIN_ROLE" postgres
  if [ -n "${PGDATA:-}" ] && [ -d "$PGDATA/global" ]; then
    { catalog_names | LC_ALL=C awk 'length($0) <= 63';
      catalog_names | LC_ALL=C tr -c 'A-Za-z0-9_\n' '\n' \
        | grep -E '^[A-Za-z_][A-Za-z0-9_]{0,62}$'; } \
      | grep -v -e '^pg_' -e '^$' | LC_ALL=C awk '!seen[$0]++' | head -n 300
  fi
}
# Prints "<in recovery> <superuser>" (`f t`) for a role that signs in.
probe() {
  psql -X -w -qAt -F ' ' -U "$1" -d template1 \
    -c 'SELECT pg_is_in_recovery(), rolsuper FROM pg_roles WHERE rolname = current_user' 2>"$err"
}
who=
state=
while :; do
  if [ -n "$who" ]; then
    out=$(probe "$who") || out=
    state=${out%% *}
  elif pg_isready -q -d template1 >"$err" 2>&1; then
    # A superuser can change any role's password; any other role that signs
    # in is kept as the fallback.
    fallback=
    names=$(mktemp)
    candidates >"$names"
    while IFS= read -r candidate; do
      if out=$(probe "$candidate"); then
        case "$out" in
          *" t") who=$candidate; state=${out%% *}; break ;;
          *) [ -n "$fallback" ] || { fallback=$candidate; fallback_state=${out%% *}; } ;;
        esac
      fi
    done <"$names"
    rm -f "$names"
    if [ -z "$who" ] && [ -n "$fallback" ]; then
      who=$fallback
      state=$fallback_state
    fi
  else
    pg_isready -d template1 >"$err" 2>&1
  fi
  if [ -n "$who" ] && [ "$state" = f ]; then break; fi
  if [ "$(date +%s)" -ge "$deadline" ]; then
    if [ -n "$who" ]; then
      rm -f "$err"
      echo "temps-login-outcome still-recovering"
      exit 0
    fi
    tail -c 1000 "$err"
    rm -f "$err"
    echo "temps-login-outcome no-local-login"
    exit 0
  fi
  sleep 1
done
rm -f "$err"
psql -X -w -qAt -v ON_ERROR_STOP=1 -U "$who" -d template1 \
  -v role="$TEMPS_LOGIN_ROLE" -v secret="$TEMPS_LOGIN_SECRET" <<'SQL' || exit $?
SET log_statement = 'none';
SET log_min_duration_statement = -1;
SET log_min_duration_sample = -1;
SET log_transaction_sample_rate = 0;
SET log_min_error_statement = 'panic';
SET pg_stat_statements.track_utility = off;
SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'role')
  THEN format('ALTER ROLE %I WITH LOGIN PASSWORD %L', :'role', :'secret')
  ELSE format('CREATE ROLE %I WITH SUPERUSER LOGIN PASSWORD %L', :'role', :'secret')
END
\gexec
SELECT 'temps-restored-role ' || rolname FROM pg_roles
  WHERE rolcanlogin AND rolname !~ '^pg_' ORDER BY rolname;
SQL
echo "temps-login-outcome set"
"#;

/// The login step could not make the restored data accept the configured
/// login.
#[derive(Debug, thiserror::Error)]
#[error("Could not set the login of PostgreSQL service '{service_name}' (user '{username}') inside the restored data in container '{container}': {reason}")]
pub(super) struct RestoredLoginError {
    pub(super) service_name: String,
    pub(super) username: String,
    pub(super) container: String,
    pub(super) reason: String,
}

impl From<RestoredLoginError> for EngineError {
    fn from(error: RestoredLoginError) -> Self {
        EngineError::failed(error.service_name.clone(), error.to_string())
    }
}

/// A SCRAM-SHA-256 verifier for `password`, in the form PostgreSQL stores in
/// `pg_authid.rolpassword` and accepts in `ALTER ROLE ... PASSWORD`:
/// `SCRAM-SHA-256$<iterations>:<salt>$<StoredKey>:<ServerKey>` (RFC 5802,
/// RFC 7677).
///
/// PostgreSQL normalises a password with SASLprep before hashing it, which
/// changes nothing for ASCII. A non-ASCII password returns `None`, and the
/// caller lets the server hash it instead.
pub(super) fn scram_sha256_verifier(
    password: &str,
    salt: &[u8],
    iterations: u32,
) -> Option<String> {
    if !password.is_ascii() || iterations == 0 {
        return None;
    }
    let salted = pbkdf2_hmac_sha256(password.as_bytes(), salt, iterations)?;
    let client_key = hmac_sha256(&salted, b"Client Key")?;
    let stored_key = Sha256::digest(client_key);
    let server_key = hmac_sha256(&salted, b"Server Key")?;
    Some(format!(
        "SCRAM-SHA-256${iterations}:{}${}:{}",
        BASE64.encode(salt),
        BASE64.encode(stored_key),
        BASE64.encode(server_key),
    ))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Option<[u8; 32]> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).ok()?;
    mac.update(message);
    Some(mac.finalize().into_bytes().into())
}

/// PBKDF2 with HMAC-SHA-256 for one 32-byte block (RFC 8018 §5.2), the
/// length SCRAM-SHA-256 needs.
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Option<[u8; 32]> {
    let mut first = Vec::with_capacity(salt.len() + 4);
    first.extend_from_slice(salt);
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut block = hmac_sha256(password, &first)?;
    let mut result = block;
    for _ in 1..iterations {
        block = hmac_sha256(password, &block)?;
        for (out, byte) in result.iter_mut().zip(block.iter()) {
            *out ^= byte;
        }
    }
    Some(result)
}

/// What the login step sends as the new password: a verifier for an ASCII
/// password, or the password itself for the server to hash (see
/// [`scram_sha256_verifier`]).
fn login_secret(password: &str) -> std::result::Result<String, String> {
    let mut salt = [0u8; 16];
    use rand::TryRng;
    rand::rngs::SysRng
        .try_fill_bytes(&mut salt)
        .map_err(|error| format!("could not generate a password salt: {error}"))?;
    Ok(scram_sha256_verifier(password, &salt, SCRAM_ITERATIONS)
        .unwrap_or_else(|| password.to_string()))
}

/// The roles the login script reported, in its order.
fn restored_roles(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.trim_end().strip_prefix(RESTORED_ROLE_LINE))
        .map(str::to_string)
        .collect()
}

/// The script with its wait bound filled in.
fn set_login_script() -> String {
    SET_LOGIN_SCRIPT.replace(
        "TEMPS_LOGIN_WAIT_SECONDS",
        &RESTORED_LOGIN_WAIT_SECONDS.to_string(),
    )
}

impl PostgresService {
    /// Set `config`'s user and password inside the restored data running in
    /// `container_name`, so the restored server accepts the login Temps holds
    /// for this service. Returns every role in the restored data that can log
    /// in; all of them except `config.username` keep their passwords from the
    /// backup.
    pub(super) async fn set_configured_login(
        &self,
        container_name: &str,
        config: &PostgresConfig,
    ) -> std::result::Result<Vec<String>, RestoredLoginError> {
        let failed = |reason: String| RestoredLoginError {
            service_name: self.name.clone(),
            username: config.username.clone(),
            container: container_name.to_string(),
            reason,
        };
        let secret = login_secret(&config.password).map_err(failed)?;
        let env = vec![
            format!("TEMPS_LOGIN_ROLE={}", config.username),
            format!("TEMPS_LOGIN_SECRET={secret}"),
        ];
        let script = set_login_script();
        let result = run_exec_as(
            &self.docker,
            container_name,
            vec!["sh".to_string(), "-c".to_string(), script],
            Some(env),
            Duration::from_secs(u64::from(RESTORED_LOGIN_WAIT_SECONDS) + 60),
            Some("postgres"),
        )
        .await;
        let output = result.map_err(|error| failed(error.to_string()))?.output;
        match login_outcome(&output) {
            Some("set") => {
                let roles = restored_roles(&output);
                info!(
                    service = %self.name,
                    container = container_name,
                    username = %config.username,
                    restored_roles = roles.len(),
                    "Set the configured login inside the restored PostgreSQL data"
                );
                Ok(roles)
            }
            Some("still-recovering") => Err(failed(format!(
                "the restored server had not finished recovery after \
                 {RESTORED_LOGIN_WAIT_SECONDS} seconds"
            ))),
            Some("no-local-login") => Err(failed(format!(
                "the restored server did not accept a local connection as '{}', 'postgres' \
                 or any role stored in the backup within {RESTORED_LOGIN_WAIT_SECONDS} seconds: \
                 the server did not start, or the backup's authentication settings do not \
                 trust local connections. Set this service's username and \
                 password to the ones of the server that made the backup, then retry. \
                 PostgreSQL said: {}",
                config.username,
                output
                    .lines()
                    .filter(|line| !line.starts_with(OUTCOME_LINE))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim()
            ))),
            other => Err(failed(format!(
                "the login script ended without a result ({}): {}",
                other.unwrap_or("no outcome line"),
                output.trim()
            ))),
        }
    }
}

/// The script's outcome word, from its last [`OUTCOME_LINE`].
fn login_outcome(output: &str) -> Option<&str> {
    output
        .lines()
        .rev()
        .find_map(|line| line.trim_end().strip_prefix(OUTCOME_LINE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_matches_a_reference_scram_implementation() {
        // Expected value from an independent implementation (Python's
        // hashlib.pbkdf2_hmac and hmac over the same password and salt).
        assert_eq!(
            scram_sha256_verifier("secret-a", b"0123456789abcdef", 4096).as_deref(),
            Some(
                "SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$\
                 RczqJhbsuR+a5xbuqSpdrFpgBCrxaJavR0+cCzasHQs=:\
                 vfdQmMD6XK60a85qvxxOOVj4kPoQAjRhXt3ZwL76I0g="
            )
        );
    }

    #[test]
    fn non_ascii_passwords_are_left_to_the_server_to_hash() {
        assert_eq!(scram_sha256_verifier("contraseña", b"salt", 4096), None);
        assert_eq!(login_secret("contraseña").as_deref(), Ok("contraseña"));
        assert!(login_secret("plain-ascii")
            .expect("salt")
            .starts_with("SCRAM-SHA-256$4096:"));
    }

    #[test]
    fn pbkdf2_matches_the_rfc_7914_test_vector() {
        // RFC 7914 §11: PBKDF2-HMAC-SHA256, P="passwd", S="salt", c=1, first 32 bytes.
        let derived = pbkdf2_hmac_sha256(b"passwd", b"salt", 1).expect("hmac");
        assert_eq!(
            hex_string(&derived),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
    }

    #[test]
    fn the_last_outcome_line_decides_the_result() {
        assert_eq!(login_outcome("x\ntemps-login-outcome set\n"), Some("set"));
        assert_eq!(
            login_outcome("FATAL: role does not exist\ntemps-login-outcome no-local-login"),
            Some("no-local-login")
        );
        assert_eq!(login_outcome("psql: error"), None);
    }

    #[test]
    fn restored_roles_are_read_from_marked_lines_only() {
        let output = "temps-restored-role app\nNOTICE: something\ntemps-restored-role postgres\n";
        assert_eq!(restored_roles(output), vec!["app", "postgres"]);
    }

    #[test]
    fn login_script_carries_no_values_and_disables_statement_logging() {
        let script = set_login_script();
        assert!(!script.contains("TEMPS_LOGIN_WAIT_SECONDS"));
        // Every channel that can write the statement to the server log, or
        // keep its text in pg_stat_statements, is off before the secret.
        let secret_at = script.find(":'secret'").expect("secret is used");
        for setting in [
            "SET log_statement = 'none'",
            "SET log_min_duration_statement = -1",
            "SET log_min_duration_sample = -1",
            "SET log_transaction_sample_rate = 0",
            "SET log_min_error_statement = 'panic'",
            "SET pg_stat_statements.track_utility = off",
        ] {
            let at = script.find(setting).unwrap_or(usize::MAX);
            assert!(at < secret_at, "{setting} must come before the secret");
        }
        assert!(script.contains(":'secret'"));
        assert!(script.contains("\"$TEMPS_LOGIN_SECRET\""));
    }

    fn hex_string(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Runs `script` in `container` as `postgres`; `Ok` with its output when
    /// it exits zero.
    async fn sh(
        docker: &Docker,
        container: &str,
        script: &str,
    ) -> std::result::Result<String, String> {
        run_exec_as(
            docker,
            container,
            vec!["sh".to_string(), "-c".to_string(), script.to_string()],
            None,
            Duration::from_secs(60),
            Some("postgres"),
        )
        .await
        .map(|result| result.output)
        .map_err(|error| error.to_string())
    }

    /// Whether `role` signs in with `password` over TCP, from the
    /// container's own address: the image trusts the socket and loopback,
    /// so only this checks the password.
    async fn tcp_login(docker: &Docker, container: &str, role: &str, password: &str) -> bool {
        let script = format!(
            "PGPASSWORD='{password}' psql -X -w -qAt -h \"$(hostname -i | cut -d' ' -f1)\" \
             -U {role} -d template1 -c 'SELECT 1'"
        );
        sh(docker, container, &script)
            .await
            .is_ok_and(|output| output.trim() == "1")
    }

    #[tokio::test]
    async fn docker_sets_only_the_configured_login_inside_restored_data() {
        use bollard::models::{ContainerCreateBody, HostConfig};
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StartContainerOptions,
        };

        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => Arc::new(docker),
            Err(error) => {
                println!("Docker not available, skipping: {error}");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }
        let image = "gotempsh/postgres-walg:18-bookworm";
        if docker.inspect_image(image).await.is_err() {
            println!("{image} is not available locally, skipping");
            return;
        }

        let container = format!("temps_restored_login_test_{}", rand::random::<u32>());
        docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&container)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    env: Some(vec![
                        "POSTGRES_USER=origin-admin".to_string(),
                        "POSTGRES_PASSWORD=origin-secret".to_string(),
                        "POSTGRES_DB=appdb".to_string(),
                    ]),
                    host_config: Some(HostConfig {
                        auto_remove: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create the test container");

        let outcome = async {
            docker
                .start_container(&container, None::<StartContainerOptions>)
                .await
                .map_err(|error| format!("start: {error}"))?;
            // Ready once the final server (not the init one, which listens on
            // the socket only) accepts TCP connections.
            let mut ready = false;
            for _ in 0..90 {
                if sh(
                    &docker,
                    &container,
                    "pg_isready -q -h \"$(hostname -i | cut -d' ' -f1)\" -U origin-admin",
                )
                .await
                .is_ok()
                {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            if !ready {
                return Err("the test server never accepted TCP connections".to_string());
            }
            sh(
                &docker,
                &container,
                "psql -X -qAt -U origin-admin -d appdb -c \"CREATE ROLE reader LOGIN PASSWORD 'reader-secret'\"",
            )
            .await?;

            let service = PostgresService::new("restored-login-test".to_string(), docker.clone());
            let mut config: PostgresConfig = serde_json::from_value(serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "appdb",
                "username": "origin-admin",
                "password": "temps-managed-secret",
                "max_connections": 100,
                "docker_image": image,
            }))
            .map_err(|error| format!("config: {error}"))?;

            // An existing role: its password changes, nothing else does.
            let roles = service
                .set_configured_login(&container, &config)
                .await
                .map_err(|error| error.to_string())?;
            assert!(roles.contains(&"origin-admin".to_string()), "{roles:?}");
            assert!(roles.contains(&"reader".to_string()), "{roles:?}");
            assert!(tcp_login(&docker, &container, "origin-admin", "temps-managed-secret").await);
            assert!(!tcp_login(&docker, &container, "origin-admin", "origin-secret").await);
            assert!(tcp_login(&docker, &container, "reader", "reader-secret").await);

            // A role the backup does not have, with no `postgres` role either:
            // the script finds the backup's superuser, whose name has a
            // hyphen, in the restored catalog, and creates the role.
            config.username = "manager".to_string();
            config.password = "manager-secret".to_string();
            service
                .set_configured_login(&container, &config)
                .await
                .map_err(|error| error.to_string())?;
            assert!(tcp_login(&docker, &container, "manager", "manager-secret").await);
            let superuser = sh(
                &docker,
                &container,
                "psql -X -qAt -U origin-admin -d appdb -c \"SELECT rolsuper FROM pg_roles WHERE rolname = 'manager'\"",
            )
            .await?;
            assert_eq!(superuser.trim(), "t");
            // The stored value is a verifier, never the password itself.
            let stored = sh(
                &docker,
                &container,
                "psql -X -qAt -U origin-admin -d appdb -c \"SELECT rolpassword FROM pg_authid WHERE rolname = 'manager'\"",
            )
            .await?;
            assert!(stored.trim().starts_with("SCRAM-SHA-256$4096:"), "{stored}");
            Ok::<(), String>(())
        }
        .await;

        let _ = docker
            .remove_container(
                &container,
                Some(
                    RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await;
        if let Err(error) = outcome {
            panic!("{error}");
        }
    }
}
