//! Smoke test for the in-sandbox git credential pipeline (helper +
//! daemon + system git config).
//!
//! What this verifies:
//! 1. The sandbox image builds successfully with the new credential
//!    binaries baked in.
//! 2. Inside the resulting container:
//!    a. `~/.git-credentials` does NOT exist (the file has been retired
//!    in favor of the daemon).
//!    b. `printenv` does NOT show any `GITHUB_TOKEN` / `GH_TOKEN` /
//!    `GITLAB_TOKEN` / `GL_TOKEN` env vars.
//!    c. `/run/temps-git/` exists with mode 0750, owner
//!    `temps-git:git-users`.
//!    d. `/etc/temps/` exists with mode 0700, owner `temps-git:temps-git`.
//!    e. As user `temps`: cannot read the env file (`cat
//!    /etc/temps/credential-daemon.env` fails with EACCES even when
//!    the file exists).
//!    f. `git config --system --get credential.helper` resolves to the
//!    helper binary path.
//!    g. `git config --system --get credential.useHttpPath` returns
//!    `true`.
//!    h. The helper binary is executable and at the expected path.
//!    i. The daemon binary is executable and at the expected path.
//!
//! Skips gracefully when Docker isn't available (per project rule:
//! Docker tests must NOT use `#[ignore]`, they must runtime-skip).

use std::time::Duration;

use bollard::Docker;
use futures::StreamExt;
use temps_agents::sandbox::docker::dockerfile_for_runtime;

/// Generate the build-context tar bytes the sandbox image expects. Mirrors
/// the in-crate `build_context_tar` (private) by walking the public bundle
/// helpers. Kept as a test helper so we don't have to expose the internal
/// fn just for one test.
fn build_test_context() -> Vec<u8> {
    let dockerfile = dockerfile_for_runtime("node");

    let mut tar_buf = Vec::new();
    {
        let mut tar_builder = tar::Builder::new(&mut tar_buf);

        let dockerfile_bytes = dockerfile.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(dockerfile_bytes.len() as u64);
        header.set_path("Dockerfile").unwrap();
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder.append(&header, dockerfile_bytes).unwrap();

        temps_agents::sandbox::pty_agent_bundle::append_to_tar(&mut tar_builder).unwrap();
        temps_agents::sandbox::git_credential_bundle::append_to_tar(&mut tar_builder).unwrap();

        tar_builder.finish().unwrap();
    }
    tar_buf
}

async fn docker_or_skip() -> Option<Docker> {
    let docker = match Docker::connect_with_defaults() {
        Ok(d) => d,
        Err(_) => {
            eprintln!("Docker not available, skipping");
            return None;
        }
    };
    if docker.ping().await.is_err() {
        eprintln!("Docker daemon not responding, skipping");
        return None;
    }
    Some(docker)
}

async fn build_image(docker: &Docker, tag: &str) -> bool {
    let tar_buf = build_test_context();
    let body = http_body_util::Full::new(bytes::Bytes::from(tar_buf));
    let options = bollard::query_parameters::BuildImageOptionsBuilder::new()
        .t(tag)
        .build();

    let mut stream = docker.build_image(options, None, Some(http_body_util::Either::Left(body)));
    while let Some(result) = stream.next().await {
        match result {
            Ok(info) => {
                if let Some(ref err) = info.error_detail {
                    eprintln!(
                        "build error: {}",
                        err.message.as_deref().unwrap_or("unknown")
                    );
                    return false;
                }
                if let Some(ref line) = info.stream {
                    eprint!("{line}");
                }
            }
            Err(e) => {
                eprintln!("build failed: {e}");
                return false;
            }
        }
    }
    true
}

async fn exec_in_container(
    docker: &Docker,
    container_id: &str,
    cmd: Vec<&str>,
    user: Option<&str>,
) -> (String, String, i64) {
    let exec = docker
        .create_exec(
            container_id,
            bollard::models::ExecConfig {
                cmd: Some(cmd.into_iter().map(String::from).collect()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                user: user.map(String::from),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let output = docker
        .start_exec(
            &exec.id,
            Some(bollard::exec::StartExecOptions {
                detach: false,
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    let mut stdout = String::new();
    let mut stderr = String::new();

    if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
        while let Some(item) = output.next().await {
            match item {
                Ok(bollard::container::LogOutput::StdOut { message }) => {
                    stdout.push_str(&String::from_utf8_lossy(&message));
                }
                Ok(bollard::container::LogOutput::StdErr { message }) => {
                    stderr.push_str(&String::from_utf8_lossy(&message));
                }
                _ => {}
            }
        }
    }

    let info = docker.inspect_exec(&exec.id).await.unwrap();
    let exit_code = info.exit_code.unwrap_or(-1);
    (stdout, stderr, exit_code)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sandbox_image_credential_pipeline_smoke() {
    let docker = match docker_or_skip().await {
        Some(d) => d,
        None => return,
    };

    // Build the image. Use a unique tag so this test doesn't fight with
    // the production image cache.
    let tag = "temps-sandbox-credential-smoke:test";
    if !build_image(&docker, tag).await {
        panic!("image build failed — see stderr above");
    }

    // Run the container with `sleep infinity`. We assert against it via
    // separate exec calls.
    let container = docker
        .create_container(
            None::<bollard::query_parameters::CreateContainerOptions>,
            bollard::models::ContainerCreateBody {
                image: Some(tag.to_string()),
                cmd: Some(vec!["sleep".into(), "infinity".into()]),
                ..Default::default()
            },
        )
        .await
        .expect("create container");

    docker
        .start_container(
            &container.id,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .expect("start container");

    // Tiny grace period — the entrypoint backgrounds two supervisors
    // before exec-ing sleep, no need to wait long.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // RAII cleanup so the container is removed regardless of which
    // assertion fails. We don't bother with catch_unwind; tokio runs
    // each test in its own process anyway.
    struct ContainerGuard<'a> {
        docker: &'a Docker,
        id: &'a str,
    }
    impl<'a> Drop for ContainerGuard<'a> {
        fn drop(&mut self) {
            let docker = self.docker.clone();
            let id = self.id.to_string();
            // Best-effort cleanup. We're in Drop, so we spawn into the
            // current runtime and let it run async cleanup.
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(rt) = rt {
                    rt.block_on(async {
                        let _ = docker
                            .remove_container(
                                &id,
                                Some(bollard::query_parameters::RemoveContainerOptions {
                                    force: true,
                                    ..Default::default()
                                }),
                            )
                            .await;
                    });
                }
            })
            .join()
            .ok();
        }
    }
    let _guard = ContainerGuard {
        docker: &docker,
        id: &container.id,
    };

    // (a) ~/.git-credentials must not exist.
    {
        let (out, _, code) = exec_in_container(
            &docker,
            &container.id,
            vec!["sh", "-c", "test -e /home/temps/.git-credentials; echo $?"],
            Some("temps"),
        )
        .await;
        assert_eq!(code, 0, "exec status");
        assert_eq!(
            out.trim(),
            "1",
            "~/.git-credentials must not exist (got `test -e` exit code != 1)"
        );
    }

    // (b) GITHUB_TOKEN / GH_TOKEN / GITLAB_TOKEN / GL_TOKEN must not
    // be in the temps user's environment. Note: this env is whatever
    // the image baked in plus what the sandbox-entrypoint sets;
    // message_executor's per-session writes happen later, which is
    // out of scope for this smoke test.
    {
        let (out, _, _) = exec_in_container(
            &docker,
            &container.id,
            vec![
                "sh",
                "-c",
                "env | grep -E '^(GITHUB_TOKEN|GH_TOKEN|GITLAB_TOKEN|GL_TOKEN)=' || true",
            ],
            Some("temps"),
        )
        .await;
        assert!(
            out.trim().is_empty(),
            "no git tokens should be in env, got: {out:?}"
        );
    }

    // (c) /run/temps-git exists with mode 0750, owner temps-git:git-users.
    {
        let (out, _, _) = exec_in_container(
            &docker,
            &container.id,
            vec!["stat", "-c", "%a %U %G", "/run/temps-git"],
            None,
        )
        .await;
        assert!(
            out.trim().starts_with("750 temps-git git-users"),
            "/run/temps-git stat unexpected: {out:?}"
        );
    }

    // (d) /etc/temps exists with mode 0710, owner temps-git:git-users.
    // Mode 0710 (rwx for owner, --x for group, --- for other) is the
    // tight-but-functional setup: the daemon (temps-git) reads/writes
    // freely, group `git-users` (which `temps` is in) can traverse but
    // not list — this is what lets the entrypoint's `[ -e $file ]`
    // check work as `temps` while still preventing user code from
    // enumerating /etc/temps.
    {
        let (out, _, _) = exec_in_container(
            &docker,
            &container.id,
            vec!["stat", "-c", "%a %U %G", "/etc/temps"],
            None,
        )
        .await;
        assert!(
            out.trim().starts_with("710 temps-git git-users"),
            "/etc/temps stat unexpected: {out:?}"
        );
    }

    // (e) As temps user, cannot read a 0600 file in /etc/temps owned by
    // temps-git. The Dockerfile pre-creates the empty placeholder, so
    // we seed it directly via uid 1001 (the only uid that can write).
    // The 0600 mode + temps-git ownership keeps user `temps` out.
    {
        let (_, stderr_create, code_create) = exec_in_container(
            &docker,
            &container.id,
            vec![
                "sh",
                "-c",
                "printf 'TOPSECRET' > /etc/temps/credential-daemon.env \
                 && chmod 0600 /etc/temps/credential-daemon.env",
            ],
            Some("1001:1001"),
        )
        .await;
        assert_eq!(
            code_create, 0,
            "could not seed env file as uid 1001: {stderr_create}"
        );

        let (_, _, code_read) = exec_in_container(
            &docker,
            &container.id,
            vec!["cat", "/etc/temps/credential-daemon.env"],
            Some("temps"),
        )
        .await;
        assert_ne!(
            code_read, 0,
            "user `temps` was able to read the daemon env file — uid isolation broken"
        );
    }

    // (f) git credential.helper must be set.
    {
        let (out, _, code) = exec_in_container(
            &docker,
            &container.id,
            vec!["git", "config", "--system", "--get", "credential.helper"],
            None,
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(
            out.trim(),
            "/usr/local/bin/temps-git-credential-helper",
            "credential.helper not set"
        );
    }

    // (g) credential.useHttpPath must be true.
    {
        let (out, _, code) = exec_in_container(
            &docker,
            &container.id,
            vec![
                "git",
                "config",
                "--system",
                "--get",
                "credential.useHttpPath",
            ],
            None,
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(out.trim(), "true", "credential.useHttpPath not true");
    }

    // (h) helper binary exists and is executable.
    {
        let (out, _, _) = exec_in_container(
            &docker,
            &container.id,
            vec![
                "stat",
                "-c",
                "%a",
                "/usr/local/bin/temps-git-credential-helper",
            ],
            None,
        )
        .await;
        assert!(
            out.trim().starts_with("755") || out.trim().starts_with("0755"),
            "helper binary mode unexpected: {out:?}"
        );
    }

    // (i) daemon binary exists and is executable.
    {
        let (out, _, _) = exec_in_container(
            &docker,
            &container.id,
            vec![
                "stat",
                "-c",
                "%a",
                "/usr/local/bin/temps-git-credential-daemon",
            ],
            None,
        )
        .await;
        assert!(
            out.trim().starts_with("755") || out.trim().starts_with("0755"),
            "daemon binary mode unexpected: {out:?}"
        );
    }

    // (j) Pre-baked credential-daemon.env exists with the right perms.
    // The image's Dockerfile creates an empty placeholder so the
    // message_executor can overwrite it as `temps-git` later without
    // needing CAP_DAC_OVERRIDE (which the sandbox drops).
    {
        let (out, _, _) = exec_in_container(
            &docker,
            &container.id,
            vec!["stat", "-c", "%a %U %G", "/etc/temps/credential-daemon.env"],
            Some("1001:1001"), // temps-git can stat its own file
        )
        .await;
        assert!(
            out.trim().starts_with("600 temps-git temps-git"),
            "/etc/temps/credential-daemon.env stat unexpected: {out:?}"
        );
    }

    // (k) message_executor's actual code path: write the env file as
    // uid 1001 then launch the daemon (also as 1001) via the same
    // `setsid` shell trampoline message_executor uses. Verifies the
    // daemon binds the socket and the process is running as
    // `temps-git`. The control-plane URL is intentionally unreachable
    // (port 1) so any IPC mint requests would fail — but the daemon
    // still creates the socket on startup before accepting connections.
    {
        let (_, stderr_w, code_w) = exec_in_container(
            &docker,
            &container.id,
            vec![
                "sh",
                "-c",
                "umask 077 && cat > /etc/temps/credential-daemon.env <<'EOF'
TEMPS_API_URL='http://127.0.0.1:1'
TEMPS_API_TOKEN='dt_smoke_unreachable'
EOF
chmod 0600 /etc/temps/credential-daemon.env",
            ],
            Some("1001:1001"),
        )
        .await;
        assert_eq!(code_w, 0, "writing as uid 1001 should succeed: {stderr_w}");

        // Mirror what message_executor::write_credential_daemon_env
        // does after writing the env file: launch the daemon detached.
        let (_, stderr_l, code_l) = exec_in_container(
            &docker,
            &container.id,
            vec![
                "sh",
                "-c",
                "setsid /usr/local/bin/temps-git-credential-daemon \
                    >> /tmp/temps-git-credential-daemon.log \
                    2>&1 < /dev/null &
                 disown 2>/dev/null || true",
            ],
            Some("1001:1001"),
        )
        .await;
        assert_eq!(
            code_l, 0,
            "launching daemon as uid 1001 should succeed: {stderr_l}"
        );

        // Give the daemon a moment to bind the socket.
        tokio::time::sleep(Duration::from_secs(2)).await;

        let (out, _, code) = exec_in_container(
            &docker,
            &container.id,
            vec!["test", "-S", "/run/temps-git/git.sock"],
            None,
        )
        .await;
        assert_eq!(
            code, 0,
            "credential daemon socket missing after launch. Out: {out:?}"
        );

        let (out, _, _) = exec_in_container(
            &docker,
            &container.id,
            vec!["sh", "-c", "ps -eo user,comm | grep temps-git-cre || true"],
            None,
        )
        .await;
        assert!(
            out.contains("temps-git"),
            "credential daemon process not running, ps output: {out:?}"
        );
    }
}
