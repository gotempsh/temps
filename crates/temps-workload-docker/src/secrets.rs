// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Secret files delivered at `/run/secrets` (read-only bind of a
//! per-container host directory, mode 0700, files 0400, owned by the
//! image's `USER`). Shared by the legacy deployer and [`crate::DockerDriver`]
//! so both deliver secrets the same way.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bollard::Docker;
use tracing::warn;

/// Resolve the persistent root directory for materialized secret files.
///
/// Order of preference, mirroring the rest of the codebase
/// (`temps-sandbox`, agent session storage):
///   1. `$TEMPS_DATA_DIR/secrets`
///   2. `$HOME/.temps/secrets`
///   3. `./.temps/secrets` (last-resort fallback for headless container
///      runtimes where neither env var is set)
///
/// Crucially, NONE of these resolve to `std::env::temp_dir()`. On most
/// Linux distros `/tmp` is mounted as tmpfs and wiped at boot, which
/// caused container `/run/secrets` mounts to point at empty/missing
/// directories after a host reboot and forced a redeploy.
pub fn default_secrets_root() -> PathBuf {
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: TEMPS_DATA_DIR (with host lookup of HOME as fallback) locates the data directory and is fixed before the database exists"
    )]
    let base = std::env::var("TEMPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".temps")
        });
    base.join("secrets")
}

pub fn validate_secret_dir_name(name: &str) -> std::io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "invalid container name '{}': must be a single path component",
                name
            ),
        ));
    }

    Ok(())
}

/// Writes secrets as files into a per-container host directory for Docker
/// to bind-mount into `/run/secrets`. The directory is created (or recreated)
/// fresh on each call so stale entries from a previous deployment of the same
/// container name don't leak through. Directory mode 0700, file mode 0400.
///
/// When `owner` is provided, the directory and every file inside are
/// `chown`ed to that (uid, gid). Combined with 0700/0400 this means only
/// that UID inside the container can read the secrets — matching the
/// image's declared `USER` so distroless/nonroot images work without
/// world-readable files.
///
/// Rejects keys that would escape the directory (path separators, `.`, `..`)
/// to defend against a maliciously-crafted secret name.
#[cfg_attr(not(unix), allow(unused_variables))]
pub fn write_secrets_to_host_dir(
    dir: &Path,
    secrets: &HashMap<String, String>,
    owner: Option<(u32, u32)>,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write;

    // Ensure the parent (secrets root) exists. The runtime's `new()` already
    // best-effort creates it, but a hostile chmod or fresh install between
    // restarts could have left it missing — recreate so we don't fail with
    // ENOENT. We deliberately do NOT chmod the parent here: that's the
    // runtime's job, and on shared hosts the parent may be intentionally
    // owned by a different user.
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }

    // Recreate fresh — wipes any stale files from a previous container with
    // the same name (rolling deploys, retries after failure, etc.).
    //
    // After a host reboot we may inherit a leaf directory whose contents
    // were chowned to the previous container's USER (e.g. nonroot uid
    // 65532) while the parent is owned by the temps user. `remove_dir_all`
    // on such a tree fails with EACCES because we can't unlink files we
    // don't own inside a directory we *do* own (sticky-bit semantics on
    // some filesystems). Reset the leaf's perms to 0700 owned-by-us first
    // so the recursive remove can succeed.
    if dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort: if this chmod fails (we don't own the dir), the
            // remove below will still surface the real error.
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        if let Err(e) = fs::remove_dir_all(dir) {
            // If the leaf itself is owned by another uid (e.g. previous
            // container's USER) and we can't traverse into it, fall back
            // to renaming it aside so the deploy can proceed. The orphan
            // is logged for operator cleanup; it's not in /tmp anymore so
            // it won't auto-vanish on reboot, but it's also not blocking
            // the deploy.
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                let orphan = dir.with_extension(format!(
                    "orphan-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                fs::rename(dir, &orphan).map_err(|rename_err| {
                    std::io::Error::new(
                        rename_err.kind(),
                        format!(
                            "failed to clear stale secrets dir {} \
                             (rename to {} also failed: {}; original error: {})",
                            dir.display(),
                            orphan.display(),
                            rename_err,
                            e
                        ),
                    )
                })?;
                warn!(
                    "Renamed unowned stale secrets dir to {} so deploy could proceed; \
                     please remove manually",
                    orphan.display()
                );
            } else {
                return Err(e);
            }
        }
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }

    for (key, value) in secrets {
        // Keys must be plain identifiers — defense-in-depth even though
        // SecretService::validate_secret_key already enforces this.
        if key.is_empty()
            || key == "."
            || key == ".."
            || key.contains('/')
            || key.contains('\\')
            || key.contains('\0')
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid secret key '{}': contains path-separator characters",
                    key
                ),
            ));
        }
        let path = dir.join(key);
        let mut f = fs::File::create(&path)?;
        f.write_all(value.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400))?;
        }
    }

    // Chown after files are written so we don't have to re-chown on every
    // write. Dir chown happens last so we still have permission to create
    // files in it while writing (when running as non-root, chown-to-self
    // is a no-op; when running as root we own it either way).
    //
    // chown(2) requires root on every sensible OS, or chown-to-self. When
    // Temps itself runs unprivileged (local macOS dev is the common case)
    // and the image runs as a non-root uid like 1000, the chown will fail
    // with EPERM. Rather than breaking the deploy, fall back to
    // world-readable permissions (0755 dir, 0444 files) so the container
    // can read its secrets. The container still has `cap_drop: ALL` and
    // `no-new-privileges`, and the bind mount is read-only — the trust
    // boundary is still the container itself.
    #[cfg(unix)]
    if let Some((uid, gid)) = owner {
        use std::os::unix::fs::{chown, PermissionsExt};

        let chown_result: std::io::Result<()> = (|| {
            for key in secrets.keys() {
                chown(dir.join(key), Some(uid), Some(gid))?;
            }
            chown(dir, Some(uid), Some(gid))?;
            Ok(())
        })();

        if let Err(e) = chown_result {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                warn!(
                    "chown of secrets dir {} to {}:{} denied (Temps is not root); \
                     falling back to world-readable mode so nonroot containers can read. \
                     Run Temps as root for strict per-uid ownership.",
                    dir.display(),
                    uid,
                    gid
                );
                // World-readable fallback. Parent dir already restricts
                // access on the host side (only the Temps user can traverse
                // into the secrets root under $TEMPS_DATA_DIR/secrets);
                // these bits are what the container sees.
                fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
                for key in secrets.keys() {
                    fs::set_permissions(dir.join(key), fs::Permissions::from_mode(0o444))?;
                }
            } else {
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Parses a Docker image `USER` spec into a numeric `(uid, gid)` pair.
///
/// Handles the forms Docker accepts in a Dockerfile `USER` directive and
/// in `Config.User`: `""` (root), `"0"`, `"1000"`, `"1000:1000"`,
/// `"1000:gname"`, `":1000"`. Named users/groups cannot be resolved
/// without consulting the image's `/etc/passwd` — those return `None`
/// so the caller can decide whether to look them up or fall back.
///
/// When only a uid is given, gid defaults to the same value (matches
/// Docker's behavior: `USER 1000` runs as uid=1000, gid=1000).
pub fn parse_numeric_user_spec(spec: &str) -> Option<(u32, u32)> {
    let spec = spec.trim();
    if spec.is_empty() || spec == "root" {
        return Some((0, 0));
    }

    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    let uid = if user_part.is_empty() {
        0
    } else {
        user_part.parse::<u32>().ok()?
    };

    let gid = match group_part {
        Some(g) if !g.is_empty() => g.parse::<u32>().ok()?,
        _ => uid,
    };

    Some((uid, gid))
}

/// The numeric `(uid, gid)` image `image` runs as, from its `Config.User`,
/// so its secret files can be owned by that user.
///
/// `(0, 0)` (Docker's default) when the image cannot be inspected or names
/// a user that only the image's `/etc/passwd` could resolve; both cases are
/// logged, since a nonroot image may then be unable to read its secrets.
pub async fn image_owner(docker: &Docker, image: &str) -> (u32, u32) {
    let inspect = match docker.inspect_image(image).await {
        Ok(inspect) => inspect,
        Err(e) => {
            warn!(
                "Failed to inspect image '{}' for secrets chown; defaulting to root: {}",
                image, e
            );
            return (0, 0);
        }
    };
    let user_spec = inspect
        .config
        .as_ref()
        .and_then(|c| c.user.as_deref())
        .unwrap_or("");
    match parse_numeric_user_spec(user_spec) {
        Some(pair) => pair,
        None => {
            warn!(
                "Image '{}' declares USER='{}' which is not numeric; \
                 secrets will be owned by root and may be unreadable. \
                 Use numeric UIDs (e.g. USER 1000:1000) for secrets \
                 to work with this image.",
                image, user_spec
            );
            (0, 0)
        }
    }
}

/// The bind that mounts `dir` read-only at `/run/secrets`.
pub fn secrets_bind(dir: &Path) -> String {
    format!("{}:/run/secrets:ro", dir.display())
}

/// Remove `dir` (a container's secrets directory) if it exists. Best
/// effort: a failure is logged and left for the next deploy of the same
/// name, which recreates the directory fresh.
pub fn remove_secrets_dir(dir: &Path) {
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            warn!(
                "Failed to clean up secrets host dir {}: {}",
                dir.display(),
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_write_secrets_to_host_dir_creates_files_with_correct_perms() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c1");

        let mut secrets = HashMap::new();
        secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());
        secrets.insert("API_KEY".to_string(), "abc\ndef".to_string());

        write_secrets_to_host_dir(&dir, &secrets, None).expect("write");

        let db = std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap();
        assert_eq!(db, "s3cret");
        let api = std::fs::read_to_string(dir.join("API_KEY")).unwrap();
        assert_eq!(api, "abc\ndef");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_overwrites_stale_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c2");

        let mut first = HashMap::new();
        first.insert("OLD".to_string(), "old".to_string());
        write_secrets_to_host_dir(&dir, &first, None).unwrap();
        assert!(dir.join("OLD").exists());

        let mut second = HashMap::new();
        second.insert("NEW".to_string(), "new".to_string());
        write_secrets_to_host_dir(&dir, &second, None).unwrap();
        // Stale file from first call must be gone
        assert!(!dir.join("OLD").exists());
        assert!(dir.join("NEW").exists());
    }

    #[test]
    fn test_default_secrets_root_uses_temps_data_dir() {
        // Smoke test the env precedence. We can't unset HOME without
        // breaking other tests, so verify TEMPS_DATA_DIR wins when set.
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var("TEMPS_DATA_DIR").ok();
        std::env::set_var("TEMPS_DATA_DIR", tmp.path());
        let root = default_secrets_root();
        assert_eq!(root, tmp.path().join("secrets"));
        // Critically, this must NOT live under /tmp (unless TEMPS_DATA_DIR
        // points there explicitly) — that was the bug we just fixed.
        match prev {
            Some(v) => std::env::set_var("TEMPS_DATA_DIR", v),
            None => std::env::remove_var("TEMPS_DATA_DIR"),
        }
    }

    #[test]
    fn test_default_secrets_root_is_persistent_path() {
        // Without TEMPS_DATA_DIR, the resolved root must live under
        // $HOME/.temps/secrets — i.e. NOT under std::env::temp_dir(),
        // which is tmpfs on most Linux distros and was the source of
        // the post-reboot redeploy regression.
        let prev_data = std::env::var("TEMPS_DATA_DIR").ok();
        std::env::remove_var("TEMPS_DATA_DIR");
        let root = default_secrets_root();
        assert!(
            !root.starts_with(std::env::temp_dir()),
            "secrets root unexpectedly resolved to a tmpfs path: {}",
            root.display()
        );
        if let Some(v) = prev_data {
            std::env::set_var("TEMPS_DATA_DIR", v);
        }
    }

    #[test]
    fn test_validate_secret_dir_name_rejects_path_traversal_components() {
        for bad in [
            "",
            ".",
            "..",
            "../escaped/deployment-1",
            "a/b",
            "a\\b",
            "with\0null",
        ] {
            let err = validate_secret_dir_name(bad).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "name={}", bad);
        }

        validate_secret_dir_name("project-1").unwrap();
    }

    #[test]
    fn test_write_secrets_to_host_dir_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c3");

        for bad in ["..", ".", "../escape", "a/b", "a\\b", "with\0null"] {
            let mut secrets = HashMap::new();
            secrets.insert(bad.to_string(), "v".to_string());
            let err = write_secrets_to_host_dir(&dir, &secrets, None).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "key={}", bad);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_empty_creates_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c4");
        let secrets: HashMap<String, String> = HashMap::new();
        write_secrets_to_host_dir(&dir, &secrets, None).unwrap();
        assert!(dir.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn test_parse_numeric_user_spec() {
        // Empty / root → (0, 0)
        assert_eq!(parse_numeric_user_spec(""), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("  "), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("root"), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("0"), Some((0, 0)));

        // Uid only → gid defaults to uid (Docker's behavior)
        assert_eq!(parse_numeric_user_spec("1000"), Some((1000, 1000)));
        assert_eq!(parse_numeric_user_spec("65532"), Some((65532, 65532)));

        // uid:gid
        assert_eq!(parse_numeric_user_spec("1000:2000"), Some((1000, 2000)));
        assert_eq!(parse_numeric_user_spec("65532:65532"), Some((65532, 65532)));

        // :gid (uid defaults to 0 — matches Docker)
        assert_eq!(parse_numeric_user_spec(":1000"), Some((0, 1000)));

        // uid: (trailing colon → gid falls back to uid)
        assert_eq!(parse_numeric_user_spec("1000:"), Some((1000, 1000)));

        // Named users/groups are not numeric — caller falls back
        assert_eq!(parse_numeric_user_spec("node"), None);
        assert_eq!(parse_numeric_user_spec("node:node"), None);
        assert_eq!(parse_numeric_user_spec("1000:node"), None);
        assert_eq!(parse_numeric_user_spec("node:1000"), None);

        // Malformed
        assert_eq!(parse_numeric_user_spec("abc"), None);
        assert_eq!(parse_numeric_user_spec("-1"), None);
        assert_eq!(parse_numeric_user_spec("1:2:3"), None);
    }

    #[test]
    fn test_write_secrets_to_host_dir_chown_to_self_is_noop() {
        // chown(2) only succeeds when chown-ing to yourself (unless root).
        // We verify the happy path by chown-ing to our own uid/gid: the
        // function must succeed and leave the mode bits intact. This guards
        // against the chown being accidentally reordered before the file
        // writes (which would fail) or changing the permission bits.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let dir = tmp.path().join("c5");

            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            let my_gid = std::fs::metadata(tmp.path()).unwrap().gid();

            write_secrets_to_host_dir(&dir, &secrets, Some((my_uid, my_gid))).unwrap();

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_falls_back_to_world_readable_on_eperm() {
        // When Temps runs unprivileged and is asked to chown to a uid that
        // isn't its own, chown returns EPERM. The function must not fail
        // the deploy — it must downgrade permissions so the container can
        // still read its secrets. Only runs as non-root (skipped under sudo).
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            if my_uid == 0 {
                eprintln!("running as root; skipping EPERM fallback test");
                return;
            }

            let dir = tmp.path().join("c6");
            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            // Ask for a uid/gid we definitely don't own — forces EPERM.
            write_secrets_to_host_dir(&dir, &secrets, Some((65532, 65532)))
                .expect("fallback must succeed, not error");

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o755, "dir must be world-traversable on fallback");

            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o444, "file must be world-readable on fallback");

            // Content still intact.
            assert_eq!(
                std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap(),
                "s3cret"
            );
        }
    }
}
