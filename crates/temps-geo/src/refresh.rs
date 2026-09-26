// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Downloading and hot-swapping the GeoLite2 city database.
//!
//! One module owns every path that can put an `.mmdb` on disk: `temps serve`'s
//! startup recovery of a missing database and the scheduled refresh job both
//! call in here. Before this existed the startup download lived in temps-cli
//! and nothing ever refreshed the file, so an instance served whatever build
//! it first downloaded for the rest of its life -- IPs that had since moved
//! city geolocated to the old city forever.
//!
//! Nothing in this module ever puts a license key into an error, a log field,
//! or the settings row: URLs are never formatted into messages, `reqwest`
//! errors are stripped with [`reqwest::Error::without_url`], and every message
//! that crosses a boundary goes through [`redact_license_key`].

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{debug, info, warn};
use url::Url;

use crate::settings_service::GeoSettingsService;
use crate::{GeoIpError, GeoIpService};

/// What replaces a license key wherever a message might otherwise carry it.
pub const REDACTED: &str = "***REDACTED***";

/// Remove the license key from `message` before it reaches a log line, an
/// error response, or the settings row.
///
/// Every message that originates from the download path is built without the
/// request URL, so this is a second line of defence against a dependency that
/// formats one in anyway.
pub fn redact_license_key(message: impl Into<String>, license_key: Option<&str>) -> String {
    let message = message.into();
    match license_key {
        Some(key) if !key.is_empty() => message.replace(key, REDACTED),
        _ => message,
    }
}

/// Filename of the city database, as shipped by MaxMind and as every Temps
/// lookup path resolves it.
pub const CITY_DB_FILENAME: &str = "GeoLite2-City.mmdb";

/// MaxMind's authenticated download endpoint. Used when the operator has
/// configured a license key; serves a `.tar.gz` holding a dated directory with
/// the `.mmdb` inside.
const MAXMIND_DOWNLOAD_ENDPOINT: &str = "https://download.maxmind.com/app/geoip_download";

/// Edition downloaded from MaxMind. City-level data is what the analytics and
/// proxy paths consume.
const MAXMIND_EDITION_ID: &str = "GeoLite2-City";

/// Fallback source: the copy committed to this repository, downloaded directly
/// as a bare `.mmdb`. Keeps a fresh instance working with zero configuration,
/// at the cost of being only as current as the repository snapshot -- which is
/// exactly why the status endpoint reports which source was used.
///
/// Fetched **only** by the one-time startup recovery in
/// [`ensure_mmdb_present`], never by the scheduled job: see
/// [`run_refresh_cycle`] for why a recurring fetch of a project-controlled
/// host is not an acceptable default for an unlicensed instance.
pub const BUNDLED_GEOLITE2_URL: &str =
    "https://raw.githubusercontent.com/gotempsh/temps/refs/heads/main/crates/temps-cli/GeoLite2-City.mmdb";

/// A real GeoLite2-City database is tens of megabytes. Anything smaller is an
/// error page, a Git LFS pointer, or a truncated transfer.
const MIN_MMDB_BYTES: usize = 1_000_000;

/// Hard ceiling on both the downloaded body and the decompressed archive
/// member. The city database is ~60 MB, so 192 MiB leaves it room to triple
/// while keeping the worst case affordable on the 3 vCPU / 4 GB reference host
/// -- the download is held in memory, validated, and compared against the file
/// on disk, so this figure is the real memory bound of a refresh, not a
/// theoretical one. It also caps a decompression bomb from the archive path.
const MAX_MMDB_BYTES: usize = 192 * 1024 * 1024;

/// Initial capacity of the download buffer.
///
/// Deliberately not derived from `Content-Length`: a declared length is
/// attacker-controlled input, and pre-allocating it would let a single
/// response header commit up to [`MAX_MMDB_BYTES`] of memory before one byte
/// was validated. The buffer grows geometrically from here instead, which
/// costs a handful of reallocations on a ~60 MB body and bounds the damage a
/// lying header can do to nothing at all.
const INITIAL_BODY_CAPACITY: usize = 8 * 1024 * 1024;

/// Total time allowed for one download attempt. Generous because the file is
/// large and operators run on small uplinks, but bounded so a hung connection
/// cannot wedge the refresh job until the next tick.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Ensures the scheduled refresh job is spawned once per process.
///
/// `temps serve` builds two plugin registries in one process (proxy and
/// console API) and each registers its own `GeoPlugin`, so a naive spawn at
/// registration time would run two jobs downloading the same ~60 MB file on
/// the same schedule -- and both racing to write the same path.
static REFRESH_JOB_SPAWNED: AtomicBool = AtomicBool::new(false);

/// Where the city database came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbSource {
    /// MaxMind's official endpoint, using the operator's license key.
    MaxMindOfficial,
    /// The `.mmdb` committed to the Temps repository (no license key needed).
    BundledGithub,
}

impl DbSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MaxMindOfficial => "maxmind_official",
            Self::BundledGithub => "bundled_github",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "maxmind_official" => Some(Self::MaxMindOfficial),
            "bundled_github" => Some(Self::BundledGithub),
            _ => None,
        }
    }
}

impl std::fmt::Display for DbSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Outcome of one refresh attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// New bytes were validated, written and swapped in.
    Updated {
        source: DbSource,
        build_epoch: u64,
        size_bytes: usize,
    },
    /// The download byte-matched what is already on disk, so nothing was
    /// written and nothing was swapped.
    Unchanged { source: DbSource, build_epoch: u64 },
    /// No MaxMind license key is configured, so the scheduled refresh made no
    /// network request at all. Recorded in the settings row (and therefore
    /// surfaced by `/api/geo/status`, `temps doctor` and the settings page) so
    /// "nothing is refreshing" always comes with the reason and the fix.
    SkippedNoLicenseKey,
}

/// Resolved path of the city database for this process.
pub fn city_db_path() -> PathBuf {
    crate::geoip_service::resolve_mmdb_path(CITY_DB_FILENAME)
}

/// The download URL and the source it represents.
///
/// Returns a `Result` rather than the plain tuple because building a `Url` is
/// fallible and this crate does not `unwrap` in production paths; the error
/// names the source only, never the URL being built.
pub fn geo_db_source_url(license_key: Option<&str>) -> Result<(Url, DbSource), GeoIpError> {
    match license_key {
        Some(key) if !key.is_empty() => {
            let mut url = Url::parse(MAXMIND_DOWNLOAD_ENDPOINT).map_err(|e| {
                GeoIpError::InvalidSourceUrl {
                    db_source: DbSource::MaxMindOfficial.as_str(),
                    reason: e.to_string(),
                }
            })?;
            // `query_pairs_mut` percent-encodes the key, so a license key
            // containing URL-significant characters cannot corrupt the query.
            url.query_pairs_mut()
                .append_pair("edition_id", MAXMIND_EDITION_ID)
                .append_pair("license_key", key)
                .append_pair("suffix", "tar.gz");
            Ok((url, DbSource::MaxMindOfficial))
        }
        _ => {
            let url =
                Url::parse(BUNDLED_GEOLITE2_URL).map_err(|e| GeoIpError::InvalidSourceUrl {
                    db_source: DbSource::BundledGithub.as_str(),
                    reason: e.to_string(),
                })?;
            Ok((url, DbSource::BundledGithub))
        }
    }
}

/// User-Agent sent to MaxMind. Versioned, because that request is
/// authenticated with the operator's own license key and MaxMind already knows
/// exactly who is asking -- a version there helps them and us diagnose a
/// rejected download.
const MAXMIND_USER_AGENT: &str = concat!("temps/", env!("CARGO_PKG_VERSION"));

/// User-Agent sent on every other request, including the bundled-URL fetch.
///
/// Unversioned on purpose: that fetch goes to a project-controlled host from
/// an instance we know nothing else about, so `public IP + exact version` is a
/// fingerprint of somebody's self-hosted deployment (including which
/// vulnerabilities it has) that they never opted into publishing.
const DEFAULT_USER_AGENT: &str = "temps";

/// HTTP client for geo database downloads, with an explicit total timeout.
pub fn build_http_client() -> Result<reqwest::Client, GeoIpError> {
    reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(DEFAULT_USER_AGENT)
        .build()
        .map_err(|e| GeoIpError::DownloadFailed {
            db_source: "http_client",
            reason: e.without_url().to_string(),
        })
}

/// Download the newest city database and return its bytes.
///
/// The bytes are validated before they are returned -- size floor plus an
/// actual `maxminddb` parse -- so a caller can never write a corrupt or
/// truncated download over a working database.
pub async fn fetch_latest_mmdb_bytes(
    license_key: Option<&str>,
    http: &reqwest::Client,
) -> Result<(Vec<u8>, DbSource), GeoIpError> {
    let (url, source) = geo_db_source_url(license_key)?;
    let db_source = source.as_str();

    info!(source = db_source, "downloading GeoLite2 city database");

    let mut request = http.get(url);
    if matches!(source, DbSource::MaxMindOfficial) {
        request = request.header(reqwest::header::USER_AGENT, MAXMIND_USER_AGENT);
    }

    let response = request
        .send()
        .await
        .map_err(|e| GeoIpError::DownloadFailed {
            db_source,
            reason: redact_license_key(e.without_url().to_string(), license_key),
        })?;

    let status = response.status();
    if !status.is_success() {
        // Status only: MaxMind echoes the query string (license key included)
        // in some error bodies, so neither the body nor the URL is surfaced.
        return Err(GeoIpError::DownloadFailed {
            db_source,
            reason: format!("HTTP {}", status),
        });
    }

    if let Some(declared) = response.content_length() {
        if declared > MAX_MMDB_BYTES as u64 {
            return Err(GeoIpError::DownloadFailed {
                db_source,
                reason: format!(
                    "declared body of {} bytes exceeds the {} byte limit",
                    declared, MAX_MMDB_BYTES
                ),
            });
        }
    }

    let downloaded = read_body_capped(response, db_source, license_key).await?;

    let bytes = match source {
        DbSource::MaxMindOfficial => extract_mmdb_from_tar_gz_blocking(downloaded).await?,
        DbSource::BundledGithub => downloaded,
    };

    let build_epoch = validate_mmdb_bytes(&bytes, db_source)?;
    info!(
        source = db_source,
        size_bytes = bytes.len(),
        build_epoch,
        "downloaded GeoLite2 city database passed validation"
    );

    Ok((bytes, source))
}

/// Read the response body, aborting past [`MAX_MMDB_BYTES`] instead of letting
/// an oversized body decide how much memory this process allocates.
async fn read_body_capped(
    mut response: reqwest::Response,
    db_source: &'static str,
    license_key: Option<&str>,
) -> Result<Vec<u8>, GeoIpError> {
    // Fixed, small initial capacity -- never `Content-Length`. See
    // `INITIAL_BODY_CAPACITY`.
    let mut buffer: Vec<u8> = Vec::with_capacity(INITIAL_BODY_CAPACITY);

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| GeoIpError::DownloadFailed {
            db_source,
            reason: redact_license_key(e.without_url().to_string(), license_key),
        })?
    {
        if buffer.len() + chunk.len() > MAX_MMDB_BYTES {
            return Err(GeoIpError::DownloadFailed {
                db_source,
                reason: format!("body exceeded the {} byte limit", MAX_MMDB_BYTES),
            });
        }
        buffer.extend_from_slice(&chunk);
    }

    Ok(buffer)
}

/// Decompress on a blocking thread: a ~60 MB gzip member costs hundreds of
/// milliseconds of CPU, which must not stall the async runtime.
async fn extract_mmdb_from_tar_gz_blocking(archive: Vec<u8>) -> Result<Vec<u8>, GeoIpError> {
    tokio::task::spawn_blocking(move || extract_mmdb_from_tar_gz(&archive))
        .await
        .map_err(|e| GeoIpError::ArchiveInvalid {
            db_source: DbSource::MaxMindOfficial.as_str(),
            reason: format!("extraction task failed to run: {}", e),
        })?
}

/// Pull the single `.mmdb` member out of MaxMind's `.tar.gz`.
///
/// The archive is remote input, so extraction is deliberately paranoid even
/// though nothing is unpacked to a caller-visible path: only regular files are
/// considered (which rejects symlinks, hardlinks and devices), every path
/// component must be a plain name (no `..`, no root, no prefix), and only a
/// `.mmdb` filename is accepted. The first match wins and the rest of the
/// archive is ignored.
fn extract_mmdb_from_tar_gz(archive: &[u8]) -> Result<Vec<u8>, GeoIpError> {
    let db_source = DbSource::MaxMindOfficial.as_str();
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);

    let entries = tar.entries().map_err(|e| GeoIpError::ArchiveInvalid {
        db_source,
        reason: format!("archive is not readable as a tar stream: {}", e),
    })?;

    for entry in entries {
        let mut entry = entry.map_err(|e| GeoIpError::ArchiveInvalid {
            db_source,
            reason: format!("failed to read an archive entry: {}", e),
        })?;

        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() {
            debug!(
                entry_type = ?entry_type,
                "skipping non-regular archive entry"
            );
            continue;
        }

        let path = entry
            .path()
            .map_err(|e| GeoIpError::ArchiveInvalid {
                db_source,
                reason: format!("archive entry has an unreadable path: {}", e),
            })?
            .to_path_buf();

        if !is_safe_relative_path(&path) {
            return Err(GeoIpError::ArchiveInvalid {
                db_source,
                reason: format!(
                    "archive entry '{}' is absolute or escapes the archive root",
                    path.display()
                ),
            });
        }

        if !has_mmdb_extension(&path) {
            continue;
        }

        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(MAX_MMDB_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| GeoIpError::ArchiveInvalid {
                db_source,
                reason: format!(
                    "failed to read '{}' from the archive: {}",
                    path.display(),
                    e
                ),
            })?;

        if bytes.len() > MAX_MMDB_BYTES {
            return Err(GeoIpError::ArchiveInvalid {
                db_source,
                reason: format!(
                    "archive member '{}' decompresses past the {} byte limit",
                    path.display(),
                    MAX_MMDB_BYTES
                ),
            });
        }

        return Ok(bytes);
    }

    Err(GeoIpError::ArchiveInvalid {
        db_source,
        reason: "archive contains no '.mmdb' file".to_string(),
    })
}

/// A tar member path is only accepted when every component is a plain name, so
/// `..`, `/etc/passwd` and Windows prefixes are all rejected.
fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn has_mmdb_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mmdb"))
}

/// Size floor plus a real parse. Returns the database's `build_epoch`.
///
/// Parsing borrows `bytes` (no copy) and is what actually rejects a corrupt
/// download: the size check alone passes for a 2 MB HTML error page.
pub fn validate_mmdb_bytes(bytes: &[u8], db_source: &'static str) -> Result<u64, GeoIpError> {
    if bytes.len() < MIN_MMDB_BYTES {
        return Err(GeoIpError::InvalidDatabase {
            db_source,
            size_bytes: bytes.len(),
            reason: format!(
                "smaller than the {} byte minimum for a GeoLite2 city database",
                MIN_MMDB_BYTES
            ),
        });
    }

    let reader =
        maxminddb::Reader::from_source(bytes).map_err(|e| GeoIpError::InvalidDatabase {
            db_source,
            size_bytes: bytes.len(),
            reason: format!("not a readable MaxMind database: {}", e),
        })?;

    Ok(reader.metadata().build_epoch)
}

/// Write `bytes` to `path` via a randomly named temporary file in the same
/// directory and an atomic rename, so a reader never observes a half-written
/// database at `path`.
///
/// The temporary file comes from `tempfile::NamedTempFile::new_in`, which
/// creates it with `O_EXCL` under an unpredictable name -- the same approach
/// `geoip_service::private_mapping` already uses in this crate. A fixed
/// sibling name (`GeoLite2-City.mmdb.tmp`) was both guessable and written with
/// a call that follows symlinks, so anything able to create that one path
/// could redirect ~60 MB of attacker-influenced download bytes into a file of
/// its choosing and have it truncated first.
///
/// `bytes` is an `Arc` rather than a slice because the whole write runs on a
/// blocking thread (a ~60 MB `write` must not sit on the async reactor) and
/// the buffer therefore has to be owned by that thread -- without the `Arc`
/// this would mean a second full copy of the database on a 4 GB host.
pub async fn write_mmdb_atomically(
    path: &Path,
    bytes: std::sync::Arc<Vec<u8>>,
) -> Result<(), GeoIpError> {
    let final_path = path.to_path_buf();
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        // A bare filename: the temp file belongs beside it, in the cwd, so the
        // rename stays within one filesystem.
        _ => PathBuf::from("."),
    };
    let size_bytes = bytes.len();

    tokio::task::spawn_blocking(move || -> Result<(), GeoIpError> {
        use std::io::Write;

        std::fs::create_dir_all(&parent).map_err(|e| GeoIpError::WriteFailed {
            path: parent.display().to_string(),
            reason: format!("failed to create the parent directory: {}", e),
        })?;

        // In the destination directory, so `persist` below is a rename within
        // one filesystem rather than a copy through `std::env::temp_dir()`.
        let mut temp =
            tempfile::NamedTempFile::new_in(&parent).map_err(|e| GeoIpError::WriteFailed {
                path: parent.display().to_string(),
                reason: format!("failed to create a temporary file for the database: {}", e),
            })?;

        // Dropping `temp` on any error below removes the partial file, so a
        // failed refresh cannot leak ~60 MB per attempt.
        temp.write_all(&bytes)
            .map_err(|e| GeoIpError::WriteFailed {
                path: temp.path().display().to_string(),
                reason: format!("failed to write the downloaded database: {}", e),
            })?;
        temp.flush().map_err(|e| GeoIpError::WriteFailed {
            path: temp.path().display().to_string(),
            reason: format!("failed to flush the downloaded database: {}", e),
        })?;

        temp.persist(&final_path)
            .map_err(|e| GeoIpError::WriteFailed {
                path: final_path.display().to_string(),
                reason: format!("failed to move the downloaded database into place: {}", e),
            })?;
        Ok(())
    })
    .await
    .map_err(|e| GeoIpError::WriteFailed {
        path: path.display().to_string(),
        reason: format!("the database write task failed to run: {}", e),
    })??;

    debug!(
        path = %path.display(),
        size_bytes,
        "wrote geo database"
    );
    Ok(())
}

/// Does `path` already hold exactly `bytes`?
///
/// Compared in chunks with an early exit so an unchanged 60 MB database costs a
/// sequential read (almost always from page cache) and no allocation of a
/// second copy.
fn file_has_identical_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    use std::io::BufReader;

    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() != bytes.len() as u64 {
        return Ok(false);
    }

    let mut reader = BufReader::new(file);
    let mut buffer = [0u8; 64 * 1024];
    let mut offset = 0usize;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(offset == bytes.len());
        }
        let end = offset + read;
        if end > bytes.len() || buffer[..read] != bytes[offset..end] {
            return Ok(false);
        }
        offset = end;
    }
}

/// `bytes` is shared rather than copied: cloning it here would put a third
/// full copy of a ~60 MB database in memory (download buffer + this + the page
/// cache) for a comparison that reads it immutably.
async fn file_matches(path: &Path, bytes: std::sync::Arc<Vec<u8>>) -> bool {
    if !path.exists() {
        return false;
    }
    let path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || file_has_identical_bytes(&path, &bytes)).await {
        Ok(Ok(identical)) => identical,
        Ok(Err(e)) => {
            debug!(error = %e, "could not compare the downloaded database with the file on disk; treating it as changed");
            false
        }
        Err(e) => {
            debug!(error = %e, "comparison task failed to run; treating the download as changed");
            false
        }
    }
}

/// Download the database if `path` has none yet.
///
/// This is the startup behaviour `temps serve` has always had -- fetch only
/// when the file is missing -- kept identical and moved here so the startup
/// path and the scheduled job share one implementation.
pub async fn ensure_mmdb_present(
    path: &Path,
    license_key: Option<&str>,
    http: &reqwest::Client,
) -> Result<bool, GeoIpError> {
    if path.exists() {
        debug!(path = %path.display(), "geo database already present; skipping download");
        return Ok(false);
    }

    let (bytes, source) = fetch_latest_mmdb_bytes(license_key, http).await?;
    let size_bytes = bytes.len();
    write_mmdb_atomically(path, std::sync::Arc::new(bytes)).await?;
    info!(
        path = %path.display(),
        source = source.as_str(),
        size_mb = format!("{:.1}", size_bytes as f64 / 1024.0 / 1024.0),
        "downloaded missing geo database"
    );
    Ok(true)
}

/// One refresh attempt: download, validate, compare, then write and swap.
///
/// The loaded database is only ever replaced by bytes that already parsed, so
/// a failed or corrupt download degrades to "keep serving the current data"
/// rather than to "no geo data".
pub async fn attempt_refresh(
    service: &GeoIpService,
    license_key: Option<&str>,
    http: &reqwest::Client,
    path: &Path,
) -> Result<RefreshOutcome, GeoIpError> {
    let (bytes, source) = fetch_latest_mmdb_bytes(license_key, http).await?;
    let build_epoch = validate_mmdb_bytes(&bytes, source.as_str())?;
    // One allocation, shared by the comparison and the write below.
    install_downloaded_database(
        service,
        source,
        build_epoch,
        std::sync::Arc::new(bytes),
        path,
    )
    .await
}

/// Put a validated download in front of lookups: skip, write, or write and
/// reload, decided against what the service currently *serves*.
///
/// The skip is gated on the **loaded** database's `build_epoch`, never on what
/// happens to be on disk. Comparing against disk made a failed reload
/// permanent: the tick that wrote the file and then failed to load it left the
/// new bytes on disk, so every later tick downloading those same bytes saw
/// "disk already matches", reported `Unchanged`, recorded a successful check
/// and never retried the reload -- the process kept serving the old database
/// with nothing anywhere saying so.
///
/// The disk write is still skipped when the file already holds these bytes
/// (that is a redundant ~60 MB write, not a correctness question), but the
/// reload is always retried while the loaded build is behind the download, so a
/// stuck reader self-heals on the next tick.
async fn install_downloaded_database(
    service: &GeoIpService,
    source: DbSource,
    build_epoch: u64,
    bytes: std::sync::Arc<Vec<u8>>,
    path: &Path,
) -> Result<RefreshOutcome, GeoIpError> {
    if service.build_epoch() == Some(build_epoch) {
        debug!(
            source = source.as_str(),
            build_epoch, "the loaded geo database is already this build; skipping write and swap"
        );
        return Ok(RefreshOutcome::Unchanged {
            source,
            build_epoch,
        });
    }

    let size_bytes = bytes.len();
    if file_matches(path, std::sync::Arc::clone(&bytes)).await {
        debug!(
            source = source.as_str(),
            build_epoch,
            path = %path.display(),
            "the geo database on disk already holds this build but the loaded reader does not; \
             retrying the reload without rewriting the file"
        );
    } else {
        write_mmdb_atomically(path, bytes).await?;
    }

    let loaded_epoch = service.refresh_from_path(path)?;

    Ok(RefreshOutcome::Updated {
        source,
        build_epoch: loaded_epoch,
        size_bytes,
    })
}

/// One *scheduled* refresh cycle: [`attempt_refresh`] plus persistence of the
/// result to the settings row.
///
/// Every outcome is recorded so `temps doctor` and the status endpoint can
/// always tell "checked recently and fine" from "last check failed" from "not
/// refreshing, and here is why", which is the only signal a self-hosted
/// operator has that geolocation has gone stale.
///
/// **A scheduled cycle only reaches the network when a MaxMind license key is
/// configured.** Without one, [`geo_db_source_url`] would fall back to
/// [`BUNDLED_GEOLITE2_URL`] on every tick, which would make every unlicensed
/// instance beacon its public IP to a project-controlled host on a fixed
/// cadence forever, and would re-download and parse whatever happens to be on
/// `main` at that moment. Neither is something an operator asked for by
/// installing Temps. The bundled URL therefore stays what it was before the
/// scheduled job existed: a one-time, only-if-missing fallback at startup
/// ([`ensure_mmdb_present`]).
///
/// `license_key` is the plaintext key the caller just decrypted for this one
/// attempt. It is used to build the download URL and to redact any failure
/// message, and is never persisted or logged.
pub async fn run_refresh_cycle(
    service: &GeoIpService,
    settings: &GeoSettingsService,
    license_key: Option<&str>,
    http: &reqwest::Client,
    path: &Path,
) -> Result<RefreshOutcome, GeoIpError> {
    let Some(license_key) = license_key.filter(|key| !key.is_empty()) else {
        info!(
            path = %path.display(),
            "skipping the scheduled GeoLite2 refresh: no MaxMind license key is configured. \
             Add one under Settings -> Metrics Monitoring -> Geolocation database (a free \
             MaxMind account) to keep the database current; lookups keep using the database \
             already on disk until then"
        );
        settings.record_check_skipped_no_license_key().await?;
        return Ok(RefreshOutcome::SkippedNoLicenseKey);
    };
    let license_key = Some(license_key);

    match attempt_refresh(service, license_key, http, path).await {
        Ok(outcome) => {
            let (source, build_epoch, refreshed) = match &outcome {
                RefreshOutcome::Updated {
                    source,
                    build_epoch,
                    ..
                } => (*source, *build_epoch, true),
                RefreshOutcome::Unchanged {
                    source,
                    build_epoch,
                } => (*source, *build_epoch, false),
                // `attempt_refresh` never returns this; the skip is handled
                // above, before any network call.
                RefreshOutcome::SkippedNoLicenseKey => {
                    return Ok(RefreshOutcome::SkippedNoLicenseKey)
                }
            };
            settings
                .record_check_success(source, build_epoch, refreshed)
                .await?;
            Ok(outcome)
        }
        Err(error) => {
            let reason = redact_license_key(error.to_string(), license_key);
            if let Err(persist_error) = settings.record_check_failure(&reason).await {
                warn!(
                    error = %persist_error,
                    "could not persist the failed geo database check to settings"
                );
            }
            Err(error)
        }
    }
}

/// Start the periodic refresh loop, at most once per process.
///
/// The settings are re-read at the start of every tick rather than captured at
/// spawn time, so an admin who changes the license key or the interval through
/// the settings UI gets the new behaviour on the next tick -- no restart. That
/// is also why the loop sleeps for a freshly computed duration instead of
/// holding a fixed `tokio::time::interval`: an interval built once could never
/// pick up a changed cadence.
///
/// A settings read that fails is logged and retried with the default cadence
/// rather than killing the loop -- a transient database blip must not leave
/// the instance permanently without geo refreshes.
///
/// Returns `false` when a loop is already running (the second plugin registry
/// in `temps serve`).
pub fn spawn_refresh_job(
    service: std::sync::Arc<GeoIpService>,
    settings: std::sync::Arc<GeoSettingsService>,
) -> bool {
    if REFRESH_JOB_SPAWNED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        debug!("geo database refresh job is already running in this process");
        return false;
    }

    let http = match build_http_client() {
        Ok(client) => client,
        Err(e) => {
            // Reset so a later registration can retry rather than leaving the
            // process permanently without a refresh loop.
            REFRESH_JOB_SPAWNED.store(false, Ordering::SeqCst);
            warn!(
                error = %e,
                "could not build the HTTP client for geo database refresh; refreshes are disabled \
                 until the server is restarted"
            );
            return false;
        }
    };

    let path = city_db_path();
    info!(
        path = %path.display(),
        "scheduling GeoLite2 city database refresh from the platform settings"
    );

    tokio::spawn(async move {
        loop {
            // Decide the wait from the *current* policy, so shortening the
            // interval takes effect on the very next tick. The database on
            // disk was validated at startup, so the loop always sleeps before
            // its first check rather than downloading immediately on boot.
            let interval = match settings.load().await {
                Ok(geo) => geo.effective_refresh_interval(),
                Err(error) => {
                    warn!(
                        error = %error,
                        default_interval_hours = temps_core::DEFAULT_GEO_REFRESH_INTERVAL_HOURS,
                        "could not read the geolocation settings; waiting the default interval \
                         before retrying"
                    );
                    std::time::Duration::from_secs(
                        u64::from(temps_core::DEFAULT_GEO_REFRESH_INTERVAL_HOURS) * 3600,
                    )
                }
            };
            tokio::time::sleep(interval).await;

            // Re-read after sleeping: the key may have been configured, or
            // replaced, while this tick was waiting.
            let geo = match settings.load().await {
                Ok(geo) => geo,
                Err(error) => {
                    // Nothing useful can be done this tick -- the outcome
                    // could not be recorded either -- so wait and try again.
                    warn!(
                        error = %error,
                        "could not read the geolocation settings; skipping this refresh check"
                    );
                    continue;
                }
            };
            let interval_hours = geo.effective_refresh_interval_hours();
            let license_key = match settings.license_key(&geo) {
                Ok(license_key) => license_key,
                Err(error) => {
                    // A key that cannot be decrypted is an operator problem (a
                    // rotated server encryption key). It is reported and this
                    // tick records itself as skipped -- it does not silently
                    // fall back to the bundled URL, which would turn a
                    // decryption failure into a recurring fetch from a
                    // project-controlled host that the operator never chose.
                    warn!(
                        error = %error,
                        "could not decrypt the stored MaxMind license key; skipping this geo \
                         refresh check. Re-enter the key under Settings -> Metrics Monitoring \
                         to restore automatic refreshes"
                    );
                    None
                }
            };

            match run_refresh_cycle(&service, &settings, license_key.as_deref(), &http, &path).await
            {
                Ok(RefreshOutcome::Updated {
                    source,
                    build_epoch,
                    size_bytes,
                }) => info!(
                    source = source.as_str(),
                    build_epoch,
                    size_bytes,
                    path = %path.display(),
                    "refreshed the GeoLite2 city database"
                ),
                Ok(RefreshOutcome::Unchanged {
                    source,
                    build_epoch,
                }) => debug!(
                    source = source.as_str(),
                    build_epoch, "GeoLite2 city database is already current"
                ),
                // Already logged with the reason and the fix inside
                // `run_refresh_cycle`, and recorded in the settings row.
                Ok(RefreshOutcome::SkippedNoLicenseKey) => {}
                Err(error) => warn!(
                    error = %redact_license_key(error.to_string(), license_key.as_deref()),
                    interval_hours,
                    "GeoLite2 city database refresh failed; keeping the currently loaded database \
                     and retrying on the next tick"
                ),
            }
        }
    });

    true
}

/// How often the on-disk city database is `stat`ed for a change made by
/// somebody else.
///
/// Deliberately a fixed constant rather than a share of the configurable
/// `refresh_interval_hours`: this poll has nothing to do with downloading. Its
/// job is to notice, quickly and cheaply, that the file changed -- by the
/// refresh job in another process, by an operator dropping in a new `.mmdb`, or
/// by a bind-mount update -- so a minute of lag is the right order of magnitude
/// whatever the download cadence is. One `stat` a minute is not measurable.
pub const DB_FILE_WATCH_INTERVAL: Duration = Duration::from_secs(60);

/// Ensures at most one file watcher per process, for the same reason
/// [`REFRESH_JOB_SPAWNED`] exists: the monolithic `temps serve` registers
/// `GeoPlugin` twice (proxy registry and console registry) in one process.
static FILE_WATCHER_SPAWNED: AtomicBool = AtomicBool::new(false);

/// What `stat` says about the database file, which is all the watcher needs to
/// decide whether re-opening it is worth doing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    modified: Option<std::time::SystemTime>,
    len: u64,
}

fn file_fingerprint(path: &Path) -> Option<FileFingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileFingerprint {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// Watch the city database file and hot-reload `service` whenever the file on
/// disk turns out to hold a different build than the one loaded.
///
/// This is what makes a **split-role** deployment (ADR-017) correct. `temps
/// proxy` and `temps serve --role=console` are separate OS processes, each with
/// its own `GeoIpService` and its own `ArcSwap`. Only the console has an
/// `EncryptionService`, so only the console runs [`spawn_refresh_job`] and only
/// the console's reader is swapped when a refresh lands -- the proxy kept
/// serving the database it opened at boot until someone restarted it, silently
/// geolocating analytics and log enrichment from a database that could be
/// arbitrarily old. The watcher needs no license key, no network, no
/// `ConfigService` and no `EncryptionService`, so it runs in every context,
/// including the proxy's deliberately minimal one. In the monolith it is a
/// near-permanent no-op: the file only changes when that same process just
/// refreshed it, which already swapped the shared reader.
///
/// Returns `false` when a watcher is already running in this process, when the
/// service is a mock (nothing on disk to watch), or when the thread cannot be
/// started.
pub fn spawn_db_file_watcher(service: std::sync::Arc<GeoIpService>) -> bool {
    if service.build_epoch().is_none() {
        debug!("mock geo service is enabled; not watching the database file for changes");
        return false;
    }

    if FILE_WATCHER_SPAWNED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        debug!("the geo database file watcher is already running in this process");
        return false;
    }

    let path = city_db_path();
    match spawn_db_file_watcher_every(&service, path.clone(), DB_FILE_WATCH_INTERVAL) {
        Ok(_handle) => {
            info!(
                path = %path.display(),
                poll_interval_secs = DB_FILE_WATCH_INTERVAL.as_secs(),
                "watching the GeoLite2 city database file so a refresh performed by another \
                 process is picked up without a restart"
            );
            true
        }
        Err(error) => {
            FILE_WATCHER_SPAWNED.store(false, Ordering::SeqCst);
            warn!(
                path = %path.display(),
                error = %error,
                "could not start the GeoLite2 database file watcher; this process will keep \
                 serving the database it loaded at startup until it is restarted"
            );
            false
        }
    }
}

/// The watcher runs on a dedicated OS thread, **not** on `tokio::spawn`.
///
/// `temps proxy` drives plugin registration from a throwaway
/// `new_current_thread` runtime that is dropped as soon as registration returns
/// (see `temps_proxy::server::setup_proxy_server`), so anything spawned onto
/// the ambient runtime from `register_services` dies immediately -- in exactly
/// the process this watcher exists for. A thread also suits the work: the poll
/// is a blocking `stat`, and the reload copies and maps ~60 MB, which has no
/// business running on a reactor.
///
/// Holding the service **weakly** gives the thread a termination condition: it
/// stops the first time the `GeoIpService` it would refresh no longer exists.
fn spawn_db_file_watcher_every(
    service: &std::sync::Arc<GeoIpService>,
    path: PathBuf,
    interval: Duration,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let service = std::sync::Arc::downgrade(service);
    std::thread::Builder::new()
        .name("geo-db-watch".to_string())
        .spawn(move || watch_db_file(service, path, interval))
}

fn watch_db_file(service: std::sync::Weak<GeoIpService>, path: PathBuf, interval: Duration) {
    // What the loaded reader corresponds to. Seeded from the current file so a
    // process that just opened it does not immediately re-open it.
    let mut loaded_from = file_fingerprint(&path);
    // The fingerprint a failure was already reported for, so a permanently
    // broken file warns once instead of every poll.
    let mut reported_failure: Option<FileFingerprint> = None;

    loop {
        std::thread::sleep(interval);

        let Some(service) = service.upgrade() else {
            debug!(
                path = %path.display(),
                "the geo service was dropped; stopping the database file watcher"
            );
            return;
        };

        let Some(current) = file_fingerprint(&path) else {
            // Missing or unreadable right now (a refresh is mid-rename, or the
            // file was removed). The loaded database keeps serving lookups and
            // the next poll looks again.
            continue;
        };

        if Some(&current) == loaded_from.as_ref() {
            continue;
        }

        match service.refresh_from_path_if_changed(&path) {
            Ok(Some(build_epoch)) => {
                info!(
                    path = %path.display(),
                    build_epoch,
                    "reloaded the GeoLite2 city database after it changed on disk"
                );
                loaded_from = Some(current);
                reported_failure = None;
            }
            Ok(None) => {
                debug!(
                    path = %path.display(),
                    "the GeoLite2 city database file changed but holds the build already loaded"
                );
                loaded_from = Some(current);
                reported_failure = None;
            }
            Err(error) => {
                // `loaded_from` is deliberately left alone so the next poll
                // retries: a reload that fails must not be remembered as done.
                if reported_failure.as_ref() != Some(&current) {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "the GeoLite2 city database changed on disk but could not be loaded; \
                         keeping the database currently in memory and retrying on the next poll"
                    );
                    reported_failure = Some(current);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build an archive whose member names are written straight into the tar
    /// header, bypassing `Builder::append_data`'s own path validation. A
    /// hostile archive is exactly what the extractor has to defend against, so
    /// the tests have to be able to produce one.
    fn tar_gz_with(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            let name_bytes = name.as_bytes();
            assert!(name_bytes.len() < 100, "test name must fit a tar header");
            if let Some(gnu) = header.as_gnu_mut() {
                gnu.name[..name_bytes.len()].copy_from_slice(name_bytes);
            }
            header.set_cksum();
            builder
                .append(&header, contents.as_slice())
                .expect("append tar entry");
        }
        let tar_bytes = builder.into_inner().expect("finish tar");

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).expect("gzip tar");
        encoder.finish().expect("finish gzip")
    }

    #[test]
    fn source_url_uses_maxmind_when_a_license_key_is_configured() {
        let (url, source) = geo_db_source_url(Some("abc123")).expect("build url");
        assert_eq!(source, DbSource::MaxMindOfficial);
        assert_eq!(url.host_str(), Some("download.maxmind.com"));
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(pairs.contains(&("edition_id".to_string(), "GeoLite2-City".to_string())));
        assert!(pairs.contains(&("license_key".to_string(), "abc123".to_string())));
        assert!(pairs.contains(&("suffix".to_string(), "tar.gz".to_string())));
    }

    #[test]
    fn source_url_percent_encodes_an_awkward_license_key() {
        let (url, _) = geo_db_source_url(Some("a&b=c d")).expect("build url");
        assert!(url.query().unwrap_or_default().contains("a%26b%3Dc+d"));
        let key = url
            .query_pairs()
            .find(|(k, _)| k == "license_key")
            .map(|(_, v)| v.into_owned());
        assert_eq!(key, Some("a&b=c d".to_string()));
    }

    #[test]
    fn source_url_falls_back_to_the_bundled_database() {
        let (url, source) = geo_db_source_url(None).expect("build url");
        assert_eq!(source, DbSource::BundledGithub);
        assert_eq!(url.as_str(), BUNDLED_GEOLITE2_URL);
        assert!(url.query().is_none());

        let (_, source) = geo_db_source_url(Some("")).expect("build url");
        assert_eq!(source, DbSource::BundledGithub);
    }

    #[test]
    fn extraction_returns_the_mmdb_member() {
        let payload = vec![7u8; 4096];
        let archive = tar_gz_with(vec![
            ("GeoLite2-City_20260101/COPYRIGHT.txt", b"(c)".to_vec()),
            ("GeoLite2-City_20260101/GeoLite2-City.mmdb", payload.clone()),
        ]);

        let extracted = extract_mmdb_from_tar_gz(&archive).expect("extract mmdb");
        assert_eq!(extracted, payload);
    }

    #[test]
    fn extraction_rejects_a_traversing_entry() {
        let archive = tar_gz_with(vec![("../../etc/evil.mmdb", vec![1u8; 32])]);
        let error = extract_mmdb_from_tar_gz(&archive).expect_err("must reject traversal");
        assert!(matches!(error, GeoIpError::ArchiveInvalid { .. }));
        assert!(error.to_string().contains("escapes the archive root"));
    }

    #[test]
    fn extraction_rejects_an_absolute_entry() {
        let archive = tar_gz_with(vec![("/tmp/absolute.mmdb", vec![1u8; 32])]);
        let error = extract_mmdb_from_tar_gz(&archive).expect_err("must reject an absolute path");
        assert!(matches!(error, GeoIpError::ArchiveInvalid { .. }));
        assert!(error.to_string().contains("absolute"));
    }

    #[test]
    fn extraction_skips_a_symlink_entry() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(tar::EntryType::Symlink);
        if let Some(gnu) = header.as_gnu_mut() {
            let name = b"GeoLite2-City_20260101/GeoLite2-City.mmdb";
            gnu.name[..name.len()].copy_from_slice(name);
            let target = b"/etc/passwd";
            gnu.linkname[..target.len()].copy_from_slice(target);
        }
        header.set_cksum();
        builder
            .append(&header, std::io::empty())
            .expect("append symlink entry");
        let tar_bytes = builder.into_inner().expect("finish tar");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).expect("gzip tar");
        let archive = encoder.finish().expect("finish gzip");

        // Skipped as a non-regular entry, so the archive looks like it has no
        // database at all rather than yielding the symlink target.
        let error = extract_mmdb_from_tar_gz(&archive).expect_err("a symlink must not be followed");
        assert!(error.to_string().contains("no '.mmdb' file"));
    }

    #[test]
    fn extraction_reports_an_archive_without_a_database() {
        let archive = tar_gz_with(vec![("GeoLite2-City_20260101/README.md", b"hi".to_vec())]);
        let error = extract_mmdb_from_tar_gz(&archive).expect_err("must report a missing mmdb");
        assert!(error.to_string().contains("no '.mmdb' file"));
    }

    #[test]
    fn extraction_rejects_bytes_that_are_not_an_archive() {
        let error = extract_mmdb_from_tar_gz(b"definitely not a gzip stream")
            .expect_err("must reject non-archive input");
        assert!(matches!(error, GeoIpError::ArchiveInvalid { .. }));
    }

    #[test]
    fn safe_path_predicate_rejects_escapes() {
        assert!(is_safe_relative_path(Path::new("dir/GeoLite2-City.mmdb")));
        assert!(!is_safe_relative_path(Path::new("../GeoLite2-City.mmdb")));
        assert!(!is_safe_relative_path(Path::new("/GeoLite2-City.mmdb")));
        assert!(!is_safe_relative_path(Path::new("")));
    }

    #[test]
    fn mmdb_extension_is_matched_case_insensitively() {
        assert!(has_mmdb_extension(Path::new("a/GeoLite2-City.MMDB")));
        assert!(!has_mmdb_extension(Path::new("a/GeoLite2-City.tar.gz")));
        assert!(!has_mmdb_extension(Path::new("a/mmdb")));
    }

    #[test]
    fn validation_rejects_a_too_small_payload() {
        let error = validate_mmdb_bytes(&[0u8; 128], "bundled_github")
            .expect_err("must reject a tiny payload");
        match error {
            GeoIpError::InvalidDatabase { size_bytes, .. } => assert_eq!(size_bytes, 128),
            other => panic!("unexpected error: {}", other),
        }
    }

    #[test]
    fn validation_rejects_a_large_payload_that_is_not_a_database() {
        let error = validate_mmdb_bytes(&vec![0u8; MIN_MMDB_BYTES + 1], "bundled_github")
            .expect_err("must reject non-mmdb bytes");
        assert!(error
            .to_string()
            .contains("not a readable MaxMind database"));
    }

    fn shared(bytes: impl Into<Vec<u8>>) -> std::sync::Arc<Vec<u8>> {
        std::sync::Arc::new(bytes.into())
    }

    #[tokio::test]
    async fn atomic_write_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parent = dir.path().join("nested");
        let path = parent.join(CITY_DB_FILENAME);
        write_mmdb_atomically(&path, shared(b"database bytes".to_vec()))
            .await
            .expect("write database");

        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"database bytes".to_vec()
        );
        // The temp file is randomly named, so the guarantee has to be stated
        // as "nothing else is left in the directory" rather than as one path.
        let leftovers: Vec<_> = std::fs::read_dir(&parent)
            .expect("read the destination directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name != CITY_DB_FILENAME)
            .collect();
        assert!(
            leftovers.is_empty(),
            "unexpected leftovers: {:?}",
            leftovers
        );
    }

    /// The temp file must be unguessable (and created exclusively), so a
    /// predictable sibling path cannot be pre-created as a symlink pointing at
    /// a file the server would then truncate and overwrite.
    #[tokio::test]
    async fn atomic_write_does_not_use_a_predictable_sibling_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CITY_DB_FILENAME);
        let decoy = dir.path().join("decoy-must-not-be-touched");
        std::fs::write(&decoy, b"untouched").expect("seed the decoy");

        // What the old implementation would have written through.
        let predictable = dir.path().join(format!("{}.tmp", CITY_DB_FILENAME));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&decoy, &predictable).expect("plant the symlink");
        #[cfg(not(unix))]
        std::fs::write(&predictable, b"placeholder").expect("plant a placeholder");

        write_mmdb_atomically(&path, shared(b"new database bytes".to_vec()))
            .await
            .expect("write database");

        assert_eq!(
            std::fs::read(&decoy).expect("read the decoy"),
            b"untouched".to_vec(),
            "the write must not follow a planted temp-path symlink"
        );
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"new database bytes".to_vec()
        );
    }

    #[tokio::test]
    async fn atomic_write_replaces_an_existing_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CITY_DB_FILENAME);
        std::fs::write(&path, b"old").expect("seed");

        write_mmdb_atomically(&path, shared(b"new bytes".to_vec()))
            .await
            .expect("overwrite");
        assert_eq!(std::fs::read(&path).expect("read back"), b"new bytes");
    }

    #[tokio::test]
    async fn identical_bytes_are_detected_and_differences_are_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CITY_DB_FILENAME);
        let bytes = vec![3u8; 300 * 1024];
        std::fs::write(&path, &bytes).expect("seed");

        assert!(file_matches(&path, shared(bytes.clone())).await);

        let mut changed = bytes.clone();
        // Last byte, so the comparison has to walk the whole file to notice.
        if let Some(last) = changed.last_mut() {
            *last = 9;
        }
        assert!(!file_matches(&path, shared(changed)).await);
        assert!(!file_matches(&path, shared(bytes[..bytes.len() - 1].to_vec())).await);
        assert!(!file_matches(&dir.path().join("absent.mmdb"), shared(bytes)).await);
    }

    /// The memory bound is part of the feature's contract on a 4 GB host, so
    /// it is asserted rather than left to a comment: the download is held in
    /// memory, validated, and compared against the file on disk.
    #[test]
    fn download_limits_stay_within_the_reference_hosts_budget() {
        assert_eq!(MAX_MMDB_BYTES, 192 * 1024 * 1024);
        // Room for the ~60 MB database to grow several times over, without
        // letting one response commit half a gigabyte on a 4 GB host.
        assert!(MAX_MMDB_BYTES > MIN_MMDB_BYTES.saturating_mul(8));
        // A declared Content-Length must never drive the initial allocation.
        const { assert!(INITIAL_BODY_CAPACITY <= MAX_MMDB_BYTES / 16) };
    }

    #[test]
    fn the_bundled_source_is_fetched_without_a_version_in_the_user_agent() {
        assert_eq!(DEFAULT_USER_AGENT, "temps");
        assert!(!DEFAULT_USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
        assert!(MAXMIND_USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn redaction_strips_the_key_from_a_message() {
        let redacted = redact_license_key(
            "GET https://example.invalid/?license_key=super-secret-key failed",
            Some("super-secret-key"),
        );
        assert!(!redacted.contains("super-secret-key"));
        assert!(redacted.contains(REDACTED));
    }

    #[test]
    fn redaction_is_a_no_op_without_a_key() {
        assert_eq!(redact_license_key("plain message", None), "plain message");
        assert_eq!(
            redact_license_key("plain message", Some("")),
            "plain message"
        );
    }

    /// Build a settings service over a mock row, plus an HTTP client whose DNS
    /// is redirected to a closed local port.
    ///
    /// The redirect is what makes the next test a real assertion: if the
    /// license-key gate ever regresses, the cycle attempts the bundled-URL
    /// fetch, that attempt fails against 127.0.0.1:1, and the test reports an
    /// error instead of quietly making an outbound request from CI.
    fn skip_test_fixture() -> (
        std::sync::Arc<GeoSettingsService>,
        reqwest::Client,
        tempfile::TempDir,
    ) {
        let row = temps_entities::settings::Model {
            id: 1,
            data: temps_core::AppSettings::default().to_json(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let db = std::sync::Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![row.clone()], vec![row.clone()], vec![row]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let server_config = std::sync::Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .expect("valid test server config"),
        );
        let settings = std::sync::Arc::new(GeoSettingsService::new(
            std::sync::Arc::new(temps_config::ConfigService::new(server_config, db)),
            std::sync::Arc::new(
                temps_core::EncryptionService::new(&"d".repeat(64)).expect("encryption service"),
            ),
        ));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .resolve(
                "raw.githubusercontent.com",
                std::net::SocketAddr::from(([127, 0, 0, 1], 1)),
            )
            .build()
            .expect("build the test http client");
        (settings, http, tempfile::tempdir().expect("tempdir"))
    }

    #[tokio::test]
    async fn a_scheduled_cycle_without_a_license_key_makes_no_request() {
        let (settings, http, dir) = skip_test_fixture();
        let service = GeoIpService::Mock(crate::MockGeoIpService::new());
        let path = dir.path().join(CITY_DB_FILENAME);

        let outcome = run_refresh_cycle(&service, &settings, None, &http, &path)
            .await
            .expect("an unlicensed tick is a no-op, not a failure");

        assert_eq!(outcome, RefreshOutcome::SkippedNoLicenseKey);
        assert!(
            !path.exists(),
            "nothing may be downloaded or written without a license key"
        );
    }

    #[tokio::test]
    async fn an_empty_license_key_is_treated_as_no_key_by_the_scheduled_cycle() {
        let (settings, http, dir) = skip_test_fixture();
        let service = GeoIpService::Mock(crate::MockGeoIpService::new());
        let path = dir.path().join(CITY_DB_FILENAME);

        let outcome = run_refresh_cycle(&service, &settings, Some(""), &http, &path)
            .await
            .expect("an empty key must skip rather than fall back to the bundled URL");

        assert_eq!(outcome, RefreshOutcome::SkippedNoLicenseKey);
        assert!(!path.exists());
    }

    /// The city database committed to this repository, read once and shared by
    /// every test that needs a genuine `.mmdb` (it is ~58 MB).
    ///
    /// `None` when the file is not in this checkout, in which case the tests
    /// that need it skip rather than fail -- the same convention the
    /// Docker-dependent tests in this workspace follow.
    fn bundled_city_db() -> Option<std::sync::Arc<Vec<u8>>> {
        static BUNDLED: std::sync::OnceLock<Option<std::sync::Arc<Vec<u8>>>> =
            std::sync::OnceLock::new();
        BUNDLED
            .get_or_init(|| {
                let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("temps-cli")
                    .join(CITY_DB_FILENAME);
                std::fs::read(path).ok().map(std::sync::Arc::new)
            })
            .clone()
    }

    fn build_epoch_of(bytes: &[u8]) -> u64 {
        maxminddb::Reader::from_source(bytes)
            .expect("the fixture must be a readable database")
            .metadata()
            .build_epoch
    }

    /// Re-stamp a database's `build_epoch`, producing a second *build* of the
    /// same data.
    ///
    /// Every test here is about one build replacing another, and MaxMind
    /// publishes one file at a time, so the second build is made by rewriting
    /// the metadata field of the first. The new value is written over the
    /// existing field at its existing length, so no offset in the file moves.
    fn with_build_epoch(bytes: &[u8], epoch: u64) -> Vec<u8> {
        const MARKER: &[u8] = b"\xAB\xCD\xEFMaxMind.com";
        // A utf8 string (type 2) of length 11, i.e. control byte 0b010_01011.
        const KEY: &[u8] = b"\x4Bbuild_epoch";

        let metadata_at = bytes
            .windows(MARKER.len())
            .rposition(|window| window == MARKER)
            .expect("the fixture must carry a metadata marker");
        let key_at = metadata_at
            + bytes[metadata_at..]
                .windows(KEY.len())
                .position(|window| window == KEY)
                .expect("the metadata must carry a build_epoch key");
        let control = key_at + KEY.len();

        // uint64 is an extended type: a control byte whose type bits are zero
        // and whose low five bits are the payload length, then `type - 7` == 2.
        assert_eq!(
            bytes[control] >> 5,
            0,
            "build_epoch must be stored as an extended type"
        );
        assert_eq!(bytes[control + 1], 2, "build_epoch must be a uint64");
        let len = usize::from(bytes[control] & 0x1F);
        let encoded = epoch.to_be_bytes();
        let significant = 8 - len;
        assert!(
            encoded[..significant].iter().all(|byte| *byte == 0),
            "epoch {} does not fit the existing {}-byte field",
            epoch,
            len
        );

        let mut patched = bytes.to_vec();
        patched[control + 2..control + 2 + len].copy_from_slice(&encoded[significant..]);
        assert_eq!(build_epoch_of(&patched), epoch);
        patched
    }

    /// ADR-017 split roles: `temps proxy` and `temps serve --role=console` are
    /// separate OS processes holding separate readers over one file, and only
    /// the console ever refreshes it. Without the watcher the proxy process
    /// serves the build it opened at boot until someone restarts it.
    #[tokio::test]
    async fn the_file_watcher_reloads_a_database_another_process_replaced() {
        let Some(base) = bundled_city_db() else {
            println!("{} is not in this checkout; skipping", CITY_DB_FILENAME);
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CITY_DB_FILENAME);
        std::fs::write(&path, base.as_slice()).expect("seed the database");
        let initial_epoch = build_epoch_of(&base);
        let newer_epoch = initial_epoch + 86_400;

        // Two independent services over one path, as the two processes have.
        let console = GeoIpService::from_city_db_path(&path).expect("console reader");
        let proxy =
            std::sync::Arc::new(GeoIpService::from_city_db_path(&path).expect("proxy reader"));
        assert_eq!(proxy.build_epoch(), Some(initial_epoch));

        // Short poll so the test does not wait a real minute.
        let _watcher = spawn_db_file_watcher_every(&proxy, path.clone(), Duration::from_millis(25))
            .expect("start the watcher");

        // The console process refreshes: the file is replaced and *its* reader
        // is swapped. Nothing tells the proxy's reader anything.
        write_mmdb_atomically(&path, shared(with_build_epoch(&base, newer_epoch)))
            .await
            .expect("write the refreshed database");
        assert_eq!(
            console
                .refresh_from_path(&path)
                .expect("the console reloads its own reader"),
            newer_epoch
        );

        let picked_up = tokio::time::timeout(Duration::from_secs(20), async {
            while proxy.build_epoch() != Some(newer_epoch) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        assert!(
            picked_up.is_ok(),
            "the proxy process kept serving build {} after the console refreshed the file to {}",
            initial_epoch,
            newer_epoch
        );

        // Dropping the last strong reference is the watcher thread's stop
        // signal, so the test leaves nothing polling behind it.
        drop(proxy);
    }

    /// The poll exists to notice an out-of-process write quickly; it must stay
    /// far below any download cadence rather than track it.
    #[test]
    fn the_watch_interval_is_far_shorter_than_any_refresh_cadence() {
        assert_eq!(DB_FILE_WATCH_INTERVAL, Duration::from_secs(60));
        assert!(
            DB_FILE_WATCH_INTERVAL
                < Duration::from_secs(
                    u64::from(temps_core::DEFAULT_GEO_REFRESH_INTERVAL_HOURS) * 3600
                )
        );
    }

    /// Tick N wrote the new build to disk and then failed to load it. Tick N+1
    /// downloads the identical bytes: deciding "unchanged" from *disk* reported
    /// a successful no-op check forever and never retried the reload, so the
    /// process served the old build with nothing anywhere saying so.
    #[tokio::test]
    async fn a_failed_reload_is_retried_when_the_next_download_is_identical() {
        let Some(base) = bundled_city_db() else {
            println!("{} is not in this checkout; skipping", CITY_DB_FILENAME);
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CITY_DB_FILENAME);
        std::fs::write(&path, base.as_slice()).expect("seed the database");

        let service = GeoIpService::from_city_db_path(&path).expect("reader");
        let loaded_epoch = build_epoch_of(&base);
        let downloaded_epoch = loaded_epoch + 86_400;

        // Tick N: the download reached disk, the swap never reached memory.
        let newer = with_build_epoch(&base, downloaded_epoch);
        std::fs::write(&path, &newer).expect("simulate the write of a tick whose reload failed");
        assert_eq!(service.build_epoch(), Some(loaded_epoch));
        let before = file_fingerprint(&path);

        // Tick N+1: MaxMind has not republished, so these are the same bytes
        // the file already holds.
        let outcome = install_downloaded_database(
            &service,
            DbSource::MaxMindOfficial,
            downloaded_epoch,
            shared(newer.clone()),
            &path,
        )
        .await
        .expect("the reload must be retried");

        assert_eq!(
            outcome,
            RefreshOutcome::Updated {
                source: DbSource::MaxMindOfficial,
                build_epoch: downloaded_epoch,
                size_bytes: newer.len(),
            },
            "an identical download must retry the reload, not report Unchanged"
        );
        assert_eq!(
            service.build_epoch(),
            Some(downloaded_epoch),
            "the loaded reader must have caught up with the file"
        );
        assert_eq!(
            file_fingerprint(&path),
            before,
            "the file already held these bytes, so it must not be rewritten"
        );
    }

    /// The other half of the same decision: once the *loaded* build matches the
    /// download there is genuinely nothing to do.
    #[tokio::test]
    async fn a_download_matching_the_loaded_build_is_reported_unchanged() {
        let Some(base) = bundled_city_db() else {
            println!("{} is not in this checkout; skipping", CITY_DB_FILENAME);
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CITY_DB_FILENAME);
        std::fs::write(&path, base.as_slice()).expect("seed the database");

        let service = GeoIpService::from_city_db_path(&path).expect("reader");
        let build_epoch = build_epoch_of(&base);

        let outcome = install_downloaded_database(
            &service,
            DbSource::MaxMindOfficial,
            build_epoch,
            shared(base.as_slice().to_vec()),
            &path,
        )
        .await
        .expect("an already-loaded build is a no-op, not a failure");

        assert_eq!(
            outcome,
            RefreshOutcome::Unchanged {
                source: DbSource::MaxMindOfficial,
                build_epoch,
            }
        );
    }

    #[test]
    fn db_source_round_trips_through_its_wire_value() {
        for source in [DbSource::MaxMindOfficial, DbSource::BundledGithub] {
            assert_eq!(DbSource::parse(source.as_str()), Some(source));
        }
        assert_eq!(DbSource::parse("something_else"), None);
    }
}
