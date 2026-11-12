// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

const URL: &str = "postgresql://temps:s3cr3t-pw@127.0.0.1:5432/temps?sslmode=disable";

/// The unit the installer writes, with generic paths and a made-up password.
fn installer_unit(home: &Path) -> String {
    format!(
        "[Unit]\nDescription=Temps Platform Server\nAfter=network.target docker.service\n\n\
         [Service]\nType=simple\nUser=root\nWorkingDirectory={home}/.temps\n\
         ExecStart={home}/.temps/bin/temps serve \\\n  --address=\"0.0.0.0:80\" \\\n  \
         --tls-address=\"0.0.0.0:443\" \\\n  --database-url=\"{URL}\" \\\n  \
         --data-dir=\"{home}/.temps/data\" \\\n  --console-address=\"0.0.0.0:8081\"\n\
         Restart=always\nEnvironment=HOME={home}\n\n[Install]\nWantedBy=multi-user.target\n",
        home = home.display()
    )
}

fn write(path: &Path, contents: &str) {
    test_support::write_trusted(path, contents).unwrap();
}

/// Make `path` unreadable; `false` when this user reads it anyway (root).
#[cfg(unix)]
fn make_unreadable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::read(path).is_err()
}

fn found(probes: Vec<ServiceProbe>) -> ServiceDefinition {
    match probes.into_iter().next() {
        Some(ServiceProbe::Found(definition)) => *definition,
        other => panic!("expected a definition, got {other:?}"),
    }
}

fn systemd_at(unit: &Path) -> ServiceLocations {
    ServiceLocations {
        systemd_unit: Some(unit.to_path_buf()),
        launchd_agent: None,
    }
}

#[test]
fn the_installer_unit_yields_its_url_and_data_dir() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let unit = root.path().join("temps.service");
    write(&unit, &installer_unit(&home));

    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition
            .database_url
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .1,
        UrlLocation::CommandLine
    );
    assert_eq!(
        definition.data_dir,
        Ok((home.join(".temps/data"), DataDirLocation::CommandLine))
    );
    assert_eq!(definition.working_directory, Some(home.join(".temps")));
    assert!(definition.problems.is_empty(), "{:?}", definition.problems);
}

#[test]
fn an_environment_file_overrides_environment_and_is_named_as_the_origin() {
    let root = tempfile::tempdir().unwrap();
    let env_file = root.path().join("temps.env");
    write(
        &env_file,
        &format!("# database\nTEMPS_DATABASE_URL=\"{URL}\"\nTEMPS_DATA_DIR=/srv/temps\n"),
    );
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=\"TEMPS_DATABASE_URL=postgres://other/db\" HOME=/home/svc\n\
             EnvironmentFile={}\nExecStart=/usr/local/bin/temps serve\n",
            env_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.database_url.unwrap().unwrap().1,
        UrlLocation::EnvironmentFile(env_file)
    );
    assert_eq!(
        definition.data_dir,
        Ok((PathBuf::from("/srv/temps"), DataDirLocation::Environment))
    );
}

#[test]
fn the_command_line_flag_wins_over_the_service_environment() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=TEMPS_DATABASE_URL=postgres://env/db\n\
             ExecStart=/opt/temps serve --database-url {URL}\n"
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
}

#[test]
fn a_drop_in_replaces_exec_start_and_resets_environment() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        "[Service]\nEnvironment=TEMPS_DATABASE_URL=postgres://old/db\n\
         ExecStart=/usr/bin/temps serve --database-url=postgres://old/db\n",
    );
    write(
        &root.path().join("temps.service.d/override.conf"),
        &format!(
            "[Service]\nEnvironment=\nEnvironment=TEMPS_DATABASE_URL={URL}\n\
             ExecStart=\nExecStart=/usr/bin/temps serve --data-dir /srv/data\n"
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.database_url.unwrap().unwrap().1,
        UrlLocation::Environment
    );
    assert_eq!(
        definition.data_dir,
        Ok((PathBuf::from("/srv/data"), DataDirLocation::CommandLine))
    );
}

#[test]
fn a_systemd_credential_and_a_url_file_are_read() {
    let root = tempfile::tempdir().unwrap();
    let url_file = root.path().join("database-url");
    write(&url_file, &format!("{URL}\n"));
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nLoadCredential=temps_database_url:{}\nExecStart=/usr/bin/temps serve\n",
            url_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.database_url.unwrap().unwrap().1,
        UrlLocation::Credential(url_file.clone())
    );

    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=TEMPS_DATABASE_URL_FILE={}\nExecStart=/usr/bin/temps serve\n",
            url_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(
        definition.database_url.unwrap().unwrap().1,
        UrlLocation::UrlFile(url_file)
    );
}

#[test]
fn a_missing_url_file_is_an_error_not_a_silent_fallthrough() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        "[Service]\nEnvironment=TEMPS_DATABASE_URL_FILE=/nonexistent/database-url\n\
         LoadCredential=temps_database_url:/nonexistent/credential\n\
         ExecStart=/usr/bin/temps serve\n",
    );
    let definition = found(probe(&systemd_at(&unit)));
    let error = definition.database_url.unwrap_err();
    assert!(error.contains("/nonexistent/database-url"), "{error}");
}

#[test]
fn a_unit_that_does_not_run_serve_supplies_no_url() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!("[Service]\nExecStart=/usr/bin/temps proxy --database-url={URL}\n"),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), None);
    assert!(definition
        .problems
        .iter()
        .any(|p| p.contains("temps serve")));
}

#[test]
fn variables_in_exec_start_expand_from_the_service_environment() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=DB={URL} HOME=/home/svc\n\
             ExecStart=-/usr/bin/temps serve --database-url=${{DB}} --data-dir ${{HOME}}/data\n"
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.data_dir,
        Ok((
            PathBuf::from("/home/svc/data"),
            DataDirLocation::CommandLine
        ))
    );
}

#[test]
fn a_variable_alone_in_exec_start_splits_into_words_as_systemd_does() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    // `$ARGS` as a word of its own becomes two arguments; `${ARGS}` would
    // stay one.
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=\"ARGS=--database-url={URL} --data-dir=/srv/temps\"\n\
             ExecStart=/usr/bin/temps serve $ARGS\n"
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.data_dir,
        Ok((PathBuf::from("/srv/temps"), DataDirLocation::CommandLine))
    );

    // Within a word, `$NAME` is passed as written: only `${NAME}` expands.
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=DB={URL} DIR=/srv/temps\n\
             ExecStart=/usr/bin/temps serve --database-url=$DB --data-dir=$DIR\n"
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some("$DB"));
    assert_eq!(
        definition.data_dir,
        Ok((PathBuf::from("/$DIR"), DataDirLocation::CommandLine))
    );

    // `${ARGS}` is a single argument, spaces and all.
    write(
        &unit,
        "[Service]\nEnvironment=\"ARGS=--data-dir=/srv/temps --database-url=unused\"\n\
         ExecStart=/usr/bin/temps serve ${ARGS}\n",
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(
        definition.data_dir,
        Ok((
            PathBuf::from("/srv/temps --database-url=unused"),
            DataDirLocation::CommandLine
        ))
    );
}

#[test]
fn a_later_load_credential_for_the_database_url_replaces_the_earlier_one() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old-url");
    let new = root.path().join("new-url");
    write(&old, "postgresql://temps@127.0.0.1:5432/old\n");
    write(&new, &format!("{URL}\n"));
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nLoadCredential=temps_database_url:{}\nExecStart=/usr/bin/temps serve\n",
            old.display()
        ),
    );
    let drop_in = root.path().join("temps.service.d/50-database.conf");
    write(
        &drop_in,
        &format!(
            "[Service]\nLoadCredential=temps_database_url:{}\n",
            new.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.database_url.unwrap().unwrap().1,
        UrlLocation::Credential(new)
    );

    // Without a path the credential comes from the credential store, which
    // this command cannot read: the URL is unknown, never the earlier file.
    write(&drop_in, "[Service]\nLoadCredential=temps_database_url\n");
    let definition = found(probe(&systemd_at(&unit)));
    assert!(definition.database_url.is_err(), "{definition:?}");
}

#[test]
fn a_unit_that_does_not_run_serve_supplies_no_url_from_its_environment() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=TEMPS_DATABASE_URL={URL} TEMPS_DATA_DIR=/srv/temps\n\
             ExecStart=/usr/bin/temps proxy\n"
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), None);
    assert!(definition.data_dir.is_err(), "{definition:?}");

    let agent = root.path().join("dev.temps.temps.plist");
    write(
        &agent,
        &format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n\
             <key>ProgramArguments</key>\n<array>\n\
             <string>/usr/local/bin/temps</string>\n<string>proxy</string>\n</array>\n\
             <key>EnvironmentVariables</key>\n<dict>\n\
             <key>TEMPS_DATABASE_URL</key>\n<string>{URL}</string>\n</dict>\n\
             </dict>\n</plist>\n"
        ),
    );
    let definition = found(probe(&ServiceLocations {
        systemd_unit: None,
        launchd_agent: Some(agent),
    }));
    assert_eq!(definition.url(), None);
    assert!(definition
        .problems
        .iter()
        .any(|p| p.contains("temps serve")));
}

#[test]
fn the_data_dir_defaults_to_the_service_home_and_resolves_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        "[Service]\nEnvironment=HOME=/home/svc\nExecStart=/usr/bin/temps serve\n",
    );
    assert_eq!(
        found(probe(&systemd_at(&unit))).data_dir,
        Ok((
            PathBuf::from("/home/svc/.temps"),
            DataDirLocation::HomeDefault
        ))
    );

    write(
        &unit,
        "[Service]\nWorkingDirectory=/srv/temps\nExecStart=/usr/bin/temps serve --data-dir ./data\n",
    );
    assert_eq!(
        found(probe(&systemd_at(&unit))).data_dir,
        Ok((
            PathBuf::from("/srv/temps/data"),
            DataDirLocation::CommandLine
        ))
    );
}

#[cfg(unix)]
#[test]
fn the_home_comes_from_user_as_systemd_and_the_server_find_it() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    let Some(root_home) = account_home("0") else {
        return;
    };

    // User= sets HOME, which ExecStart= can use.
    write(
        &unit,
        "[Service]\nUser=root\nExecStart=/usr/bin/temps serve --data-dir=${HOME}/data\n",
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(
        definition.env("HOME"),
        Some(&*root_home.display().to_string())
    );
    assert_eq!(
        definition.data_dir,
        Ok((root_home.join("data"), DataDirLocation::CommandLine))
    );

    // No User=: the server runs as root and finds root's home.
    write(&unit, "[Service]\nExecStart=/usr/bin/temps serve\n");
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.env("HOME"), None);
    assert_eq!(
        definition.data_dir,
        Ok((root_home.join(".temps"), DataDirLocation::HomeDefault))
    );

    // Environment=HOME wins over User='s home.
    write(
        &unit,
        "[Service]\nUser=root\nEnvironment=HOME=/home/svc\nExecStart=/usr/bin/temps serve\n",
    );
    assert_eq!(
        found(probe(&systemd_at(&unit))).data_dir,
        Ok((
            PathBuf::from("/home/svc/.temps"),
            DataDirLocation::HomeDefault
        ))
    );

    // An account that does not exist leaves it unknown, and says why.
    write(
        &unit,
        "[Service]\nUser=temps-test-no-such-user\nExecStart=/usr/bin/temps serve\n",
    );
    let error = found(probe(&systemd_at(&unit))).data_dir.unwrap_err();
    assert!(error.contains("User=temps-test-no-such-user"), "{error}");
}

#[test]
fn a_missing_unit_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let probes = probe(&systemd_at(&root.path().join("temps.service")));
    assert!(matches!(probes.as_slice(), [ServiceProbe::Absent { .. }]));
}

#[cfg(unix)]
#[test]
fn a_unit_other_users_can_rewrite_is_ignored() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(&unit, &installer_unit(root.path()));
    std::fs::set_permissions(&unit, std::fs::Permissions::from_mode(0o666)).unwrap();
    match probe(&systemd_at(&unit)).as_slice() {
        [ServiceProbe::Ignored { reason, .. }] => {
            assert!(reason.contains("writable by every user"), "{reason}")
        }
        other => panic!("expected the unit to be ignored, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_unit_its_group_can_rewrite_is_ignored() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(&unit, &installer_unit(root.path()));
    std::fs::set_permissions(&unit, std::fs::Permissions::from_mode(0o664)).unwrap();
    match probe(&systemd_at(&unit)).as_slice() {
        [ServiceProbe::Ignored { reason, .. }] => {
            assert!(reason.contains("writable by group"), "{reason}");
            assert!(reason.contains("chmod g-w"), "{reason}");
        }
        other => panic!("expected the unit to be ignored, got {other:?}"),
    }
}

#[test]
fn credentials_named_with_percent_d_are_read_from_their_load_credential_source() {
    let root = tempfile::tempdir().unwrap();
    let url_file = root.path().join("credstore/database-url");
    write(&url_file, &format!("{URL}\n"));
    let key_file = root.path().join("credstore/encryption-key");
    write(&key_file, "key");
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nLoadCredential=db:{}\nLoadCredential=key:{}\n\
             Environment=TEMPS_DATABASE_URL_FILE=%d/db TEMPS_ENCRYPTION_KEY_FILE=%d/key \
             LITERAL=100%%\nExecStart=/usr/bin/temps serve\n",
            url_file.display(),
            key_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition
            .database_url
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .1,
        UrlLocation::UrlFile(url_file)
    );
    assert_eq!(
        definition.env("TEMPS_ENCRYPTION_KEY_FILE"),
        Some(&*key_file.display().to_string())
    );
    assert_eq!(definition.env("LITERAL"), Some("100%"));
    assert!(definition.environment_gaps.is_empty());
}

#[test]
fn relative_paths_resolve_against_the_services_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let url_file = root.path().join("secrets/database-url");
    write(&url_file, &format!("{URL}\n"));
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nWorkingDirectory={}\n\
             Environment=TEMPS_DATABASE_URL_FILE=secrets/database-url \
             TEMPS_AUTH_SECRET_FILE=./secrets/auth\n\
             ExecStart=/usr/bin/temps serve --data-dir data\n",
            root.path().display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.env("TEMPS_AUTH_SECRET_FILE"),
        Some(&*root.path().join("secrets/auth").display().to_string())
    );
    assert_eq!(
        definition.data_dir,
        Ok((root.path().join("data"), DataDirLocation::CommandLine))
    );

    // No WorkingDirectory=: systemd starts the service in `/`.
    write(
        &unit,
        "[Service]\nEnvironment=TEMPS_AUTH_SECRET_FILE=secrets/auth\n\
         ExecStart=/usr/bin/temps serve --data-dir srv/data\n",
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.working_directory, Some(PathBuf::from("/")));
    assert_eq!(
        definition.env("TEMPS_AUTH_SECRET_FILE"),
        Some("/secrets/auth")
    );
    assert_eq!(
        definition.data_dir,
        Ok((PathBuf::from("/srv/data"), DataDirLocation::CommandLine))
    );

    // A WorkingDirectory= that cannot be resolved leaves them unknown.
    write(
        &unit,
        "[Service]\nUser=temps-test-no-such-user\nWorkingDirectory=~\n\
         Environment=TEMPS_AUTH_SECRET_FILE=secrets/auth \
         TEMPS_DATABASE_URL_FILE=database-url\n\
         ExecStart=/usr/bin/temps serve --data-dir data\n",
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.working_directory, None);
    let url_error = definition.database_url.as_ref().unwrap_err();
    assert!(url_error.contains("WorkingDirectory=~"), "{url_error}");
    let dir_error = definition.data_dir.as_ref().unwrap_err();
    assert!(dir_error.contains("is relative"), "{dir_error}");
    let gap = definition.incomplete_environment().unwrap();
    assert!(gap.contains("TEMPS_AUTH_SECRET_FILE"), "{gap}");
}

#[cfg(unix)]
#[test]
fn an_unreadable_environment_file_leaves_what_it_may_set_unknown() {
    let root = tempfile::tempdir().unwrap();
    let env_file = root.path().join("temps.env");
    write(&env_file, "TEMPS_DATABASE_URL=postgres://from-file/db\n");
    if !make_unreadable(&env_file) {
        return;
    }
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=TEMPS_DATABASE_URL={URL} HOME=/home/svc\n\
             EnvironmentFile={}\nExecStart=/usr/bin/temps serve\n",
            env_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    // Not Environment='s URL: the file the server reads may override it.
    let error = definition.database_url.as_ref().unwrap_err();
    assert!(error.contains("temps.env"), "{error}");
    assert!(definition.data_dir.is_err());
    assert!(definition.incomplete_environment().is_some());

    // Flags on ExecStart= are certain whatever the file sets...
    write(
        &unit,
        &format!(
            "[Service]\nEnvironmentFile={}\n\
             ExecStart=/usr/bin/temps serve --database-url={URL} --data-dir=/srv/data\n",
            env_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.data_dir,
        Ok((PathBuf::from("/srv/data"), DataDirLocation::CommandLine))
    );

    // ...unless they expand a variable: the file, applied after
    // Environment=, may override even one Environment= sets.
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=DB={URL} DIR=/srv/data\nEnvironmentFile={}\n\
             ExecStart=/usr/bin/temps serve --database-url=${{DB}} --data-dir=${{DIR}}\n",
            env_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    let error = definition.database_url.as_ref().unwrap_err();
    assert!(error.contains("$DB, $DIR"), "{error}");
    assert!(definition.data_dir.is_err());

    // The file may also supply the subcommand: whether the unit runs
    // `serve` at all is unknown, so its URL is too, never "none".
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=TEMPS_DATABASE_URL={URL}\nEnvironmentFile={}\n\
             ExecStart=/usr/bin/temps $ARGS\n",
            env_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    let error = definition.database_url.as_ref().unwrap_err();
    assert!(
        error.contains("$ARGS") && error.contains("temps.env"),
        "{error}"
    );
    assert!(definition.data_dir.is_err());
    assert!(
        !definition
            .problems
            .iter()
            .any(|p| p.contains("temps serve")),
        "{:?}",
        definition.problems
    );

    write(
        &unit,
        &format!(
            "[Service]\nEnvironmentFile={}\n\
             ExecStart=/usr/bin/temps serve --database-url=${{DB}} --data-dir=/srv/data\n",
            env_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    let error = definition.database_url.as_ref().unwrap_err();
    assert!(error.contains("$DB"), "{error}");
    assert!(definition.data_dir.is_err());
}

#[test]
fn the_launchd_agent_yields_its_url_and_data_dir() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let agent = home.join(LAUNCHD_AGENT_PATH);
    let escaped_url = URL.replace('&', "&amp;");
    write(
        &agent,
        &format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n\
             <key>Label</key>\n<string>dev.temps.temps</string>\n\
             <key>ProgramArguments</key>\n<array>\n\
             <string>{h}/.temps/bin/temps</string>\n<string>serve</string>\n\
             <string>--database-url={escaped_url}</string>\n\
             <string>--data-dir={h}/.temps/data</string>\n</array>\n\
             <key>WorkingDirectory</key>\n<string>{h}/.temps</string>\n</dict>\n</plist>\n",
            h = home.display()
        ),
    );
    let definition = found(probe(&ServiceLocations {
        systemd_unit: None,
        launchd_agent: Some(agent),
    }));
    assert_eq!(definition.kind, ServiceKind::Launchd);
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition.data_dir,
        Ok((home.join(".temps/data"), DataDirLocation::CommandLine))
    );
    assert_eq!(definition.env("HOME"), Some(&*home.display().to_string()));
}

#[test]
fn debug_output_never_contains_the_url_or_environment_values() {
    let root = tempfile::tempdir().unwrap();
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=TEMPS_AUTH_SECRET=hunter2-hunter2\n\
             ExecStart=/usr/bin/temps serve --database-url={URL}\n"
        ),
    );
    let debug = format!("{:?}", found(probe(&systemd_at(&unit))));
    assert!(!debug.contains("s3cr3t-pw"), "{debug}");
    assert!(!debug.contains("hunter2"), "{debug}");
    assert!(debug.contains("TEMPS_AUTH_SECRET"), "{debug}");
}

#[test]
fn words_split_like_systemd() {
    assert_eq!(
        split_words(r#"a "b c" --x="d e" 'f\'g' h\ i"#),
        ["a", "b c", "--x=d e", "f'g", "h i"]
    );
    assert_eq!(
        parse_environment_file("A=1\n; note\nB='two words'\nC=\"q\\\"d\"\n"),
        [
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "two words".to_string()),
            ("C".to_string(), "q\"d".to_string()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn drop_ins_merge_across_load_paths_as_systemd_does() {
    let root = tempfile::tempdir().unwrap();
    let low = root.path().join("usr-lib");
    let high = root.path().join("run");
    let unit = low.join("temps.service");
    write(
        &unit,
        "[Service]\nExecStart=/usr/bin/temps serve --data-dir /base\n",
    );
    // The same name in a higher-priority directory hides the lower one.
    write(
        &low.join("temps.service.d/10-dir.conf"),
        "[Service]\nExecStart=\nExecStart=/usr/bin/temps serve --data-dir /low\n",
    );
    write(
        &high.join("temps.service.d/10-dir.conf"),
        "[Service]\nExecStart=\nExecStart=/usr/bin/temps serve --data-dir /high\n",
    );
    // Type-level drop-ins apply to every service.
    write(
        &high.join("service.d/20-env.conf"),
        "[Service]\nEnvironment=FROM_TYPE_DROP_IN=1\n",
    );
    // A drop-in linked to /dev/null masks the same name below it.
    write(
        &low.join("temps.service.d/30-masked.conf"),
        "[Service]\nEnvironment=MASKED=1\n",
    );
    std::fs::create_dir_all(high.join("temps.service.d")).unwrap();
    std::os::unix::fs::symlink("/dev/null", high.join("temps.service.d/30-masked.conf")).unwrap();
    // A unit-specific drop-in hides a type-wide one of the same name even
    // from a lower-priority directory.
    write(
        &high.join("service.d/40-database.conf"),
        "[Service]\nEnvironment=TEMPS_DATABASE_URL=postgres://db.invalid/type-wide\n",
    );
    write(
        &low.join("temps.service.d/40-database.conf"),
        "[Service]\nEnvironment=TEMPS_DATABASE_URL=postgres://db.invalid/unit-specific\n",
    );

    let mut sources = vec![(unit.clone(), std::fs::read_to_string(&unit).unwrap())];
    sources.extend(read_drop_ins(&unit, &[high.clone(), low.clone()]).unwrap());
    let definition = systemd_definition(&unit, &sources);
    assert_eq!(
        definition.data_dir,
        Ok((PathBuf::from("/high"), DataDirLocation::CommandLine))
    );
    assert_eq!(definition.env("FROM_TYPE_DROP_IN"), Some("1"));
    assert_eq!(definition.env("MASKED"), None);
    assert_eq!(
        definition.url(),
        Some("postgres://db.invalid/unit-specific")
    );

    // The installer's unit is read with systemd's load paths, runtime
    // overrides included.
    let roots = drop_in_roots(Path::new(SYSTEMD_UNIT_PATH));
    for expected in ["/run/systemd/system", "/etc/systemd/system.control"] {
        assert!(roots.contains(&PathBuf::from(expected)), "{roots:?}");
    }
}

#[test]
fn a_drop_in_entry_that_cannot_be_listed_fails_the_unit() {
    let dir = Path::new("/etc/systemd/system/temps.service.d");
    let mut chosen = BTreeMap::new();
    let error = choose_drop_ins(
        dir,
        [
            Ok(dir.join("10-dir.conf")),
            Err(std::io::Error::other("injected entry failure")),
            Ok(dir.join("20-db.conf")),
        ]
        .into_iter(),
        &mut chosen,
    )
    .unwrap_err();
    assert!(
        error.contains("temps.service.d cannot be listed")
            && error.contains("injected entry failure"),
        "{error}"
    );

    // Readable entries are chosen as before: `.conf` only, the first name wins.
    let mut chosen = BTreeMap::new();
    chosen.insert(
        "10-dir.conf".into(),
        PathBuf::from("/run/higher/10-dir.conf"),
    );
    choose_drop_ins(
        dir,
        [
            Ok(dir.join("10-dir.conf")),
            Ok(dir.join("README")),
            Ok(dir.join("20-db.conf")),
        ]
        .into_iter(),
        &mut chosen,
    )
    .unwrap();
    assert_eq!(
        chosen.into_values().collect::<Vec<_>>(),
        vec![
            PathBuf::from("/run/higher/10-dir.conf"),
            dir.join("20-db.conf")
        ]
    );
}

#[test]
fn unset_environment_removes_what_the_server_does_not_get() {
    let root = tempfile::tempdir().unwrap();
    let url_file = root.path().join("database-url");
    write(&url_file, &format!("{URL}\n"));
    let unit = root.path().join("temps.service");
    write(
        &unit,
        &format!(
            "[Service]\nEnvironment=TEMPS_DATABASE_URL=postgres://old/db KEEP=a DROP_EXACT=b\n\
             LoadCredential=temps_database_url:{}\n\
             UnsetEnvironment=TEMPS_DATABASE_URL KEEP=other DROP_EXACT=b\n\
             ExecStart=/usr/bin/temps serve\n",
            url_file.display()
        ),
    );
    let definition = found(probe(&systemd_at(&unit)));
    assert_eq!(definition.url(), Some(URL));
    assert_eq!(
        definition
            .database_url
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .1,
        UrlLocation::Credential(url_file)
    );
    // `NAME=VALUE` removes only that exact assignment.
    assert_eq!(definition.env("KEEP"), Some("a"));
    assert_eq!(definition.env("DROP_EXACT"), None);
}

#[test]
fn environment_files_use_their_own_escape_rules() {
    let parsed = parse_environment_file(
        "# comment\n\
         TEMPS_DATA_DIR=/srv/temps\\ data\n\
         SINGLE='kept\\as is'\n\
         DOUBLE=\"a\\\"b\\\\c\\d\"\n\
         JOINED=one\\\ntwo\n\
         TRAILING=value   \n\
         SPACED=  two words  \n\
         HASH=#not-a-comment\n\
         EMPTY=\n\
         not an assignment\n",
    );
    assert_eq!(
        parsed,
        [
            ("TEMPS_DATA_DIR", "/srv/temps data"),
            ("SINGLE", "kept\\as is"),
            ("DOUBLE", "a\"b\\c\\d"),
            ("JOINED", "onetwo"),
            ("TRAILING", "value"),
            ("SPACED", "two words"),
            ("HASH", "#not-a-comment"),
            ("EMPTY", ""),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()))
    );
}
