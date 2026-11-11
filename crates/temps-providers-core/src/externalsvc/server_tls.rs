// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-side TLS for managed database engines (ADR 093): the per-service
//! mode, the material the control plane hands an engine, and the pieces every
//! engine needs to put that material into its data volume.
//!
//! The control plane owns the CA and the certificates; an engine only receives
//! a [`ServerTlsMaterial`] through its [`super::ServiceConfig`] parameters
//! (under [`SERVER_TLS_MATERIAL_PARAM`]) when it is about to (re)create a
//! container. That parameter is transient: engines never serialize it back
//! into the stored configuration.

use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::warn;
use utoipa::ToSchema;

/// `ServiceConfig.parameters` key carrying the effective [`ServerTlsMode`].
/// Injected by the control plane from the service's TLS record; never stored
/// in the encrypted parameter blob, never accepted from a client.
pub const SERVER_TLS_MODE_PARAM: &str = "server_tls_mode";

/// `ServiceConfig.parameters` key carrying the [`ServerTlsMaterial`] to
/// deliver. Same rules as [`SERVER_TLS_MODE_PARAM`].
pub const SERVER_TLS_MATERIAL_PARAM: &str = "server_tls_material";

/// Docker label recording the fingerprint of the certificate a container was
/// created with, so a renewed certificate is detected as drift.
pub const SERVER_TLS_FINGERPRINT_LABEL: &str = "temps.server_tls_fingerprint";

/// Server-side TLS mode of one managed service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerTlsMode {
    /// The server does not offer TLS. The default for every service created
    /// before managed TLS existed.
    #[default]
    Off,
    /// The server offers TLS with a certificate from the managed-services CA
    /// and still accepts plaintext. Only clients that ask for TLS
    /// (`sslmode=prefer`/`require`/`verify-full`) are encrypted; nothing that
    /// connects today breaks.
    Available,
    /// The server rejects plaintext connections. Not yet supported (ADR 093
    /// Phase 2): it needs every linked app and every internal consumer to use
    /// TLS first.
    Required,
}

impl ServerTlsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Available => "available",
            Self::Required => "required",
        }
    }

    /// Whether the server offers TLS in this mode.
    pub fn offers_tls(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether the server accepts plaintext connections in this mode.
    pub fn accepts_plaintext(self) -> bool {
        !matches!(self, Self::Required)
    }
}

impl std::fmt::Display for ServerTlsMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stored or requested mode that is not one of `off`/`available`/`required`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("'{value}' is not a server TLS mode; expected one of: off, available, required")]
pub struct UnknownServerTlsMode {
    pub value: String,
}

impl FromStr for ServerTlsMode {
    type Err = UnknownServerTlsMode;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "available" => Ok(Self::Available),
            "required" => Ok(Self::Required),
            _ => Err(UnknownServerTlsMode {
                value: value.to_string(),
            }),
        }
    }
}

/// The certificate and key an engine installs for its server. Transient:
/// passed in by the control plane, never persisted by the engine, never
/// logged (`Debug` prints only the fingerprint).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTlsMaterial {
    /// Server certificate followed by nothing else (PEM). The CA is not
    /// appended: clients trust the CA, not a chain the server sends.
    pub cert_pem: String,
    /// Server private key (PEM). SECRET.
    pub key_pem: String,
    /// SHA-256 (lowercase hex) of the certificate's DER encoding.
    pub fingerprint_sha256: String,
}

impl std::fmt::Debug for ServerTlsMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerTlsMaterial")
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .finish_non_exhaustive()
    }
}

/// Numeric owner of the files an engine reads, resolved from the image's own
/// `/etc/passwd` so the key is owned by whatever uid the image runs its
/// server as (999 in the Debian `postgres` images, 1000 in others).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOwner {
    pub uid: u64,
    pub gid: u64,
}

/// Find `user` in the contents of an `/etc/passwd` file.
pub fn passwd_owner(passwd: &str, user: &str) -> Option<FileOwner> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        if name != user {
            return None;
        }
        let _password = fields.next()?;
        let uid = fields.next()?.trim().parse().ok()?;
        let gid = fields.next()?.trim().parse().ok()?;
        Some(FileOwner { uid, gid })
    })
}

/// File names inside the TLS directory.
pub const SERVER_CERT_FILE: &str = "server.crt";
pub const SERVER_KEY_FILE: &str = "server.key";

/// The two directories, next to `dir`, that hold a certificate/key pair.
/// `dir` itself is a symlink to one of them.
pub fn tls_slots(dir: &str) -> [String; 2] {
    [format!("{dir}-a"), format!("{dir}-b")]
}

/// The slot an install writes: never the one `dir` points to now (`current`,
/// the link's target), so the pair a server may still start from is never
/// rewritten in place.
pub fn next_tls_slot(dir: &str, current: Option<&str>) -> String {
    let [a, b] = tls_slots(dir);
    if current == Some(a.as_str()) {
        b
    } else {
        a
    }
}

/// A tar archive holding `slot/` (0700), `slot/server.crt` (0644) and
/// `slot/server.key` (0600), then `dir` as a symlink to `slot`, all owned by
/// `owner`.
///
/// Uploaded through Docker's archive API (which keeps the header ownership
/// and mode) into a never-started container that mounts the service's
/// volume, so no image code runs against the data to fix permissions.
/// PostgreSQL refuses a key that is group- or world-readable unless root owns
/// it, so the mode here is load-bearing.
///
/// The link comes last, so the switch to the new pair is one entry: an
/// upload that fails or is cut off before it leaves `dir` on the previous,
/// complete pair, and the server that a failed replacement restarts finds
/// a matching certificate and key. A `dir` that is still a plain directory
/// (an install before slots) is replaced by the link the same way.
pub fn tls_slot_archive(
    dir: &str,
    slot: &str,
    owner: FileOwner,
    material: &ServerTlsMaterial,
) -> std::io::Result<Vec<u8>> {
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let mut builder = tar::Builder::new(Vec::new());

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_path(format!("{slot}/"))?;
    header.set_mode(0o700);
    header.set_uid(owner.uid);
    header.set_gid(owner.gid);
    header.set_size(0);
    header.set_mtime(mtime);
    header.set_cksum();
    builder.append(&header, std::io::empty())?;

    for (name, mode, body) in [
        (SERVER_CERT_FILE, 0o644, material.cert_pem.as_bytes()),
        (SERVER_KEY_FILE, 0o600, material.key_pem.as_bytes()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_path(format!("{slot}/{name}"))?;
        header.set_mode(mode);
        header.set_uid(owner.uid);
        header.set_gid(owner.gid);
        header.set_size(body.len() as u64);
        header.set_mtime(mtime);
        header.set_cksum();
        builder.append(&header, body)?;
    }

    // Relative, so it resolves wherever the volume is mounted.
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_path(dir)?;
    header.set_link_name(slot)?;
    header.set_mode(0o777);
    header.set_uid(owner.uid);
    header.set_gid(owner.gid);
    header.set_size(0);
    header.set_mtime(mtime);
    header.set_cksum();
    builder.append(&header, std::io::empty())?;
    builder.into_inner()
}

/// Where [`install_tls_directory`] puts the material.
#[derive(Debug, Clone, Copy)]
pub struct TlsInstallTarget<'a> {
    /// Service name, for error messages.
    pub service_name: &'a str,
    /// The service's own (already vetted, already local) image. The helper
    /// container is created from it but never started, so none of its code
    /// runs; it only supplies `/etc/passwd` and a filesystem to mount into.
    pub image: &'a str,
    /// Data volume to write into.
    pub volume: &'a str,
    /// Where the volume is mounted in the engine's container.
    pub mount_target: &'a str,
    /// Directory created under `mount_target` (e.g. `tls`).
    pub dir: &'a str,
    /// The image user that must own the files (e.g. `postgres`).
    pub owner_user: &'a str,
}

/// Why [`install_tls_directory`] failed. Every variant names the service and
/// the volume, so the error is actionable on its own.
#[derive(Debug, thiserror::Error)]
pub enum TlsInstallError {
    #[error("Failed to create the TLS install helper for service '{service}' on volume '{volume}': {reason}")]
    CreateHelper {
        service: String,
        volume: String,
        reason: String,
    },
    #[error("Failed to read /etc/passwd from image '{image}' for service '{service}': {reason}")]
    ReadPasswd {
        service: String,
        image: String,
        reason: String,
    },
    #[error("Image '{image}' of service '{service}' has no '{user}' user to own its TLS key")]
    UnknownOwner {
        service: String,
        image: String,
        user: String,
    },
    #[error(
        "Failed to write the TLS files of service '{service}' into volume '{volume}': {reason}"
    )]
    Write {
        service: String,
        volume: String,
        reason: String,
    },
}

/// Largest `/etc/passwd` read from an image. Real ones are a few KiB.
const MAX_PASSWD_BYTES: usize = 256 * 1024;

/// Write `material` into `target.volume` so that it reads as
/// `<mount_target>/<dir>/server.{crt,key}`, owned by the image's
/// `owner_user`, key mode 0600.
///
/// A helper container that mounts the volume is created and never started:
/// Docker's archive API reads the image's `/etc/passwd` and writes the files
/// through it, and keeps the uid/gid/mode of the uploaded tar headers. The
/// helper is removed on every path, including this future being dropped
/// (a cancelled backup or request): a left-over helper would keep the
/// service's volume referenced forever.
///
/// The pair goes into whichever of [`tls_slots`] `dir` does not point to,
/// then `dir` is switched to it ([`tls_slot_archive`]): the pair in use is
/// never rewritten, so a failed install leaves it complete.
pub async fn install_tls_directory(
    docker: &bollard::Docker,
    target: TlsInstallTarget<'_>,
    material: &ServerTlsMaterial,
) -> Result<(), TlsInstallError> {
    let helper = helper_container_name(target.volume);
    // Armed before the create request: a create dropped mid-flight may
    // still finish in the daemon, under this name only this call uses.
    let mut cleanup = HelperCleanup::new(docker, &helper);
    let created = docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(&helper)
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some(target.image.to_string()),
                network_disabled: Some(true),
                labels: Some(std::collections::HashMap::from([(
                    format!("{}role", temps_core::DOCKER_LABEL_PREFIX),
                    "tls-install-helper".to_string(),
                )])),
                host_config: Some(bollard::models::HostConfig {
                    mounts: Some(vec![bollard::models::Mount {
                        target: Some(target.mount_target.to_string()),
                        source: Some(target.volume.to_string()),
                        typ: Some(bollard::models::MountType::VOLUME),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
    // Only the container this call created is ever removed, by its ID.
    let helper_id = match created {
        Ok(response) => response.id,
        Err(e) => {
            return Err(TlsInstallError::CreateHelper {
                service: target.service_name.to_string(),
                volume: target.volume.to_string(),
                reason: e.to_string(),
            })
        }
    };
    cleanup.created(&helper_id);
    let result = install_through_helper(docker, &helper_id, target, material).await;
    cleanup.remove_now().await;
    result
}

/// Removes the install helper when [`install_tls_directory`] returns or is
/// dropped. On drop the removal runs on its own task, since a dropped
/// future cannot await.
struct HelperCleanup {
    docker: bollard::Docker,
    /// The helper's ID once Docker returned it, its unique name before.
    container: String,
    armed: bool,
}

impl HelperCleanup {
    fn new(docker: &bollard::Docker, name: &str) -> Self {
        Self {
            docker: docker.clone(),
            container: name.to_string(),
            armed: true,
        }
    }

    fn created(&mut self, id: &str) {
        self.container = id.to_string();
    }

    /// Remove the helper now. Disarmed only once the removal returned: if
    /// this future is dropped mid-request, `Drop` still removes it.
    async fn remove_now(&mut self) {
        remove_helper(&self.docker, &self.container).await;
        self.armed = false;
    }
}

impl Drop for HelperCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                container = %self.container,
                "TLS install helper left behind: no runtime to remove it on"
            );
            return;
        };
        let docker = self.docker.clone();
        let container = std::mem::take(&mut self.container);
        runtime.spawn(async move { remove_helper(&docker, &container).await });
    }
}

/// A name no other container has: a fixed name could belong to an
/// operator's container or to a concurrent install.
fn helper_container_name(volume: &str) -> String {
    format!("{volume}-tls-install-{}", uuid::Uuid::new_v4().simple())
}

async fn install_through_helper(
    docker: &bollard::Docker,
    helper: &str,
    target: TlsInstallTarget<'_>,
    material: &ServerTlsMaterial,
) -> Result<(), TlsInstallError> {
    let passwd = read_small_file(docker, helper, "/etc/passwd", MAX_PASSWD_BYTES)
        .await
        .map_err(|reason| TlsInstallError::ReadPasswd {
            service: target.service_name.to_string(),
            image: target.image.to_string(),
            reason,
        })?;
    let owner =
        passwd_owner(&String::from_utf8_lossy(&passwd), target.owner_user).ok_or_else(|| {
            TlsInstallError::UnknownOwner {
                service: target.service_name.to_string(),
                image: target.image.to_string(),
                user: target.owner_user.to_string(),
            }
        })?;
    let write_error = |reason: String| TlsInstallError::Write {
        service: target.service_name.to_string(),
        volume: target.volume.to_string(),
        reason,
    };
    let dir_path = format!("{}/{}", target.mount_target, target.dir);
    let current = read_link_target(docker, helper, &dir_path)
        .await
        .map_err(|reason| write_error(format!("could not read {dir_path}: {reason}")))?;
    let slot = next_tls_slot(target.dir, current.as_deref());
    let archive = tls_slot_archive(target.dir, &slot, owner, material)
        .map_err(|e| write_error(e.to_string()))?;
    docker
        .upload_to_container(
            helper,
            Some(bollard::query_parameters::UploadToContainerOptions {
                path: target.mount_target.to_string(),
                ..Default::default()
            }),
            bollard::body_full(archive.into()),
        )
        .await
        .map_err(|e| write_error(e.to_string()))
}

/// Where the symlink at `path` points, from a (possibly never-started)
/// container: `None` when nothing is there or it is not a symlink (a TLS
/// directory written before slots). Only the archive's first header is
/// read, whatever `path` holds.
async fn read_link_target(
    docker: &bollard::Docker,
    container: &str,
    path: &str,
) -> Result<Option<String>, String> {
    use futures::StreamExt;
    let mut stream = docker.download_from_container(
        container,
        Some(bollard::query_parameters::DownloadFromContainerOptions {
            path: path.to_string(),
        }),
    );
    let mut head = Vec::with_capacity(TAR_HEADER_BYTES);
    while head.len() < TAR_HEADER_BYTES {
        match stream.next().await {
            Some(Ok(chunk)) => head.extend_from_slice(&chunk),
            Some(Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })) => return Ok(None),
            Some(Err(e)) => return Err(e.to_string()),
            None => break,
        }
    }
    drop(stream);
    let mut archive = tar::Archive::new(head.as_slice());
    let mut entries = archive.entries().map_err(|e| e.to_string())?;
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    let entry = entry.map_err(|e| e.to_string())?;
    if !entry.header().entry_type().is_symlink() {
        return Ok(None);
    }
    let target = entry.link_name().map_err(|e| e.to_string())?;
    Ok(target.map(|t| t.to_string_lossy().into_owned()))
}

/// One tar header block: all [`read_link_target`] needs.
const TAR_HEADER_BYTES: usize = 512;

/// Read one small regular file from a (possibly never-started) container.
async fn read_small_file(
    docker: &bollard::Docker,
    container: &str,
    path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    use futures::StreamExt;
    let mut stream = docker.download_from_container(
        container,
        Some(bollard::query_parameters::DownloadFromContainerOptions {
            path: path.to_string(),
        }),
    );
    // The tar wrapper adds headers and padding; allow for them.
    let limit = max_bytes + 64 * 1024;
    let mut archive = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if archive.len() + chunk.len() > limit {
            return Err(format!("{path} is larger than {max_bytes} bytes"));
        }
        archive.extend_from_slice(&chunk);
    }
    let mut reader = tar::Archive::new(archive.as_slice());
    let entries = reader.entries().map_err(|e| e.to_string())?;
    for entry in entries {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if entry.header().entry_type().is_file() {
            let mut body = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut body).map_err(|e| e.to_string())?;
            return Ok(body);
        }
    }
    Err(format!("{path} is not a regular file"))
}

/// Remove the helper this module created, by the ID Docker returned for it.
async fn remove_helper(docker: &bollard::Docker, helper: &str) {
    match docker
        .remove_container(
            helper,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
    {
        // Gone already, or a dropped create that never reached the daemon.
        Ok(())
        | Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {}
        Err(e) => warn!(
            container = helper,
            error = %e,
            "Could not remove the TLS install helper container; remove it by hand"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material() -> ServerTlsMaterial {
        ServerTlsMaterial {
            cert_pem: "-----BEGIN CERTIFICATE-----\ncert\n-----END CERTIFICATE-----\n".into(),
            key_pem: "-----BEGIN PRIVATE KEY-----\nkey\n-----END PRIVATE KEY-----\n".into(),
            fingerprint_sha256: "ab12".into(),
        }
    }

    #[test]
    fn every_install_uses_its_own_helper_name() {
        let first = helper_container_name("postgres-orders_data");
        let second = helper_container_name("postgres-orders_data");
        assert_ne!(first, second);
        // Never the fixed name an operator's container could already have.
        assert_ne!(first, "postgres-orders_data-tls-install");
        assert!(first.starts_with("postgres-orders_data-tls-install-"));
    }

    #[test]
    fn modes_round_trip_through_their_wire_names() {
        for mode in [
            ServerTlsMode::Off,
            ServerTlsMode::Available,
            ServerTlsMode::Required,
        ] {
            assert_eq!(mode.as_str().parse::<ServerTlsMode>().unwrap(), mode);
            assert_eq!(
                serde_json::to_value(mode).unwrap(),
                serde_json::json!(mode.as_str())
            );
        }
        assert_eq!(
            " Available ".parse::<ServerTlsMode>(),
            Ok(ServerTlsMode::Available)
        );
        assert_eq!(ServerTlsMode::default(), ServerTlsMode::Off);
    }

    #[test]
    fn unknown_modes_are_rejected_with_the_value() {
        for value in ["", "on", "prefer", "verify-full", "true"] {
            let err = value.parse::<ServerTlsMode>().unwrap_err();
            assert_eq!(err.value, value);
            assert!(err.to_string().contains("off, available, required"));
        }
    }

    #[test]
    fn only_required_rejects_plaintext_and_only_off_offers_nothing() {
        assert!(!ServerTlsMode::Off.offers_tls());
        assert!(ServerTlsMode::Available.offers_tls());
        assert!(ServerTlsMode::Required.offers_tls());
        assert!(ServerTlsMode::Off.accepts_plaintext());
        assert!(ServerTlsMode::Available.accepts_plaintext());
        assert!(!ServerTlsMode::Required.accepts_plaintext());
    }

    #[test]
    fn material_debug_never_prints_the_key() {
        let debug = format!("{:?}", material());
        assert!(!debug.contains("PRIVATE KEY"), "{debug}");
        assert!(debug.contains("ab12"));
    }

    #[test]
    fn passwd_lookup_finds_the_engine_user_and_nothing_else() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      postgres:x:999:999::/var/lib/postgresql:/bin/bash\n\
                      postgresx:x:1:1::/:/bin/sh\n";
        assert_eq!(
            passwd_owner(passwd, "postgres"),
            Some(FileOwner { uid: 999, gid: 999 })
        );
        assert_eq!(passwd_owner(passwd, "mysql"), None);
        assert_eq!(
            passwd_owner("postgres:x:abc:1::/:/bin/sh", "postgres"),
            None
        );
    }

    #[test]
    fn the_archive_writes_a_slot_then_switches_the_link_to_it() {
        let owner = FileOwner { uid: 999, gid: 998 };
        let bytes = tls_slot_archive("tls", "tls-b", owner, &material()).unwrap();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let mut seen = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let header = entry.header().clone();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            let link = entry
                .link_name()
                .unwrap()
                .map(|l| l.to_string_lossy().to_string());
            let mut body = String::new();
            std::io::Read::read_to_string(&mut entry, &mut body).unwrap();
            assert_eq!(header.uid().unwrap(), 999);
            assert_eq!(header.gid().unwrap(), 998);
            seen.push((path, header.mode().unwrap(), body, link));
        }
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[0].0, "tls-b/");
        assert_eq!(seen[0].1, 0o700);
        assert_eq!(seen[1].0, "tls-b/server.crt");
        assert_eq!(seen[1].1, 0o644);
        assert!(seen[1].2.contains("BEGIN CERTIFICATE"));
        assert_eq!(seen[2].0, "tls-b/server.key");
        assert_eq!(
            seen[2].1, 0o600,
            "PostgreSQL refuses a group/world-readable key"
        );
        assert!(seen[2].2.contains("PRIVATE KEY"));
        // Last, so a cut-off upload never switches to a partial pair.
        assert_eq!(seen[3].0, "tls");
        assert_eq!(seen[3].3.as_deref(), Some("tls-b"), "a relative link");
    }

    #[test]
    fn an_install_never_writes_the_slot_in_use() {
        assert_eq!(next_tls_slot("tls", None), "tls-a");
        assert_eq!(next_tls_slot("tls", Some("tls-a")), "tls-b");
        assert_eq!(next_tls_slot("tls", Some("tls-b")), "tls-a");
        // A link this module did not write is replaced, never followed.
        assert_eq!(next_tls_slot("tls", Some("elsewhere")), "tls-a");
    }

    /// A renewed pair never overwrites the one in use: an upload cut off
    /// mid-key leaves the link on the previous, complete pair; a finished
    /// one switches to the new pair; installs alternate between the two
    /// slots; and a TLS directory written before slots is replaced.
    #[tokio::test]
    async fn a_failed_renewal_leaves_the_previous_pair_in_place() {
        use crate::externalsvc::recreate_guard::test_support::{docker_or_skip, ensure_image};
        let Some(docker) =
            docker_or_skip("a_failed_renewal_leaves_the_previous_pair_in_place").await
        else {
            return;
        };
        ensure_image(&docker, "alpine:3.22").await;
        let run = uuid::Uuid::new_v4().simple().to_string();
        let volume = format!("tls-slot-test-{run}");
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume.clone()),
                ..Default::default()
            })
            .await
            .expect("create test volume");
        let reader_name = format!("tls-slot-test-{run}-reader");
        let reader = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&reader_name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some("alpine:3.22".to_string()),
                    host_config: Some(bollard::models::HostConfig {
                        mounts: Some(vec![bollard::models::Mount {
                            target: Some("/data".to_string()),
                            source: Some(volume.clone()),
                            typ: Some(bollard::models::MountType::VOLUME),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create reader container")
            .id;
        let target = TlsInstallTarget {
            service_name: "slot-test",
            image: "alpine:3.22",
            volume: &volume,
            mount_target: "/data",
            dir: "tls",
            owner_user: "root",
        };
        let pair = |n: u8| ServerTlsMaterial {
            cert_pem: format!("CERT {n}\n"),
            key_pem: format!("KEY {n}\n"),
            fingerprint_sha256: format!("fp{n}"),
        };
        let read = |path: &'static str| {
            let docker = docker.clone();
            let reader = reader.clone();
            async move {
                String::from_utf8(read_small_file(&docker, &reader, path, 4096).await.unwrap())
                    .unwrap()
            }
        };
        let link = || {
            let docker = docker.clone();
            let reader = reader.clone();
            async move {
                read_link_target(&docker, &reader, "/data/tls")
                    .await
                    .unwrap()
            }
        };
        let upload = |archive: Vec<u8>| {
            let docker = docker.clone();
            let reader = reader.clone();
            async move {
                docker
                    .upload_to_container(
                        &reader,
                        Some(bollard::query_parameters::UploadToContainerOptions {
                            path: "/data".to_string(),
                            ..Default::default()
                        }),
                        bollard::body_full(archive.into()),
                    )
                    .await
            }
        };
        let root = FileOwner { uid: 0, gid: 0 };

        // Before slots: a plain directory holding pair 0.
        assert_eq!(link().await, None, "nothing installed yet");
        let mut legacy = tar::Builder::new(Vec::new());
        for (path, body) in [
            ("tls/server.crt", "CERT 0\n"),
            ("tls/server.key", "KEY 0\n"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_mode(0o600);
            header.set_size(body.len() as u64);
            header.set_cksum();
            legacy.append(&header, body.as_bytes()).unwrap();
        }
        upload(legacy.into_inner().unwrap()).await.unwrap();
        assert_eq!(link().await, None, "a directory is not a slot link");

        install_tls_directory(&docker, target, &pair(1))
            .await
            .unwrap();
        assert_eq!(link().await.as_deref(), Some("tls-a"));
        assert_eq!(read("/data/tls/server.crt").await, "CERT 1\n");
        assert_eq!(read("/data/tls/server.key").await, "KEY 1\n");

        // A renewal cut off inside the key: the link still names pair 1.
        let full = tls_slot_archive("tls", "tls-b", root, &pair(2)).unwrap();
        let key_at = full
            .windows(5)
            .position(|w| w == b"KEY 2")
            .expect("the key body is in the archive");
        let cut = upload(full[..key_at + 2].to_vec()).await;
        assert!(cut.is_err(), "a truncated archive is rejected: {cut:?}");
        assert_eq!(link().await.as_deref(), Some("tls-a"));
        assert_eq!(read("/data/tls/server.crt").await, "CERT 1\n");
        assert_eq!(read("/data/tls/server.key").await, "KEY 1\n");

        install_tls_directory(&docker, target, &pair(2))
            .await
            .unwrap();
        assert_eq!(link().await.as_deref(), Some("tls-b"));
        assert_eq!(read("/data/tls/server.crt").await, "CERT 2\n");
        assert_eq!(read("/data/tls/server.key").await, "KEY 2\n");

        install_tls_directory(&docker, target, &pair(3))
            .await
            .unwrap();
        assert_eq!(link().await.as_deref(), Some("tls-a"), "slots alternate");
        assert_eq!(read("/data/tls/server.crt").await, "CERT 3\n");
        assert_eq!(read("/data/tls/server.key").await, "KEY 3\n");

        remove_helper(&docker, &reader).await;
        let _ = docker
            .remove_volume(
                &volume,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;
    }

    /// A removal cut off mid-request leaves the cleanup armed, so its `Drop`
    /// still removes the helper.
    #[tokio::test]
    async fn an_interrupted_removal_stays_armed() {
        // Accepts the connection and never answers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let docker = bollard::Docker::connect_with_http(
            &format!("http://{addr}"),
            30,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let mut cleanup = HelperCleanup::new(&docker, "tls-install-helper-never-answered");
        let interrupted =
            tokio::time::timeout(std::time::Duration::from_millis(200), cleanup.remove_now()).await;
        assert!(interrupted.is_err(), "the removal never got an answer");
        assert!(cleanup.armed, "Drop must still remove the helper");
        cleanup.armed = false;
        hold.abort();
    }

    /// A dropped install (a cancelled backup or request) still removes its
    /// helper, by ID once Docker returned one and by its unique name before,
    /// and never touches another container.
    #[tokio::test]
    async fn a_dropped_install_removes_its_helper() {
        use crate::externalsvc::recreate_guard::test_support::{docker_or_skip, ensure_image};
        let Some(docker) = docker_or_skip("a_dropped_install_removes_its_helper").await else {
            return;
        };
        ensure_image(&docker, "alpine:3.22").await;
        let run = uuid::Uuid::new_v4().simple().to_string();
        let create = |name: String| {
            let docker = docker.clone();
            async move {
                docker
                    .create_container(
                        Some(
                            bollard::query_parameters::CreateContainerOptionsBuilder::new()
                                .name(&name)
                                .build(),
                        ),
                        bollard::models::ContainerCreateBody {
                            image: Some("alpine:3.22".to_string()),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("create test container")
                    .id
            }
        };
        let exists = |name: String| {
            let docker = docker.clone();
            async move {
                docker
                    .inspect_container(
                        &name,
                        None::<bollard::query_parameters::InspectContainerOptions>,
                    )
                    .await
                    .is_ok()
            }
        };
        let gone_soon = |name: String| async move {
            for _ in 0..50 {
                if !exists(name.clone()).await {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            false
        };

        let by_id = format!("tls-cleanup-test-{run}-id");
        let id = create(by_id.clone()).await;
        let mut cleanup = HelperCleanup::new(&docker, &by_id);
        cleanup.created(&id);
        drop(cleanup);
        assert!(
            gone_soon(by_id).await,
            "a created helper is removed on drop"
        );

        // Dropped before Docker answered the create: removed by name.
        let by_name = format!("tls-cleanup-test-{run}-name");
        drop(HelperCleanup::new(&docker, &by_name));
        create(by_name.clone()).await;
        // That drop ran before this container existed; one dropped after
        // a create that did finish removes it.
        drop(HelperCleanup::new(&docker, &by_name));
        assert!(gone_soon(by_name).await, "a helper is removed by its name");

        // A finished install disarms it: nothing else is removed later.
        let bystander = format!("tls-cleanup-test-{run}-bystander");
        create(bystander.clone()).await;
        let mut cleanup = HelperCleanup::new(&docker, &format!("tls-cleanup-test-{run}-absent"));
        cleanup.remove_now().await;
        drop(cleanup);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            exists(bystander.clone()).await,
            "only its own helper is removed"
        );
        remove_helper(&docker, &bystander).await;
    }
}
