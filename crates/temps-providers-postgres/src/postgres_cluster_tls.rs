// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed server TLS on the data nodes of a pg_auto_failover cluster
//! (ADR 093).
//!
//! Every data node is created with `pg_autoctl … --ssl-self-signed`, so it
//! already serves TLS with a certificate no client can verify, and accepts
//! plaintext too (`host` rules sit beside the `hostssl` ones). Managed TLS
//! swaps that certificate for one the managed-services CA issued, on the
//! running member and without a restart:
//!
//! 1. the certificate and key arrive in the exec environment (never in the
//!    container's configuration) and are written to [`CLUSTER_TLS_DIR`],
//!    outside `PGDATA`, so a standby's base backup never copies them;
//! 2. `pg_autoctl config set ssl.cert_file / ssl.key_file` records the paths
//!    in pg_autoctl's own configuration, on the member's volume, which
//!    pg_autoctl turns into `ssl_cert_file` / `ssl_key_file` at every start;
//! 3. a reload makes PostgreSQL load the new pair (it reloads its TLS context
//!    on SIGHUP), and the script checks the certificate the member actually
//!    serves before reporting success.
//!
//! Turning it off points pg_autoctl back at its self-signed pair in `PGDATA`.
//! pg_autoctl's own connections to the monitor and its peers keep
//! `sslmode=require`, so replication and failover never depend on which
//! certificate a member serves.

/// Directory of a data node's managed certificate and key, on the member's
/// persistent volume and outside `PGDATA`.
pub const CLUSTER_TLS_DIR: &str = "/var/lib/postgresql/tls";

/// Exec environment variable carrying the certificate PEM.
pub const CLUSTER_TLS_CERT_ENV: &str = "TEMPS_TLS_CERT_PEM";

/// Exec environment variable carrying the private key PEM.
pub const CLUSTER_TLS_KEY_ENV: &str = "TEMPS_TLS_KEY_PEM";

/// Prefix of the line the scripts print with the served certificate's
/// SHA-256 fingerprint (lowercase hex, no separators).
pub const SERVED_FINGERPRINT_MARKER: &str = "temps-served-fingerprint=";

/// Exit code of the apply script when the member is not an initialized data
/// node yet: `pg_autoctl create` is still running (a standby taking its
/// first base backup), or its keeper is not running.
pub const NOT_INITIALIZED_EXIT_CODE: i64 = 10;

/// Exit code of the read-only status script when PostgreSQL is not accepting
/// connections, so what it serves cannot be read.
pub const NOT_ACCEPTING_EXIT_CODE: i64 = 11;

/// Which certificate a data node should serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterServerCertificate {
    /// The managed certificate passed in the exec environment.
    Managed,
    /// pg_auto_failover's own self-signed certificate in `PGDATA`.
    SelfSigned,
}

/// Shell function printing the SHA-256 fingerprint of the certificate the
/// local member serves, in the same form as
/// `temps_core::managed_service_pki::certificate_fingerprint_sha256`.
const SERVED_FINGERPRINT_FN: &str = r#"served_fingerprint() {
  echo | timeout 5 openssl s_client -starttls postgres -connect "127.0.0.1:$NODE_PORT" 2>/dev/null \
    | openssl x509 -noout -fingerprint -sha256 2>/dev/null \
    | sed 's/^.*=//; s/://g' | tr 'A-F' 'a-f'
}"#;

const PREAMBLE: &str = r#"set -e
trap 'echo "temps: server TLS step stopped at line $LINENO: $BASH_COMMAND" >&2' ERR
PGDATA=/var/lib/postgresql/pgdata
CONF="$PGDATA/postgresql-auto-failover.conf"
if [ -z "${NODE_PORT:-}" ]; then
  echo "temps: NODE_PORT is not set in this container; it is not a pg_auto_failover data node" >&2
  exit 2
fi
if ! gosu postgres pg_autoctl config get --pgdata "$PGDATA" postgresql.pgdata >/dev/null 2>&1; then
  echo "temps: $PGDATA is not an initialized pg_auto_failover data node yet" >&2
  exit 10
fi
# `pg_autoctl create` holds its configuration in memory and writes it when it
# finishes, which would undo a `config set` made meanwhile: act only once the
# keeper (`pg_autoctl run`) runs and no `create` does. Patterns are anchored
# at the start of the command line, so this shell (`bash -c …`, whose script
# contains them as text) never matches itself.
KEEPER=""
for CMDLINE in /proc/[0-9]*/cmdline; do
  case "$(tr '\0' ' ' < "$CMDLINE" 2>/dev/null)" in
    "pg_autoctl create "*|/*"/pg_autoctl create "*)
      echo "temps: pg_autoctl is still creating this data node" >&2
      exit 10
      ;;
    "pg_autoctl run "*|/*"/pg_autoctl run "*) KEEPER=1 ;;
  esac
done
if [ -z "$KEEPER" ]; then
  echo "temps: the pg_autoctl keeper is not running on this data node" >&2
  exit 10
fi"#;

const INSTALL_MANAGED: &str = r#"if [ -z "${TEMPS_TLS_CERT_PEM:-}" ] || [ -z "${TEMPS_TLS_KEY_PEM:-}" ]; then
  echo "temps: the managed certificate or key was not passed to the member" >&2
  exit 2
fi
umask 077
install -d -o postgres -g postgres -m 0700 "$TLS_DIR"
printf '%s\n' "$TEMPS_TLS_CERT_PEM" > "$TLS_DIR/server.crt.new"
printf '%s\n' "$TEMPS_TLS_KEY_PEM" > "$TLS_DIR/server.key.new"
chown postgres:postgres "$TLS_DIR/server.crt.new" "$TLS_DIR/server.key.new"
chmod 0644 "$TLS_DIR/server.crt.new"
chmod 0600 "$TLS_DIR/server.key.new"
if [ "$(openssl x509 -in "$TLS_DIR/server.crt.new" -noout -pubkey)" != "$(openssl pkey -in "$TLS_DIR/server.key.new" -pubout)" ]; then
  rm -f "$TLS_DIR/server.crt.new" "$TLS_DIR/server.key.new"
  echo "temps: the managed certificate does not match its private key; nothing was changed" >&2
  exit 3
fi
mv -f "$TLS_DIR/server.key.new" "$TLS_DIR/server.key"
mv -f "$TLS_DIR/server.crt.new" "$TLS_DIR/server.crt"
CERT="$TLS_DIR/server.crt"
KEY="$TLS_DIR/server.key""#;

const SELECT_SELF_SIGNED: &str = r#"CERT="$PGDATA/server.crt"
KEY="$PGDATA/server.key"
if [ ! -f "$CERT" ] || [ ! -f "$KEY" ]; then
  echo "temps: pg_auto_failover's self-signed certificate is missing from $PGDATA; the managed certificate stays in place" >&2
  exit 4
fi"#;

/// pg_autoctl reloads its configuration on `config set`, and a second change
/// arriving during that reload can be missed, so the reload is repeated until
/// PostgreSQL's settings and the served certificate both match.
const APPLY_AND_VERIFY: &str = r#"gosu postgres pg_autoctl config set --pgdata "$PGDATA" ssl.cert_file "$CERT" >/dev/null
gosu postgres pg_autoctl config set --pgdata "$PGDATA" ssl.key_file "$KEY" >/dev/null
EXPECTED=$(openssl x509 -in "$CERT" -noout -fingerprint -sha256 | sed 's/^.*=//; s/://g' | tr 'A-F' 'a-f')
for _ in $(seq 1 30); do
  if grep -qxF "ssl_cert_file = '$CERT'" "$CONF" && grep -qxF "ssl_key_file = '$KEY'" "$CONF" \
    && [ "$(served_fingerprint)" = "$EXPECTED" ]; then
    APPLIED=1
    break
  fi
  gosu postgres pg_autoctl reload --pgdata "$PGDATA" >/dev/null 2>&1 || true
  sleep 2
done
if [ -z "${APPLIED:-}" ]; then
  echo "temps: PostgreSQL on port $NODE_PORT does not serve certificate $EXPECTED after 60s; it serves '$(served_fingerprint)' with $(grep -E '^ssl_(cert|key)_file' "$CONF" | tr '\n' ' ')" >&2
  exit 5
fi"#;

/// The command that makes a running data node serve `target`. Run as root
/// in the member (`docker exec` locally, the agent on a worker); for
/// [`ClusterServerCertificate::Managed`] the exec environment carries
/// [`CLUSTER_TLS_CERT_ENV`] and [`CLUSTER_TLS_KEY_ENV`]. Idempotent. On
/// success it prints [`SERVED_FINGERPRINT_MARKER`] with the fingerprint now
/// served; it exits [`NOT_INITIALIZED_EXIT_CODE`] on a member that is not
/// an initialized data node yet, and non-zero with the reason otherwise.
/// How long the apply script may run inside the data node before it and
/// every process it started are killed. The control plane waits longer than
/// this plus [`MEMBER_KILL_GRACE_SECS`], so when it gives up on a member the
/// script is already gone: a Docker exec cannot be killed from outside, and
/// a script left running could switch the certificate after a newer pass.
pub const MEMBER_APPLY_DEADLINE_SECS: u64 = 90;
/// The same bound for the read-only status script.
pub const MEMBER_STATUS_DEADLINE_SECS: u64 = 8;
/// Seconds between the polite stop at a deadline and the forced kill.
pub const MEMBER_KILL_GRACE_SECS: u64 = 5;

/// `script` run by bash under GNU `timeout`, which signals the whole process
/// group it starts (bash, pg_autoctl, openssl), not only bash.
fn bounded_bash(deadline_secs: u64, script: String) -> Vec<String> {
    vec![
        "timeout".to_string(),
        "-k".to_string(),
        MEMBER_KILL_GRACE_SECS.to_string(),
        deadline_secs.to_string(),
        "bash".to_string(),
        "-c".to_string(),
        script,
    ]
}

pub fn server_tls_apply_command(target: ClusterServerCertificate) -> Vec<String> {
    let mut script = vec![
        PREAMBLE.to_string(),
        format!("TLS_DIR={CLUSTER_TLS_DIR}"),
        SERVED_FINGERPRINT_FN.to_string(),
    ];
    match target {
        ClusterServerCertificate::Managed => script.push(INSTALL_MANAGED.to_string()),
        ClusterServerCertificate::SelfSigned => script.push(SELECT_SELF_SIGNED.to_string()),
    }
    script.push(APPLY_AND_VERIFY.to_string());
    if target == ClusterServerCertificate::SelfSigned {
        // Nothing points at the managed files any more.
        script.push(r#"rm -rf "$TLS_DIR""#.to_string());
    }
    script.push(format!(r#"echo "{SERVED_FINGERPRINT_MARKER}$EXPECTED""#));
    bounded_bash(MEMBER_APPLY_DEADLINE_SECS, script.join("\n"))
}

/// The command printing the fingerprint of the certificate a data node
/// serves right now (an empty value when it serves none). Read-only.
pub fn served_certificate_command() -> Vec<String> {
    let script = [
        "set -e".to_string(),
        r#"if [ -z "${NODE_PORT:-}" ]; then echo "temps: NODE_PORT is not set" >&2; exit 2; fi"#
            .to_string(),
        format!(
            r#"if ! pg_isready -q -h 127.0.0.1 -p "$NODE_PORT"; then echo "temps: PostgreSQL is not accepting connections on port $NODE_PORT" >&2; exit {NOT_ACCEPTING_EXIT_CODE}; fi"#
        ),
        SERVED_FINGERPRINT_FN.to_string(),
        format!(r#"echo "{SERVED_FINGERPRINT_MARKER}$(served_fingerprint)""#),
    ];
    bounded_bash(MEMBER_STATUS_DEADLINE_SECS, script.join("\n"))
}

/// The served fingerprint a script printed: `Some("")` when the member
/// serves no certificate, `None` when no marker line was printed.
pub fn parse_served_fingerprint(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(SERVED_FINGERPRINT_MARKER))
        .map(|value| value.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(target: ClusterServerCertificate) -> String {
        let cmd = server_tls_apply_command(target);
        assert_eq!(cmd[4..6], ["bash".to_string(), "-c".to_string()]);
        cmd[6].clone()
    }

    #[test]
    fn managed_certificate_is_written_outside_pgdata_with_a_private_key() {
        let s = script(ClusterServerCertificate::Managed);
        assert!(s.contains("TLS_DIR=/var/lib/postgresql/tls"));
        assert!(s.contains("install -d -o postgres -g postgres -m 0700"));
        assert!(s.contains(r#"chmod 0600 "$TLS_DIR/server.key.new""#));
        assert!(s.contains("umask 077"));
        // Written whole, then renamed into place.
        assert!(s.contains(r#"mv -f "$TLS_DIR/server.key.new" "$TLS_DIR/server.key""#));
        // A mismatched pair is refused before anything is switched.
        let check = s.find("does not match its private key").unwrap();
        assert!(check < s.find("pg_autoctl config set").unwrap());
        assert!(!s.contains("rm -rf \"$TLS_DIR\""));
    }

    #[test]
    fn the_key_never_appears_in_the_command_itself() {
        for target in [
            ClusterServerCertificate::Managed,
            ClusterServerCertificate::SelfSigned,
        ] {
            let s = script(target);
            assert!(!s.contains("BEGIN"), "{s}");
            assert!(!s.contains("set -x"), "{s}");
        }
        let s = script(ClusterServerCertificate::Managed);
        assert!(s.contains("$TEMPS_TLS_KEY_PEM"));
        assert_eq!(CLUSTER_TLS_KEY_ENV, "TEMPS_TLS_KEY_PEM");
        assert_eq!(CLUSTER_TLS_CERT_ENV, "TEMPS_TLS_CERT_PEM");
    }

    #[test]
    fn both_paths_go_through_pg_autoctl_and_are_verified_on_the_wire() {
        for target in [
            ClusterServerCertificate::Managed,
            ClusterServerCertificate::SelfSigned,
        ] {
            let s = script(target);
            assert!(
                s.contains("pg_autoctl config set --pgdata \"$PGDATA\" ssl.cert_file \"$CERT\"")
            );
            assert!(s.contains("pg_autoctl config set --pgdata \"$PGDATA\" ssl.key_file \"$KEY\""));
            assert!(s.contains("pg_autoctl reload"));
            assert!(s.contains("timeout 5 openssl s_client -starttls postgres"));
            assert!(s.contains(r#"[ "$(served_fingerprint)" = "$EXPECTED" ]"#));
            assert!(s.ends_with(r#"echo "temps-served-fingerprint=$EXPECTED""#));
            // Never touches HBA rules: plaintext clients keep connecting.
            assert!(!s.contains("pg_hba"));
            assert!(!s.contains("hostnossl"));
        }
    }

    #[test]
    fn turning_off_restores_the_self_signed_pair_and_removes_the_managed_files() {
        let s = script(ClusterServerCertificate::SelfSigned);
        assert!(s.contains(r#"CERT="$PGDATA/server.crt""#));
        assert!(s.contains(r#"KEY="$PGDATA/server.key""#));
        assert!(!s.contains("TEMPS_TLS_KEY_PEM"));
        let applied = s.find("APPLIED=1").unwrap();
        let removed = s.find(r#"rm -rf "$TLS_DIR""#).unwrap();
        assert!(removed > applied, "files are removed only once unused");
    }

    #[test]
    fn nothing_is_changed_while_pg_autoctl_create_runs() {
        let s = script(ClusterServerCertificate::Managed);
        let guard = s
            .find(r#""pg_autoctl create "*|/*"/pg_autoctl create "*)"#)
            .unwrap();
        assert!(guard < s.find("install -d").unwrap());
        assert!(s.contains(r#""pg_autoctl run "*|/*"/pg_autoctl run "*) KEEPER=1 ;;"#));
        // Never a leading wildcard: it would match this script's own shell.
        assert!(!s.contains(r#"*"pg_autoctl"#));
        assert!(s.contains("the pg_autoctl keeper is not running"));
        assert_eq!(s.matches("exit 10").count(), 3);
    }

    #[test]
    fn status_reports_a_member_that_is_not_accepting_connections() {
        let status = &served_certificate_command()[6];
        let ready = status.find("pg_isready -q -h 127.0.0.1").unwrap();
        assert!(ready < status.find("served_fingerprint)").unwrap());
        assert!(status.contains("exit 11"));
    }

    #[test]
    fn every_member_script_dies_at_its_own_deadline() {
        for (cmd, deadline) in [
            (
                server_tls_apply_command(ClusterServerCertificate::Managed),
                MEMBER_APPLY_DEADLINE_SECS,
            ),
            (
                server_tls_apply_command(ClusterServerCertificate::SelfSigned),
                MEMBER_APPLY_DEADLINE_SECS,
            ),
            (served_certificate_command(), MEMBER_STATUS_DEADLINE_SECS),
        ] {
            assert_eq!(
                cmd[..4],
                [
                    "timeout".to_string(),
                    "-k".to_string(),
                    MEMBER_KILL_GRACE_SECS.to_string(),
                    deadline.to_string(),
                ],
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn served_fingerprint_is_parsed_from_the_last_marker() {
        assert_eq!(
            parse_served_fingerprint("noise\ntemps-served-fingerprint=ABcd\n"),
            Some("abcd".to_string())
        );
        assert_eq!(
            parse_served_fingerprint("temps-served-fingerprint=\n"),
            Some(String::new())
        );
        assert_eq!(parse_served_fingerprint("nothing here"), None);
        let status = served_certificate_command();
        assert!(status[6].contains("openssl s_client -starttls postgres"));
        assert!(!status[6].contains("config set"));
    }
}

#[cfg(all(test, feature = "docker-tests"))]
mod docker_tests;
