// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared helpers for the RustFS client (`rc`) helper containers used by the
//! S3/MinIO and RustFS backup and restore paths.
//!
//! These paths used to run MinIO's `mc`. MinIO withdrew every distribution of
//! it (`quay.io/minio/mc` and Docker Hub `minio/mc` no longer pull, even by
//! digest), so every backup and restore failed at the image pull. `rc` works
//! against any S3-compatible endpoint (AWS S3, R2, MinIO, RustFS) and keeps
//! mc's command shapes, with these differences the callers account for:
//!
//! - Credentials come from `RC_HOST_<alias>=<scheme>://<key>:<secret>@<host>`
//!   (`MC_HOST_*` is ignored). The userinfo is percent-decoded, so the same
//!   [`super::mc_host_credential`] encoding applies (always with no session
//!   token, see [`rc_host_env`]). Every alias here is defined this way; none
//!   is created with `rc alias set`, which would put keys in argv and, for a
//!   temporary credential, fail its own probe (it cannot send a token).
//! - Session tokens (temporary S3 credentials): unlike mc, rc has NO
//!   per-alias session token. A third `:token` userinfo field is read as part
//!   of the secret (verified against a header-capturing sink: no
//!   `x-amz-security-token` is sent, so real STS credentials get
//!   SignatureDoesNotMatch), and its config file has no token field. The only
//!   mechanism is the global `-H "x-amz-security-token: <token>"` flag, which
//!   is signed correctly but applies to every request of that invocation —
//!   including the local RustFS/MinIO side of a mirror, which rejects an
//!   unknown token (500 InternalError). So when the remote side carries a
//!   token, a mirror is staged through a scratch directory inside the helper
//!   container: one `rc` call talks only to the local side without `-H`, the
//!   other talks only to the remote side with `-H` ([`mirror_command`]).
//!   The token reaches the container only as the
//!   [`REMOTE_SESSION_TOKEN_ENV`] environment variable and is expanded by
//!   `sh` inside it, so it stays out of the exec/container command line, API
//!   output and logs. It IS in the running `rc` process's argv (`-H` takes
//!   it as an argument) for the life of that call, i.e. visible to host
//!   users via `/proc/<pid>/cmdline` / `docker top` — rc offers no env- or
//!   file-based way to set the header.
//! - `rc mirror` cannot take an alias root (`alias/`) as its source. It
//!   mirrors one bucket or prefix at a time, so "mirror every bucket" means
//!   listing the buckets and mirroring each one ([`mirror_all_buckets`]).
//! - `rc ls --json` prints ONE pretty-printed JSON document
//!   (`{"items":[...],"truncated":false}`), not mc's one-object-per-line
//!   output. Each item's `key` is the full key relative to the bucket
//!   (`backups/2026/bucket-a/`), where mc printed only the last segment
//!   (`bucket-a/`). See [`parse_ls_dir_names`].
//! - Exit codes: 0 ok, 2 usage, 3 network or not found while listing, 4 auth,
//!   5 not found. Unlike mc, a failed listing never exits 0.

use bollard::Docker;
use futures::TryStreamExt;
use tracing::info;

use super::SensitiveValues;

/// RustFS client (`rc`) image for every backup/restore helper container.
///
/// Pinned by tag AND digest: the tag documents the version, the digest makes
/// the pull immutable (Docker verifies it and ignores the tag). The image is
/// multi-arch, its entrypoint is `rc`, and it ships `/bin/sh` plus busybox
/// `awk`/`grep`/`sed`/`mktemp`, which the `sh -c` restore script relies on.
pub const RC_IMAGE: &str =
    "rustfs/rc:v0.1.36@sha256:ab024bfebee49a750ce886b4c70963ccd9ddaa03f491704a90710641d7a26699";

/// Container environment variable carrying the remote (backup) side's STS
/// session token. Referenced by the scripts below as a shell expansion, so the
/// value never appears in an exec command line (it does end up in the `rc`
/// process argv while that call runs — see the module docs).
pub const REMOTE_SESSION_TOKEN_ENV: &str = "TEMPS_REMOTE_SESSION_TOKEN";

/// `TEMPS_REMOTE_SESSION_TOKEN=<token>` for the helper container, or `None`
/// for a long-lived credential (the variable is then absent, and every
/// command takes the direct, unstaged form).
pub fn remote_session_token_env(session_token: Option<&str>) -> Option<String> {
    session_token
        .filter(|token| !token.is_empty())
        .map(|token| format!("{REMOTE_SESSION_TOKEN_ENV}={token}"))
}

/// `RC_HOST_<alias>=<scheme>://<key>:<secret>@<host>` for one alias.
///
/// Never carries a session token: rc would read a third userinfo field as
/// part of the secret key. `endpoint` may be `http://…`, `https://…` or a bare
/// `host:port` (plain HTTP, the historical assumption for internal servers).
pub fn rc_host_env(alias: &str, endpoint: &str, access_key: &str, secret_key: &str) -> String {
    let (scheme, host) = if let Some(rest) = endpoint.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        ("http", rest)
    } else {
        ("http", endpoint)
    };
    format!(
        "RC_HOST_{alias}={scheme}://{}@{}",
        super::mc_host_credential(access_key, secret_key, None),
        host.trim_end_matches('/')
    )
}

/// Which side of a mirror is the remote one that needs the session token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSide {
    /// Long-lived credentials on both sides: one direct `rc mirror`.
    Neither,
    /// The source needs `-H x-amz-security-token` (restores from a backup).
    Source,
    /// The destination needs it (backups to a backup location).
    Destination,
}

impl TokenSide {
    /// `Source`/`Destination` when a token is present, otherwise `Neither`.
    pub fn when(has_token: bool, side: TokenSide) -> TokenSide {
        if has_token {
            side
        } else {
            TokenSide::Neither
        }
    }
}

/// Mirror staged through a scratch directory so only the token side's `rc`
/// call carries `-H`. Positional parameters (never interpolated into the
/// script text): `$1` source, `$2` destination, `$3` `remove`|`keep`,
/// `$4` which side carries the token (`source`|`destination`).
const STAGED_MIRROR_SCRIPT: &str = r#"set -e
if [ -z "${TEMPS_REMOTE_SESSION_TOKEN:-}" ]; then
  echo 'staged mirror: TEMPS_REMOTE_SESSION_TOKEN is not set' >&2
  exit 2
fi
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
REMOVE=
if [ "$3" = remove ]; then REMOVE=--remove; fi
if [ "$4" = source ]; then
  rc -H "x-amz-security-token: ${TEMPS_REMOTE_SESSION_TOKEN}" mirror --overwrite "$1" "$stage/"
  rc mirror --overwrite $REMOVE "$stage/" "$2"
else
  rc mirror --overwrite "$1" "$stage/"
  rc -H "x-amz-security-token: ${TEMPS_REMOTE_SESSION_TOKEN}" mirror --overwrite $REMOVE "$stage/" "$2"
fi"#;

/// `rc ls --json "$1"` on the token-carrying side.
const TOKEN_LS_SCRIPT: &str =
    r#"exec rc -H "x-amz-security-token: ${TEMPS_REMOTE_SESSION_TOKEN}" ls --json "$1""#;

/// Command (exec argv, or container cmd with the `rc` image's entrypoint
/// overridden to nothing) mirroring `source` to `destination`.
///
/// With [`TokenSide::Neither`] this is the direct
/// `rc mirror --overwrite [--remove] <src> <dst>`. Otherwise it is
/// `sh -c <STAGED_MIRROR_SCRIPT> sh <src> <dst> <remove|keep> <side>`: the
/// paths travel as positional parameters (no shell interpolation) and the
/// token is read from the container environment, so neither appears in the
/// script text.
pub fn mirror_command(
    source: &str,
    destination: &str,
    remove_extraneous: bool,
    token_side: TokenSide,
) -> Vec<String> {
    let side = match token_side {
        TokenSide::Neither => {
            let mut command = vec![
                "rc".to_string(),
                "mirror".to_string(),
                "--overwrite".to_string(),
            ];
            if remove_extraneous {
                command.push("--remove".to_string());
            }
            command.push(source.to_string());
            command.push(destination.to_string());
            return command;
        }
        TokenSide::Source => "source",
        TokenSide::Destination => "destination",
    };
    vec![
        "sh".to_string(),
        "-c".to_string(),
        STAGED_MIRROR_SCRIPT.to_string(),
        "sh".to_string(),
        source.to_string(),
        destination.to_string(),
        if remove_extraneous { "remove" } else { "keep" }.to_string(),
        side.to_string(),
    ]
}

/// `rc ls --json <target>`, with `-H x-amz-security-token` (read from the
/// container environment) when `target` is on the token side.
pub fn ls_json_command(target: &str, token_side: bool) -> Vec<String> {
    if token_side {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            TOKEN_LS_SCRIPT.to_string(),
            "sh".to_string(),
            target.to_string(),
        ]
    } else {
        vec![
            "rc".to_string(),
            "ls".to_string(),
            "--json".to_string(),
            target.to_string(),
        ]
    }
}

/// Failures of the `rc` helper operations. Every captured client output in a
/// variant has already been passed through [`SensitiveValues`] by the code
/// that built it, so these messages are safe to log, persist and return.
#[derive(Debug, thiserror::Error)]
pub enum RcClientError {
    #[error("rc could not list '{target}' (exit code {exit_code}): {stderr}")]
    ListFailed {
        target: String,
        exit_code: i64,
        stderr: String,
    },

    #[error("could not read the `rc ls --json` output for '{target}' ({reason}): {output}")]
    UnparsableListing {
        target: String,
        reason: String,
        output: String,
    },

    #[error("`rc ls` of '{target}' reported an error: {message}")]
    ListingReportedError { target: String, message: String },

    #[error(
        "`rc ls` of '{target}' returned a truncated listing ({returned} entries); \
         refusing to back up or restore a partial bucket set as if it were complete"
    )]
    TruncatedListing { target: String, returned: usize },

    #[error(
        "rc mirror of bucket '{bucket}' from '{source_path}' to '{destination}' failed \
         (exit code {exit_code}): {output}"
    )]
    MirrorFailed {
        bucket: String,
        source_path: String,
        destination: String,
        exit_code: i64,
        output: String,
    },

    #[error("Docker exec {operation} in rc helper container '{container_id}' failed: {error}")]
    Docker {
        container_id: String,
        operation: &'static str,
        #[source]
        error: bollard::errors::Error,
    },
}

/// Folder names from `rc ls --json <alias>/<path>` output, in listing order.
///
/// Only directory entries (`"is_dir": true`) count. The name is the last path
/// segment of the entry's key, so the same parser covers both listings the
/// callers make:
/// - an alias root (`rc ls --json src/`), whose keys are bucket names
///   (`"bucket-a"`);
/// - a backup prefix (`rc ls --json bkp/backups/2026/`), whose keys are full
///   bucket-relative keys (`"backups/2026/bucket-a/"`).
///
/// Errors on anything that is not a complete `rc` listing document, instead
/// of silently reporting "nothing to restore" for output it cannot read. `rc
/// ls` pages through the S3 listing itself (verified: 1,100 prefixes under
/// one folder come back in one document with `"truncated": false`), so
/// `"truncated": true` is never expected; if it appears, the listing is
/// incomplete and this is a hard [`RcClientError::TruncatedListing`] — a
/// backup or restore over a partial bucket set must not report success. A
/// document without a `truncated` field is treated the same way, since its
/// completeness cannot be proven.
///
/// `target` names the listed path in errors; `sensitive_values` scrubs any
/// credential out of the output echoed into them.
pub fn parse_ls_dir_names(
    target: &str,
    stdout: &str,
    sensitive_values: &SensitiveValues<'_>,
) -> Result<Vec<String>, RcClientError> {
    let unparsable = |reason: String| RcClientError::UnparsableListing {
        target: target.to_string(),
        reason,
        output: sensitive_values.redact(&truncate_for_error(stdout)),
    };

    let document: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| unparsable(format!("not a JSON document: {e}")))?;

    if let Some(error) = document.get("error") {
        return Err(RcClientError::ListingReportedError {
            target: target.to_string(),
            message: sensitive_values.redact(&error.to_string()),
        });
    }

    let items = document
        .get("items")
        .and_then(|items| items.as_array())
        .ok_or_else(|| unparsable("no `items` array".to_string()))?;

    match document.get("truncated").and_then(|t| t.as_bool()) {
        Some(false) => {}
        Some(true) => {
            return Err(RcClientError::TruncatedListing {
                target: target.to_string(),
                returned: items.len(),
            })
        }
        None => {
            return Err(unparsable(
                "no boolean `truncated` field, so completeness cannot be verified".to_string(),
            ))
        }
    }

    Ok(items
        .iter()
        .filter(|item| item.get("is_dir").and_then(|d| d.as_bool()) == Some(true))
        .filter_map(|item| item.get("key").and_then(|k| k.as_str()))
        .filter_map(|key| {
            key.trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty() && *name != "." && *name != "..")
                .map(str::to_string)
        })
        .collect())
}

fn truncate_for_error(output: &str) -> String {
    const MAX: usize = 512;
    let trimmed = output.trim();
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let mut end = MAX;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

/// Output of one `docker exec` with stdout and stderr kept apart (`rc ls
/// --json` writes the document to stdout and diagnostics to stderr).
#[derive(Debug, Clone)]
pub(crate) struct ExecOutput {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Run `cmd` in `container_id` and capture its exit code, stdout and stderr.
pub(crate) async fn exec_capture(
    docker: &Docker,
    container_id: &str,
    cmd: &[&str],
) -> Result<ExecOutput, RcClientError> {
    let docker_error = |operation: &'static str| {
        move |error: bollard::errors::Error| RcClientError::Docker {
            container_id: container_id.to_string(),
            operation,
            error,
        }
    };
    let exec = docker
        .create_exec(
            container_id,
            bollard::exec::CreateExecOptions {
                cmd: Some(cmd.to_vec()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(docker_error("create"))?;

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let bollard::exec::StartExecResults::Attached { mut output, .. } = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(docker_error("start"))?
    {
        while let Some(chunk) = output.try_next().await.map_err(docker_error("stream"))? {
            match chunk {
                bollard::container::LogOutput::StdOut { message } => {
                    stdout.push_str(&String::from_utf8_lossy(&message));
                }
                bollard::container::LogOutput::StdErr { message } => {
                    stderr.push_str(&String::from_utf8_lossy(&message));
                }
                _ => {}
            }
        }
    }

    let exit_code = docker
        .inspect_exec(&exec.id)
        .await
        .map_err(docker_error("inspect"))?
        .exit_code
        .unwrap_or(-1);
    Ok(ExecOutput {
        exit_code,
        stdout,
        stderr,
    })
}

/// `rc ls --json <target>` inside a running helper container, parsed into
/// folder names (see [`parse_ls_dir_names`]). `token_side` sends the remote
/// session token (see [`ls_json_command`]).
pub(crate) async fn list_dir_names(
    docker: &Docker,
    container_id: &str,
    target: &str,
    token_side: bool,
    sensitive_values: &SensitiveValues<'_>,
) -> Result<Vec<String>, RcClientError> {
    let command = ls_json_command(target, token_side);
    let command: Vec<&str> = command.iter().map(String::as_str).collect();
    let listing = exec_capture(docker, container_id, &command).await?;
    if listing.exit_code != 0 {
        return Err(RcClientError::ListFailed {
            target: target.to_string(),
            exit_code: listing.exit_code,
            stderr: sensitive_values.redact(listing.stderr.trim()),
        });
    }
    parse_ls_dir_names(target, &listing.stdout, sensitive_values)
}

/// `rc mirror --overwrite <source_alias>/<bucket>/ <dest_prefix>/<bucket>/`
/// for every bucket behind `source_alias` (the local, token-less side),
/// inside a running `rc` helper container. `dest_has_token` stages each
/// mirror so only the destination call carries the session token (see
/// [`mirror_command`]); the container must then have
/// [`REMOTE_SESSION_TOKEN_ENV`] set.
///
/// This reproduces what `mc mirror --overwrite <alias>/ <dest_prefix>` did in
/// one call, which `rc` does not support (it rejects an alias root as the
/// mirror source): the destination layout is still one folder per source
/// bucket under `dest_prefix`, which is what every restore path lists. The
/// destination bucket (the first segment of `dest_prefix` after the alias)
/// must already exist; `rc` creates the prefixes below it.
///
/// Returns the mirrored bucket names. Any `rc` failure becomes an error whose
/// output has been passed through `sensitive_values`.
pub(crate) async fn mirror_all_buckets(
    docker: &Docker,
    container_id: &str,
    source_alias: &str,
    dest_prefix: &str,
    dest_has_token: bool,
    sensitive_values: &SensitiveValues<'_>,
) -> Result<Vec<String>, RcClientError> {
    let source_root = format!("{}/", source_alias.trim_end_matches('/'));
    let buckets =
        list_dir_names(docker, container_id, &source_root, false, sensitive_values).await?;
    info!("rc: mirroring {} bucket(s): {:?}", buckets.len(), buckets);

    let dest_prefix = dest_prefix.trim_end_matches('/');
    for bucket in &buckets {
        let source = format!("{}{}/", source_root, bucket);
        let dest = format!("{}/{}/", dest_prefix, bucket);
        let command = mirror_command(
            &source,
            &dest,
            false,
            TokenSide::when(dest_has_token, TokenSide::Destination),
        );
        let command: Vec<&str> = command.iter().map(String::as_str).collect();
        let mirror = exec_capture(docker, container_id, &command).await?;
        if mirror.exit_code != 0 {
            return Err(RcClientError::MirrorFailed {
                bucket: bucket.clone(),
                source_path: source,
                destination: dest,
                exit_code: mirror.exit_code,
                output: sensitive_values
                    .redact(format!("{}\n{}", mirror.stderr.trim(), mirror.stdout.trim()).trim()),
            });
        }
    }
    Ok(buckets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(stdout: &str) -> Result<Vec<String>, RcClientError> {
        parse_ls_dir_names("bkp/bucket/pre/", stdout, &SensitiveValues::new())
    }

    #[test]
    fn a_truncated_listing_is_a_hard_error_not_a_partial_result() {
        let output = r#"{"items":[{"key":"pre/a/","is_dir":true}],"truncated":true}"#;
        match parse(output) {
            Err(RcClientError::TruncatedListing { target, returned }) => {
                assert_eq!(target, "bkp/bucket/pre/");
                assert_eq!(returned, 1);
            }
            other => panic!("expected TruncatedListing, got {other:?}"),
        }
    }

    #[test]
    fn a_listing_without_a_truncated_flag_cannot_prove_completeness() {
        let output = r#"{"items":[{"key":"pre/a/","is_dir":true}]}"#;
        assert!(matches!(
            parse(output),
            Err(RcClientError::UnparsableListing { .. })
        ));
    }

    #[test]
    fn listing_errors_are_redacted() {
        let sensitive = SensitiveValues::new().credential("AKIASECRETKEYID", "sekrit", None);
        let err = parse_ls_dir_names(
            "bkp/",
            r#"{"error":"denied for AKIASECRETKEYID"}"#,
            &sensitive,
        )
        .unwrap_err()
        .to_string();
        assert!(!err.contains("AKIASECRETKEYID"), "{err}");
        let err = parse_ls_dir_names("bkp/", "garbage sekrit", &sensitive)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("sekrit"), "{err}");
    }

    /// Exact output of `rc ls --json s/` (an alias root) from rustfs/rc v0.1.36.
    const ROOT_LISTING: &str = r#"{
  "items": [
    {
      "key": "dst",
      "last_modified": "2026-09-25T06:43:33Z",
      "is_dir": true
    },
    {
      "key": "src1",
      "last_modified": "2026-09-25T06:43:33Z",
      "is_dir": true
    }
  ],
  "truncated": false
}"#;

    /// Exact output of `rc ls --json s/dst/pre/` from rustfs/rc v0.1.36: keys
    /// are the full bucket-relative key, not just the folder name.
    const PREFIX_LISTING: &str = r#"{
  "items": [
    {
      "key": "pre/src1/",
      "is_dir": true
    },
    {
      "key": "pre/src2/",
      "is_dir": true
    }
  ],
  "truncated": false
}"#;

    #[test]
    fn parses_bucket_names_from_an_alias_root_listing() {
        assert_eq!(
            parse(ROOT_LISTING).unwrap(),
            vec!["dst".to_string(), "src1".to_string()]
        );
    }

    #[test]
    fn strips_the_listed_prefix_from_folder_keys() {
        assert_eq!(
            parse(PREFIX_LISTING).unwrap(),
            vec!["src1".to_string(), "src2".to_string()]
        );
    }

    #[test]
    fn strips_a_multi_segment_prefix() {
        let output =
            r#"{"items":[{"key":"backups/2026/09/app-data/","is_dir":true}],"truncated":false}"#;
        assert_eq!(parse(output).unwrap(), vec!["app-data"]);
    }

    #[test]
    fn ignores_objects_next_to_the_bucket_folders() {
        let output = r#"{"items":[
            {"key":"pre/manifest.json","size":12,"is_dir":false},
            {"key":"pre/bucket-a/","is_dir":true}
        ],"truncated":false}"#;
        assert_eq!(parse(output).unwrap(), vec!["bucket-a"]);
    }

    #[test]
    fn an_empty_listing_is_no_buckets_not_an_error() {
        let output = "{\n  \"items\": [],\n  \"truncated\": false\n}\n";
        assert!(parse(output).unwrap().is_empty());
    }

    #[test]
    fn rejects_mc_style_ndjson_instead_of_reporting_nothing_to_restore() {
        let mc_output = "{\"status\":\"success\",\"type\":\"folder\",\"key\":\"bkt1/\"}\n\
                         {\"status\":\"success\",\"type\":\"folder\",\"key\":\"bkt2/\"}\n";
        assert!(parse(mc_output).is_err());
    }

    #[test]
    fn surfaces_an_rc_error_document() {
        let output = r#"{"error":"Failed to list objects: Not found: Bucket not found: dst","details":{"type":"not_found"}}"#;
        let err = parse(output).unwrap_err().to_string();
        assert!(err.contains("Bucket not found"), "{err}");
    }

    #[test]
    fn rejects_empty_output() {
        assert!(parse("").is_err());
    }

    #[test]
    fn rc_host_env_never_carries_a_session_token_and_encodes_the_secret() {
        assert_eq!(
            rc_host_env("backup-dest", "https://s3.example.test/", "AKIA", "se/cret"),
            "RC_HOST_backup-dest=https://AKIA:se%2Fcret@s3.example.test"
        );
        assert_eq!(
            rc_host_env("dest", "localhost:9000", "k", "s"),
            "RC_HOST_dest=http://k:s@localhost:9000"
        );
        assert_eq!(
            rc_host_env("src", "http://minio:9000", "k", "s"),
            "RC_HOST_src=http://k:s@minio:9000"
        );
    }

    #[test]
    fn session_token_env_is_absent_for_long_lived_credentials() {
        assert_eq!(remote_session_token_env(None), None);
        assert_eq!(remote_session_token_env(Some("")), None);
        assert_eq!(
            remote_session_token_env(Some("tok/en")).as_deref(),
            Some("TEMPS_REMOTE_SESSION_TOKEN=tok/en")
        );
    }

    #[test]
    fn mirror_without_a_token_is_a_direct_rc_mirror() {
        assert_eq!(
            mirror_command("src/b/", "dst/p/b/", true, TokenSide::Neither),
            vec![
                "rc",
                "mirror",
                "--overwrite",
                "--remove",
                "src/b/",
                "dst/p/b/"
            ]
        );
        assert_eq!(
            mirror_command("src/b/", "dst/p/b/", false, TokenSide::Neither),
            vec!["rc", "mirror", "--overwrite", "src/b/", "dst/p/b/"]
        );
    }

    #[test]
    fn staged_mirror_passes_paths_as_arguments_and_never_the_token() {
        let command = mirror_command("src/b/", "dst/p/b/", false, TokenSide::Destination);
        assert_eq!(command[0..2], ["sh", "-c"]);
        assert_eq!(
            command[3..],
            ["sh", "src/b/", "dst/p/b/", "keep", "destination"]
        );
        let script = &command[2];
        // The token is only ever a shell expansion of the container env.
        assert!(script.contains("${TEMPS_REMOTE_SESSION_TOKEN}"));
        assert!(!script.contains("src/b/") && !script.contains("dst/p/b/"));
        // Exactly one -H per direction, on the token-side call only.
        let destination_branch = script.split("else").nth(1).unwrap();
        let lines: Vec<&str> = destination_branch
            .lines()
            .filter(|l| l.contains("rc "))
            .collect();
        assert_eq!(lines.len(), 2, "{destination_branch}");
        assert!(
            !lines[0].contains("-H"),
            "local step must not send the token: {}",
            lines[0]
        );
        assert!(lines[0].contains(r#""$1" "$stage/""#));
        assert!(lines[1].contains("-H \"x-amz-security-token:"));
        assert!(lines[1].contains(r#""$stage/" "$2""#));
        let source_branch = script
            .split("if [ \"$4\" = source ]; then")
            .nth(1)
            .unwrap()
            .split("else")
            .next()
            .unwrap();
        let lines: Vec<&str> = source_branch
            .lines()
            .filter(|l| l.contains("rc "))
            .collect();
        assert!(lines[0].contains("-H \"x-amz-security-token:") && lines[0].contains(r#""$1""#));
        assert!(!lines[1].contains("-H") && lines[1].contains(r#""$2""#));
        assert!(script.contains("trap 'rm -rf \"$stage\"' EXIT"));

        let restore = mirror_command("bkp/p/b/", "dest/b/", true, TokenSide::Source);
        assert_eq!(
            restore[3..],
            ["sh", "bkp/p/b/", "dest/b/", "remove", "source"]
        );
    }

    #[test]
    fn ls_sends_the_token_only_on_the_token_side() {
        assert_eq!(
            ls_json_command("src/", false),
            vec!["rc", "ls", "--json", "src/"]
        );
        let command = ls_json_command("bkp/b/p/", true);
        assert_eq!(command[0..2], ["sh", "-c"]);
        assert_eq!(command[3..], ["sh", "bkp/b/p/"]);
        assert!(command[2].contains("-H \"x-amz-security-token: ${TEMPS_REMOTE_SESSION_TOKEN}\""));
    }

    #[test]
    fn rc_image_is_pinned_by_digest() {
        assert!(RC_IMAGE.starts_with("rustfs/rc:v0.1.36@sha256:"));
        assert_eq!(RC_IMAGE.rsplit_once("@sha256:").unwrap().1.len(), 64);
    }

    /// Runs `command` in a one-shot `rc` container on `network` and returns
    /// (exit code, combined logs, the container's recorded argv).
    #[cfg(feature = "docker-tests")]
    async fn run_rc(
        docker: &Docker,
        network: &str,
        env: Vec<String>,
        command: Vec<String>,
    ) -> (i64, String, Vec<String>) {
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptions,
            WaitContainerOptions,
        };
        let (entrypoint, args) = command.split_first().unwrap();
        let name = format!("temps-test-rcsink-run-{}", rand::random::<u32>());
        let container = docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                bollard::models::ContainerCreateBody {
                    image: Some(RC_IMAGE.to_string()),
                    entrypoint: Some(vec![entrypoint.clone()]),
                    cmd: Some(args.to_vec()),
                    env: Some(env),
                    host_config: Some(bollard::models::HostConfig {
                        network_mode: Some(network.to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .unwrap();
        let exit = docker
            .wait_container(&container.id, None::<WaitContainerOptions>)
            .try_collect::<Vec<_>>()
            .await
            .ok()
            .and_then(|v| v.into_iter().next().map(|r| r.status_code))
            // bollard reports a non-zero exit as an error; it is read back
            // from the inspect below.
            .unwrap_or(-1);
        let inspect = docker.inspect_container(&container.id, None).await.unwrap();
        let exit = inspect
            .state
            .as_ref()
            .and_then(|s| s.exit_code)
            .unwrap_or(exit);
        let mut argv = inspect
            .config
            .as_ref()
            .and_then(|c| c.entrypoint.clone())
            .unwrap_or_default();
        argv.extend(
            inspect
                .config
                .as_ref()
                .and_then(|c| c.cmd.clone())
                .unwrap_or_default(),
        );
        let logs = docker
            .logs(
                &container.id,
                Some(LogsOptionsBuilder::new().stdout(true).stderr(true).build()),
            )
            .try_collect::<Vec<_>>()
            .await
            .map(|v| v.into_iter().map(|c| c.to_string()).collect::<String>())
            .unwrap_or_default();
        let _ = docker
            .remove_container(
                &container.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        (exit, logs, argv)
    }

    /// Proves the staged mirror against real HTTP traffic: the destination
    /// is a sink that records every request's headers (and answers 403), the
    /// source a real RustFS that rejects an unknown session token.
    ///
    /// - step A (RustFS → scratch) must succeed, which it can only do without
    ///   the header (the control call below shows RustFS rejecting it);
    /// - step B (scratch → sink) must carry `x-amz-security-token`;
    /// - the token must appear in no container argv, only in its env.
    #[cfg(feature = "docker-tests")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn staged_mirror_sends_the_token_only_to_the_token_side() {
        use bollard::query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptions};

        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker unavailable, skipping: {e}");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }
        for image in [
            RC_IMAGE,
            "busybox:latest",
            crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE,
        ] {
            crate::utils::pull_image_with_retry(&docker, image, None)
                .await
                .unwrap_or_else(|e| panic!("pull {image}: {e}"));
        }

        let run_id = rand::random::<u32>();
        let network = format!("temps-test-rcsink-net-{run_id}");
        let local_name = format!("temps-test-rcsink-local-{run_id}");
        let sink_name = format!("temps-test-rcsink-sink-{run_id}");
        docker
            .create_network(bollard::models::NetworkCreateRequest {
                name: network.clone(),
                ..Default::default()
            })
            .await
            .unwrap();

        let start =
            |name: String, image: &'static str, env: Vec<String>, cmd: Option<Vec<String>>| {
                let docker = docker.clone();
                let network = network.clone();
                async move {
                    let container = docker
                        .create_container(
                            Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                            bollard::models::ContainerCreateBody {
                                image: Some(image.to_string()),
                                env: Some(env),
                                cmd,
                                host_config: Some(bollard::models::HostConfig {
                                    network_mode: Some(network),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        )
                        .await
                        .unwrap();
                    docker
                        .start_container(
                            &container.id,
                            None::<bollard::query_parameters::StartContainerOptions>,
                        )
                        .await
                        .unwrap();
                }
            };
        start(
            local_name.clone(),
            crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE,
            vec![
                "RUSTFS_ACCESS_KEY=localkey".to_string(),
                "RUSTFS_SECRET_KEY=local/secret+1".to_string(),
            ],
            None,
        )
        .await;
        // Header-capturing sink: logs each request's headers, answers 403.
        let handler = r#"printf '#!/bin/sh\nwhile IFS= read -r l; do l=$(printf %%s "$l" | tr -d "\\r"); [ -z "$l" ] && break; echo "$l" >> /tmp/req.log; done\necho --- >> /tmp/req.log\nprintf "HTTP/1.1 403 Forbidden\\r\\nContent-Length: 0\\r\\nConnection: close\\r\\n\\r\\n"\n' > /h.sh; chmod +x /h.sh; touch /tmp/req.log; exec nc -lk -p 9000 -e /h.sh"#;
        start(
            sink_name.clone(),
            "busybox:latest",
            vec![],
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                handler.to_string(),
            ]),
        )
        .await;

        let token = format!("temps-test-session-token-{run_id}");
        let env = vec![
            rc_host_env(
                "local",
                &format!("http://{local_name}:9000"),
                "localkey",
                "local/secret+1",
            ),
            rc_host_env("remote", &format!("http://{sink_name}:9000"), "k", "s"),
            remote_session_token_env(Some(&token)).unwrap(),
        ];

        // Seed the source (waits for RustFS to come up).
        let (seed_exit, seed_logs, _) = run_rc(
            &docker,
            &network,
            env.clone(),
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "for i in $(seq 1 60); do rc mb --ignore-existing local/src && break; sleep 1; done; \
                 echo payload > /tmp/f && rc cp /tmp/f local/src/obj.txt"
                    .to_string(),
            ],
        )
        .await;

        // Control: RustFS rejects a request that carries an unknown token.
        let (control_exit, _, _) = run_rc(
            &docker,
            &network,
            env.clone(),
            vec![
                "sh".to_string(),
                "-c".to_string(),
                r#"rc -H "x-amz-security-token: ${TEMPS_REMOTE_SESSION_TOKEN}" ls local/src/"#
                    .to_string(),
            ],
        )
        .await;

        let command = mirror_command(
            "local/src/",
            "remote/bkt/pre/src/",
            false,
            TokenSide::Destination,
        );
        let (mirror_exit, mirror_logs, argv) =
            run_rc(&docker, &network, env.clone(), command).await;

        let (_, ls_logs, ls_argv) = run_rc(
            &docker,
            &network,
            env.clone(),
            ls_json_command("remote/bkt/pre/", true),
        )
        .await;

        let sink_log = {
            let exec = docker
                .create_exec(
                    &sink_name,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(vec!["cat", "/tmp/req.log"]),
                        attach_stdout: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let mut out = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&exec.id, None).await.unwrap()
            {
                while let Ok(Some(chunk)) = output.try_next().await {
                    out.push_str(&chunk.to_string());
                }
            }
            out
        };

        for name in [&local_name, &sink_name] {
            let _ = docker
                .remove_container(
                    name,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
        let _ = docker.remove_network(&network).await;

        assert_eq!(seed_exit, 0, "seeding RustFS failed: {seed_logs}");
        assert_ne!(
            control_exit, 0,
            "RustFS must reject an unknown session token, or step A proves nothing"
        );
        assert!(
            mirror_logs.contains("obj.txt"),
            "step A (no token) must copy the object out of RustFS: {mirror_logs}"
        );
        assert_ne!(mirror_exit, 0, "the sink rejects step B: {mirror_logs}");
        let sink_lower = sink_log.to_ascii_lowercase();
        let expected = format!("x-amz-security-token: {token}");
        assert!(
            sink_lower.contains(&expected),
            "step B must send the session token to the token side: {sink_log} / {ls_logs}"
        );
        assert!(
            sink_lower.contains("prefix=pre%2fsrc%2f"),
            "step B must target the destination prefix on the token side: {sink_log}"
        );
        for request in sink_lower.split("---").filter(|r| !r.trim().is_empty()) {
            assert!(
                request.contains(&expected),
                "every request to the token side carries the token: {request}"
            );
        }
        for recorded in [&argv, &ls_argv] {
            assert!(
                !recorded.iter().any(|arg| arg.contains(&token)),
                "the token must never be in argv: {recorded:?}"
            );
        }
    }
}
