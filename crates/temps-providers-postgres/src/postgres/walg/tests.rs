// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`] (`walg.rs`), kept beside it so the implementation
//! file stays readable: the generated `walg.env`, the archive target check,
//! the start-time repair, and the Docker test against a real WAL-G image
//! and a plain-HTTP S3 store (`docker-tests` feature; skips without Docker).

/// The reported outage end to end, against real WAL-G and a real plain-HTTP
/// S3 store reached by IP address: the settings Temps generates archive a
/// segment, and the settings it generated before this fix are refused with
/// the same error the outage logged. Skips when Docker or an image is
/// unavailable.
#[cfg(feature = "docker-tests")]
mod plain_http_ip_archive_docker_tests {
    use super::super::{archive_walg_env_lines, PostgresService};
    use crate::externalsvc::S3Credentials;
    use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    };
    use futures::StreamExt;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    const RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0";
    const WALG_IMAGE: &str = "gotempsh/postgres-walg:18-bookworm";
    const ACCESS_KEY: &str = "walg-test-access";
    const SECRET_KEY: &str = "walg-test-secret-key";
    const BUCKET: &str = "archive";

    async fn ensure_image(docker: &bollard::Docker, image: &str) -> bool {
        if docker.inspect_image(image).await.is_ok() {
            return true;
        }
        let (from_image, tag) = image.rsplit_once(':').unwrap_or((image, "latest"));
        let options = CreateImageOptionsBuilder::default()
            .from_image(from_image)
            .tag(tag)
            .build();
        let mut pull = docker.create_image(Some(options), None, None);
        while let Some(step) = pull.next().await {
            if let Err(error) = step {
                eprintln!("Skipping plain-HTTP WAL-G test: cannot pull {image}: {error}");
                return false;
            }
        }
        true
    }

    async fn run(
        docker: &Arc<bollard::Docker>,
        s3_name: &str,
        walg_name: &str,
    ) -> Result<(), String> {
        // The S3 store: published on a random loopback port so this process
        // can create the bucket, and reached by its bridge IP from the WAL-G
        // container, exactly like an S3 source entered as `<ip>:9000`.
        let mut ports = HashMap::new();
        ports.insert(
            "9000/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(String::new()),
            }]),
        );
        docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::default()
                        .name(s3_name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(RUSTFS_IMAGE.to_string()),
                    env: Some(vec![
                        format!("RUSTFS_ACCESS_KEY={ACCESS_KEY}"),
                        format!("RUSTFS_SECRET_KEY={SECRET_KEY}"),
                    ]),
                    exposed_ports: Some(vec!["9000/tcp".to_string()]),
                    host_config: Some(HostConfig {
                        port_bindings: Some(ports),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("create S3 container: {e}"))?;
        docker
            .start_container(s3_name, None)
            .await
            .map_err(|e| format!("start S3 container: {e}"))?;
        let inspect = docker
            .inspect_container(s3_name, None)
            .await
            .map_err(|e| format!("inspect S3 container: {e}"))?;
        let settings = inspect
            .network_settings
            .ok_or("S3 container has no network")?;
        let ip = settings
            .networks
            .as_ref()
            .and_then(|networks| networks.values().find_map(|n| n.ip_address.clone()))
            .filter(|ip| !ip.is_empty())
            .ok_or("S3 container has no bridge IP")?;
        let host_port = settings
            .ports
            .as_ref()
            .and_then(|ports| ports.get("9000/tcp").cloned().flatten())
            .and_then(|bindings| bindings.into_iter().find_map(|b| b.host_port))
            .ok_or("S3 port not published")?;

        let admin = S3Credentials {
            access_key_id: ACCESS_KEY.to_string(),
            secret_key: SECRET_KEY.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some(format!("127.0.0.1:{host_port}")),
            bucket_name: BUCKET.to_string(),
            bucket_path: "pg".to_string(),
            force_path_style: true,
        }
        .build_s3_client()
        .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            match admin.create_bucket().bucket(BUCKET).send().await {
                Ok(_) => break,
                Err(error) if tokio::time::Instant::now() >= deadline => {
                    return Err(format!("create bucket: {error:?}"))
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }

        // The WAL-G side: Temps's PostgreSQL image, idle.
        docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::default()
                        .name(walg_name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(WALG_IMAGE.to_string()),
                    entrypoint: Some(vec!["sleep".to_string()]),
                    cmd: Some(vec!["600".to_string()]),
                    user: Some("postgres".to_string()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("create WAL-G container: {e}"))?;
        docker
            .start_container(walg_name, None)
            .await
            .map_err(|e| format!("start WAL-G container: {e}"))?;

        // The reported S3 source: plain HTTP, an IP, no scheme, path style
        // left unticked.
        let source = S3Credentials {
            access_key_id: ACCESS_KEY.to_string(),
            secret_key: SECRET_KEY.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some(format!("{ip}:9000")),
            bucket_name: BUCKET.to_string(),
            bucket_path: "pg".to_string(),
            force_path_style: false,
        };
        let prefix = format!("s3://{BUCKET}/pg/walg");
        let service = PostgresService::new("plain-http-archive".to_string(), docker.clone());
        let resolved = source
            .resolve_endpoint_for_container(docker, walg_name)
            .await;
        let env = archive_walg_env_lines(&source, &prefix, resolved.as_deref());
        if !env.contains(&format!("AWS_ENDPOINT=http://{ip}:9000"))
            || !env.contains(&"AWS_S3_FORCE_PATH_STYLE=true".to_string())
        {
            return Err(format!("generated walg.env is wrong: {:?}", env));
        }

        // 1. The check Temps now runs before enabling archiving passes, and
        //    running it again reuses the same probe key.
        for _ in 0..2 {
            service
                .check_archive_target_env(walg_name, &env, &source)
                .await
                .map_err(|e| format!("check with the generated settings failed: {e}"))?;
        }

        // 2. A real segment archives through `walg.env`, as archive_command
        //    runs it.
        service
            .write_walg_env_file(walg_name, &env)
            .await
            .map_err(|e| format!("write walg.env: {e}"))?;
        let push = crate::externalsvc::exec_util::run_exec(
            docker,
            walg_name,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "set -e; mkdir -p /tmp/pg_wal/archive_status; \
                 head -c 16777216 /dev/zero > /tmp/pg_wal/000000010000000000000001; \
                 . /var/lib/postgresql/walg.env && S3_MAX_RETRIES=1 timeout 60 \
                 wal-g wal-push /tmp/pg_wal/000000010000000000000001 \
                 && wal-g st ls -r"
                    .to_string(),
            ],
            None,
            Duration::from_secs(90),
        )
        .await
        .map_err(|e| format!("wal-push exec: {e}"))?;
        if push.exit_code != 0 || !push.output.contains("000000010000000000000001") {
            return Err(format!(
                "wal-push failed (exit {}): {}",
                push.exit_code, push.output
            ));
        }
        // With credentials that may delete, the probe is removed.
        if push.output.contains(super::super::ARCHIVE_CHECK_PROBE_KEY) {
            return Err(format!("the check left its probe behind: {}", push.output));
        }

        // 3. What Temps generated before the fix, both ways it failed:
        //    a bare endpoint, which WAL-G dials over HTTPS, and, until path
        //    style was ticked, a `<bucket>.<ip>` host that cannot resolve.
        let legacy = |path_style: bool| {
            let mut env = vec![
                format!("WALG_S3_PREFIX={prefix}"),
                format!("AWS_ACCESS_KEY_ID={ACCESS_KEY}"),
                format!("AWS_SECRET_ACCESS_KEY={SECRET_KEY}"),
                "AWS_REGION=us-east-1".to_string(),
                format!("AWS_ENDPOINT={ip}:9000"),
            ];
            if path_style {
                env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
            }
            env
        };
        for (path_style, cause) in [
            (true, "HTTP response to HTTPS client"),
            (false, "no such host"),
        ] {
            match service
                .check_archive_target_env(walg_name, &legacy(path_style), &source)
                .await
            {
                Ok(_) => return Err("the pre-fix settings unexpectedly archived".to_string()),
                Err(error) => {
                    let text = error.to_string();
                    if !text.contains(cause) || !text.contains("missing (WAL-G uses https)") {
                        return Err(format!("unexpected pre-fix failure: {text}"));
                    }
                    if text.contains(SECRET_KEY) || text.contains("docker exec") {
                        return Err(format!("the check error is not clean: {text}"));
                    }
                }
            }
        }

        // 4. An existing install on its next start: the pre-fix walg.env is
        //    in place and PostgreSQL has already started a wal-push with it,
        //    which hangs retrying HTTPS against a plain-HTTP store. The
        //    start-time repair rewrites the file and ends that attempt, and
        //    the next segment archives.
        service
            .write_walg_env_file(walg_name, &legacy(true))
            .await
            .map_err(|e| format!("write legacy walg.env: {e}"))?;
        let exec = |script: &str| {
            crate::externalsvc::exec_util::run_exec(
                docker,
                walg_name,
                vec!["sh".to_string(), "-c".to_string(), script.to_string()],
                None,
                Duration::from_secs(90),
            )
        };
        // A line an operator added by hand, which the repair must keep.
        exec("echo '# kept: operator note' >> /var/lib/postgresql/walg.env")
            .await
            .map_err(|e| format!("append to walg.env: {e}"))?;
        exec(
            "head -c 16777216 /dev/zero > /tmp/pg_wal/000000010000000000000002; \
             . /var/lib/postgresql/walg.env && \
             nohup wal-g wal-push /tmp/pg_wal/000000010000000000000002 >/tmp/stale-push.log 2>&1 &",
        )
        .await
        .map_err(|e| format!("start the stale wal-push: {e}"))?;
        let running = "for p in /proc/[0-9]*; do \
                         case \"$(tr '\\0' ' ' < $p/cmdline 2>/dev/null)\" in \
                           'wal-g wal-push '*) echo running;; esac; done; exit 0";
        tokio::time::sleep(Duration::from_secs(2)).await;
        if !exec(running)
            .await
            .map_err(|e| e.to_string())?
            .output
            .contains("running")
        {
            return Err("the stale wal-push did not keep running".to_string());
        }

        service.repair_walg_env_on_start(walg_name).await;

        let live = service
            .read_live_walg_env(walg_name)
            .await
            .map_err(|e| format!("read repaired walg.env: {e}"))?
            .ok_or("walg.env is gone after the repair")?;
        if !live.contains(&format!("AWS_ENDPOINT=http://{ip}:9000"))
            || !live.contains(&format!("AWS_SECRET_ACCESS_KEY={SECRET_KEY}"))
            || !live.contains(&format!("WALG_S3_PREFIX={prefix}"))
        {
            return Err(format!("walg.env was not repaired in place: {live:?}"));
        }
        let snapshot = service
            .read_walg_env_snapshot(walg_name)
            .await
            .map_err(|e| format!("read repaired walg.env: {e}"))?
            .ok_or("walg.env is gone after the repair")?;
        let raw = snapshot.raw.clone();
        if !raw.contains("# kept: operator note") {
            return Err(format!("the repair dropped a line it does not fix: {raw}"));
        }
        // A write built from an older snapshot is refused in the real image
        // (its sh, cksum and flock), and the file is left as it is.
        let refused = service
            .write_file_lines_if_unchanged(
                walg_name,
                &["export AWS_ENDPOINT='https://stale.example'".to_string()],
                "/var/lib/postgresql/walg.env",
                Some("0 0"),
            )
            .await
            .map_err(|e| format!("conditional write: {e}"))?;
        if refused != super::super::WalgEnvWrite::ChangedConcurrently {
            return Err(format!(
                "a stale conditional write was not refused: {refused:?}"
            ));
        }
        let after = service
            .read_walg_env_snapshot(walg_name)
            .await
            .map_err(|e| format!("read walg.env: {e}"))?
            .ok_or("walg.env is gone after a refused write")?;
        if after.checksum != snapshot.checksum || after.raw.contains("stale.example") {
            return Err(format!("a refused write changed walg.env: {}", after.raw));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        if exec(running)
            .await
            .map_err(|e| e.to_string())?
            .output
            .contains("running")
        {
            return Err("the wal-push started with the old walg.env is still running".to_string());
        }
        let retried = exec(
            ". /var/lib/postgresql/walg.env && S3_MAX_RETRIES=1 timeout 60 \
             wal-g wal-push /tmp/pg_wal/000000010000000000000002 && wal-g st ls -r",
        )
        .await
        .map_err(|e| format!("retry wal-push: {e}"))?;
        if retried.exit_code != 0 || !retried.output.contains("000000010000000000000002") {
            return Err(format!(
                "the segment did not archive after the repair (exit {}): {}",
                retried.exit_code, retried.output
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn generated_walg_env_archives_to_a_plain_http_store_by_ip() {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Skipping plain-HTTP WAL-G test: no Docker configuration");
            return;
        };
        if docker.ping().await.is_err() {
            eprintln!("Skipping plain-HTTP WAL-G test: Docker is not reachable");
            return;
        }
        if !ensure_image(&docker, RUSTFS_IMAGE).await || !ensure_image(&docker, WALG_IMAGE).await {
            return;
        }
        let docker = Arc::new(docker);
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let s3_name = format!("temps-test-walg-http-s3-{}", &run_id[..12]);
        let walg_name = format!("temps-test-walg-http-pg-{}", &run_id[..12]);
        let outcome = run(&docker, &s3_name, &walg_name).await;
        // Only the two containers this test created.
        for name in [&walg_name, &s3_name] {
            let _ = docker
                .remove_container(
                    name,
                    Some(
                        RemoveContainerOptionsBuilder::default()
                            .force(true)
                            .v(true)
                            .build(),
                    ),
                )
                .await;
        }
        if let Err(reason) = outcome {
            panic!("{reason}");
        }
    }
}

mod target_check_tests {
    use super::super::{
        archive_target_check_exec_deadline, archive_target_check_script, archive_target_settings,
        summarize_walg_output, ArchiveTargetCheckError, ARCHIVE_CHECK_PROBE_KEY,
        ARCHIVE_PROBE_CLEANUP_TIMEOUT_SECS, ARCHIVE_TARGET_CHECK_TIMEOUT_SECS, CHECK_EXIT_MARKER,
    };
    use crate::externalsvc::S3Credentials;

    fn source(endpoint: Option<&str>, force_path_style: bool) -> S3Credentials {
        S3Credentials {
            access_key_id: "test-access".to_string(),
            secret_key: "test-secret".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: endpoint.map(str::to_string),
            bucket_name: "archive".to_string(),
            bucket_path: "pg".to_string(),
            force_path_style,
        }
    }

    #[test]
    fn the_check_overwrites_one_fixed_probe_and_never_a_random_one() {
        let script = archive_target_check_script();
        // `st check write` names its probe randomly, so a credential that
        // cannot delete would leave one object per check.
        assert!(!script.contains("st check"), "{script}");
        assert!(
            script.contains("wal-g st put -f --no-compress --no-encrypt"),
            "{script}"
        );
        assert!(
            script.contains(&format!("\"$probe\" {ARCHIVE_CHECK_PROBE_KEY}")),
            "{script}"
        );
        // The removal names the probe key exactly, never an empty prefix,
        // and only runs after a successful upload.
        assert!(
            script.contains(&format!("wal-g st rm {ARCHIVE_CHECK_PROBE_KEY} ")),
            "{script}"
        );
        assert!(!ARCHIVE_CHECK_PROBE_KEY.is_empty());
        assert!(!ARCHIVE_CHECK_PROBE_KEY.contains('/'));
        assert!(script.find("st put").unwrap_or(usize::MAX) < script.find("st rm").unwrap_or(0));
        assert!(script.contains(&format!("echo \"{CHECK_EXIT_MARKER}$code\"; exit 0")));
    }

    /// An upload that succeeds just inside its bound, followed by a removal
    /// that hangs until its own bound, still reports success: the exec
    /// deadline outlasts both, so it is never what fails a writable target.
    #[test]
    fn the_exec_deadline_outlasts_a_slow_upload_and_a_stalled_cleanup() {
        let script = archive_target_check_script();
        assert!(
            script.contains(&format!(
                "timeout {ARCHIVE_TARGET_CHECK_TIMEOUT_SECS} wal-g st put"
            )),
            "{script}"
        );
        assert!(
            script.contains(&format!(
                "timeout {ARCHIVE_PROBE_CLEANUP_TIMEOUT_SECS} wal-g st rm"
            )),
            "{script}"
        );
        // The only two commands that talk to the target are bounded.
        assert_eq!(script.matches("wal-g st ").count(), 2, "{script}");
        assert_eq!(script.matches("timeout ").count(), 2, "{script}");
        let worst_case = ARCHIVE_TARGET_CHECK_TIMEOUT_SECS + ARCHIVE_PROBE_CLEANUP_TIMEOUT_SECS;
        assert!(
            archive_target_check_exec_deadline().as_secs() >= worst_case + 10,
            "deadline {:?} vs {worst_case}s of sequential bounds",
            archive_target_check_exec_deadline()
        );
    }

    #[test]
    fn settings_name_the_scheme_and_why_path_style_is_forced() {
        let env = vec![
            "AWS_ENDPOINT=http://10.0.0.5:9000".to_string(),
            "AWS_S3_FORCE_PATH_STYLE=true".to_string(),
        ];
        let settings = archive_target_settings(&env, &source(Some("10.0.0.5:9000"), false));
        assert_eq!(settings.endpoint, "http://10.0.0.5:9000");
        assert_eq!(settings.scheme, "http");
        assert!(settings.force_path_style);
        assert_eq!(
            settings.path_style_reason,
            "forced: the endpoint is an IP address"
        );
    }

    #[test]
    fn settings_for_aws_s3_say_so() {
        let settings = archive_target_settings(&[], &source(None, false));
        assert_eq!(settings.endpoint, "AWS S3 (region us-east-1)");
        assert_eq!(settings.scheme, "aws");
        assert!(!settings.force_path_style);
    }

    #[test]
    fn the_exit_marker_is_split_from_walg_output() {
        let (output, code) =
            super::super::split_check_exit("ERROR: refused\n__TEMPS_WALG_CHECK_EXIT__=1\n")
                .unwrap();
        assert_eq!(output, "ERROR: refused\n");
        assert_eq!(code, 1);
    }

    /// A failed read of walg.env keeps what failed and drops what the
    /// command printed, which is the file and its credentials.
    #[test]
    fn a_reader_error_never_carries_the_file() {
        let error = "Container exec failed in pg-1: docker inspect_exec failed for abc in \
                     container pg-1: timeout. output captured (60 bytes):\n\
                     __TEMPS_WALG_ENV_RAW__\nexport AWS_SECRET_ACCESS_KEY='hunter2'\n";
        let kept = super::super::exec_error_without_output(error);
        assert!(kept.contains("docker inspect_exec failed"), "{kept}");
        assert!(!kept.contains("hunter2"), "{kept}");
        assert!(!kept.contains("AWS_SECRET_ACCESS_KEY"), "{kept}");
    }

    /// The script always exits 0, so output cut short before its marker
    /// (or with a damaged one) must never read as a passed check.
    #[test]
    fn output_without_a_readable_exit_marker_is_no_result() {
        assert_eq!(
            super::super::split_check_exit("uploading"),
            Err("uploading".to_string())
        );
        assert!(super::super::split_check_exit("__TEMPS_WALG_CHECK_EXIT__=").is_err());
        assert!(super::super::split_check_exit("__TEMPS_WALG_CHECK_EXIT__=0x").is_err());
    }

    #[test]
    fn a_scheme_less_endpoint_reports_what_walg_does_with_it() {
        let env = vec!["AWS_ENDPOINT=10.0.0.5:9000".to_string()];
        let settings = archive_target_settings(&env, &source(Some("10.0.0.5:9000"), false));
        assert_eq!(settings.scheme, "missing (WAL-G uses https)");
        assert!(!settings.force_path_style);
        assert_eq!(settings.path_style_reason, "as configured on the S3 source");
    }

    #[test]
    fn walg_errors_are_kept_and_noise_dropped() {
        let output = "INFO: 2026/01/01 starting\n\
            ERROR: 2026/01/01 failed to read from the storage: send request failed\n\
            caused by: Head \"https://10.0.0.5:9000/archive/x\": http: server gave HTTP response to HTTPS client\n";
        let summary = summarize_walg_output(output);
        assert!(summary.contains("server gave HTTP response to HTTPS client"));
        assert!(!summary.contains("starting"));
        assert_eq!(summarize_walg_output(""), "no output");
    }

    #[test]
    fn the_error_names_endpoint_scheme_path_style_and_cause() {
        let error = ArchiveTargetCheckError {
            service_name: "orders".to_string(),
            endpoint: "http://10.0.0.5:9000".to_string(),
            scheme: "http".to_string(),
            force_path_style: false,
            path_style_reason: "as configured on the S3 source".to_string(),
            message: "dial tcp: lookup archive.10.0.0.5: no such host".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("'orders'"));
        assert!(text.contains("http://10.0.0.5:9000"));
        assert!(text.contains("scheme http"));
        assert!(text.contains("path-style addressing off"));
        assert!(text.contains("no such host"));
        assert!(text.contains("Archiving settings were not changed"));
    }
}

mod repair_tests {
    use super::super::{
        patch_walg_env_file, repair_walg_env_lines, walg_env_drift, WalgEnvRepairProbe,
    };
    use temps_core::s3_endpoint::S3Scheme;

    #[test]
    fn the_repair_keeps_every_line_it_does_not_fix() {
        let raw = "# added by an operator\n\
                   export WALG_S3_PREFIX='s3://archive/pg/walg'\n\
                   export AWS_ENDPOINT='10.0.0.5:9000'\n\
                   export S3_MAX_RETRIES='1'\n\
                   \n\
                   AWS_S3_FORCE_PATH_STYLE=false\n\
                   export AWS_ENDPOINT_SUFFIX='kept'\n";
        let evaluated = lines(&[
            "WALG_S3_PREFIX=s3://archive/pg/walg",
            "AWS_ENDPOINT=10.0.0.5:9000",
            "AWS_S3_FORCE_PATH_STYLE=false",
            "AWS_ENDPOINT_SUFFIX=kept",
        ]);
        let repaired = repair_walg_env_lines(&evaluated, Some(S3Scheme::Http)).unwrap();
        let patched = patch_walg_env_file(raw, &repaired);
        assert_eq!(
            patched,
            lines(&[
                "# added by an operator",
                "export WALG_S3_PREFIX='s3://archive/pg/walg'",
                "export S3_MAX_RETRIES='1'",
                "",
                "export AWS_ENDPOINT_SUFFIX='kept'",
                "export AWS_ENDPOINT='http://10.0.0.5:9000'",
                "export AWS_S3_FORCE_PATH_STYLE='true'",
            ])
        );
    }

    #[test]
    fn a_scheme_only_repair_leaves_the_path_style_line_alone() {
        let raw = "export AWS_ENDPOINT='minio.internal.example:9000'\n\
                   export AWS_S3_FORCE_PATH_STYLE='true'\n";
        let repaired = repair_walg_env_lines(
            &lines(&[
                "AWS_ENDPOINT=minio.internal.example:9000",
                "AWS_S3_FORCE_PATH_STYLE=true",
            ]),
            Some(S3Scheme::Http),
        )
        .unwrap();
        assert_eq!(
            patch_walg_env_file(raw, &repaired),
            lines(&[
                "export AWS_ENDPOINT='http://minio.internal.example:9000'",
                "export AWS_S3_FORCE_PATH_STYLE='true'",
            ])
        );
    }

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// A legacy bare endpoint: what a restart writes depends on which
    /// scheme the store answers first, so only a backup (which rewrites the
    /// file from the source) is promised to remove the difference.
    #[test]
    fn a_legacy_live_file_is_reported_with_the_backup_that_rewrites_it() {
        let live = lines(&[
            "WALG_S3_PREFIX=s3://archive/pg/walg",
            "AWS_SECRET_ACCESS_KEY=hunter2",
            "AWS_ENDPOINT=10.0.0.5:9000",
        ]);
        let generated = lines(&[
            "WALG_S3_PREFIX=s3://archive/pg/walg",
            "AWS_SECRET_ACCESS_KEY=hunter2",
            "AWS_ENDPOINT=http://10.0.0.5:9000",
            "AWS_S3_FORCE_PATH_STYLE=true",
        ]);
        let drift = walg_env_drift(&live, &generated).expect("they differ");
        assert!(
            drift.contains("10.0.0.5:9000 (no scheme, so WAL-G uses https)"),
            "{drift}"
        );
        assert!(drift.contains("instead of http://10.0.0.5:9000"), "{drift}");
        assert!(
            drift.contains("path-style addressing off instead of on"),
            "{drift}"
        );
        assert!(drift.contains("Run a backup"), "{drift}");
        assert!(!drift.contains("Restart the service"), "{drift}");
        assert!(!drift.contains("hunter2"), "{drift}");
    }

    /// A store that answers HTTPS: the restart's repair writes
    /// `https://`, not the source's `http://`, so it is never promised.
    #[test]
    fn a_bare_hostname_is_not_promised_the_sources_scheme_by_a_restart() {
        let live = lines(&["AWS_ENDPOINT=objects.example.com"]);
        let generated = lines(&["AWS_ENDPOINT=http://objects.example.com"]);
        let drift = walg_env_drift(&live, &generated).expect("they differ");
        assert!(drift.contains("Run a backup"), "{drift}");
        assert!(!drift.contains("Restart the service"), "{drift}");
    }

    /// Only path style missing for an IP endpoint: the repair forces it
    /// whatever the store answers, so a restart is the fix.
    #[test]
    fn a_missing_path_style_for_an_ip_is_fixed_by_a_restart() {
        let live = lines(&["AWS_ENDPOINT=http://10.0.0.5:9000"]);
        let generated = lines(&[
            "AWS_ENDPOINT=http://10.0.0.5:9000",
            "AWS_S3_FORCE_PATH_STYLE=true",
        ]);
        let drift = walg_env_drift(&live, &generated).expect("they differ");
        assert!(drift.contains("Restart the service"), "{drift}");
    }

    #[test]
    fn a_live_file_pointing_elsewhere_needs_a_backup_to_rewrite_it() {
        let live = lines(&["AWS_ENDPOINT=https://old.example.com"]);
        let generated = lines(&["AWS_ENDPOINT=https://s3.example.com"]);
        let drift = walg_env_drift(&live, &generated).expect("they differ");
        assert!(drift.contains("Run a backup"), "{drift}");
    }

    /// A legacy bare endpoint on another host: the start-time repair would
    /// add a scheme but keep the old host, so a restart cannot fix it.
    #[test]
    fn a_legacy_file_pointing_elsewhere_is_not_fixed_by_a_restart() {
        let live = lines(&["AWS_ENDPOINT=old.example:9000"]);
        let generated = lines(&["AWS_ENDPOINT=http://new.example:9000"]);
        let drift = walg_env_drift(&live, &generated).expect("they differ");
        assert!(drift.contains("Run a backup"), "{drift}");
        assert!(!drift.contains("Restart the service"), "{drift}");
    }

    #[test]
    fn a_live_file_matching_the_settings_is_not_drift() {
        let generated = lines(&[
            "AWS_ENDPOINT=http://10.0.0.5:9000",
            "AWS_S3_FORCE_PATH_STYLE=true",
            "AWS_SECRET_ACCESS_KEY=rotated",
        ]);
        let live = lines(&[
            "AWS_ENDPOINT=http://10.0.0.5:9000",
            "AWS_S3_FORCE_PATH_STYLE=true",
            "AWS_SECRET_ACCESS_KEY=old",
        ]);
        assert_eq!(walg_env_drift(&live, &generated), None);
    }

    #[test]
    fn a_bare_ip_endpoint_gets_a_scheme_and_path_style() {
        let before = lines(&[
            "WALG_S3_PREFIX=s3://archive/pg/walg",
            "AWS_ACCESS_KEY_ID=test-access",
            "AWS_SECRET_ACCESS_KEY=test-secret",
            "AWS_ENDPOINT=10.0.0.5:9000",
        ]);
        let after = repair_walg_env_lines(&before, Some(S3Scheme::Http)).expect("needs repair");
        assert_eq!(
            after,
            lines(&[
                "WALG_S3_PREFIX=s3://archive/pg/walg",
                "AWS_ACCESS_KEY_ID=test-access",
                "AWS_SECRET_ACCESS_KEY=test-secret",
                "AWS_ENDPOINT=http://10.0.0.5:9000",
                "AWS_S3_FORCE_PATH_STYLE=true",
            ])
        );
    }

    #[test]
    fn an_explicit_false_path_style_is_replaced_for_an_ip() {
        let after = repair_walg_env_lines(
            &lines(&[
                "AWS_ENDPOINT=http://10.0.0.5:9000",
                "AWS_S3_FORCE_PATH_STYLE=false",
            ]),
            Some(S3Scheme::Http),
        )
        .expect("needs repair");
        assert_eq!(
            after,
            lines(&[
                "AWS_ENDPOINT=http://10.0.0.5:9000",
                "AWS_S3_FORCE_PATH_STYLE=true"
            ])
        );
    }

    #[test]
    fn a_bare_hostname_gets_a_scheme_only() {
        let after = repair_walg_env_lines(
            &lines(&["AWS_ENDPOINT=objects.internal:9000"]),
            Some(S3Scheme::Http),
        )
        .expect("needs repair");
        assert_eq!(after, lines(&["AWS_ENDPOINT=http://objects.internal:9000"]));
    }

    /// WAL-G dials a scheme-less endpoint over HTTPS. A store it reaches
    /// that way keeps TLS: the scheme written is the one that worked.
    #[test]
    fn a_bare_hostname_reached_over_tls_keeps_tls() {
        let after = repair_walg_env_lines(
            &lines(&["AWS_ENDPOINT=objects.example.com"]),
            Some(S3Scheme::Https),
        )
        .expect("needs repair");
        assert_eq!(after, lines(&["AWS_ENDPOINT=https://objects.example.com"]));
        assert_eq!(WalgEnvRepairProbe::ORDER[0], S3Scheme::Https);
    }

    /// When no scheme was found to work, the endpoint is never guessed:
    /// a hostname is left alone, and an IP endpoint only gets path style.
    #[test]
    fn a_bare_endpoint_with_no_reachable_scheme_keeps_its_endpoint() {
        assert_eq!(
            repair_walg_env_lines(&lines(&["AWS_ENDPOINT=objects.example.com"]), None),
            None
        );
        assert_eq!(
            repair_walg_env_lines(&lines(&["AWS_ENDPOINT=10.0.0.5:9000"]), None),
            Some(lines(&[
                "AWS_ENDPOINT=10.0.0.5:9000",
                "AWS_S3_FORCE_PATH_STYLE=true"
            ]))
        );
    }

    /// The probe only lists the archive prefix, bounded, and always
    /// reports WAL-G's exit code on its marker line.
    #[test]
    fn the_scheme_probe_only_lists() {
        let script = WalgEnvRepairProbe::script();
        assert!(script.contains("timeout 15 wal-g st ls"), "{script}");
        assert!(
            !script.contains("st put") && !script.contains("st rm"),
            "{script}"
        );
        assert!(
            script.contains("__TEMPS_WALG_CHECK_EXIT__=$code"),
            "{script}"
        );
    }

    #[test]
    fn a_current_file_is_left_alone() {
        let http = Some(S3Scheme::Http);
        assert_eq!(
            repair_walg_env_lines(
                &lines(&[
                    "AWS_ENDPOINT=http://10.0.0.5:9000",
                    "AWS_S3_FORCE_PATH_STYLE=true",
                ]),
                http
            ),
            None
        );
        assert_eq!(
            repair_walg_env_lines(&lines(&["AWS_ENDPOINT=https://s3.example.com"]), http),
            None
        );
        // AWS S3 itself: nothing to repair.
        assert_eq!(
            repair_walg_env_lines(&lines(&["WALG_S3_PREFIX=s3://a/b"]), http),
            None
        );
    }
}

mod archive_env_tests {
    use super::super::archive_walg_env_lines;
    use crate::externalsvc::S3Credentials;

    fn http_ip_source() -> S3Credentials {
        S3Credentials {
            access_key_id: "test-access".to_string(),
            secret_key: "test-secret".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some("10.0.0.5:9000".to_string()),
            bucket_name: "archive".to_string(),
            bucket_path: "pg".to_string(),
            // The reported case: path style left off in the form.
            force_path_style: false,
        }
    }

    /// The reported outage: a plain-HTTP store at an IP address, stored
    /// without a scheme. `walg.env` must name `http://` (WAL-G otherwise
    /// dials HTTPS) and force path style (`<bucket>.<ip>` cannot resolve).
    #[tokio::test]
    async fn walg_env_for_a_plain_http_ip_source_has_a_scheme_and_path_style() {
        let s3 = http_ip_source();
        // A non-loopback endpoint never touches Docker; a lazy client is
        // enough.
        let docker = bollard::Docker::connect_with_http(
            "http://127.0.0.1:1",
            1,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("lazy docker client");
        let resolved = s3.resolve_endpoint_for_container(&docker, "unused").await;
        assert_eq!(resolved.as_deref(), Some("http://10.0.0.5:9000"));

        let env = archive_walg_env_lines(&s3, "s3://archive/pg/walg", resolved.as_deref());
        assert!(
            env.contains(&"AWS_ENDPOINT=http://10.0.0.5:9000".to_string()),
            "{env:?}"
        );
        assert!(
            env.contains(&"AWS_S3_FORCE_PATH_STYLE=true".to_string()),
            "{env:?}"
        );
    }

    #[test]
    fn walg_env_keeps_https_and_the_operators_choice_for_a_hostname() {
        let s3 = S3Credentials {
            endpoint: Some("https://s3.example.com".to_string()),
            ..http_ip_source()
        };
        let env =
            archive_walg_env_lines(&s3, "s3://archive/pg/walg", Some("https://s3.example.com"));
        assert!(env.contains(&"AWS_ENDPOINT=https://s3.example.com".to_string()));
        assert!(!env
            .iter()
            .any(|line| line.starts_with("AWS_S3_FORCE_PATH_STYLE")));
    }

    #[test]
    fn walg_env_adds_a_scheme_to_a_bare_resolved_endpoint() {
        let env =
            archive_walg_env_lines(&http_ip_source(), "s3://a/b", Some("objects.internal:9000"));
        assert!(env.contains(&"AWS_ENDPOINT=http://objects.internal:9000".to_string()));
    }

    #[test]
    fn walg_env_for_aws_s3_has_no_endpoint() {
        let s3 = S3Credentials {
            endpoint: None,
            ..http_ip_source()
        };
        let env = archive_walg_env_lines(&s3, "s3://a/b", None);
        assert!(!env.iter().any(|line| line.starts_with("AWS_ENDPOINT")));
        assert!(!env
            .iter()
            .any(|line| line.starts_with("AWS_S3_FORCE_PATH_STYLE")));
    }
}

mod general_tests {
    use super::super::*;
    #[cfg(feature = "docker-tests")]
    use crate::externalsvc::{ExternalService, ServiceConfig};
    use bollard::Docker;
    use std::sync::Arc;

    /// A private, run-owned PostgreSQL fixture exercises PostgreSQL's real
    /// archive_command show hook. The fixture supplies separate off/on servers;
    /// this test only reads their settings and never changes their workloads.
    #[tokio::test]
    async fn archive_settings_handles_postgres_disabled_command_display() {
        let Some(fixture_path) = std::env::var_os("TEMPS_TEST_ARCHIVE_SETTINGS_FIXTURE") else {
            eprintln!(
                "Skipping archive settings integration regression: no owned fixture supplied"
            );
            return;
        };
        #[derive(serde::Deserialize)]
        struct Fixture {
            off: PostgresConfig,
            on: PostgresConfig,
        }
        let fixture: Fixture = serde_json::from_slice(
            &std::fs::read(fixture_path).expect("read private owned PostgreSQL fixture"),
        )
        .expect("parse private owned PostgreSQL fixture");
        let docker = Arc::new(Docker::connect_with_local_defaults().expect("Docker configuration"));
        let service = PostgresService::new("archive-settings-regression".to_string(), docker);
        let off = service
            .active_archive_settings(&fixture.off, "/bin/true")
            .await
            .expect("off policy must not wait for a command hidden by PostgreSQL");
        assert_eq!(off, ("off".to_string(), "(disabled)".to_string()));
        assert!(fixture.off.container_name.is_some());
        let refusal = service
            .enable_wal_archiving(
                fixture.off.container_name.as_deref().unwrap(),
                &["WALG_S3_PREFIX=s3://test-bucket/unused".to_string()],
                &fixture.off,
                true,
            )
            .await
            .expect_err("an imported off-policy container must never be recreated");
        assert!(refusal
            .to_string()
            .contains("imported database has archive_mode off"));
        assert_eq!(
            service
                .active_archive_settings(&fixture.off, "/bin/true")
                .await
                .expect("the refused imported workload remains usable"),
            off
        );
        let on = service
            .active_archive_settings(&fixture.on, "/bin/true")
            .await
            .expect("on policy must expose the exact configured command");
        assert_eq!(on, ("on".to_string(), "/bin/true".to_string()));
        let error = service
            .active_archive_settings(&fixture.on, "wal-g wal-push %p")
            .await
            .expect_err("active archiving must reject a different command");
        assert!(error
            .reason
            .contains("expected archive command did not apply"));
    }

    #[test]
    fn imported_restore_rejects_effective_command_line_archive_policy() {
        assert!(imported_archive_mode_is_command_line("on", "command line"));
        assert!(imported_archive_mode_is_command_line(
            "always",
            "command line"
        ));
        assert!(!imported_archive_mode_is_command_line(
            "off",
            "command line"
        ));
        assert!(!imported_archive_mode_is_command_line(
            "on",
            "configuration file"
        ));
    }

    #[test]
    fn imported_restore_detects_startup_archive_mode_override() {
        let command = |args: &[&str]| {
            args.iter()
                .map(|arg| String::from(*arg))
                .collect::<Vec<_>>()
        };
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=on"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-carchive_mode=always"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "--archive_mode=on"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=off"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=on",
            "-c",
            "archive_mode=off"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_command=/bin/true"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "ARCHIVE_MODE=ON"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-cARCHIVE_MODE=ALWAYS"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "ARCHIVE_MODE=on",
            "-c",
            "archive_mode=off"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=off",
            "-c",
            "ARCHIVE_MODE=always"
        ])));

        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "--archive-mode=on"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "--ARCHIVE-MODE=always",
            "--archive_mode=off"
        ])));
    }

    fn backup_with_metadata(metadata: serde_json::Value) -> temps_entities::backups::Model {
        temps_entities::backups::Model {
            id: 1,
            name: "backup".into(),
            backup_id: "selected-backup".into(),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "external_service".into(),
            state: "completed".into(),
            started_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: "s3://bucket/repository".into(),
            error_message: None,
            metadata: metadata.to_string(),
            checksum: None,
            compression_type: "lz4".into(),
            created_by: 1,
            expires_at: None,
            tags: "[]".into(),
        }
    }

    #[test]
    fn walg_restore_selector_uses_persisted_backup_identity() {
        let backup = backup_with_metadata(serde_json::json!({
            "walg_identity_version": 1,
            "walg_target_user_data": {"temps_backup_id": "selected-backup"}
        }));
        assert_eq!(
            walg_target_user_data(&backup).expect("valid metadata should parse"),
            Some(r#"{"temps_backup_id":"selected-backup"}"#.to_string())
        );
    }

    #[test]
    fn walg_restore_selector_rejects_incomplete_identity_metadata() {
        let backup = backup_with_metadata(serde_json::json!({"walg_identity_version": 1}));
        let error = walg_target_user_data(&backup).expect_err("missing selector must fail closed");
        assert!(error
            .to_string()
            .contains("missing its WAL-G target user data"));
    }

    #[test]
    fn walg_restore_selector_rejects_mismatched_backup_identity() {
        let backup = backup_with_metadata(serde_json::json!({
            "walg_identity_version": 1,
            "walg_target_user_data": {"temps_backup_id": "different-backup"}
        }));
        let error = walg_target_user_data(&backup).expect_err("mismatched selector must fail");
        assert!(error.to_string().contains("for a different backup"));
    }

    #[test]
    fn sourced_walg_values_are_shell_quoted() {
        assert_eq!(
            shell_export_assignment("WALG_S3_PREFIX=s3://bucket/ok;touch${IFS}/tmp/pwn;#"),
            Some("export WALG_S3_PREFIX='s3://bucket/ok;touch${IFS}/tmp/pwn;#'".to_string())
        );
        assert_eq!(
            shell_export_assignment("AWS_SECRET_ACCESS_KEY=abc'def"),
            Some("export AWS_SECRET_ACCESS_KEY='abc'\\''def'".to_string())
        );
        assert!(shell_export_assignment("BAD-KEY=value").is_none());
    }

    /// Regression test: `compute_desired_enable_archiving()` must find a
    /// pre-existing `walg.env` on the data volume even when there is no
    /// live container to probe directly. Without the volume-probe fallback,
    /// `start()`'s `need_create` path (force_recreate, node failover, or an
    /// externally-removed container) would silently bake `archive_mode=off`
    /// into the recreated container even though WAL-G was already
    /// configured on the volume.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_walg_env_probed_from_volume_when_container_missing() {
        use std::net::TcpListener;
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("failed to bind for port allocation")
            .local_addr()
            .expect("failed to read local addr")
            .port();

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-walg-volume-probe".to_string(), docker.clone());

        let config = ServiceConfig {
            name: "test-walg-volume-probe".to_string(),
            service_type: crate::externalsvc::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port.to_string(),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:18-bookworm"
            }),
        };

        service.init(config).await.expect("init should succeed");
        let container_name = service.get_container_name();

        // No walg.env written yet -- archiving must read as not desired.
        assert!(
            !service.compute_desired_enable_archiving().await,
            "fresh service should not have archiving enabled"
        );

        service
            .write_walg_env_file(&container_name, &["AWS_ACCESS_KEY_ID=test".to_string()])
            .await
            .expect("failed to write walg.env");

        // Sanity check: the live container reports archiving desired.
        assert!(
            service.compute_desired_enable_archiving().await,
            "live container should report walg.env as present"
        );

        // Remove the container but keep the volume, simulating
        // force_recreate / node failover / an externally-removed container.
        docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .expect("failed to remove container");

        assert!(
            service.compute_desired_enable_archiving().await,
            "archiving must still read as desired from the volume once the container is gone"
        );

        // Cleanup: volume (container is already gone).
        let _ = docker
            .remove_volume(
                &format!("{container_name}_data"),
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;
    }
}

/// The `walg.env` write, run with a real `sh` against a scratch directory.
mod atomic_write_tests {
    use super::super::{
        atomic_write_command, parse_walg_env_snapshot, WalgEnvSnapshot, WALG_ENV_CHANGED_EXIT,
    };
    use std::path::PathBuf;
    use std::process::Command;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "temps-walg-write-{name}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("walg.env")
        }

        fn entries(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const OLD: &str = "export WALG_S3_PREFIX='s3://archive/pg/walg'\n\
                       export AWS_SECRET_ACCESS_KEY='test-secret'\n\
                       export AWS_ENDPOINT='10.0.0.5:9000'\n";

    fn run(prelude: &str, command: &str) -> std::process::ExitStatus {
        Command::new("sh")
            .arg("-c")
            .arg(format!("{prelude}{command}"))
            .status()
            .unwrap()
    }

    #[test]
    fn a_complete_write_replaces_the_file_with_owner_only_access() {
        let scratch = Scratch::new("ok");
        std::fs::write(scratch.file(), OLD).unwrap();
        // A leftover from a write killed mid-way is removed.
        std::fs::write(scratch.0.join("walg.env.tmp.1"), "partial").unwrap();
        let lines = vec![
            "export AWS_ENDPOINT='http://10.0.0.5:9000'".to_string(),
            "# it's kept".to_string(),
        ];
        let status = run(
            "",
            &atomic_write_command(&lines, scratch.file().to_str().unwrap(), None),
        );
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(scratch.file()).unwrap(),
            "export AWS_ENDPOINT='http://10.0.0.5:9000'\n# it's kept\n"
        );
        // Only the file and its lock remain: no temporary file is left.
        assert_eq!(
            scratch.entries(),
            vec!["walg.env".to_string(), "walg.env.lock".to_string()]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(scratch.file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// A write that fails part way (here a file-size limit, as a full disk
    /// would) leaves the old file exactly as it was, so archiving keeps the
    /// settings it had instead of a truncated credential file.
    #[test]
    fn a_failed_write_leaves_the_old_file_intact() {
        let scratch = Scratch::new("fail");
        std::fs::write(scratch.file(), OLD).unwrap();
        let lines: Vec<String> = (0..200)
            .map(|index| format!("export WALG_PADDING_{index}='{}'", "x".repeat(64)))
            .collect();
        // `ulimit -f 1` caps any file this shell writes at one block, far
        // below the new contents.
        let status = run(
            "ulimit -f 1; ",
            &atomic_write_command(&lines, scratch.file().to_str().unwrap(), None),
        );
        assert!(!status.success());
        assert_eq!(std::fs::read_to_string(scratch.file()).unwrap(), OLD);
    }

    fn checksum_of(path: &std::path::Path) -> String {
        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("cksum < '{}'", path.display()))
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// A repair built from the file it read is written; one built from a
    /// file that has since been rewritten (a repoint finishing in between)
    /// is refused and the newer file is kept.
    #[test]
    fn a_write_built_from_an_older_snapshot_is_refused() {
        let scratch = Scratch::new("cas");
        std::fs::write(scratch.file(), OLD).unwrap();
        let snapshot = checksum_of(&scratch.file());
        let path = scratch.file().to_str().unwrap().to_string();

        // Another writer replaces the file after the snapshot was taken.
        let newer = "export WALG_S3_PREFIX='s3://other/pg/walg'\n";
        std::fs::write(scratch.file(), newer).unwrap();
        let repair = vec!["export AWS_ENDPOINT='http://10.0.0.5:9000'".to_string()];
        let status = run("", &atomic_write_command(&repair, &path, Some(&snapshot)));
        assert_eq!(status.code(), Some(WALG_ENV_CHANGED_EXIT as i32));
        assert_eq!(std::fs::read_to_string(scratch.file()).unwrap(), newer);

        // Built from the current file, the same repair goes through.
        let current = checksum_of(&scratch.file());
        let status = run("", &atomic_write_command(&repair, &path, Some(&current)));
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(scratch.file()).unwrap(),
            "export AWS_ENDPOINT='http://10.0.0.5:9000'\n"
        );
    }

    /// Two writers never interleave: a write waits for the lock another
    /// holds. Needs `flock` (util-linux), as in the PostgreSQL images; skips
    /// where it is missing.
    #[test]
    fn a_write_waits_for_the_lock_another_writer_holds() {
        if !run("", "command -v flock >/dev/null 2>&1").success() {
            eprintln!("Skipping: flock is not installed here");
            return;
        }
        let scratch = Scratch::new("lock");
        std::fs::write(scratch.file(), OLD).unwrap();
        let path = scratch.file().to_str().unwrap().to_string();
        let ready = format!("{path}.ready");
        let release = format!("{path}.release");
        // The holder says when it owns the lock and keeps it until told to
        // let go: no timing guess either way. It gives up after 30 seconds,
        // so a failed assertion never leaves it running.
        let mut holder = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "exec 9>>'{path}.lock'; flock 9; : > '{ready}'; i=0; \
                 while [ ! -e '{release}' ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done"
            ))
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !std::path::Path::new(&ready).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the holder never took the lock"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let lines = vec!["export AWS_ENDPOINT='http://10.0.0.5:9000'".to_string()];
        let mut writer = Command::new("sh")
            .arg("-c")
            .arg(atomic_write_command(&lines, &path, None))
            .spawn()
            .unwrap();
        // However long the writer takes to start, it cannot finish (or touch
        // the file) while the lock is held.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            writer.try_wait().unwrap().is_none(),
            "the write finished while another writer held the lock"
        );
        assert_eq!(std::fs::read_to_string(scratch.file()).unwrap(), OLD);

        std::fs::write(&release, "").unwrap();
        assert!(holder.wait().unwrap().success());
        assert!(writer.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(scratch.file()).unwrap(),
            "export AWS_ENDPOINT='http://10.0.0.5:9000'\n"
        );
    }

    #[test]
    fn a_snapshot_keeps_checksum_text_and_assignments_together() {
        let output = "__TEMPS_WALG_ENV_CKSUM__\n\
                      1234 56\n\
                      __TEMPS_WALG_ENV_RAW__\n\
                      # note\n\
                      export AWS_ENDPOINT='10.0.0.5:9000'\n\
                      \n\
                      __TEMPS_WALG_ENV_EVAL__\n\
                      AWS_ENDPOINT=10.0.0.5:9000\n\
                      __TEMPS_WALG_ENV_END__\n";
        assert_eq!(
            parse_walg_env_snapshot(output),
            Ok(Some(WalgEnvSnapshot {
                checksum: "1234 56".to_string(),
                raw: "# note\nexport AWS_ENDPOINT='10.0.0.5:9000'\n".to_string(),
                evaluated: vec!["AWS_ENDPOINT=10.0.0.5:9000".to_string()],
            }))
        );
        assert_eq!(parse_walg_env_snapshot("__TEMPS_NO_WALG_ENV__\n"), Ok(None));
        assert!(parse_walg_env_snapshot("AWS_ENDPOINT=10.0.0.5:9000\n").is_err());
        // A file with no WALG_/AWS_ assignments is a complete, empty list.
        let empty = "__TEMPS_WALG_ENV_CKSUM__\n1 2\n__TEMPS_WALG_ENV_RAW__\n# note\n\n\
                     __TEMPS_WALG_ENV_EVAL__\n__TEMPS_WALG_ENV_END__\n";
        assert_eq!(
            parse_walg_env_snapshot(empty).map(|s| s.map(|s| s.evaluated)),
            Ok(Some(Vec::new()))
        );
    }

    /// Output that stops before the end marker (a stream cut short) is an
    /// incomplete snapshot, never an empty list of assignments that would
    /// make the repair skip a legacy endpoint.
    #[test]
    fn a_snapshot_cut_short_is_incomplete() {
        let head = "__TEMPS_WALG_ENV_CKSUM__\n1 2\n__TEMPS_WALG_ENV_RAW__\n\
                    export AWS_ENDPOINT='10.0.0.5:9000'\n\n__TEMPS_WALG_ENV_EVAL__\n";
        assert!(parse_walg_env_snapshot(head).is_err());
        assert!(parse_walg_env_snapshot(&format!("{head}AWS_ENDPOINT=10.0.0.5:9000\n")).is_err());
    }

    /// The snapshot script itself, against a real file: the reported
    /// checksum is the one the conditional write compares.
    #[test]
    fn the_snapshot_script_reports_the_checksum_a_write_compares() {
        let scratch = Scratch::new("snap");
        std::fs::write(scratch.file(), OLD).unwrap();
        let script = super::super::WALG_ENV_SNAPSHOT_SCRIPT.replace(
            "/var/lib/postgresql/walg.env",
            scratch.file().to_str().unwrap(),
        );
        let output = Command::new("sh").arg("-c").arg(script).output().unwrap();
        let snapshot = parse_walg_env_snapshot(&String::from_utf8(output.stdout).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.checksum, checksum_of(&scratch.file()));
        assert_eq!(
            snapshot.raw.lines().collect::<Vec<_>>(),
            OLD.lines().collect::<Vec<_>>()
        );
        assert!(snapshot
            .evaluated
            .contains(&"AWS_ENDPOINT=10.0.0.5:9000".to_string()));
        assert!(snapshot
            .evaluated
            .contains(&"AWS_SECRET_ACCESS_KEY=test-secret".to_string()));
    }

    /// The snapshot holds credentials: when the copy fails part way (here
    /// a `cp` that writes half the file and fails, as on a full `/tmp`),
    /// the script still removes it, and a successful run leaves nothing
    /// behind either.
    #[test]
    fn the_snapshot_copy_is_removed_on_every_path() {
        let scratch = Scratch::new("snapclean");
        std::fs::write(scratch.file(), OLD).unwrap();
        let tmp = scratch.0.join("tmp");
        let bin = scratch.0.join("bin");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let failing_cp = bin.join("cp");
        std::fs::write(
            &failing_cp,
            "#!/bin/sh\nprintf 'export AWS_SECRET_ACCESS_KEY=par' > \"$2\"\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&failing_cp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let script = super::super::WALG_ENV_SNAPSHOT_SCRIPT.replace(
            "/var/lib/postgresql/walg.env",
            scratch.file().to_str().unwrap(),
        );
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var_os("PATH")
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let leftovers = |dir: &std::path::Path| std::fs::read_dir(dir).unwrap().count();

        for _ in 0..3 {
            let status = Command::new("sh")
                .arg("-c")
                .arg(&script)
                .env("TMPDIR", &tmp)
                .env("PATH", &path)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(1), "a failed copy is an error");
        }
        assert_eq!(leftovers(&tmp), 0, "a failed copy left its snapshot behind");

        let output = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .env("TMPDIR", &tmp)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            parse_walg_env_snapshot(&String::from_utf8(output.stdout).unwrap())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            leftovers(&tmp),
            0,
            "a successful snapshot left its copy behind"
        );
    }
}
