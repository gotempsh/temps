// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn conflict(fqdn: &str, owner_id: i64) -> DnsNameConflict {
    DnsNameConflict {
        fqdn: fqdn.to_string(),
        owner_kind: "environment".to_string(),
        owner_id,
        holder_kind: "service_role".to_string(),
        holder_id: 4,
    }
}

#[test]
fn dns_name_conflicts_pass_when_none() {
    assert!(matches!(
        dns_name_conflicts_result(&[], 20),
        CheckResult::Pass(_)
    ));
}

#[test]
fn dns_name_conflicts_warn_names_both_owners() {
    let CheckResult::Warn(message) =
        dns_name_conflicts_result(&[conflict("primary.foo.temps.local", 12)], 20)
    else {
        panic!("a conflict must warn");
    };
    assert!(
        message.contains("'primary.foo.temps.local' is held by service_role 4")
            && message.contains("environment 12 cannot publish it"),
        "{message}"
    );
    assert!(!message.contains("and more"), "{message}");
}

#[test]
fn dns_name_conflicts_truncate_past_the_limit() {
    let rows: Vec<_> = (0..3)
        .map(|i| conflict(&format!("n{i}.temps.local"), i))
        .collect();
    let CheckResult::Warn(message) = dns_name_conflicts_result(&rows, 2) else {
        panic!("conflicts must warn");
    };
    assert!(message.contains("n1.temps.local") && !message.contains("n2.temps.local"));
    assert!(message.contains("and more"), "{message}");
}

fn lock_capacity(max_locks: i64, chunks: &[(&str, i64)]) -> temps_schema::LockCapacity {
    temps_schema::LockCapacity {
        max_locks_per_transaction: max_locks,
        max_connections: 100,
        max_prepared_transactions: 0,
        timescaledb: true,
        hypertables: chunks
            .iter()
            .map(
                |(hypertable, chunks)| temps_schema::lock_capacity::HypertableChunks {
                    hypertable: hypertable.to_string(),
                    chunks: *chunks,
                },
            )
            .collect(),
    }
}

#[test]
fn lock_capacity_passes_with_room_to_spare() {
    let result = lock_capacity_result(Ok(&lock_capacity(64, &[("events", 120)])));
    assert!(
        matches!(&result, CheckResult::Pass(message) if message.contains("6400")),
        "{result:?}"
    );
}

#[test]
fn lock_capacity_warns_with_the_value_to_set_and_the_restart() {
    let result = lock_capacity_result(Ok(&lock_capacity(
        64,
        &[("proxy_logs", 1_460), ("events", 1_460)],
    )));
    let CheckResult::Warn(message) = result else {
        panic!("expected a warning, got {result:?}");
    };
    assert!(message.contains("2920 TimescaleDB chunk(s)"), "{message}");
    assert!(message.contains("at least 384"), "{message}");
    assert!(message.contains("restart PostgreSQL"), "{message}");
    assert!(message.contains("before installing"), "{message}");
}

#[test]
fn lock_capacity_without_timescaledb_is_informational() {
    let mut capacity = lock_capacity(64, &[]);
    capacity.timescaledb = false;
    assert!(matches!(
        lock_capacity_result(Ok(&capacity)),
        CheckResult::Info(_)
    ));
    assert!(matches!(
        lock_capacity_result(Err(&"permission denied".to_string())),
        CheckResult::Warn(_)
    ));
}

#[test]
fn telemetry_foreign_keys_warn_until_dropped() {
    assert!(matches!(
        telemetry_foreign_keys_result(Ok(&Vec::new())),
        CheckResult::Pass(_)
    ));
    let remaining = vec![temps_schema::TelemetryForeignKey {
        table: "events".to_string(),
        constraint: "fk_events_project_id".to_string(),
        parent: "projects".to_string(),
    }];
    let CheckResult::Warn(message) = telemetry_foreign_keys_result(Ok(&remaining)) else {
        panic!("expected a warning");
    };
    assert!(message.contains("events.fk_events_project_id"), "{message}");
    assert!(message.contains("project deletes are refused"), "{message}");
}

#[test]
fn migration_check_reports_every_schema_state() {
    use temps_schema::SchemaStatus;
    let names = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();

    assert!(matches!(
        migration_check_result(Ok(&SchemaStatus::UpToDate { applied: 12 })),
        CheckResult::Pass(ref msg) if msg == "12 applied, up to date"
    ));
    assert!(matches!(
        migration_check_result(Ok(&SchemaStatus::PendingUpgrade {
            applied: 12,
            pending: names(&["m_next"]),
        })),
        CheckResult::Warn(ref msg) if msg.contains("1 PENDING")
    ));
    assert!(matches!(
        migration_check_result(Ok(&SchemaStatus::Fresh)),
        CheckResult::Warn(ref msg) if msg.contains("No migrations applied yet")
    ));
    let error = "permission denied for table seaql_migrations".to_string();
    assert!(matches!(
        migration_check_result(Err(&error)),
        CheckResult::Warn(ref msg) if msg.contains("permission denied")
    ));

    let rolled_back = SchemaStatus::NewerThanBinary {
        applied: 13,
        unknown: names(&["m_from_newer_release"]),
        pending: Vec::new(),
    };
    match migration_check_result(Ok(&rolled_back)) {
        CheckResult::Warn(msg) => {
            assert!(msg.contains("newer than this Temps binary"), "{msg}");
            assert!(msg.contains("m_from_newer_release"), "{msg}");
            assert!(!msg.contains("lacks"), "{msg}");
        }
        other => panic!("expected Warn, got {other:?}"),
    }
    let divergent = SchemaStatus::NewerThanBinary {
        applied: 13,
        unknown: names(&["m_from_newer_release"]),
        pending: names(&["m_only_in_this_build"]),
    };
    match migration_check_result(Ok(&divergent)) {
        CheckResult::Warn(msg) => {
            assert!(msg.contains("1 migration(s) the database lacks"), "{msg}");
            assert!(msg.contains("m_only_in_this_build"), "{msg}");
        }
        other => panic!("expected Warn, got {other:?}"),
    }
}

#[test]
fn test_update_check_nightly_is_not_offered_an_older_beta() {
    // Previously: "v0.1.0-beta.56 available (prerelease) - run `temps upgrade`".
    let current = "v0.1.0-nightly.20261004.32a8e9c1";
    let channel = upgrade::UpgradeChannel::for_installed_version(current);
    assert_eq!(channel, upgrade::UpgradeChannel::Nightly);
    match update_check_result(current, channel, "v0.1.0-beta.56") {
        CheckResult::Pass(msg) => assert!(msg.contains("nightly channel"), "{msg}"),
        other => panic!("expected Pass, got {other:?}"),
    }
}

#[test]
fn test_update_check_suggests_channel_specific_upgrade() {
    match update_check_result(
        "v0.1.0-nightly.20261003.6f74680e",
        upgrade::UpgradeChannel::Nightly,
        "v0.1.0-nightly.20261004.32a8e9c1",
    ) {
        CheckResult::Warn(msg) => {
            assert!(msg.contains("v0.1.0-nightly.20261004.32a8e9c1"), "{msg}");
            assert!(msg.contains("temps upgrade --channel nightly"), "{msg}");
        }
        other => panic!("expected Warn, got {other:?}"),
    }
    match update_check_result(
        "v0.1.0-beta.55",
        upgrade::UpgradeChannel::Beta,
        "v0.1.0-beta.56",
    ) {
        CheckResult::Warn(msg) => assert!(msg.contains("--channel beta"), "{msg}"),
        other => panic!("expected Warn, got {other:?}"),
    }
}

#[test]
fn test_update_check_same_version_is_up_to_date() {
    assert!(matches!(
        update_check_result("v0.1.0", upgrade::UpgradeChannel::Stable, "v0.1.0"),
        CheckResult::Pass(_)
    ));
}

#[test]
fn test_update_check_dev_build_is_informational() {
    assert!(matches!(
        update_check_result("dev", upgrade::UpgradeChannel::Stable, "v0.1.0"),
        CheckResult::Info(_)
    ));
}

#[test]
fn test_parse_pg_url_basic() {
    let (host, port) = parse_pg_url("postgres://user:pass@localhost:5432/db").unwrap();
    assert_eq!(host, "localhost");
    assert_eq!(port, 5432);
}

#[test]
fn test_parse_pg_url_default_port() {
    let (host, port) = parse_pg_url("postgres://user:pass@myhost/db").unwrap();
    assert_eq!(host, "myhost");
    assert_eq!(port, 5432);
}

#[test]
fn test_parse_pg_url_custom_port() {
    let (host, port) = parse_pg_url("postgresql://u:p@db.example.com:5433/mydb").unwrap();
    assert_eq!(host, "db.example.com");
    assert_eq!(port, 5433);
}

#[test]
fn test_parse_pg_url_with_query_params() {
    let (host, port) = parse_pg_url("postgres://u:p@localhost:5432/db?sslmode=require").unwrap();
    assert_eq!(host, "localhost");
    assert_eq!(port, 5432);
}

#[test]
fn test_parse_pg_url_ipv6() {
    let (host, port) = parse_pg_url("postgres://u:p@[::1]:5432/db").unwrap();
    assert_eq!(host, "::1");
    assert_eq!(port, 5432);
}

#[test]
fn test_parse_pg_url_invalid_scheme() {
    let result = parse_pg_url("mysql://u:p@localhost:3306/db");
    assert!(result.is_err());
}

#[test]
fn initial_admin_does_not_treat_a_failed_query_as_an_empty_database() {
    assert!(matches!(
        initial_admin_result(&UserCount::QueryFailed("relation \"users\" does not exist".into()), None),
        CheckResult::Warn(message)
            if message.contains("Could not count users") && message.contains("temps migrate")
    ));
}

#[test]
fn initial_admin_says_why_it_was_not_checked() {
    assert!(matches!(
        initial_admin_result(&UserCount::NoDatabaseUrl, None),
        CheckResult::Warn(message) if message.contains("no database URL was found")
    ));
    assert!(matches!(
        initial_admin_result(&UserCount::Unreachable, None),
        CheckResult::Warn(message) if message.contains("could not be reached")
    ));
}

#[test]
fn initial_admin_reports_completed_bootstrap_from_persisted_users() {
    assert!(matches!(
        initial_admin_result(&UserCount::Count(2), None),
        CheckResult::Pass(message) if message.contains("bootstrap has completed")
    ));
}

fn unit_service(home: &Path, service_section: &str) -> ServiceDefinition {
    let unit = home.join("temps.service");
    crate::service_definition::test_support::write_trusted(
        &unit,
        format!(
            "[Service]\n{service_section}ExecStart=/usr/bin/temps serve --data-dir=/srv/temps\n"
        ),
    )
    .unwrap();
    match service_definition::probe(&ServiceLocations {
        systemd_unit: Some(unit),
        launchd_agent: None,
    })
    .pop()
    {
        Some(ServiceProbe::Found(service)) => *service,
        other => panic!("expected the unit to parse, got {other:?}"),
    }
}

fn no_admin(serving: Option<&ServiceDefinition>) -> String {
    match initial_admin_result(&UserCount::Count(0), serving) {
        CheckResult::Warn(message) => message,
        other => panic!("no users warns, got {other:?}"),
    }
}

#[test]
fn initial_admin_says_how_serve_creates_it_without_a_service() {
    let message = no_admin(None);
    assert!(
        message.contains("asks for the admin's email")
            && message.contains("TEMPS_ADMIN_EMAIL")
            && message.contains("TEMPS_ADMIN_PASSWORD_FILE")
            && message.contains("console does not start without them"),
        "{message}"
    );
}

#[test]
fn initial_admin_says_the_service_cannot_create_it_without_its_inputs() {
    let home = tempfile::tempdir().unwrap();
    // Neither set: no terminal to ask on, so the console does not start.
    let message = no_admin(Some(&unit_service(home.path(), "")));
    assert!(
        message.contains("sets neither")
            && message.contains("does not start its console")
            && message.contains("run `temps serve` once in a terminal"),
        "{message}"
    );
    assert!(
        !message.contains("Run `temps serve` to complete"),
        "{message}"
    );

    // Only one set: serve refuses both halves.
    let message = no_admin(Some(&unit_service(
        home.path(),
        "Environment=TEMPS_ADMIN_EMAIL=admin@example.test\n",
    )));
    assert!(
        message.contains("sets TEMPS_ADMIN_EMAIL but not TEMPS_ADMIN_PASSWORD_FILE"),
        "{message}"
    );

    // Both set: the server creates it once its console starts.
    let message = no_admin(Some(&unit_service(
        home.path(),
        "Environment=TEMPS_ADMIN_EMAIL=admin@example.test \
         TEMPS_ADMIN_PASSWORD_FILE=/etc/temps/admin-password\n",
    )));
    assert!(
        message.contains("so `temps serve` creates the first admin")
            && message.contains("its console has not started"),
        "{message}"
    );
}

#[cfg(unix)]
#[test]
fn initial_admin_says_when_the_services_environment_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let env_file = home.path().join("temps.env");
    std::fs::write(&env_file, "TEMPS_ADMIN_EMAIL=admin@example.test\n").unwrap();
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&env_file).is_ok() {
        // Running as root: every file is readable.
        return;
    }
    let service = unit_service(
        home.path(),
        &format!("EnvironmentFile={}\n", env_file.display()),
    );
    let message = no_admin(Some(&service));
    assert!(
        message.contains("sets them is unknown") && !message.contains("sets neither"),
        "{message}"
    );
}

#[test]
fn test_diagnostic_report_counts() {
    let mut report = DiagnosticReport::new();
    report.add("a", CheckResult::Pass("ok".into()));
    report.add("b", CheckResult::Pass("ok".into()));
    report.add("c", CheckResult::Warn("meh".into()));
    report.add("d", CheckResult::Fail("bad".into()));
    report.add("e", CheckResult::Info("fyi".into()));

    assert_eq!(report.pass_count, 2);
    assert_eq!(report.warn_count, 1);
    assert_eq!(report.fail_count, 1);
}

#[test]
fn age_is_described_in_operator_terms() {
    assert_eq!(describe_age(0), "less than a day old");
    assert_eq!(describe_age(-1), "less than a day old");
    assert_eq!(describe_age(1), "1 day old");
    assert_eq!(describe_age(3), "3 days old");
    assert_eq!(describe_age(90), "90 days old (about 3 months)");
    assert_eq!(describe_age(800), "800 days old (about 2 years)");
}

#[test]
fn default_data_dir_detects_legacy_nested_runtime_layout() {
    let home = tempfile::tempdir().expect("create temporary home");
    let nested = home.path().join(".temps/data");
    std::fs::create_dir_all(&nested).expect("create nested data directory");
    std::fs::write(nested.join("auth_secret"), "test-secret").expect("write nested auth secret");

    assert_eq!(default_data_dir(home.path()), nested);
}

#[test]
fn other_data_dirs_with_names_the_sibling_install_only() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".temps");
    let nested = root.join("data");
    let marker = Path::new("firecracker/state.json");
    std::fs::create_dir_all(root.join("firecracker")).unwrap();
    std::fs::write(root.join(marker), b"{}").unwrap();

    // Provisioned under the root only: setting up the nested dir must
    // point at the root, and setting up the root must report nothing.
    assert_eq!(
        other_data_dirs_with(&nested, home.path(), marker),
        vec![root.clone()]
    );
    assert!(other_data_dirs_with(&root, home.path(), marker).is_empty());
}

#[test]
fn default_data_dir_prefers_current_root_layout() {
    let home = tempfile::tempdir().expect("create temporary home");
    let root = home.path().join(".temps");
    let nested = root.join("data");
    std::fs::create_dir_all(&nested).expect("create nested data directory");
    std::fs::write(root.join("auth_secret"), "current-secret").expect("write root auth secret");
    std::fs::write(nested.join("auth_secret"), "legacy-secret").expect("write nested auth secret");

    assert_eq!(default_data_dir(home.path()), root);
}

/// What the installer leaves on disk once `temps serve` has started: the
/// server's secrets in `~/.temps/data`, plus a copy of `encryption_key`
/// (only) in `~/.temps`. Returns the unit and the server's data dir.
fn installer_layout(home: &Path) -> (ServiceDefinition, PathBuf) {
    let data_dir = home.join(".temps/data");
    std::fs::create_dir_all(&data_dir).unwrap();
    // The two files serve creates at startup, written here so an
    // exported TEMPS_ENCRYPTION_KEY / TEMPS_AUTH_SECRET cannot change
    // the fixture.
    for (file, secret) in [
        (
            temps_config::AUTH_SECRET_FILE,
            "3f9a1c7e5b2d8046af13ce97b5d20864e1f7a3c9052bd6e48a1f7c3e95b02d64",
        ),
        (
            temps_config::ENCRYPTION_KEY_FILE,
            "b7e2d94a1c6f3058e9a2d7c41b8f6e03a5c9d2e7f1b4a8c6d3e0f9b2a7c5e1d4",
        ),
    ] {
        std::fs::write(data_dir.join(file), secret).unwrap();
    }
    std::fs::copy(
        data_dir.join("encryption_key"),
        home.join(".temps/encryption_key"),
    )
    .unwrap();
    let unit = home.join("temps.service");
    crate::service_definition::test_support::write_trusted(
        &unit,
        format!(
            "[Service]\nWorkingDirectory={h}/.temps\n\
                 ExecStart={h}/.temps/bin/temps serve \\\n  \
                 --database-url=\"postgresql://temps:pw@127.0.0.1:5432/temps\" \\\n  \
                 --data-dir=\"{h}/.temps/data\"\nEnvironment=HOME={h}\n",
            h = home.display()
        ),
    )
    .unwrap();
    let service = match service_definition::probe(&service_definition::ServiceLocations {
        systemd_unit: Some(unit),
        launchd_agent: None,
    })
    .pop()
    {
        Some(service_definition::ServiceProbe::Found(service)) => *service,
        other => panic!("expected the unit to parse, got {other:?}"),
    };
    (service, data_dir)
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn the_installers_copied_key_no_longer_hides_the_real_data_dir() {
    let home = tempfile::tempdir().unwrap();
    let (_, data_dir) = installer_layout(home.path());
    // `~/.temps` holds only the copied encryption_key: the layout guess
    // must pick the directory the server writes both secrets to.
    assert_eq!(default_data_dir(home.path()), data_dir);
}

#[test]
fn doctor_checks_the_data_dir_the_installed_service_uses() {
    let home = tempfile::tempdir().unwrap();
    let (service, data_dir) = installer_layout(home.path());
    let DoctorDataDir { dir, source, .. } =
        resolve_doctor_data_dir(None, Some(&service), &[], home.path());
    assert_eq!(dir, data_dir);
    assert!(
        source.contains("--data-dir") && source.contains("temps.service"),
        "{source}"
    );

    let explicit = home.path().join("elsewhere");
    let dir = resolve_doctor_data_dir(Some(&explicit), Some(&service), &[], home.path()).dir;
    assert_eq!(dir, explicit, "--data-dir / TEMPS_DATA_DIR still win");
}

#[test]
fn an_ignored_service_definition_is_shown_with_its_reason() {
    let home = tempfile::tempdir().unwrap();
    let probes = vec![ServiceProbe::Ignored {
        path: home.path().join("temps.service"),
        reason: "is writable by group 4, so it is not trusted; remove that with `chmod g-w`"
            .to_string(),
    }];
    let mut report = DiagnosticReport::new();
    check_installed_service(None, &probes, &mut report);
    assert!(
        matches!(
            report.checks.as_slice(),
            [(label, CheckResult::Warn(message))]
                if label == "Ignored"
                    && message.contains("temps.service")
                    && message.contains("chmod g-w")
        ),
        "{:?}",
        report.checks
    );

    let DoctorDataDir {
        source, unverified, ..
    } = resolve_doctor_data_dir(None, None, &probes, home.path());
    assert!(unverified);
    assert!(
        source.contains("temps.service") && source.contains("could not be used"),
        "{source}"
    );
    assert!(source.contains("; pass --data-dir"), "{source}");
    assert!(!source.contains("  "), "a run of spaces in {source:?}");
}

#[test]
fn a_service_without_a_known_data_dir_says_so() {
    let home = tempfile::tempdir().unwrap();
    let unit = home.path().join("temps.service");
    crate::service_definition::test_support::write_trusted(
        &unit,
        "[Service]\nUser=temps-test-no-such-user\nExecStart=/usr/bin/temps serve\n",
    )
    .unwrap();
    let service = match service_definition::probe(&ServiceLocations {
        systemd_unit: Some(unit),
        launchd_agent: None,
    })
    .pop()
    {
        Some(ServiceProbe::Found(service)) => *service,
        other => panic!("expected the unit to parse, got {other:?}"),
    };
    let DoctorDataDir {
        source, unverified, ..
    } = resolve_doctor_data_dir(None, Some(&service), &[], home.path());
    assert!(unverified);
    assert!(
        source.contains("is unknown") && source.contains("--data-dir"),
        "{source}"
    );
}

#[cfg(unix)]
#[test]
fn secrets_are_unverified_when_the_service_environment_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let (_, data_dir) = installer_layout(home.path());
    let env_file = home.path().join("temps.env");
    crate::service_definition::test_support::write_trusted(
        &env_file,
        "TEMPS_ENCRYPTION_KEY=elsewhere\n",
    )
    .unwrap();
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&env_file).is_ok() {
        // Running as root: every file is readable.
        return;
    }
    let unit = home.path().join("temps.service");
    crate::service_definition::test_support::write_trusted(
        &unit,
        format!(
            "[Service]\nEnvironmentFile={}\nExecStart=/usr/bin/temps serve --data-dir={}\n",
            env_file.display(),
            data_dir.display()
        ),
    )
    .unwrap();
    let service = match service_definition::probe(&ServiceLocations {
        systemd_unit: Some(unit),
        launchd_agent: None,
    })
    .pop()
    {
        Some(ServiceProbe::Found(service)) => *service,
        other => panic!("expected the unit to parse, got {other:?}"),
    };
    let doctor = DoctorCommand {
        database_url: None,
        data_dir: None,
        scope: None,
    };
    let mut report = DiagnosticReport::new();
    doctor.check_data_dir(
        &data_dir,
        "test",
        false,
        Some(&service),
        SecretsEnvironment::of_service(Some(&service)),
        &mut report,
    );
    for name in ["Encryption key", "Auth secret"] {
        let result = report.checks.iter().find(|(label, _)| label == name);
        assert!(
            matches!(result, Some((_, CheckResult::Warn(message)))
                    if message.contains("Could not verify") && message.contains("temps.env")),
            "{name}: {result:?}"
        );
    }
    assert_eq!(report.fail_count, 0, "{:?}", report.checks);
}

#[test]
fn a_guessed_data_dir_is_flagged_when_an_installed_server_exists() {
    let home = tempfile::tempdir().unwrap();
    let (service, data_dir) = installer_layout(home.path());
    let doctor = DoctorCommand {
        database_url: None,
        data_dir: None,
        scope: None,
    };
    for unverified in [true, false] {
        let mut report = DiagnosticReport::new();
        doctor.check_data_dir(
            &data_dir,
            "test",
            unverified,
            Some(&service),
            SecretsEnvironment::of_service(Some(&service)),
            &mut report,
        );
        let flagged = report.checks.iter().any(|(label, result)| {
            label == "Server's data directory"
                && matches!(result, CheckResult::Warn(message) if message.contains("--data-dir"))
        });
        assert_eq!(flagged, unverified, "{:?}", report.checks);
    }

    // No installed server: the default layout is the answer, not a guess.
    let empty = tempfile::tempdir().unwrap();
    assert!(!resolve_doctor_data_dir(None, None, &[], empty.path()).unverified);
}

#[test]
fn geolite_says_when_the_servers_working_directory_is_unknown() {
    let data = tempfile::tempdir().unwrap();
    for (working_directory, mentions_unknown) in [(None, true), (Some(data.path()), false)] {
        let result = geolite_result(data.path(), working_directory);
        assert!(
            matches!(&result, CheckResult::Warn(message)
                    if message.contains("working directory is unknown") == mentions_unknown),
            "{result:?}"
        );
    }
}

#[test]
fn doctor_says_when_this_shells_secrets_replace_the_services() {
    let home = tempfile::tempdir().unwrap();
    let (service, data_dir) = installer_layout(home.path());
    let doctor = DoctorCommand {
        database_url: None,
        data_dir: None,
        scope: None,
    };
    let mut report = DiagnosticReport::new();
    doctor.check_data_dir(
        &data_dir,
        "test",
        false,
        Some(&service),
        SecretsEnvironment::choose_with(Some(&service), vec![temps_config::AUTH_SECRET_ENV]),
        &mut report,
    );
    let source = report
        .checks
        .iter()
        .find(|(label, _)| label == "Secrets source");
    assert!(
        matches!(source, Some((_, CheckResult::Info(message)))
                if message.contains(&format!("this shell sets {}", temps_config::AUTH_SECRET_ENV))
                    && message.contains("secret settings were not")),
        "{:?}",
        report.checks
    );
}

#[cfg(unix)]
#[test]
fn doctor_fails_a_shell_secret_that_is_not_unicode_instead_of_passing_the_data_dir() {
    use std::os::unix::ffi::OsStrExt;
    // A name no other test reads, so setting it races with nothing.
    const VARIABLE: &str = "TEMPS_TEST_DOCTOR_NOT_UNICODE_SECRET";
    std::env::set_var(VARIABLE, std::ffi::OsStr::from_bytes(b"\xff\xfe"));
    let home = tempfile::tempdir().unwrap();
    let (service, data_dir) = installer_layout(home.path());
    let doctor = DoctorCommand {
        database_url: None,
        data_dir: None,
        scope: None,
    };
    let mut report = DiagnosticReport::new();
    doctor.check_data_dir(
        &data_dir,
        "test",
        false,
        Some(&service),
        SecretsEnvironment::Shell(vec![VARIABLE]),
        &mut report,
    );
    std::env::remove_var(VARIABLE);
    assert!(
        report
            .checks
            .iter()
            .any(|(label, result)| label == "Secrets"
                && matches!(result, CheckResult::Fail(message)
                if message.contains(VARIABLE) && message.contains("not valid Unicode"))),
        "{:?}",
        report.checks
    );
    // The data directory's valid key is not passed in its place.
    assert!(
        !report
            .checks
            .iter()
            .any(|(label, result)| label == "Encryption key"
                && matches!(result, CheckResult::Pass(_))),
        "{:?}",
        report.checks
    );
}

#[test]
fn the_auth_secret_passes_on_the_layout_serve_uses() {
    let home = tempfile::tempdir().unwrap();
    let (service, _) = installer_layout(home.path());
    let data_dir = resolve_doctor_data_dir(None, Some(&service), &[], home.path()).dir;
    let inspection = temps_config::inspect_installation_secrets_with(&data_dir, no_env);
    assert!(
        matches!(
            auth_secret_result(&inspection.auth_secret),
            CheckResult::Pass(_)
        ),
        "{:?}",
        auth_secret_result(&inspection.auth_secret)
    );
    assert!(matches!(
        encryption_key_result(&inspection.encryption_key, true),
        CheckResult::Pass(_)
    ));
    assert!(matches!(logs_dir_result(&data_dir), CheckResult::Warn(_)));
}

#[test]
fn a_missing_auth_secret_is_a_warning_because_serve_generates_it() {
    let dir = tempfile::tempdir().unwrap();
    let inspection = temps_config::inspect_installation_secrets_with(dir.path(), no_env);
    let CheckResult::Warn(message) = auth_secret_result(&inspection.auth_secret) else {
        panic!("a missing auth secret must not fail");
    };
    assert!(message.contains("generates it when it starts"), "{message}");
    assert!(
        !dir.path().join("auth_secret").exists(),
        "doctor must not create secrets"
    );
}

#[test]
fn an_auth_secret_serve_rejects_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("auth_secret"), "too-short").unwrap();
    let inspection = temps_config::inspect_installation_secrets_with(dir.path(), no_env);
    let CheckResult::Fail(message) = auth_secret_result(&inspection.auth_secret) else {
        panic!("an invalid auth secret must fail");
    };
    assert!(
        !message.contains("too-short"),
        "the secret is never printed: {message}"
    );
}

#[test]
fn a_conflicting_injected_auth_secret_fails_like_serve() {
    let dir = tempfile::tempdir().unwrap();
    let env = |name: &str| match name {
        "TEMPS_AUTH_SECRET" => Some("a".repeat(64)),
        "TEMPS_AUTH_SECRET_FILE" => Some("/run/secrets/auth".to_string()),
        _ => None,
    };
    let inspection = temps_config::inspect_installation_secrets_with(dir.path(), env);
    assert!(matches!(
        auth_secret_result(&inspection.auth_secret),
        CheckResult::Fail(message) if message.contains("would refuse to start")
    ));
}

#[cfg(unix)]
#[test]
fn an_auth_secret_this_user_cannot_read_is_unverified_not_failed() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: root reads any file");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth_secret");
    std::fs::write(&path, "b".repeat(64)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let inspection = temps_config::inspect_installation_secrets_with(dir.path(), no_env);
    assert!(matches!(
        auth_secret_result(&inspection.auth_secret),
        CheckResult::Warn(message) if message.contains("Could not verify")
    ));
}

#[test]
fn a_lost_encryption_key_on_an_initialized_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let inspection = temps_config::inspect_installation_secrets_with(dir.path(), no_env);
    assert!(matches!(
        encryption_key_result(&inspection.encryption_key, false),
        CheckResult::Warn(_)
    ));
    assert!(matches!(
        encryption_key_result(&inspection.encryption_key, true),
        CheckResult::Fail(message) if message.contains("cannot decrypt")
    ));
}

#[test]
fn a_missing_data_dir_is_not_a_setup_step_when_a_server_is_installed() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    // An existing directory needs no explanation.
    assert!(data_dir_absence(root.path(), false, true).is_none());

    // The installed server's own directory: serve creates it.
    let result = data_dir_absence(&missing, false, true);
    assert!(
        matches!(&result, Some(CheckResult::Warn(message))
            if message.contains("creates it when it first starts")
                && !message.contains("temps setup")),
        "{result:?}"
    );
    // A guess while the server's is unknown says nothing about the server.
    let result = data_dir_absence(&missing, true, true);
    assert!(
        matches!(&result, Some(CheckResult::Warn(message))
            if message.contains("only this user's default") && !message.contains("temps setup")),
        "{result:?}"
    );
    // Nothing installed: setting up is the next step.
    assert!(matches!(
        data_dir_absence(&missing, false, false),
        Some(CheckResult::Fail(message)) if message.contains("temps setup")
    ));
}

#[cfg(unix)]
#[test]
fn a_data_dir_this_user_cannot_see_is_not_reported_missing() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("private");
    std::fs::create_dir_all(parent.join("data")).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::metadata(parent.join("data")).is_ok();
    let result = data_dir_absence(&parent.join("data"), false, true);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        // Running as root: every directory is visible.
        return;
    }
    assert!(
        matches!(&result, Some(CheckResult::Warn(message))
            if message.contains("cannot be inspected by this user")
                && !message.contains("does not exist")),
        "{result:?}"
    );
}

#[test]
fn the_logs_directory_matches_what_serve_creates_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let CheckResult::Warn(message) = logs_dir_result(dir.path()) else {
        panic!("a missing logs dir warns");
    };
    // serve creates it only once its console gets past the Docker check,
    // so a server that is running can still lack it.
    assert!(
        message.contains("each time its console starts")
            && message.contains("its console failed to start"),
        "{message}"
    );
    assert!(!message.contains("first deployment"), "{message}");

    std::fs::create_dir(dir.path().join("logs")).unwrap();
    assert!(matches!(logs_dir_result(dir.path()), CheckResult::Pass(_)));

    let blocked = tempfile::tempdir().unwrap();
    std::fs::write(blocked.path().join("logs"), b"").unwrap();
    assert!(matches!(
        logs_dir_result(blocked.path()),
        CheckResult::Fail(_)
    ));
}

#[test]
fn geolite_is_looked_up_where_serve_opens_it() {
    let data_dir = tempfile::tempdir().unwrap();
    let working_dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        geolite_result(data_dir.path(), Some(working_dir.path())),
        CheckResult::Warn(_)
    ));
    let in_working_dir = working_dir.path().join(temps_geo::CITY_DB_FILENAME);
    std::fs::write(&in_working_dir, b"mmdb").unwrap();
    assert!(matches!(
        geolite_result(data_dir.path(), Some(working_dir.path())),
        CheckResult::Pass(path) if path == in_working_dir.display().to_string()
    ));
    let in_data_dir = data_dir.path().join(temps_geo::CITY_DB_FILENAME);
    std::fs::write(&in_data_dir, b"mmdb").unwrap();
    assert!(matches!(
        geolite_result(data_dir.path(), None),
        CheckResult::Pass(path) if path == in_data_dir.display().to_string()
    ));
}
