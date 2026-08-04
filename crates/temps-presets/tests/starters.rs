//! Builds and runs every temps-examples starter through the real preset.
//!
//! The unit tests in `temps-presets` assert on the *text* of a generated
//! Dockerfile, which catches typos and nothing else. A Dockerfile that renders
//! perfectly can still fail to build, build into an image that will not start,
//! or start into a process that never answers a request — and each of those
//! reaches the user as a failed deploy.
//!
//! So this goes all the way: render through [`AutopackPreset`] exactly as the
//! deployment pipeline does, `docker build` it, run it, and ask it for a page.
//! It also asserts the two properties that are invisible until they bite:
//!
//! * the container answers on `$PORT`, because that is what the proxy connects
//!   to and nothing else checks it;
//! * it stops on `SIGTERM` rather than waiting out the kill timeout, because a
//!   container that ignores `SIGTERM` turns every redeploy into a hard kill of
//!   in-flight requests. (Both nixpacks and railpack fail this one.)
//!
//! ## Running
//!
//! Needs Docker with BuildKit and a checkout of the starters:
//!
//! ```sh
//! git clone --depth 1 https://github.com/gotempsh/temps-examples /tmp/temps-examples
//! TEMPS_EXAMPLES_DIR=/tmp/temps-examples/examples/starters \
//!   cargo test -p temps-presets --test starters -- --ignored --test-threads 4
//! ```
//!
//! Narrow it down with `TEMPS_STARTERS_ONLY=go/gin,python/flask`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use temps_presets::{AutopackPreset, DockerfileConfig, Preset};

/// How long a starter gets to build. Swift and Rust are genuinely slow.
const BUILD_TIMEOUT: Duration = Duration::from_secs(900);
/// How long a container gets to answer its first request.
const BOOT_TIMEOUT: Duration = Duration::from_secs(60);
/// A container that handles SIGTERM stops well inside this; one that ignores it
/// waits out `docker stop`'s full grace period and gets SIGKILLed.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Starters that this matrix deliberately does not build.
///
/// `dockerfile` exists to exercise the *Dockerfile* preset — it ships its own
/// Dockerfile and expects to be built by it. Autopack ignores that file by
/// design, so building it here would test nothing and fail confusingly.
const EXCLUDED: &[&str] = &["dockerfile"];

struct Starter {
    /// Path relative to the starters root, e.g. `go/gin`.
    name: String,
    path: PathBuf,
}

fn starters_root() -> Option<PathBuf> {
    let dir = std::env::var("TEMPS_EXAMPLES_DIR").ok()?;
    let path = PathBuf::from(dir);
    path.is_dir().then_some(path)
}

/// Every starter under `root`, at most two levels deep.
///
/// A starter is a directory that directly contains at least one file. Keying
/// off a manifest list instead looks tidier but silently skips anything whose
/// manifest is not on the list — the Deno starter is a lone `main.ts`, and a
/// missed starter is indistinguishable from a passing one.
fn discover(root: &Path) -> Vec<Starter> {
    fn holds_a_file(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().any(|e| e.path().is_file()))
            .unwrap_or(false)
    }

    fn subdirectories(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs
    }

    let mut found = Vec::new();
    for dir in subdirectories(root) {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        // A language directory either *is* a starter (`astro`, `deno`) or holds
        // several (`go/gin`, `go/net-http`) — never both. Stopping at the parent
        // is what keeps `sveltekit/src` from being treated as an application.
        if holds_a_file(&dir) {
            found.push(Starter { name, path: dir });
            continue;
        }
        for child in subdirectories(&dir) {
            if !holds_a_file(&child) {
                continue;
            }
            let child_name = child.file_name().unwrap().to_string_lossy().to_string();
            found.push(Starter {
                name: format!("{name}/{child_name}"),
                path: child,
            });
        }
    }
    found.retain(|s| !EXCLUDED.contains(&s.name.as_str()));
    found
}

/// Restrict the run to `TEMPS_STARTERS_ONLY`, if set.
fn selected(starters: Vec<Starter>) -> Vec<Starter> {
    let Ok(only) = std::env::var("TEMPS_STARTERS_ONLY") else {
        return starters;
    };
    let wanted: Vec<&str> = only
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    starters
        .into_iter()
        .filter(|s| wanted.iter().any(|w| *w == s.name))
        .collect()
}

fn run(command: &mut Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|e| format!("could not run {command:?}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "{command:?} failed ({})\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// A port the operating system just confirmed is free.
///
/// Racy in principle — something else could take it between the probe and
/// `docker run` — but binding a fixed port makes concurrent starters collide
/// every time rather than occasionally.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("a local address")
        .port()
}

/// Render the starter's Dockerfile through the same preset the platform uses.
fn dockerfile_for(starter: &Starter) -> String {
    let mut config = DockerfileConfig::new(&starter.path, &starter.path, "starter");
    // The deployment pipeline always builds with BuildKit; so must this.
    config.use_buildkit = true;
    futures_lite_block_on(AutopackPreset::new().dockerfile(config)).content
}

/// Minimal block-on so the test does not need a full async runtime.
fn futures_lite_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
        .block_on(future)
}

struct Container {
    id: String,
}

impl Drop for Container {
    fn drop(&mut self) {
        // Best-effort: a leaked container would hold its port for the next run.
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Poll `url` until it answers, or give up.
fn wait_for_http(url: &str, timeout: Duration, container: &str) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        let output = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                "5",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                url,
            ])
            .output();
        match output {
            Ok(out) => {
                let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
                // Any HTTP answer means the server is up and listening on $PORT.
                // A starter is free to redirect or 404 its root path; what is
                // being tested is that something is there at all.
                if code.starts_with('2') || code.starts_with('3') || code.starts_with('4') {
                    return Ok(());
                }
                last = code;
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let logs = Command::new("docker")
        .args(["logs", "--tail", "40", container])
        .output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default();
    Err(format!(
        "no HTTP response from {url} within {timeout:?} (last: {last})\n--- container logs ---\n{logs}"
    ))
}

/// Build, run, request, and stop one starter.
fn verify(starter: &Starter) -> Result<(), String> {
    let dockerfile = dockerfile_for(starter);
    if dockerfile.contains("autopack could not plan") {
        return Err(format!(
            "the preset produced no build plan for `{}`:\n{dockerfile}",
            starter.name
        ));
    }

    let tag = format!(
        "temps-starter/{}:test",
        starter.name.replace('/', "-").to_lowercase()
    );

    // Feed the Dockerfile on stdin so the starter's own tree is never touched —
    // a stray Dockerfile left in the checkout would change what the next run
    // detects.
    let mut build = Command::new("docker");
    build
        .args(["build", "--progress", "plain", "-t", &tag, "-f", "-", "."])
        .current_dir(&starter.path)
        .env("DOCKER_BUILDKIT", "1")
        .stdin(Stdio::piped())
        // Both must be captured: BuildKit writes its log to stderr, and an
        // uncaptured stream is inherited, so the failure message arrives empty
        // and the actual error is somewhere up the console.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = build
        .spawn()
        .map_err(|e| format!("could not start docker build: {e}"))?;
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(dockerfile.as_bytes())
            .map_err(|e| format!("could not write the Dockerfile: {e}"))?;
    }
    let started = Instant::now();
    let output = child
        .wait_with_output()
        .map_err(|e| format!("docker build failed to complete: {e}"))?;
    if !output.status.success() {
        // Only the tail is useful: a BuildKit plain log is thousands of lines
        // of layer chatter and the error is always at the end.
        let log = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = log.lines().rev().take(40).collect();
        return Err(format!(
            "docker build failed for `{}`\n--- build log (last 40 lines) ---\n{}\n--- Dockerfile ---\n{dockerfile}",
            starter.name,
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    if started.elapsed() > BUILD_TIMEOUT {
        return Err(format!(
            "`{}` took longer than {BUILD_TIMEOUT:?} to build",
            starter.name
        ));
    }

    let port = free_port();
    // Deliberately not `--rm`: a container that exits on startup would be
    // removed before `docker logs` could say why, which is exactly the case
    // where the logs matter most. The Drop guard cleans up instead.
    let id = run(Command::new("docker").args([
        "run",
        "--detach",
        "--env",
        &format!("PORT={port}"),
        "--publish",
        &format!("127.0.0.1:{port}:{port}"),
        &tag,
    ]))?
    .trim()
    .to_string();
    let container = Container { id: id.clone() };

    wait_for_http(&format!("http://127.0.0.1:{port}/"), BOOT_TIMEOUT, &id)?;

    // The proxy talks to an unprivileged process or the image is a liability.
    let uid = run(Command::new("docker").args(["exec", &id, "id", "-u"]))?
        .trim()
        .to_string();
    if uid == "0" {
        return Err(format!("`{}` runs as root", starter.name));
    }

    // `docker stop` sends SIGTERM, waits, then SIGKILLs. Finishing early means
    // the signal was actually handled.
    let stop_started = Instant::now();
    run(Command::new("docker").args([
        "stop",
        "--timeout",
        &STOP_TIMEOUT.as_secs().to_string(),
        &id,
    ]))?;
    let stop_took = stop_started.elapsed();
    if stop_took >= STOP_TIMEOUT {
        return Err(format!(
            "`{}` ignored SIGTERM — `docker stop` waited the full {STOP_TIMEOUT:?} and had to SIGKILL it. \
             Every redeploy would hard-kill in-flight requests.",
            starter.name
        ));
    }

    drop(container);
    let _ = Command::new("docker")
        .args(["rmi", "-f", &tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

#[test]
#[ignore = "needs Docker and TEMPS_EXAMPLES_DIR; run explicitly with --ignored"]
fn every_starter_builds_runs_and_serves() {
    let Some(root) = starters_root() else {
        // Name what is actually missing. "not set to a directory" covers both
        // "you forgot the variable" and "the directory is not there", and those
        // want completely different fixes — the second one usually means the
        // checkout is on a branch that does not carry `examples/starters` yet.
        let requested = std::env::var("TEMPS_EXAMPLES_DIR");
        panic!(
            "{}\n\nExpected a checkout of the starters:\n  \
             git clone --depth 1 https://github.com/gotempsh/temps-examples /tmp/temps-examples\n  \
             export TEMPS_EXAMPLES_DIR=/tmp/temps-examples/examples/starters",
            match requested {
                Err(_) => "TEMPS_EXAMPLES_DIR is not set.".to_string(),
                Ok(dir) => format!(
                    "TEMPS_EXAMPLES_DIR points at `{dir}`, which is not a directory. \
                     If this is a CI checkout, the branch may not carry `examples/starters`."
                ),
            }
        );
    };

    let starters = selected(discover(&root));
    assert!(
        !starters.is_empty(),
        "no starters found under {root:?} (TEMPS_STARTERS_ONLY may not match anything)"
    );

    let mut failures = Vec::new();
    for starter in &starters {
        eprintln!("--- {} ---", starter.name);
        let started = Instant::now();
        match verify(starter) {
            Ok(()) => eprintln!("    ok in {:?}", started.elapsed()),
            Err(error) => {
                eprintln!("    FAILED: {error}");
                failures.push(format!("{}: {error}", starter.name));
            }
        }
    }

    // Report every failure rather than stopping at the first: when a change to
    // a shared code path breaks six languages, one error message sends the
    // reader chasing one language.
    assert!(
        failures.is_empty(),
        "{} of {} starters failed:\n\n{}",
        failures.len(),
        starters.len(),
        failures.join("\n\n")
    );
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        dir
    }

    #[test]
    fn finds_starters_at_both_levels() {
        let dir = tree(&["astro/package.json", "go/gin/go.mod", "go/net-http/go.mod"]);
        let mut names: Vec<_> = discover(dir.path()).into_iter().map(|s| s.name).collect();
        names.sort();
        assert_eq!(names, ["astro", "go/gin", "go/net-http"]);
    }

    #[test]
    fn a_source_directory_is_not_mistaken_for_a_starter() {
        // `sveltekit/src` has no manifest, and `sveltekit` itself does — so the
        // walk must stop at the parent instead of descending into it.
        let dir = tree(&["sveltekit/package.json", "sveltekit/src/app.html"]);
        let names: Vec<_> = discover(dir.path()).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["sveltekit"]);
    }

    #[test]
    fn only_filters_to_the_named_starters() {
        let starters = vec![
            Starter {
                name: "go/gin".into(),
                path: PathBuf::new(),
            },
            Starter {
                name: "python/flask".into(),
                path: PathBuf::new(),
            },
        ];
        // SAFETY: single-threaded within this test binary's discovery tests.
        unsafe { std::env::set_var("TEMPS_STARTERS_ONLY", "python/flask") };
        let names: Vec<_> = selected(starters).into_iter().map(|s| s.name).collect();
        unsafe { std::env::remove_var("TEMPS_STARTERS_ONLY") };
        assert_eq!(names, ["python/flask"]);
    }
}
