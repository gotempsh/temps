// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Downloads the Chromium build that local screenshots launch.
//!
//! This replaces headless_chrome's `fetch` feature, whose `zip` 8 dependency
//! cannot be moved to a patched release. It keeps that feature's behaviour so
//! a host that already downloaded Chromium reuses it: the same pinned
//! revision, the same snapshot URL and the same install directory,
//! `<data dir>/headless-chrome/<platform>-<revision>`. Like the feature it
//! replaces, the downloaded build is always used; an installed system Chrome
//! is not looked up.
//!
//! Everything here blocks. It is called from the launch closure, on a
//! blocking thread and under `CHROME_LAUNCH_LOCK`, which is exactly where
//! headless_chrome used to download.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use tracing::info;

use crate::error::{ScreenshotError, ScreenshotResult};

/// The Chromium snapshot revision headless_chrome 1.0.22 pins
/// (`headless_chrome::browser::fetcher::CUR_REV`).
pub(crate) const CHROMIUM_REVISION: &str = "1095492";

const SNAPSHOT_HOST: &str = "https://storage.googleapis.com";

/// Directory under the platform data directory, shared with headless_chrome's
/// fetcher so existing downloads are found.
const APP_DIR: &str = "headless-chrome";

/// A Chromium snapshot archive is about 150 MB; a response this large is not one.
const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;

/// Bound on what extraction may write, so a malformed archive cannot fill the disk.
const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the whole download: about 150 MB at 170 KB/s.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Longest symlink target accepted from the archive.
const MAX_SYMLINK_TARGET_BYTES: u64 = 4096;

/// Where a platform's Chromium snapshot lives and how it unpacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Platform {
    /// Install directory prefix, as headless_chrome names it.
    name: &'static str,
    /// Directory under `chromium-browser-snapshots/`.
    snapshot_dir: &'static str,
    /// The archive's file stem, which is also its top-level directory.
    archive: &'static str,
    /// The executable, relative to `archive`.
    executable: &'static str,
}

const LINUX: Platform = Platform {
    name: "linux",
    snapshot_dir: "Linux_x64",
    archive: "chrome-linux",
    executable: "chrome",
};

const MAC_ARM: Platform = Platform {
    name: "mac_arm",
    snapshot_dir: "Mac_Arm",
    archive: "chrome-mac",
    executable: "Chromium.app/Contents/MacOS/Chromium",
};

const MAC: Platform = Platform {
    name: "mac",
    snapshot_dir: "Mac",
    archive: "chrome-mac",
    executable: "Chromium.app/Contents/MacOS/Chromium",
};

fn host_platform() -> Option<Platform> {
    if cfg!(target_os = "linux") {
        Some(LINUX)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(MAC_ARM)
    } else if cfg!(target_os = "macos") {
        Some(MAC)
    } else {
        None
    }
}

/// The Chromium executable to launch, downloading it on first use.
///
/// `None` on a platform with no Chromium snapshot: headless_chrome then looks
/// for an installed Chrome itself.
pub(crate) fn chromium_executable() -> ScreenshotResult<Option<PathBuf>> {
    match ChromiumInstaller::for_this_host()? {
        Some(installer) => installer.ensure_installed().map(Some),
        None => Ok(None),
    }
}

/// Installs one Chromium revision for one platform under `install_root`.
#[derive(Debug, Clone)]
pub(crate) struct ChromiumInstaller {
    host: String,
    install_root: PathBuf,
    platform: Platform,
    revision: String,
}

impl ChromiumInstaller {
    /// The installer for this host, or `None` on a platform without a
    /// Chromium snapshot.
    fn for_this_host() -> ScreenshotResult<Option<Self>> {
        let Some(platform) = host_platform() else {
            return Ok(None);
        };
        let dirs = directories::ProjectDirs::from("", "", APP_DIR).ok_or_else(|| {
            ScreenshotError::ChromeInstall {
                revision: CHROMIUM_REVISION.to_string(),
                path: format!("<data dir>/{}", APP_DIR),
                reason: "could not determine this user's data directory (is $HOME set?)"
                    .to_string(),
            }
        })?;
        Ok(Some(Self::new(
            SNAPSHOT_HOST,
            dirs.data_dir().to_path_buf(),
            platform,
            CHROMIUM_REVISION,
        )))
    }

    fn new(host: &str, install_root: PathBuf, platform: Platform, revision: &str) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            install_root,
            platform,
            revision: revision.to_string(),
        }
    }

    fn install_name(&self) -> String {
        format!("{}-{}", self.platform.name, self.revision)
    }

    fn install_dir(&self) -> PathBuf {
        self.install_root.join(self.install_name())
    }

    /// The executable inside an install rooted at `dir`.
    fn executable_in(&self, dir: &Path) -> PathBuf {
        dir.join(self.platform.archive)
            .join(self.platform.executable)
    }

    fn download_url(&self) -> String {
        format!(
            "{}/chromium-browser-snapshots/{}/{}/{}.zip",
            self.host, self.platform.snapshot_dir, self.revision, self.platform.archive
        )
    }

    fn error(&self, path: &Path, reason: String) -> ScreenshotError {
        ScreenshotError::ChromeInstall {
            revision: self.revision.clone(),
            path: path.display().to_string(),
            reason,
        }
    }

    /// Return the installed executable, downloading and unpacking the
    /// revision first if it is not installed yet.
    ///
    /// The archive is unpacked into a staging directory and moved into place
    /// only once it holds the executable, so an interrupted install never
    /// looks complete. A directory left by one that was interrupted is
    /// replaced.
    pub(crate) fn ensure_installed(&self) -> ScreenshotResult<PathBuf> {
        let install_dir = self.install_dir();
        let executable = self.executable_in(&install_dir);
        if executable.is_file() {
            return Ok(executable);
        }

        fs::create_dir_all(&self.install_root).map_err(|e| {
            self.error(
                &self.install_root,
                format!("cannot create the directory: {}", e),
            )
        })?;

        // Named per process, so two Temps processes sharing a home directory
        // never write the same file.
        let pid = std::process::id();
        let archive_path =
            self.install_root
                .join(format!("{}.{}.zip.part", self.install_name(), pid));
        let staging_dir =
            self.install_root
                .join(format!("{}.{}.partial", self.install_name(), pid));

        let result = self.download_and_unpack(&archive_path, &staging_dir, &install_dir);
        // Best effort: on success both are already gone, and a leftover is
        // replaced by the next attempt anyway.
        let _ = fs::remove_file(&archive_path);
        let _ = fs::remove_dir_all(&staging_dir);
        result?;
        Ok(executable)
    }

    fn download_and_unpack(
        &self,
        archive_path: &Path,
        staging_dir: &Path,
        install_dir: &Path,
    ) -> ScreenshotResult<()> {
        let url = self.download_url();
        info!(
            url = %url,
            revision = %self.revision,
            path = %install_dir.display(),
            "Downloading Chromium for local screenshots (one-time download of about 150 MB)"
        );
        let downloaded = download(&url, archive_path, MAX_DOWNLOAD_BYTES).map_err(|reason| {
            self.error(archive_path, format!("downloading {}: {}", url, reason))
        })?;

        if staging_dir.exists() {
            fs::remove_dir_all(staging_dir).map_err(|e| {
                self.error(
                    staging_dir,
                    format!("cannot remove a stale staging directory: {}", e),
                )
            })?;
        }
        fs::create_dir_all(staging_dir)
            .map_err(|e| self.error(staging_dir, format!("cannot create the directory: {}", e)))?;
        extract_archive(archive_path, staging_dir, MAX_EXTRACTED_BYTES).map_err(|reason| {
            self.error(
                archive_path,
                format!("unpacking the archive from {}: {}", url, reason),
            )
        })?;

        let staged_executable = self.executable_in(staging_dir);
        if !staged_executable.is_file() {
            return Err(self.error(
                &staged_executable,
                format!(
                    "the archive from {} does not contain {}/{}",
                    url, self.platform.archive, self.platform.executable
                ),
            ));
        }

        // Another process may have finished the same install meanwhile.
        if self.executable_in(install_dir).is_file() {
            return Ok(());
        }
        if install_dir.exists() {
            fs::remove_dir_all(install_dir).map_err(|e| {
                self.error(
                    install_dir,
                    format!("cannot remove an incomplete install: {}", e),
                )
            })?;
        }
        if let Err(e) = fs::rename(staging_dir, install_dir) {
            if !self.executable_in(install_dir).is_file() {
                return Err(self.error(
                    install_dir,
                    format!("cannot move {} into place: {}", staging_dir.display(), e),
                ));
            }
        }

        info!(
            revision = %self.revision,
            path = %install_dir.display(),
            downloaded_bytes = downloaded,
            "Chromium installed for local screenshots"
        );
        Ok(())
    }
}

/// Stream `url` into `dest`, refusing responses larger than `max_bytes`.
fn download(url: &str, dest: &Path, max_bytes: u64) -> Result<u64, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| format!("cannot build the HTTP client: {}", e))?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("request failed: {}", e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the server answered HTTP {}", status));
    }
    if let Some(length) = response.content_length() {
        if length > max_bytes {
            return Err(format!(
                "the response is {} bytes, more than the {} byte limit",
                length, max_bytes
            ));
        }
    }

    let file =
        File::create(dest).map_err(|e| format!("cannot create {}: {}", dest.display(), e))?;
    let mut writer = BufWriter::new(file);
    let written = io::copy(&mut response.take(max_bytes.saturating_add(1)), &mut writer)
        .map_err(|e| format!("reading the response failed: {}", e))?;
    if written > max_bytes {
        return Err(format!(
            "the response is larger than the {} byte limit",
            max_bytes
        ));
    }
    writer
        .flush()
        .map_err(|e| format!("cannot write {}: {}", dest.display(), e))?;
    Ok(written)
}

/// Unpack `archive` into the empty directory `dest`, writing at most
/// `max_bytes`.
///
/// The archive is a download, so it is not trusted to place anything outside
/// `dest`: every entry path must stay inside it, no entry may be written
/// through a symlink, a symlink may only point inside `dest`, and
/// setuid/setgid/sticky bits are dropped.
fn extract_archive(archive: &Path, dest: &Path, max_bytes: u64) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| format!("cannot open the archive: {}", e))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| format!("not a valid zip archive: {}", e))?;

    let mut remaining = max_bytes;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| format!("cannot read entry {}: {}", index, e))?;
        let name = entry
            .name()
            .map(|name| name.into_owned())
            .unwrap_or_else(|_| format!("#{}", index));
        let relative = entry
            .enclosed_name()
            .map(|path| normalize(&path))
            .ok_or_else(|| format!("entry '{}' has a path outside the archive", name))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if passes_through_symlink(dest, &relative) {
            return Err(format!("entry '{}' is inside a symlink", name));
        }
        let out = dest.join(&relative);

        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| io_error(&out, e))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }

        if entry.is_symlink() {
            let mut target = String::new();
            (&mut entry)
                .take(MAX_SYMLINK_TARGET_BYTES)
                .read_to_string(&mut target)
                .map_err(|e| format!("cannot read the target of symlink '{}': {}", name, e))?;
            if !symlink_target_stays_inside(&relative, Path::new(&target)) {
                return Err(format!(
                    "symlink '{}' points outside the archive (to '{}')",
                    name, target
                ));
            }
            create_symlink(&target, &out)?;
            continue;
        }

        let file = File::create(&out).map_err(|e| io_error(&out, e))?;
        let mut writer = BufWriter::new(file);
        let written = io::copy(
            &mut (&mut entry).take(remaining.saturating_add(1)),
            &mut writer,
        )
        .map_err(|e| format!("cannot unpack '{}': {}", name, e))?;
        if written > remaining {
            return Err(format!(
                "the archive unpacks to more than {} bytes",
                max_bytes
            ));
        }
        remaining -= written;
        writer.flush().map_err(|e| io_error(&out, e))?;
        drop(writer);

        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777))
                .map_err(|e| io_error(&out, e))?;
        }
    }
    Ok(())
}

fn io_error(path: &Path, error: io::Error) -> String {
    format!("{}: {}", path.display(), error)
}

/// Resolve `.` and `..` lexically. `enclosed_name` already guarantees the
/// path never climbs above its root.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized
}

/// Whether any parent directory of `relative` under `dest` is a symlink.
fn passes_through_symlink(dest: &Path, relative: &Path) -> bool {
    let mut current = dest.to_path_buf();
    let Some(parent) = relative.parent() else {
        return false;
    };
    for component in parent.components() {
        current.push(component);
        if fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return true;
        }
    }
    false
}

/// Whether a symlink at `link` (relative to the extraction root) pointing to
/// `target` resolves inside the root.
///
/// The target must be relative, made of some leading `..` followed only by
/// names, and may not climb above the root. Since no entry is written through
/// a symlink, the directories those `..` climb are real ones, and every
/// symlink the names pass through was held to the same rule.
fn symlink_target_stays_inside(link: &Path, target: &Path) -> bool {
    if target.as_os_str().is_empty() {
        return false;
    }
    let depth = link
        .parent()
        .map_or(0, |parent| parent.components().count());
    let mut climbs = 0;
    let mut descended = false;
    for component in target.components() {
        match component {
            Component::ParentDir if !descended => climbs += 1,
            Component::Normal(_) => descended = true,
            Component::ParentDir
            | Component::CurDir
            | Component::RootDir
            | Component::Prefix(_) => return false,
        }
    }
    climbs <= depth
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| io_error(link, e))
}

#[cfg(not(unix))]
fn create_symlink(_target: &str, link: &Path) -> Result<(), String> {
    Err(format!(
        "{}: symlinks are not supported on this platform",
        link.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    enum Entry<'a> {
        Dir(&'a str),
        File(&'a str, &'a [u8], u32),
        Symlink(&'a str, &'a str),
    }

    fn zip_bytes(entries: &[Entry<'_>]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for entry in entries {
            match entry {
                Entry::Dir(name) => writer
                    .add_directory(*name, SimpleFileOptions::default())
                    .expect("add directory"),
                Entry::File(name, contents, mode) => {
                    writer
                        .start_file(*name, SimpleFileOptions::default().unix_permissions(*mode))
                        .expect("start file");
                    writer.write_all(contents).expect("write file");
                }
                Entry::Symlink(name, target) => writer
                    .add_symlink(*name, *target, SimpleFileOptions::default())
                    .expect("add symlink"),
            }
        }
        writer.finish().expect("finish zip").into_inner()
    }

    fn write_zip(dir: &Path, entries: &[Entry<'_>]) -> PathBuf {
        let path = dir.join("archive.zip");
        fs::write(&path, zip_bytes(entries)).expect("write zip");
        path
    }

    fn extract(entries: &[Entry<'_>]) -> (tempfile::TempDir, Result<(), String>) {
        let work = tempfile::tempdir().expect("tempdir");
        let archive = write_zip(work.path(), entries);
        let dest = work.path().join("out");
        fs::create_dir(&dest).expect("create dest");
        let result = extract_archive(&archive, &dest, MAX_EXTRACTED_BYTES);
        (work, result)
    }

    /// A Linux snapshot holding just the executable.
    fn linux_snapshot() -> Vec<u8> {
        zip_bytes(&[
            Entry::Dir("chrome-linux/"),
            Entry::File("chrome-linux/chrome", b"#!/bin/sh\n", 0o755),
            Entry::File("chrome-linux/resources.pak", b"pak", 0o644),
        ])
    }

    const LINUX_SNAPSHOT_PATH: &str =
        "/chromium-browser-snapshots/Linux_x64/1095492/chrome-linux.zip";

    fn installer(host: &str, root: &Path) -> ChromiumInstaller {
        ChromiumInstaller::new(host, root.to_path_buf(), LINUX, CHROMIUM_REVISION)
    }

    fn leftovers(root: &Path) -> Vec<String> {
        fs::read_dir(root)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.contains(".part"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn download_urls_and_install_paths_match_headless_chrome() {
        let root = Path::new("/data/headless-chrome");
        let linux = ChromiumInstaller::new(SNAPSHOT_HOST, root.into(), LINUX, "1095492");
        assert_eq!(
            linux.download_url(),
            "https://storage.googleapis.com/chromium-browser-snapshots/Linux_x64/1095492/chrome-linux.zip"
        );
        assert_eq!(
            linux.executable_in(&linux.install_dir()),
            Path::new("/data/headless-chrome/linux-1095492/chrome-linux/chrome")
        );

        let mac_arm = ChromiumInstaller::new(SNAPSHOT_HOST, root.into(), MAC_ARM, "1095492");
        assert_eq!(
            mac_arm.download_url(),
            "https://storage.googleapis.com/chromium-browser-snapshots/Mac_Arm/1095492/chrome-mac.zip"
        );
        assert_eq!(
            mac_arm.executable_in(&mac_arm.install_dir()),
            Path::new(
                "/data/headless-chrome/mac_arm-1095492/chrome-mac/Chromium.app/Contents/MacOS/Chromium"
            )
        );

        let mac = ChromiumInstaller::new(SNAPSHOT_HOST, root.into(), MAC, "1095492");
        assert_eq!(
            mac.download_url(),
            "https://storage.googleapis.com/chromium-browser-snapshots/Mac/1095492/chrome-mac.zip"
        );
    }

    #[test]
    fn extracts_files_directories_and_internal_symlinks() {
        let (work, result) = extract(&[
            Entry::Dir("chrome-mac/"),
            Entry::File("chrome-mac/Versions/120/Framework", b"framework", 0o755),
            Entry::Symlink("chrome-mac/Versions/Current", "120"),
            Entry::Symlink("chrome-mac/Framework", "Versions/Current/Framework"),
            Entry::Symlink("chrome-mac/Versions/120/Up", "../../Framework"),
            Entry::File("chrome-mac/notes.txt", b"notes", 0o644),
        ]);
        result.expect("extraction succeeds");
        let out = work.path().join("out/chrome-mac");

        assert_eq!(
            fs::read(out.join("Framework")).expect("read through links"),
            b"framework"
        );
        assert_eq!(
            fs::read_link(out.join("Versions/Current")).expect("read link"),
            Path::new("120")
        );
        assert_eq!(
            fs::read(out.join("Versions/120/Up")).expect("read up link"),
            b"framework"
        );
        assert_eq!(
            fs::read(out.join("notes.txt")).expect("read file"),
            b"notes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn keeps_permission_bits_but_drops_setuid_setgid_and_sticky() {
        use std::os::unix::fs::PermissionsExt;
        let (work, result) = extract(&[
            Entry::File("chrome-linux/chrome", b"bin", 0o4755),
            Entry::File("chrome-linux/helper", b"bin", 0o2750),
            Entry::File("chrome-linux/data.pak", b"pak", 0o1644),
        ]);
        result.expect("extraction succeeds");
        let mode = |name: &str| {
            fs::metadata(work.path().join("out/chrome-linux").join(name))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode("chrome"), 0o755);
        assert_eq!(mode("helper"), 0o750);
        assert_eq!(mode("data.pak"), 0o644);
    }

    #[test]
    fn rejects_an_entry_outside_the_destination() {
        let (work, result) = extract(&[Entry::File("../escaped", b"x", 0o644)]);
        let error = result.expect_err("an escaping entry must be rejected");
        assert!(error.contains("'../escaped'"), "unexpected error: {error}");
        assert!(!work.path().join("escaped").exists());
    }

    #[test]
    fn rejects_symlinks_that_point_outside_the_destination() {
        for target in ["../../outside", "/etc/passwd", "a/../../..", "./x", ""] {
            let (work, result) = extract(&[Entry::Symlink("dir/link", target)]);
            let error = result.expect_err(&format!("target '{target}' must be rejected"));
            assert!(
                error.contains("symlink 'dir/link'"),
                "unexpected error: {error}"
            );
            assert!(
                fs::symlink_metadata(work.path().join("out/dir/link")).is_err(),
                "no link may be created for target '{target}'"
            );
        }
    }

    #[test]
    fn rejects_an_entry_written_through_a_symlink() {
        // `deep/a/l` -> `../..` is the extraction root, which is allowed, but
        // a link placed inside it would be judged as if `l` were a real
        // directory and could climb out of the root.
        let (work, result) = extract(&[
            Entry::Dir("deep/a/"),
            Entry::Symlink("deep/a/l", "../.."),
            Entry::Symlink("deep/a/l/up", "../outside"),
        ]);
        let error = result.expect_err("an entry inside a symlink must be rejected");
        assert!(
            error.contains("'deep/a/l/up' is inside a symlink"),
            "unexpected error: {error}"
        );
        assert!(fs::symlink_metadata(work.path().join("up")).is_err());
    }

    #[test]
    fn rejects_an_archive_that_unpacks_past_the_limit() {
        let work = tempfile::tempdir().expect("tempdir");
        let archive = write_zip(
            work.path(),
            &[
                Entry::File("a", &[0u8; 64], 0o644),
                Entry::File("b", &[0u8; 64], 0o644),
            ],
        );
        let dest = work.path().join("out");
        fs::create_dir(&dest).expect("create dest");
        let error = extract_archive(&archive, &dest, 100).expect_err("limit must apply");
        assert!(
            error.contains("more than 100 bytes"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_a_file_that_is_not_a_zip_archive() {
        let work = tempfile::tempdir().expect("tempdir");
        let archive = work.path().join("archive.zip");
        fs::write(&archive, b"<html>not found</html>").expect("write");
        let error = extract_archive(&archive, work.path(), MAX_EXTRACTED_BYTES)
            .expect_err("garbage must be rejected");
        assert!(
            error.contains("not a valid zip archive"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn downloads_once_and_reuses_the_install() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", LINUX_SNAPSHOT_PATH)
            .with_status(200)
            .with_body(linux_snapshot())
            .expect(1)
            .create();
        let root = tempfile::tempdir().expect("tempdir");
        let installer = installer(&server.url(), root.path());

        let first = installer.ensure_installed().expect("first install");
        assert_eq!(first, root.path().join("linux-1095492/chrome-linux/chrome"));
        assert!(first.is_file());
        let second = installer.ensure_installed().expect("second call");
        assert_eq!(first, second);

        mock.assert();
        assert!(
            leftovers(root.path()).is_empty(),
            "left {:?}",
            leftovers(root.path())
        );
    }

    #[test]
    fn an_existing_install_is_used_without_downloading() {
        let mut server = mockito::Server::new();
        let mock = server.mock("GET", mockito::Matcher::Any).expect(0).create();
        let root = tempfile::tempdir().expect("tempdir");
        let executable = root.path().join("linux-1095492/chrome-linux/chrome");
        fs::create_dir_all(executable.parent().expect("parent")).expect("mkdir");
        fs::write(&executable, b"installed earlier").expect("write");

        let found = installer(&server.url(), root.path())
            .ensure_installed()
            .expect("existing install");
        assert_eq!(found, executable);
        mock.assert();
    }

    #[test]
    fn an_incomplete_install_is_replaced() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", LINUX_SNAPSHOT_PATH)
            .with_body(linux_snapshot())
            .expect(1)
            .create();
        let root = tempfile::tempdir().expect("tempdir");
        let stale = root.path().join("linux-1095492/chrome-linux/stale.pak");
        fs::create_dir_all(stale.parent().expect("parent")).expect("mkdir");
        fs::write(&stale, b"from an interrupted unpack").expect("write");

        let executable = installer(&server.url(), root.path())
            .ensure_installed()
            .expect("reinstall");
        assert!(executable.is_file());
        assert!(!stale.exists(), "the incomplete install must be replaced");
        mock.assert();
    }

    #[test]
    fn an_http_error_names_the_url_and_status_and_installs_nothing() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", LINUX_SNAPSHOT_PATH)
            .with_status(403)
            .create();
        let root = tempfile::tempdir().expect("tempdir");

        let error = installer(&server.url(), root.path())
            .ensure_installed()
            .expect_err("a 403 must fail the install");
        let message = error.to_string();
        assert!(matches!(error, ScreenshotError::ChromeInstall { .. }));
        assert!(
            message.contains(LINUX_SNAPSHOT_PATH),
            "unexpected error: {message}"
        );
        assert!(message.contains("403"), "unexpected error: {message}");
        assert!(message.contains("1095492"), "unexpected error: {message}");
        assert!(!root.path().join("linux-1095492").exists());
        assert!(
            leftovers(root.path()).is_empty(),
            "left {:?}",
            leftovers(root.path())
        );
    }

    #[test]
    fn an_archive_without_the_executable_is_not_installed() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", LINUX_SNAPSHOT_PATH)
            .with_body(zip_bytes(&[Entry::File(
                "chrome-linux/readme",
                b"x",
                0o644,
            )]))
            .create();
        let root = tempfile::tempdir().expect("tempdir");

        let error = installer(&server.url(), root.path())
            .ensure_installed()
            .expect_err("an archive without chrome must fail");
        assert!(
            error
                .to_string()
                .contains("does not contain chrome-linux/chrome"),
            "unexpected error: {error}"
        );
        assert!(!root.path().join("linux-1095492").exists());
        assert!(
            leftovers(root.path()).is_empty(),
            "left {:?}",
            leftovers(root.path())
        );
    }

    #[test]
    fn a_response_over_the_limit_is_refused() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/big.zip")
            .with_body(vec![0u8; 256])
            .create();
        let work = tempfile::tempdir().expect("tempdir");

        let error = download(
            &format!("{}/big.zip", server.url()),
            &work.path().join("big"),
            100,
        )
        .expect_err("the limit must apply");
        assert!(
            error.contains("100 byte limit"),
            "unexpected error: {error}"
        );
    }
}
