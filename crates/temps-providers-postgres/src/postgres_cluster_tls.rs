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
//!    container's configuration) and are written whole into one of two slot
//!    directories ([`CLUSTER_TLS_SLOTS`]), outside `PGDATA`, so a standby's
//!    base backup never copies them;
//! 2. [`CLUSTER_TLS_LINK`] is switched to that slot by one rename, so the
//!    certificate and key it names change together: no reload or restart
//!    ever finds one file of the new pair beside one of the old;
//! 3. pg_autoctl's `ssl.cert_file / ssl.key_file`, on the member's volume,
//!    name the files through the link, so a renewal never changes them;
//!    they are moved onto the link once, each step through a copy of the
//!    pair already served, so every intermediate setting is a matching pair;
//! 4. a reload makes PostgreSQL load the new pair (it reloads its TLS context
//!    on SIGHUP), and the script checks the certificate the member actually
//!    serves before reporting success.
//!
//! Turning it off switches the link to a copy of pg_autoctl's self-signed
//! pair in `PGDATA`, points the settings back at that pair (again one
//! matching step at a time), then removes the link and both slots.
//! pg_autoctl's own connections to the monitor and its peers keep
//! `sslmode=require`, so replication and failover never depend on which
//! certificate a member serves.

/// Symlink naming the slot of the pair a data node serves, on the member's
/// persistent volume and outside `PGDATA`; pg_autoctl's settings name
/// `server.crt` / `server.key` through it.
pub const CLUSTER_TLS_LINK: &str = "/var/lib/postgresql/tls-current";

/// The two slot directories a pair is written into: always the one the
/// link does not name, so the pair in use is never rewritten.
pub const CLUSTER_TLS_SLOTS: [&str; 2] = ["/var/lib/postgresql/tls-a", "/var/lib/postgresql/tls-b"];

/// A directory an earlier build wrote the pair into in place. Removed once
/// nothing points at it.
const LEGACY_CLUSTER_TLS_DIR: &str = "/var/lib/postgresql/tls";

/// Exec environment variable carrying the certificate PEM.
pub const CLUSTER_TLS_CERT_ENV: &str = "TEMPS_TLS_CERT_PEM";

/// Exec environment variable carrying the private key PEM.
pub const CLUSTER_TLS_KEY_ENV: &str = "TEMPS_TLS_KEY_PEM";

/// Prefix of the line the scripts print with the served certificate's
/// SHA-256 fingerprint (lowercase hex, no separators).
pub const SERVED_FINGERPRINT_MARKER: &str = "temps-served-fingerprint=";

/// Exit code of the apply script when the member is not an initialized data
/// node yet: `pg_autoctl create` is still running (a standby taking its
/// first base backup), its keeper is not running, or its PostgreSQL is not
/// accepting connections (the keeper is still bringing it up, for example
/// after the node rebooted).
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
fi
# A keeper whose PostgreSQL is down is bringing it up (a former primary
# rejoining after its node rebooted). It can serve no certificate, and
# signalling it to reload only gets in its way: change nothing until
# PostgreSQL accepts connections, and let a later pass apply.
if ! pg_isready -q -h 127.0.0.1 -p "$NODE_PORT"; then
  echo "temps: PostgreSQL on port $NODE_PORT is not accepting connections yet; nothing was changed" >&2
  exit 10
fi"#;

/// Shell helpers for the slots and the link. `config set` is pg_autoctl's
/// own command: it rewrites its configuration and reloads PostgreSQL.
const SLOT_FNS: &str = r#"LINK_CERT="$LINK/server.crt"
LINK_KEY="$LINK/server.key"
# Whether a certificate and key exist and belong together.
pair_matches() {
  [ -f "$1" ] && [ -f "$2" ] || return 1
  PUB=$(openssl x509 -in "$1" -noout -pubkey 2>/dev/null) || return 1
  [ -n "$PUB" ] && [ "$PUB" = "$(openssl pkey -in "$2" -pubout 2>/dev/null)" ]
}
# The slot the link does not name: the one a new pair goes into.
other_slot() {
  case "$(readlink "$LINK" 2>/dev/null || true)" in
    "$(basename "$SLOT_A")") echo "$SLOT_B" ;;
    *) echo "$SLOT_A" ;;
  esac
}
# A complete, private copy of a pair in a slot no setting names.
fill_slot() {
  rm -rf "$1"
  install -d -o postgres -g postgres -m 0700 "$1"
  install -o postgres -g postgres -m 0644 "$2" "$1/server.crt"
  install -o postgres -g postgres -m 0600 "$3" "$1/server.key"
}
# Switch the link to a slot in one rename: both files change together.
# Never to a slot whose certificate and key do not belong together.
point_link_at() {
  if ! pair_matches "$1/server.crt" "$1/server.key"; then
    rm -rf "$1"
    echo "temps: the certificate and key prepared in $1 do not match; the served pair was left as it was" >&2
    exit 4
  fi
  ln -sfn "$(basename "$1")" "$LINK.next"
  chown -h postgres:postgres "$LINK.next"
  mv -Tf "$LINK.next" "$LINK"
}
set_pair() {
  gosu postgres pg_autoctl config set --pgdata "$PGDATA" ssl.cert_file "$1" >/dev/null
  gosu postgres pg_autoctl config set --pgdata "$PGDATA" ssl.key_file "$2" >/dev/null
}
# One pg_autoctl setting. The keeper rewrites its configuration while it
# reloads, so a failed read is retried, and one that keeps failing stops the
# script: guessing a value could pair one pair's certificate with another's key.
read_setting() {
  for _ in 1 2 3 4 5; do
    if VALUE=$(gosu postgres pg_autoctl config get --pgdata "$PGDATA" "$1" 2>/dev/null); then
      printf '%s' "$VALUE"
      return 0
    fi
    sleep 1
  done
  return 1
}
if ! CUR_CERT=$(read_setting ssl.cert_file) || ! CUR_KEY=$(read_setting ssl.key_file); then
  echo "temps: pg_autoctl's certificate settings could not be read on this data node; nothing was changed" >&2
  exit 6
fi
if [ -z "$CUR_CERT" ] && [ -z "$CUR_KEY" ]; then
  CUR_CERT="$PGDATA/server.crt"
  CUR_KEY="$PGDATA/server.key"
elif [ -z "$CUR_CERT" ] || [ -z "$CUR_KEY" ]; then
  echo "temps: pg_autoctl names only one of a certificate ('$CUR_CERT') and a key ('$CUR_KEY') on this data node; nothing was changed" >&2
  exit 6
fi
# Name the files through the link. A copy of the pair served now goes into
# a slot first, so each of the two settings, set one after the other, names
# a file of that same pair: no step leaves a certificate beside another key.
move_onto_link() {
  if [ "$CUR_CERT" = "$LINK_CERT" ] && [ "$CUR_KEY" = "$LINK_KEY" ]; then
    return 0
  fi
  if ! pair_matches "$CUR_CERT" "$CUR_KEY"; then
    echo "temps: the certificate pair this data node is configured with ($CUR_CERT, $CUR_KEY) is missing or does not match; nothing was changed" >&2
    exit 4
  fi
  SLOT=$(other_slot)
  fill_slot "$SLOT" "$CUR_CERT" "$CUR_KEY"
  point_link_at "$SLOT"
  set_pair "$LINK_CERT" "$LINK_KEY"
  CUR_CERT="$LINK_CERT"
  CUR_KEY="$LINK_KEY"
}"#;

const INSTALL_MANAGED: &str = r#"if [ -z "${TEMPS_TLS_CERT_PEM:-}" ] || [ -z "${TEMPS_TLS_KEY_PEM:-}" ]; then
  echo "temps: the managed certificate or key was not passed to the member" >&2
  exit 2
fi
umask 077
STAGING="$TLS_HOME/tls-staging"
rm -rf "$STAGING"
install -d -o root -g root -m 0700 "$STAGING"
printf '%s\n' "$TEMPS_TLS_CERT_PEM" > "$STAGING/server.crt"
printf '%s\n' "$TEMPS_TLS_KEY_PEM" > "$STAGING/server.key"
if ! pair_matches "$STAGING/server.crt" "$STAGING/server.key"; then
  rm -rf "$STAGING"
  echo "temps: the managed certificate does not match its private key; nothing was changed" >&2
  exit 3
fi
move_onto_link
SLOT=$(other_slot)
fill_slot "$SLOT" "$STAGING/server.crt" "$STAGING/server.key"
rm -rf "$STAGING"
point_link_at "$SLOT"
CERT="$LINK_CERT"
KEY="$LINK_KEY""#;

const SELECT_SELF_SIGNED: &str = r#"CERT="$PGDATA/server.crt"
KEY="$PGDATA/server.key"
if ! pair_matches "$CERT" "$KEY"; then
  echo "temps: pg_auto_failover's self-signed certificate is missing from $PGDATA or does not match its key; the managed certificate stays in place" >&2
  exit 4
fi
if [ "$CUR_CERT" != "$CERT" ] || [ "$CUR_KEY" != "$KEY" ]; then
  # Serve a copy of the self-signed pair through the link first: the two
  # settings then move from one copy of that pair to the other.
  move_onto_link
  SLOT=$(other_slot)
  fill_slot "$SLOT" "$CERT" "$KEY"
  point_link_at "$SLOT"
fi"#;

/// pg_autoctl reloads its configuration on `config set`, and a second change
/// arriving during that reload can be missed, so the keeper is asked to
/// reload again, at most every [`APPLY_RELOAD_EVERY_POLLS`] polls (10 s),
/// until PostgreSQL's settings and the served certificate both match.
///
/// A PostgreSQL that stops accepting connections meanwhile ends the script
/// with [`NOT_INITIALIZED_EXIT_CODE`] and no further signal: its keeper is
/// restarting it, and a later pass finishes once it is back.
const APPLY_AND_VERIFY: &str = r#"gosu postgres pg_autoctl config set --pgdata "$PGDATA" ssl.cert_file "$CERT" >/dev/null
gosu postgres pg_autoctl config set --pgdata "$PGDATA" ssl.key_file "$KEY" >/dev/null
EXPECTED=$(openssl x509 -in "$CERT" -noout -fingerprint -sha256 | sed 's/^.*=//; s/://g' | tr 'A-F' 'a-f')
for POLL in $(seq 1 30); do
  if grep -qxF "ssl_cert_file = '$CERT'" "$CONF" && grep -qxF "ssl_key_file = '$KEY'" "$CONF" \
    && [ "$(served_fingerprint)" = "$EXPECTED" ]; then
    APPLIED=1
    break
  fi
  if ! pg_isready -q -h 127.0.0.1 -p "$NODE_PORT"; then
    echo "temps: PostgreSQL on port $NODE_PORT stopped accepting connections while its certificate was being applied; its keeper is not signalled again, a later pass finishes" >&2
    exit 10
  fi
  if [ $((POLL % RELOAD_EVERY)) -eq 0 ]; then
    gosu postgres pg_autoctl reload --pgdata "$PGDATA" >/dev/null 2>&1 || true
  fi
  sleep 2
done
if [ -z "${APPLIED:-}" ]; then
  echo "temps: PostgreSQL on port $NODE_PORT does not serve certificate $EXPECTED after 60s; it serves '$(served_fingerprint)' with $(grep -E '^ssl_(cert|key)_file' "$CONF" | tr '\n' ' ')" >&2
  exit 5
fi"#;

/// Polls (2 s apart) between two reload signals of the apply script.
pub const APPLY_RELOAD_EVERY_POLLS: u32 = 5;

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

/// The command that makes a running data node serve `target`. Run as root
/// in the member (`docker exec` locally, the agent on a worker); for
/// [`ClusterServerCertificate::Managed`] the exec environment carries
/// [`CLUSTER_TLS_CERT_ENV`] and [`CLUSTER_TLS_KEY_ENV`]. Idempotent. On
/// success it prints [`SERVED_FINGERPRINT_MARKER`] with the fingerprint now
/// served; it exits [`NOT_INITIALIZED_EXIT_CODE`] on a member that is not
/// an initialized data node yet, and non-zero with the reason otherwise.
pub fn server_tls_apply_command(target: ClusterServerCertificate) -> Vec<String> {
    let [slot_a, slot_b] = CLUSTER_TLS_SLOTS;
    let mut script = vec![
        PREAMBLE.to_string(),
        "TLS_HOME=/var/lib/postgresql".to_string(),
        format!("LINK={CLUSTER_TLS_LINK}"),
        format!("SLOT_A={slot_a}"),
        format!("SLOT_B={slot_b}"),
        format!("LEGACY_DIR={LEGACY_CLUSTER_TLS_DIR}"),
        SERVED_FINGERPRINT_FN.to_string(),
        format!("RELOAD_EVERY={APPLY_RELOAD_EVERY_POLLS}"),
        SLOT_FNS.to_string(),
    ];
    match target {
        ClusterServerCertificate::Managed => script.push(INSTALL_MANAGED.to_string()),
        ClusterServerCertificate::SelfSigned => script.push(SELECT_SELF_SIGNED.to_string()),
    }
    script.push(APPLY_AND_VERIFY.to_string());
    script.push(match target {
        // Only the slot the link names is in use.
        ClusterServerCertificate::Managed => r#"rm -rf "$(other_slot)" "$LEGACY_DIR""#
            .to_string(),
        // Nothing points at the link or the slots any more.
        ClusterServerCertificate::SelfSigned => {
            r#"rm -rf "$LINK" "$LINK.next" "$SLOT_A" "$SLOT_B" "$TLS_HOME/tls-staging" "$LEGACY_DIR""#
                .to_string()
        }
    });
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
        assert!(s.contains("LINK=/var/lib/postgresql/tls-current"));
        assert!(s.contains("SLOT_A=/var/lib/postgresql/tls-a"));
        assert!(s.contains("SLOT_B=/var/lib/postgresql/tls-b"));
        assert!(s.contains(r#"install -d -o postgres -g postgres -m 0700 "$1""#));
        assert!(s.contains(r#"install -o postgres -g postgres -m 0600 "$3" "$1/server.key""#));
        assert!(s.contains("umask 077"));
        // A mismatched pair is refused before anything is switched.
        let check = s.find("does not match its private key").unwrap();
        assert!(check < s.find("\nmove_onto_link\n").unwrap());
        assert!(check < s.find("\nfill_slot \"$SLOT\"").unwrap());
        // Only the slot no longer named is removed; the link stays.
        assert!(s.contains(r#"rm -rf "$(other_slot)" "$LEGACY_DIR""#));
        assert!(!s.contains(r#"rm -rf "$LINK""#));
    }

    /// The pair in use is never rewritten in place, and the certificate and
    /// key always switch together: the new pair is written whole into the
    /// slot the link does not name, then the link is renamed onto it.
    #[test]
    fn a_new_pair_switches_in_one_rename() {
        let s = script(ClusterServerCertificate::Managed);
        assert!(!s.contains("server.key.new") && !s.contains("server.crt.new"));
        assert!(s.contains(r#"ln -sfn "$(basename "$1")" "$LINK.next""#));
        assert!(s.contains(r#"mv -Tf "$LINK.next" "$LINK""#));
        assert_eq!(
            s.matches("mv ").count(),
            1,
            "the link rename is the only move: {s}"
        );
        let fill = s
            .find("fill_slot \"$SLOT\" \"$STAGING/server.crt\"")
            .unwrap();
        let switch = s[fill..].find("point_link_at \"$SLOT\"").unwrap() + fill;
        let verify = s.find("EXPECTED=").unwrap();
        assert!(fill < switch && switch < verify);
        // pg_autoctl's settings name the files through the link.
        assert!(s.contains(r#"CERT="$LINK_CERT""#));
        assert!(s.contains(r#"KEY="$LINK_KEY""#));
    }

    /// A setting that cannot be read is never guessed: a guess could pair
    /// one pair's certificate with another's key. And the link is never
    /// switched to, nor a setting moved to, a pair whose halves differ.
    #[test]
    fn unreadable_settings_and_mismatched_pairs_change_nothing() {
        for target in [
            ClusterServerCertificate::Managed,
            ClusterServerCertificate::SelfSigned,
        ] {
            let s = script(target);
            assert!(!s.contains("ssl.cert_file 2>/dev/null || true"), "{s}");
            assert!(s.contains(
                r#"if ! CUR_CERT=$(read_setting ssl.cert_file) || ! CUR_KEY=$(read_setting ssl.key_file); then"#
            ));
            let read = s.find("could not be read on this data node").unwrap();
            let one_only = s.find("names only one of a certificate").unwrap();
            let first_change = s
                .find("\nmove_onto_link\n")
                .or_else(|| s.find("  move_onto_link\n"))
                .unwrap();
            assert!(read < first_change && one_only < first_change, "{s}");
            // Both default together, never one at a time.
            assert!(s.contains(r#"if [ -z "$CUR_CERT" ] && [ -z "$CUR_KEY" ]; then"#));

            let switch = &s[s.find("point_link_at() {").unwrap()..];
            let check = switch
                .find(r#"if ! pair_matches "$1/server.crt" "$1/server.key"; then"#)
                .unwrap();
            assert!(check < switch.find("ln -sfn").unwrap(), "{switch}");
            assert!(s.contains(r#"if ! pair_matches "$CUR_CERT" "$CUR_KEY"; then"#));
        }
        let s = script(ClusterServerCertificate::SelfSigned);
        assert!(s.contains(r#"if ! pair_matches "$CERT" "$KEY"; then"#));
    }

    /// Moving the settings onto the link (or back to `PGDATA`) changes them
    /// one at a time, each time to a copy of the pair already served.
    #[test]
    fn the_settings_move_through_a_copy_of_the_served_pair() {
        let s = script(ClusterServerCertificate::Managed);
        let body = &s[s.find("move_onto_link() {").unwrap()..];
        let body = &body[..body.find("\n}").unwrap()];
        let copy = body
            .find(r#"fill_slot "$SLOT" "$CUR_CERT" "$CUR_KEY""#)
            .unwrap();
        let switch = body.find(r#"point_link_at "$SLOT""#).unwrap();
        let set = body.find(r#"set_pair "$LINK_CERT" "$LINK_KEY""#).unwrap();
        assert!(copy < switch && switch < set, "{body}");
        assert!(body.contains("exit 4"));

        let s = script(ClusterServerCertificate::SelfSigned);
        let select = &s[s.find("\nCERT=\"$PGDATA/server.crt\"").unwrap()..];
        let copy = select.find(r#"fill_slot "$SLOT" "$CERT" "$KEY""#).unwrap();
        let switch = select.find(r#"point_link_at "$SLOT""#).unwrap();
        let set = select.find("pg_autoctl config set").unwrap();
        assert!(copy < switch && switch < set, "{select}");
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
        let removed = s
            .find(r#"rm -rf "$LINK" "$LINK.next" "$SLOT_A" "$SLOT_B""#)
            .unwrap();
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
        assert_eq!(s.matches("exit 10").count(), 5);
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

    /// What one run of the apply script did on a data node whose keeper
    /// runs, with stand-ins for `gosu`, `pg_isready` and `openssl`.
    struct ApplyRun {
        exit_code: Option<i32>,
        stderr: String,
        /// Every `gosu` command line, in order.
        gosu_calls: Vec<String>,
    }

    impl ApplyRun {
        fn count(&self, needle: &str) -> usize {
            self.gosu_calls
                .iter()
                .filter(|c| c.contains(needle))
                .count()
        }
    }

    /// Runs the self-signed apply script on a member already configured with
    /// pg_auto_failover's pair, so it goes straight to applying. PostgreSQL
    /// accepts connections for the first `ready_checks` `pg_isready` calls
    /// and is down after them; it serves `served` once up. `None` when bash
    /// is unavailable.
    fn run_apply(ready_checks: u32, served: &str) -> Option<ApplyRun> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let pgdata = root.join("pgdata");
        std::fs::create_dir(&pgdata).unwrap();
        std::fs::write(pgdata.join("server.crt"), "cert").unwrap();
        std::fs::write(pgdata.join("server.key"), "key").unwrap();
        std::fs::write(
            pgdata.join("postgresql-auto-failover.conf"),
            format!(
                "ssl_cert_file = '{0}/server.crt'\nssl_key_file = '{0}/server.key'\n",
                pgdata.display()
            ),
        )
        .unwrap();
        // The keeper runs (`pg_autoctl run`), no `pg_autoctl create` does.
        let proc_dir = root.join("proc/41");
        std::fs::create_dir_all(&proc_dir).unwrap();
        std::fs::write(
            proc_dir.join("cmdline"),
            "pg_autoctl\0run\0--pgdata\0/var/lib/postgresql/pgdata\0",
        )
        .unwrap();
        let tools = [
            (
                "gosu",
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$GOSU_LOG"
case "$*" in
  *"config get"*"ssl.cert_file"*) printf '%s' "$PGDATA_DIR/server.crt" ;;
  *"config get"*"ssl.key_file"*) printf '%s' "$PGDATA_DIR/server.key" ;;
esac
exit 0
"#,
            ),
            (
                "pg_isready",
                r#"#!/bin/sh
CALLS=$(cat "$READY_COUNT" 2>/dev/null || echo 0)
CALLS=$((CALLS + 1))
echo "$CALLS" > "$READY_COUNT"
[ "$CALLS" -le "$READY_CHECKS" ]
"#,
            ),
            (
                "openssl",
                r#"#!/bin/sh
case "$*" in
  *"-pubkey"*|*"-pubout"*) echo PUBLIC-KEY ;;
  "x509 -in "*"-fingerprint"*) echo "sha256 Fingerprint=AB:CD" ;;
  *"-fingerprint"*) cat >/dev/null; echo "sha256 Fingerprint=$SERVED" ;;
  *s_client*) ;;
esac
"#,
            ),
            ("timeout", "#!/bin/sh\nshift\nexec \"$@\"\n"),
            ("sleep", "#!/bin/sh\nexit 0\n"),
        ];
        for (name, body) in tools {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let script = script(ClusterServerCertificate::SelfSigned)
            .replace(
                "/proc/[0-9]*/cmdline",
                &format!("{}/proc/[0-9]*/cmdline", root.display()),
            )
            .replace("/var/lib/postgresql/pgdata", pgdata.to_str().unwrap())
            .replace("/var/lib/postgresql", root.to_str().unwrap());
        let gosu_log = root.join("gosu.log");
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("NODE_PORT", "6201")
            .env("PGDATA_DIR", &pgdata)
            .env("GOSU_LOG", &gosu_log)
            .env("READY_COUNT", root.join("ready.count"))
            .env("READY_CHECKS", ready_checks.to_string())
            .env("SERVED", served)
            .output()
            .ok()?;
        Some(ApplyRun {
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            gosu_calls: std::fs::read_to_string(&gosu_log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect(),
        })
    }

    /// Regression: a former primary rejoining after its node rebooted has a
    /// running keeper but no PostgreSQL yet. The TLS reconcile used to set
    /// its certificate and signal its keeper to reload every 2 s for a
    /// minute, pass after pass. It must change nothing, send no signal, and
    /// report the member as initializing.
    #[test]
    fn a_member_whose_postgres_is_down_is_neither_changed_nor_signalled() {
        let Some(run) = run_apply(0, "AB:CD") else {
            eprintln!("bash unavailable; skipping");
            return;
        };
        assert_eq!(
            run.exit_code,
            Some(NOT_INITIALIZED_EXIT_CODE as i32),
            "{}",
            run.stderr
        );
        assert!(
            run.stderr.contains("not accepting connections yet"),
            "{}",
            run.stderr
        );
        assert_eq!(run.count("config set"), 0, "{:?}", run.gosu_calls);
        assert_eq!(run.count("reload"), 0, "{:?}", run.gosu_calls);
    }

    /// PostgreSQL going down while the certificate is applied ends the
    /// script without signalling the keeper again.
    #[test]
    fn a_member_whose_postgres_stops_mid_apply_is_not_signalled_again() {
        // Up for the gate, down at the first poll.
        let Some(run) = run_apply(1, "00:00") else {
            eprintln!("bash unavailable; skipping");
            return;
        };
        assert_eq!(
            run.exit_code,
            Some(NOT_INITIALIZED_EXIT_CODE as i32),
            "{}",
            run.stderr
        );
        assert!(
            run.stderr.contains("stopped accepting connections"),
            "{}",
            run.stderr
        );
        assert_eq!(run.count("reload"), 0, "{:?}", run.gosu_calls);
    }

    /// A member that is up but does not serve the certificate yet is asked
    /// to reload at most every 10 s, not on every 2 s poll.
    #[test]
    fn a_member_that_does_not_serve_the_certificate_yet_is_reloaded_sparingly() {
        let Some(run) = run_apply(u32::MAX, "00:00") else {
            eprintln!("bash unavailable; skipping");
            return;
        };
        assert_eq!(run.exit_code, Some(5), "{}", run.stderr);
        assert_eq!(run.count("config set"), 2, "{:?}", run.gosu_calls);
        assert_eq!(
            run.count("reload") as u32,
            30 / APPLY_RELOAD_EVERY_POLLS,
            "{:?}",
            run.gosu_calls
        );

        let Some(run) = run_apply(u32::MAX, "AB:CD") else {
            return;
        };
        assert_eq!(run.exit_code, Some(0), "{}", run.stderr);
        assert_eq!(run.count("reload"), 0, "{:?}", run.gosu_calls);
    }
}

#[cfg(all(test, feature = "docker-tests"))]
mod docker_tests;
