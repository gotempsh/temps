// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `awk` filter a `pg_dumpall --clean --if-exists` dump streams through
//! when it is applied to a live server with `psql -v ON_ERROR_STOP=1`.
//!
//! `ON_ERROR_STOP` makes any failed statement fail the restore, instead of
//! `psql` reporting the error and exiting 0 with the dump half applied. But
//! `pg_dumpall` deliberately writes statements that fail on a server that
//! already runs (its own source says "it won't hurt for the CREATE to
//! fail"), and a live service has sessions the dump's `DROP DATABASE` would
//! be refused for. The filter makes exactly those statements succeed, and
//! changes nothing else:
//!
//! - `CREATE ROLE <r>;` for a role that exists is ignored
//!   (`duplicate_object`); the `ALTER ROLE` after it still sets every
//!   attribute.
//! - `DROP ROLE IF EXISTS <r>;` for the connected role (`object_in_use`:
//!   "current user cannot be dropped") or for a role that still owns objects
//!   (`dependent_objects_still_exist`) keeps the role, with a notice.
//! - Before each `DROP DATABASE` the dump writes for a database it is about
//!   to recreate, new connections to that database are refused and its
//!   sessions ended, so the drop is not refused because an application is
//!   connected. Nothing that is not about to be dropped is touched.
//!
//! Only the statements `pg_dumpall` itself writes are rewritten: role
//! statements in the globals before the first `\connect`, and a database's
//! `DROP DATABASE` in that database's header (between `-- PostgreSQL
//! database dump` and its `\connect`). `COPY` data is passed through as it
//! is, whatever it contains. The program writes only SQL, never a `psql`
//! meta-command, which a dump from PostgreSQL 17.6 or later refuses between
//! its `\restrict` and `\unrestrict`.
//!
//! When `TEMPS_KEEP_BARE` / `TEMPS_KEEP_QUOTED` are set in the environment
//! (the `ALTER ROLE <login> WITH ` prefixes, bare and quoted), the
//! `PASSWORD '...'` clause of that role's `ALTER ROLE` line is dropped, so
//! the restoring login keeps its own password (see the worker job restore in
//! `temps-backup`). Role password hashes (SCRAM, md5) hold no quote.

/// The filter program. It contains no single quote (`\047` is one), so it
/// can be passed single-quoted in a shell script or through an environment
/// variable.
pub const PG_DUMPALL_RESTORE_FILTER_AWK: &str = r#"function lit(s) { gsub("\047", "\047\047", s); return "\047" s "\047" }
BEGIN { bare = ENVIRON["TEMPS_KEEP_BARE"]; quoted = ENVIRON["TEMPS_KEEP_QUOTED"] }
copy && /^\\\.$/ { copy = 0; print; next }
copy { print; next }
/^COPY .* FROM stdin;$/ { copy = 1; print; next }
/^-- PostgreSQL database dump$/ { header = 1 }
/^\\connect / { globals_done = 1; header = 0 }
(!globals_done || header) && /^DROP DATABASE (IF EXISTS )?.+;$/ {
  name = $0; sub(/^DROP DATABASE (IF EXISTS )?/, "", name); sub(/;$/, "", name)
  if (name ~ /^".*"$/) { name = substr(name, 2, length(name) - 2); gsub("\"\"", "\"", name) }
  print "UPDATE pg_catalog.pg_database SET datallowconn = false WHERE datname = " lit(name) ";"
  print "SELECT pg_catalog.pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity WHERE datname = " lit(name) " AND pid <> pg_catalog.pg_backend_pid();"
  print; next
}
!globals_done && /^DROP ROLE IF EXISTS .+;$/ && index($0, "$temps$") == 0 {
  print "DO $temps$ BEGIN " $0 " EXCEPTION WHEN object_in_use OR dependent_objects_still_exist THEN RAISE NOTICE \047temps: kept an existing role: %\047, SQLERRM; END $temps$;"; next
}
!globals_done && /^CREATE ROLE .+;$/ && index($0, "$temps$") == 0 {
  print "DO $temps$ BEGIN " $0 " EXCEPTION WHEN duplicate_object THEN NULL; END $temps$;"; next
}
!globals_done && ((bare != "" && index($0, bare) == 1) || (quoted != "" && index($0, quoted) == 1)) { sub(/ PASSWORD \047[^\047]*\047/, "") }
{ print }
"#;

/// The first line `pg_dumpall` writes (after its leading `--`): what tells a
/// whole-cluster dump, which the filter is for, from a single database's
/// `pg_dump`.
pub const PG_DUMPALL_HEADER: &str = "-- PostgreSQL database cluster dump";

/// Whether `head` (the start of a plain-format dump) is a `pg_dumpall`
/// dump.
pub fn is_cluster_dump(head: &str) -> bool {
    head.lines()
        .take(8)
        .any(|line| line.trim_end() == PG_DUMPALL_HEADER)
}

/// The `ALTER ROLE` line prefixes `pg_dumpall` writes for `role`, bare and
/// quoted (it quotes a name that is not a plain lower-case identifier, or is
/// a keyword): the values of `TEMPS_KEEP_BARE` and `TEMPS_KEEP_QUOTED`.
pub fn alter_role_prefixes(role: &str) -> (String, String) {
    (
        format!("ALTER ROLE {role} WITH "),
        format!("ALTER ROLE \"{}\" WITH ", role.replace('"', "\"\"")),
    )
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    /// Run the filter with the host's `awk` (`None` when it has none).
    fn filter(dump: &str, keep: Option<&str>) -> Option<String> {
        use std::io::Write;
        let awk = ["/usr/bin/awk", "/bin/awk"]
            .into_iter()
            .find(|path| std::path::Path::new(path).exists())?;
        let mut command = std::process::Command::new(awk);
        command
            .arg(PG_DUMPALL_RESTORE_FILTER_AWK)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        if let Some(role) = keep {
            let (bare, quoted) = alter_role_prefixes(role);
            command
                .env("TEMPS_KEEP_BARE", bare)
                .env("TEMPS_KEEP_QUOTED", quoted);
        }
        let mut child = command.spawn().expect("running awk");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(dump.as_bytes())
            .expect("writing the dump");
        let output = child.wait_with_output().expect("awk output");
        assert!(output.status.success(), "awk failed: {:?}", output.status);
        Some(String::from_utf8(output.stdout).expect("utf-8"))
    }

    /// The shape `pg_dumpall --clean --if-exists` writes, with a table whose
    /// rows look like the statements the filter rewrites.
    const DUMP: &str = "--\n-- PostgreSQL database cluster dump\n--\n\n\
        \\restrict k1\n\n\
        DROP DATABASE IF EXISTS \"We'ird \"\"Db\";\n\
        DROP DATABASE IF EXISTS orders;\n\n\
        DROP ROLE IF EXISTS app;\n\
        DROP ROLE IF EXISTS postgres;\n\n\
        CREATE ROLE app;\n\
        ALTER ROLE app WITH LOGIN PASSWORD 'md5aaaa';\n\
        CREATE ROLE postgres;\n\
        ALTER ROLE postgres WITH SUPERUSER LOGIN PASSWORD 'SCRAM-SHA-256$4096:c2FsdA==$c3RvcmVk:c2VydmVy';\n\n\
        \\unrestrict k1\n\
        \\connect template1\n\
        --\n-- PostgreSQL database dump\n--\n\n\
        \\restrict k2\n\
        DROP DATABASE IF EXISTS postgres;\n\
        CREATE DATABASE postgres;\n\
        \\unrestrict k2\n\
        \\connect postgres\n\
        \\restrict k3\n\
        COPY public.notes (body) FROM stdin;\n\
        DROP DATABASE IF EXISTS orders;\n\
        CREATE ROLE app;\n\
        \\.\n\
        CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$\n\
        DROP ROLE IF EXISTS app;\n\
        $$;\n\
        \\unrestrict k3\n";

    #[test]
    fn the_statements_a_live_server_refuses_are_made_to_succeed_and_nothing_else() {
        let Some(out) = filter(DUMP, None) else {
            println!("no awk on this host, skipping");
            return;
        };
        // Each database the dump recreates is closed to new sessions and its
        // sessions ended just before its drop, quoted names included.
        assert!(out.contains(
            "UPDATE pg_catalog.pg_database SET datallowconn = false WHERE datname = 'We''ird \"Db';\n\
             SELECT pg_catalog.pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity WHERE datname = 'We''ird \"Db' AND pid <> pg_catalog.pg_backend_pid();\n\
             DROP DATABASE IF EXISTS \"We'ird \"\"Db\";\n"
        ), "{out}");
        assert!(out.contains(
            "WHERE datname = 'postgres' AND pid <> pg_catalog.pg_backend_pid();\nDROP DATABASE IF EXISTS postgres;\n"
        ), "a database's own header drop is prepared too: {out}");
        assert_eq!(
            out.matches("pg_terminate_backend").count(),
            3,
            "only the drops pg_dumpall writes: {out}"
        );
        // The expected role errors are tolerated, only in the globals.
        assert!(out.contains(
            "DO $temps$ BEGIN DROP ROLE IF EXISTS postgres; EXCEPTION WHEN object_in_use OR dependent_objects_still_exist"
        ), "{out}");
        assert!(out.contains(
            "DO $temps$ BEGIN CREATE ROLE app; EXCEPTION WHEN duplicate_object THEN NULL; END $temps$;"
        ), "{out}");
        assert_eq!(out.matches("DO $temps$").count(), 4, "{out}");
        // Rows and function bodies are passed through untouched, and no
        // password is dropped when no login is kept.
        assert!(out.contains(
            "COPY public.notes (body) FROM stdin;\nDROP DATABASE IF EXISTS orders;\nCREATE ROLE app;\n\\.\n"
        ), "{out}");
        assert!(
            out.contains("AS $$\nDROP ROLE IF EXISTS app;\n$$;"),
            "{out}"
        );
        assert!(out.contains("PASSWORD 'md5aaaa'"), "{out}");
        assert!(out.contains("c2VydmVy"), "{out}");
        // No psql meta-command is added: a 17.6+ dump refuses them between
        // \restrict and \unrestrict.
        assert_eq!(
            out.lines().filter(|l| l.starts_with('\\')).count(),
            DUMP.lines().filter(|l| l.starts_with('\\')).count(),
            "{out}"
        );
    }

    #[test]
    fn a_kept_login_loses_only_its_own_dumped_password() {
        let Some(out) = filter(DUMP, Some("postgres")) else {
            println!("no awk on this host, skipping");
            return;
        };
        assert!(
            out.contains("ALTER ROLE postgres WITH SUPERUSER LOGIN;\n"),
            "{out}"
        );
        assert!(!out.contains("c2VydmVy"), "{out}");
        assert!(out.contains("PASSWORD 'md5aaaa'"), "{out}");
    }

    #[test]
    fn only_a_cluster_dump_is_filtered() {
        assert!(is_cluster_dump(DUMP));
        assert!(!is_cluster_dump(
            "--\n-- PostgreSQL database dump\n--\n\nCREATE TABLE t (n int);\n"
        ));
        assert!(!is_cluster_dump("CREATE TABLE plain_items (n int);\n"));
    }
}
