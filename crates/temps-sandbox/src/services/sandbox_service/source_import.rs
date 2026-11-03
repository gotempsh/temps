// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Seeding a sandbox from a git repository or tarball: source validation,
//! credential handling and the root-owned import scripts.

use super::*;

const SOURCE_IMPORT_TIMEOUT: Duration = Duration::from_secs(120);
const SOURCE_IMPORT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(130);
const SOURCE_IMPORT_MAX_FILES: usize = 5_000;
const SOURCE_IMPORT_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub(super) const SOURCE_IMPORT_SYSTEM_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub(super) fn source_kind(source: &SandboxSource) -> &'static str {
    match source {
        SandboxSource::Git { .. } => "git",
        SandboxSource::Tarball { .. } => "tarball",
    }
}

/// Validate an optional source destination beneath the sandbox work dir.
///
/// This validation lives in the service layer as well as the HTTP DTO path
/// because project-derived and future internal callers can bypass handlers.
pub(crate) fn validate_source_destination(destination: Option<&str>) -> Result<(), SandboxError> {
    let Some(destination) = destination else {
        return Ok(());
    };
    if destination.is_empty() || destination.len() > 512 {
        return Err(SandboxError::Validation {
            message: "source destination must contain 1 to 512 characters".into(),
        });
    }
    let path = Path::new(destination);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SandboxError::Validation {
            message:
                "source destination must be a relative path without '.', '..', or platform prefixes"
                    .into(),
        });
    }
    Ok(())
}

pub(super) fn bounded_command_error(stderr: &str, stdout: &str) -> String {
    const MAX_CHARS: usize = 1_000;
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        return "command exited without diagnostic output".to_string();
    }
    detail.chars().take(MAX_CHARS).collect()
}

/// Security validation for a seed source *after* it has been resolved.
///
/// The handler validates the caller-supplied `source` DTO, which covers the
/// request-shape rules it owns (mutually exclusive auth fields, and so on).
/// It cannot cover a source this layer derives from a project row, so the
/// two checks that actually matter for safety are repeated here on whatever
/// we finally decided to clone:
///
/// - **SSRF**, including DNS resolution. `validate_external_url` rejects
///   hosts that resolve to private, loopback, link-local or cloud-metadata
///   addresses — the project-side URL guard only checks IP *literals*, so a
///   name like `metadata.google.internal` would otherwise pass.
/// - **Embedded credentials.** A `user:password@` in the URL would be
///   persisted to `sandboxes.source_repo_url` and echoed back out of the
///   API. Rows predating the project-side guard can still carry one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceNetworkPin {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) address: std::net::IpAddr,
}

impl SourceNetworkPin {
    pub(super) fn curl_resolve_value(&self) -> String {
        match self.address {
            std::net::IpAddr::V4(address) => {
                format!("{}:{}:{}", self.host, self.port, address)
            }
            std::net::IpAddr::V6(address) => {
                format!("{}:{}:[{}]", self.host, self.port, address)
            }
        }
    }
}

pub(super) fn tarball_extract_command(
    url: &str,
    staging_dir: &str,
    network_pin: Option<&SourceNetworkPin>,
) -> String {
    let resolve = network_pin
        .map(|pin| {
            format!(
                " --resolve {}",
                shell_escape_service(&pin.curl_resolve_value())
            )
        })
        .unwrap_or_default();
    let network_environment = format!(
        "env -i PATH={} HOME=/var/empty/temps-source-import CURL_HOME=/var/empty/temps-source-import XDG_CONFIG_HOME=/var/empty/temps-source-import NO_PROXY='*'",
        shell_escape_service(SOURCE_IMPORT_SYSTEM_PATH),
    );
    format!(
        "{network_environment} curl -q --noproxy '*' --proxy '' -fsS --max-redirs 0{resolve} {url} | {network_environment} tar --no-same-owner --no-same-permissions -C {stage} -xzf -",
        url = shell_escape_service(url),
        stage = shell_escape_service(staging_dir),
    )
}

pub(super) fn clean_git_environment(auth_dir: &str, askpass_path: Option<&str>) -> String {
    let mut environment = format!(
        "env -i PATH={} HOME={} GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_COUNT=0 GIT_SSL_NO_VERIFY=false GIT_TERMINAL_PROMPT=0",
        shell_escape_service(SOURCE_IMPORT_SYSTEM_PATH),
        shell_escape_service(auth_dir),
    );
    if let Some(askpass_path) = askpass_path {
        environment.push_str(&format!(
            " GIT_ASKPASS={}",
            shell_escape_service(askpass_path)
        ));
    }
    environment
}

pub(super) async fn validate_resolved_source(
    source: &SandboxSource,
) -> Result<Option<SourceNetworkPin>, SandboxError> {
    let (url, kind) = match source {
        SandboxSource::Git { url, .. } => (url.as_str(), "git"),
        SandboxSource::Tarball { url } => (url.as_str(), "tarball"),
    };

    if let SandboxSource::Git {
        depth,
        destination,
        strip_git_metadata,
        ..
    } = source
    {
        if depth.is_some_and(|value| value == 0 || value > 1_000) {
            return Err(SandboxError::Validation {
                message: "git clone depth must be between 1 and 1000".into(),
            });
        }
        validate_source_destination(destination.as_deref())?;
        if *strip_git_metadata && destination.is_none() {
            return Err(SandboxError::Validation {
                message: "strip_git_metadata requires a destination so the sandbox root repository cannot be removed"
                    .into(),
            });
        }
    }

    if url_has_embedded_credentials(url) {
        return Err(SandboxError::Validation {
            message: format!(
                "{kind} source: url must not contain embedded credentials — \
                 use a stored git connection instead"
            ),
        });
    }

    // Sync pass first: scheme + IP-literal blocklist. Cheap, and it avoids a
    // DNS lookup for URLs that are already disqualified.
    let parsed = temps_core::url_validation::validate_external_url(url).map_err(|_| {
        SandboxError::Validation {
            message: format!(
                "{kind} source: scheme must be http or https, and the host must \
                 not be a private, loopback, or metadata address"
            ),
        }
    })?;

    // Then resolve: an IP literal is already covered above, but a *name*
    // pointing at 169.254.169.254 is only caught here.
    if let Some(url::Host::Domain(domain)) = parsed.host() {
        let port = parsed.port_or_known_default().unwrap_or(443);
        let addresses = temps_core::url_validation::resolve_and_validate_domain(domain, port)
            .await
            .map_err(|_| SandboxError::Validation {
                message: format!(
                    "{kind} source: host resolves to a private, loopback, or \
                     metadata address"
                ),
            })?;
        let address = addresses
            .iter()
            .find(|address| address.is_ipv4())
            .or_else(|| addresses.first())
            .map(std::net::SocketAddr::ip)
            .ok_or_else(|| SandboxError::Validation {
                message: format!("{kind} source: host did not resolve to a usable address"),
            })?;
        return Ok(Some(SourceNetworkPin {
            host: domain.to_string(),
            port,
            address,
        }));
    }
    Ok(None)
}

/// Does the URL carry any userinfo before the host?
///
/// Even a bare `user@` may be a token or sensitive account identifier. Clone
/// credentials must travel only through the dedicated ephemeral credential
/// channel, never through argv, process inspection, errors, or `.git/config`.
pub(crate) fn url_has_embedded_credentials(url: &str) -> bool {
    url::Url::parse(url)
        .map(|parsed| !parsed.username().is_empty() || parsed.password().is_some())
        .unwrap_or(false)
}

/// A credential passed to a process inside a custom image is already disclosed
/// to that image: it controls `/bin/sh`, `git`, dynamic-loader configuration,
/// and the network stack. Until credentialed clones move to a host-owned
/// staging helper, allow them only in the versioned images built by Temps.
pub(super) fn sandbox_image_is_trusted_for_credentials(
    handle: &temps_sandbox_providers::SandboxHandle,
) -> bool {
    handle.backend == temps_sandbox_providers::SandboxBackend::Docker
        && handle.image.starts_with("ghcr.io/gotempsh/temps-sandbox-")
}

/// Docker exec inherits the image's configured environment. A custom image
/// could otherwise preload code or ask the shell to source a workspace file
/// before a root-owned import command begins. Explicitly neutralize the
/// interpreter, loader, Git, and language startup hooks relevant to the fixed
/// import toolchain.
pub(super) fn sanitize_privileged_import_environment(environment: &mut HashMap<String, String>) {
    for name in [
        "BASH_ENV",
        "ENV",
        "GIT_EXEC_PATH",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "LD_AUDIT",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "NODE_OPTIONS",
        "PERL5LIB",
        "PYTHONHOME",
        "PYTHONPATH",
        "RUBYLIB",
    ] {
        environment.insert(name.to_string(), String::new());
    }
    for name in [
        "ALL_PROXY",
        "FTP_PROXY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "all_proxy",
        "ftp_proxy",
        "http_proxy",
        "https_proxy",
        "CURL_CA_BUNDLE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_PROXY_COMMAND",
        "GIT_SSL_CAINFO",
        "GIT_SSL_CAPATH",
        "SSL_CERT_DIR",
        "SSL_CERT_FILE",
    ] {
        environment.insert(name.to_string(), String::new());
    }
    environment.insert("NO_PROXY".to_string(), "*".to_string());
    environment.insert("no_proxy".to_string(), "*".to_string());
    environment.insert("GIT_CONFIG_COUNT".to_string(), "0".to_string());
    environment.insert("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string());
    environment.insert("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string());
    environment.insert("GIT_CONFIG_SYSTEM".to_string(), "/dev/null".to_string());
    environment.insert("GIT_SSL_NO_VERIFY".to_string(), "false".to_string());
    // Root-owned import helpers must not inherit image-provided user config.
    // `curl -q` remains the primary defense because a custom image can ship
    // files at any path, while these values also keep other tooling away from
    // the sandbox user's writable home directory.
    for name in ["HOME", "CURL_HOME", "XDG_CONFIG_HOME"] {
        environment.insert(
            name.to_string(),
            "/var/empty/temps-source-import".to_string(),
        );
    }
}

fn default_git_provider_base_url(provider_type: &str) -> Option<&'static str> {
    match provider_type.to_ascii_lowercase().as_str() {
        "github" => Some("https://github.com"),
        "gitlab" => Some("https://gitlab.com"),
        "bitbucket" => Some("https://bitbucket.org"),
        _ => None,
    }
}

pub(super) fn validate_connection_clone_origin(
    clone_url: &str,
    provider: &temps_entities::git_providers::Model,
) -> Result<(), SandboxError> {
    let provider_url = provider
        .base_url
        .as_deref()
        .or_else(|| default_git_provider_base_url(&provider.provider_type))
        .ok_or_else(|| SandboxError::Validation {
            message: format!(
                "Git provider {} has no clone origin configured",
                provider.id
            ),
        })?;
    let clone = url::Url::parse(clone_url).map_err(|error| SandboxError::Validation {
        message: format!("git source: invalid clone URL: {error}"),
    })?;
    let allowed = url::Url::parse(provider_url).map_err(|error| SandboxError::Validation {
        message: format!(
            "Git provider {} has an invalid base URL: {error}",
            provider.id
        ),
    })?;
    let same_origin = clone.scheme() == allowed.scheme()
        && clone
            .host_str()
            .zip(allowed.host_str())
            .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        && clone.port_or_known_default() == allowed.port_or_known_default();
    if !same_origin {
        return Err(SandboxError::Validation {
            message: format!(
                "Git connection {} can only authenticate clones from {}",
                provider.id,
                allowed.origin().ascii_serialization()
            ),
        });
    }
    Ok(())
}

impl SandboxService {
    /// Derive a git seed source from a project's connected repository.
    ///
    /// The point of `project_id` on create: a user who already deploys a
    /// project should be able to say "give me a workspace on this" without
    /// re-typing the clone URL or re-picking the credential. The project
    /// row already knows both, so we read them here.
    ///
    /// Checks out the project's `main_branch` and reuses its
    /// `git_provider_connection_id`, so private repos clone without the
    /// caller ever handling a token. Full history (no shallow depth) —
    /// this is a workspace someone will branch and rebase in, not a
    /// one-shot build.
    ///
    /// Authorization is the caller's responsibility: handlers gate this
    /// with `project_scope_guard!` + `project_access_guard!` before the
    /// request reaches the service.
    pub(super) async fn source_from_project(
        &self,
        project_id: i32,
    ) -> Result<Option<SandboxSource>, SandboxError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or(SandboxError::ProjectNotFound { project_id })?;

        let url = project
            .git_url
            .as_ref()
            .map(|u| u.trim())
            .filter(|u| !u.is_empty());

        let Some(url) = url else {
            // A Git project with no repository is inconsistent state and
            // must still fail closed. Git-less project types, however, are
            // intentionally valid deployment targets: their persistent
            // workspace begins empty and is populated by the agent or a file
            // upload instead of a clone.
            if project.source_type.requires_git_info() {
                return Err(SandboxError::ProjectHasNoRepo {
                    project_id,
                    name: project.name.clone(),
                });
            }
            return Ok(None);
        };

        Ok(Some(SandboxSource::Git {
            url: url.to_string(),
            revision: Some(project.main_branch.clone()),
            depth: None,
            username: None,
            password: None,
            git_connection_id: project.git_provider_connection_id,
            destination: None,
            strip_git_metadata: false,
        }))
    }

    /// Seed a fresh sandbox's `/workspace` with the requested content.
    /// Uses the provider's exec to keep the source-specific commands
    /// (`git`, `curl`, `tar`) out of the service crate.
    ///
    /// For git sources, credentials are injected via `GIT_ASKPASS` + a
    /// per-clone shim script rather than embedded in the URL or argv.
    /// This keeps the token out of `.git/config`, `ps`, and the provider's
    /// exec logs. The shim is shredded immediately after clone.
    pub(crate) async fn seed_source(
        &self,
        internal_id: i32,
        public_id: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<(), SandboxError> {
        // Resolve once immediately before the network operation and force the
        // client to dial that validated address. Validation performed earlier
        // in request handling is useful for fast failure, but cannot prevent a
        // DNS answer from changing before this command actually connects.
        let network_pin = validate_resolved_source(source).await?;
        let handle = self
            .registry
            .get(internal_id, public_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("resolve handle for seed: {}", e),
            })?;

        let work_dir = handle.work_dir.to_string_lossy().to_string();

        match source {
            SandboxSource::Git {
                url,
                revision,
                depth,
                username,
                password,
                git_connection_id,
                destination,
                strip_git_metadata,
            } => {
                let credentialed = git_connection_id.is_some()
                    || (username.as_deref().is_some() && password.as_deref().is_some());
                if credentialed && !sandbox_image_is_trusted_for_credentials(&handle) {
                    return Err(SandboxError::Validation {
                        message: format!(
                            "credentialed Git imports require a Temps-managed sandbox image; sandbox '{}' uses an untrusted custom or local runtime",
                            public_id
                        ),
                    });
                }
                // Resolve credentials. Priority: explicit (username,password)
                // pair > git_connection_id > anonymous. The validator rejects
                // the "both set" combination before we get here.
                let creds = if let Some(conn_id) = git_connection_id {
                    Some(
                        self.resolve_connection_creds(user_id, *conn_id, url)
                            .await?,
                    )
                } else if let (Some(u), Some(p)) = (username.as_deref(), password.as_deref()) {
                    Some((u.to_string(), p.to_string()))
                } else {
                    None
                };

                validate_source_destination(destination.as_deref())?;
                if *strip_git_metadata && destination.is_none() {
                    return Err(SandboxError::Validation {
                        message: "strip_git_metadata requires a destination so the sandbox root repository cannot be removed"
                            .into(),
                    });
                }
                let target_dir = destination
                    .as_deref()
                    .map(|relative| Path::new(&work_dir).join(relative))
                    .unwrap_or_else(|| PathBuf::from(&work_dir));
                let target_dir = target_dir.to_string_lossy().to_string();

                self.run_git_clone(
                    &handle,
                    internal_id,
                    &work_dir,
                    &target_dir,
                    url,
                    revision.as_deref(),
                    *depth,
                    creds,
                    *strip_git_metadata,
                    network_pin.as_ref(),
                )
                .await
            }
            SandboxSource::Tarball { url } => {
                // Stream into a bounded staging directory. The monitor kills
                // both curl and tar if extraction exceeds the deadline/quota,
                // and only a complete import is copied into the workspace.
                let staging_dir = source_import_staging_dir(handle.backend, internal_id);
                let target_guard = source_target_guard_script(&work_dir, &work_dir);
                let staging_prepare = source_import_staging_prepare_script(&staging_dir);
                let extract = tarball_extract_command(url, &staging_dir, network_pin.as_ref());
                let bounded_extract = source_import_command_script(&staging_dir, &extract);
                let bounds = source_import_bounds_script(&staging_dir);
                let aggregate_bounds =
                    source_import_aggregate_bounds_script(&work_dir, &staging_dir);
                let finalize = source_import_staging_finalize_script(&staging_dir);
                let restore_target = source_target_restore_script();
                let script = format!(
                    "set -eu; {target_guard} trap '{restore_target}' EXIT; {staging_prepare} \
                     {bounded_extract}{bounds}{aggregate_bounds}; {finalize}",
                );
                self.exec_seed_script_as_root(&handle, internal_id, script)
                    .await
            }
        }
    }

    /// Resolve a stored git provider connection to an HTTP-Basic
    /// (username, password) pair. Enforces that the connection is owned
    /// by the calling user so one user can't clone repos using another
    /// user's token.
    async fn resolve_connection_creds(
        &self,
        user_id: i32,
        connection_id: i32,
        clone_url: &str,
    ) -> Result<(String, String), SandboxError> {
        let connection = self
            .git_provider_manager
            .get_connection(connection_id)
            .await
            .map_err(|e| SandboxError::Validation {
                message: format!("Git connection {} not available: {}", connection_id, e),
            })?;

        // Ownership check. Connections without a user_id are
        // organization/platform-level and not usable from per-user
        // sandboxes.
        match connection.user_id {
            Some(owner) if owner == user_id => {}
            _ => {
                return Err(SandboxError::Validation {
                    message: format!(
                        "Git connection {} is not owned by the requesting user",
                        connection_id
                    ),
                });
            }
        }

        let provider = self
            .git_provider_manager
            .get_provider(connection.provider_id)
            .await
            .map_err(|error| SandboxError::Validation {
                message: format!(
                    "Git provider {} for connection {} is not available: {}",
                    connection.provider_id, connection_id, error
                ),
            })?;
        if !provider.is_active {
            return Err(SandboxError::Validation {
                message: format!(
                    "Git provider {} for connection {} is inactive",
                    provider.id, connection_id
                ),
            });
        }
        validate_connection_clone_origin(clone_url, &provider)?;

        let token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: format!("connection#{}", connection_id),
                reason: format!("resolve git token: {}", e),
            })?;

        // GitHub/GitLab both accept `x-access-token` as the username for
        // token-based HTTPS auth. The token goes in the password slot and
        // is injected via GIT_ASKPASS (never argv, never URL).
        Ok(("x-access-token".to_string(), token))
    }

    /// Run the actual `git clone` inside the sandbox. Credentials (if any)
    /// are injected via an ephemeral `GIT_ASKPASS` shim and exported env
    /// vars. We never log `env_map` values and never embed the token in
    /// argv or the URL.
    #[allow(clippy::too_many_arguments)]
    async fn run_git_clone(
        &self,
        handle: &temps_sandbox_providers::SandboxHandle,
        internal_id: i32,
        work_dir: &str,
        target_dir: &str,
        url: &str,
        revision: Option<&str>,
        depth: Option<u32>,
        creds: Option<(String, String)>,
        strip_git_metadata: bool,
        network_pin: Option<&SourceNetworkPin>,
    ) -> Result<(), SandboxError> {
        // Destination creation is deliberately performed as the sandbox user.
        // The privileged phase only opens and validates an already-existing
        // directory, so a malicious symlink cannot make root create paths
        // outside the workspace before validation.
        self.ensure_source_target_as_sandbox_user(handle, internal_id, target_dir)
            .await?;
        let private_root = match handle.backend {
            temps_sandbox_providers::SandboxBackend::Local => "/tmp",
            // Docker mounts /run/secrets as tmpfs. Credential material must
            // never enter the writable layer where a concurrent `commit`
            // snapshot could retain it.
            _ => "/run/secrets",
        };
        let auth_dir = format!("{private_root}/.temps-source-auth-{internal_id}");
        let askpass_path = format!("{auth_dir}/askpass.sh");
        let askpass_user_path = format!("{auth_dir}/username");
        let askpass_password_path = format!("{auth_dir}/password");
        let staging_dir = source_import_staging_dir(handle.backend, internal_id);

        // Build the clone command. `-c credential.helper=` disables any
        // host-level credential helper so the password lands only via
        // the askpass shim and is never persisted to `.git/config`.
        // Redirects are disabled for anonymous and authenticated clones. The
        // source host was DNS-validated before this point; following a new
        // location would otherwise bypass that decision.
        let git_network_config = network_pin
            .map(|pin| {
                format!(
                    " -c http.curloptResolve={}",
                    shell_escape_service(&pin.curl_resolve_value())
                )
            })
            .unwrap_or_default();
        let git_environment =
            clean_git_environment(&auth_dir, creds.is_some().then_some(askpass_path.as_str()));
        let mut clone_cmd = format!(
            "{git_environment} git -c credential.helper= -c http.followRedirects=false{git_network_config}"
        );
        clone_cmd.push_str(&format!(
            " clone --depth {} --filter=blob:limit=33554432",
            depth.unwrap_or(1)
        ));
        if let Some(r) = revision {
            if !r.is_empty() {
                // `--branch` accepts branches and tags. For raw commit
                // SHAs this fails, and we fall back to a post-clone
                // `checkout` below.
                clone_cmd.push_str(&format!(" --branch {}", shell_escape_service(r)));
            }
        }
        clone_cmd.push_str(&format!(
            " {} {}",
            shell_escape_service(url),
            shell_escape_service(&staging_dir)
        ));

        // If revision didn't resolve to a branch/tag (e.g. raw SHA),
        // fall back to a post-clone checkout. Harmless when --branch
        // already did the right thing.
        let checkout_cmd = match revision {
            Some(r) if !r.is_empty() => format!(
                " || ({git_environment} git -C {wd} -c credential.helper= -c http.followRedirects=false{git_network_config} fetch origin {rev} && {git_environment} git -C {wd} checkout {rev})",
                wd = shell_escape_service(&staging_dir),
                rev = shell_escape_service(r),
            ),
            _ => String::new(),
        };
        let target_guard = source_target_guard_script(work_dir, target_dir);
        let staging_prepare = source_import_staging_prepare_script(&staging_dir);
        let auth_prepare = source_import_staging_prepare_script(&auth_dir);
        let bounded_clone =
            source_import_command_script(&staging_dir, &format!("{clone_cmd}{checkout_cmd}"));
        let cleanup = git_metadata_cleanup_script(&staging_dir, strip_git_metadata);
        let bounds = source_import_bounds_script(&staging_dir);
        let aggregate_bounds = source_import_aggregate_bounds_script(work_dir, &staging_dir);
        let finalize = source_import_staging_finalize_script(&staging_dir);
        let restore_target = source_target_restore_script();

        // Compose the shell script. When creds are present we write the
        // root-only credential files plus an askpass shim. Git itself runs
        // under `env -i`; the shim reads those files, so secrets enter neither
        // Git's argv nor its inherited environment. We always shred/remove the
        // whole auth directory before returning so a subsequent user
        // shell in the sandbox can't read stale state.
        let script = if creds.is_some() {
            format!(
                "set +x; set -eu; \
                 {target_guard} {staging_prepare} {auth_prepare} \
                 trap 'find {auth_dir} -type f -exec shred -u {{}} \\; 2>/dev/null || true; find {auth_dir} -depth -delete 2>/dev/null || true; {restore_target}' EXIT; \
                 umask 077; printf '%s' \"$GIT_USER\" > {user_file}; printf '%s' \"$GIT_PASS\" > {password_file}; \
                 cat > {ask} <<'TEMPS_ASKPASS_EOF'\n\
#!/bin/sh\n\
case \"$1\" in\n\
  Username*) cat {user_file} ;;\n\
  *)         cat {password_file} ;;\n\
esac\n\
TEMPS_ASKPASS_EOF\n\
                 chmod 700 {ask}; \
                 {bounded_clone}{cleanup}{bounds}{aggregate_bounds}; {finalize}",
                target_guard = target_guard,
                staging_prepare = staging_prepare,
                auth_prepare = auth_prepare,
                ask = askpass_path,
                user_file = shell_escape_service(&askpass_user_path),
                password_file = shell_escape_service(&askpass_password_path),
                auth_dir = shell_escape_service(&auth_dir),
                restore_target = restore_target,
                bounded_clone = bounded_clone,
                cleanup = cleanup,
                bounds = bounds,
                aggregate_bounds = aggregate_bounds,
                finalize = finalize,
            )
        } else {
            format!(
                "set -eu; {target_guard} {staging_prepare} {auth_prepare} \
                 trap 'find {auth_dir} -depth -delete 2>/dev/null || true; {restore_target}' EXIT; \
                 GIT_TERMINAL_PROMPT=0 {bounded_clone}{cleanup}{bounds}{aggregate_bounds}; {finalize}",
                target_guard = target_guard,
                staging_prepare = staging_prepare,
                auth_prepare = auth_prepare,
                auth_dir = shell_escape_service(&auth_dir),
                restore_target = restore_target,
                bounded_clone = bounded_clone,
                cleanup = cleanup,
                bounds = bounds,
                aggregate_bounds = aggregate_bounds,
                finalize = finalize,
            )
        };

        let mut env_map: HashMap<String, String> = HashMap::new();
        if let Some((u, p)) = creds {
            env_map.insert("GIT_USER".into(), u);
            env_map.insert("GIT_PASS".into(), p);
        }
        env_map.insert("HOME".into(), auth_dir);
        env_map.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env_map.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        env_map.insert("GIT_CONFIG_SYSTEM".into(), "/dev/null".into());

        self.exec_seed_script_with_env_as_root(handle, internal_id, script, env_map)
            .await
    }

    async fn ensure_source_target_as_sandbox_user(
        &self,
        handle: &temps_sandbox_providers::SandboxHandle,
        internal_id: i32,
        target_dir: &str,
    ) -> Result<(), SandboxError> {
        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("mkdir -p -- {}", shell_escape_service(target_dir)),
        ];
        let result = self
            .registry
            .provider()
            .exec(
                handle,
                cmd,
                HashMap::from([("PATH".to_string(), SOURCE_IMPORT_SYSTEM_PATH.to_string())]),
                None,
            )
            .await
            .map_err(|error| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("prepare source destination: {error}"),
            })?;
        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "prepare source destination exited with code {}: {}",
                    result.exit_code,
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    async fn exec_seed_script_as_root(
        &self,
        handle: &temps_sandbox_providers::SandboxHandle,
        internal_id: i32,
        script: String,
    ) -> Result<(), SandboxError> {
        self.exec_seed_script_with_env_as_root(handle, internal_id, script, HashMap::new())
            .await
    }

    /// Execute a seed script with an env map. Never logs env values — the
    /// map may contain tokens. On non-zero exit we surface stdout/stderr
    /// but the sandbox layer scrubs them before they reach the user (the
    /// provider's exec impl is expected to honor this).
    async fn exec_seed_script_with_env_as_root(
        &self,
        handle: &temps_sandbox_providers::SandboxHandle,
        internal_id: i32,
        script: String,
        mut env_map: HashMap<String, String>,
    ) -> Result<(), SandboxError> {
        sanitize_privileged_import_environment(&mut env_map);
        env_map.insert("PATH".into(), SOURCE_IMPORT_SYSTEM_PATH.into());
        let cmd = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let execution = self
            .registry
            .provider()
            .exec_as_root(handle, cmd, env_map, None);
        let result = tokio::time::timeout(SOURCE_IMPORT_PROVIDER_TIMEOUT, execution)
            .await
            .map_err(|_| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "source import exceeded the {} second safety limit",
                    SOURCE_IMPORT_PROVIDER_TIMEOUT.as_secs()
                ),
            })?
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("seed source exec: {}", e),
            })?;

        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "seed source exited with code {}: {}",
                    result.exit_code,
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    /// Seed an already-running sandbox with additional content. Mirrors
    /// the SDK's ability to attach a source *after* create; useful when
    /// the caller wants to clone a repo using a token that wasn't
    /// available at create time, or to layer a second repo on top.
    ///
    /// Rejects non-running sandboxes with `InvalidState`. The underlying
    /// `seed_source` applies the same credential-safe flow used on create.
    pub async fn clone_source(
        &self,
        public_id_value: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<sandboxes::Model, SandboxError> {
        let _source_import = self.source_import_lock.lock().await;
        validate_resolved_source(source).await?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "clone source".into(),
            });
        }
        self.seed_source(row.id, &row.public_id, user_id, source)
            .await?;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        self.update_row_if(&row, changes, Expect::Live, None, "clone source")
            .await
    }
}

/// Placeholder "public id" used in error messages when the source-seed
/// step fails before we propagate it upward. We already mapped the
/// real public ID into the top-level Create error; this just gives the
/// inner ExecFailed a non-empty identifier.
fn handle_id_fallback(internal_id: i32) -> String {
    format!("sandbox#{}", internal_id)
}

/// POSIX-style single-quoted escape for embedding into `sh -c` scripts
/// from the service layer. Duplicated from `services::exec::shell_escape`
/// so we don't introduce a module cycle for a 10-line helper.
pub(super) fn shell_escape_service(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

/// Open and lock the destination directory for a source import.
///
/// Publication later uses the retained descriptor at `/dev/fd/9`, not
/// the mutable pathname. The descriptor is validated after it is opened and
/// the directory is made root-only for the duration of the import. A sandbox
/// process can therefore rename or replace the pathname while Git is running,
/// but it cannot redirect the privileged copy or add symlinks beneath the
/// directory being published into.
pub(super) fn source_target_guard_script(work_dir: &str, target_dir: &str) -> String {
    let work_dir = shell_escape_service(work_dir);
    let target_dir = shell_escape_service(target_dir);
    if work_dir == target_dir {
        return format!(
            "test -d {work_dir} && test ! -L {work_dir} || {{ echo 'sandbox work directory must be a real directory' >&2; exit 64; }}; \
             work_real=$(readlink -f -- {work_dir}); exec 9< {work_dir}; target_fd=/dev/fd/9; \
             target_real=$(readlink -f -- \"$target_fd\"); [ \"$target_real\" = \"$work_real\" ] || {{ echo 'source destination changed while opening it' >&2; exit 64; }}; \
             test -z \"$(find \"$target_fd\" -mindepth 1 -print -quit)\" || {{ echo 'source destination must be empty' >&2; exit 66; }}; \
             if [ \"$(id -u)\" -eq 0 ]; then chown root:root \"$target_fd\"; chmod 700 \"$target_fd\"; fi;"
        );
    }
    format!(
        "test -d {work_dir} && test ! -L {work_dir} || {{ echo 'sandbox work directory must be a real directory' >&2; exit 64; }}; \
         test -d {target_dir} || {{ echo 'source destination must already be a directory' >&2; exit 64; }}; \
         work_real=$(readlink -f -- {work_dir}); exec 9< {target_dir}; target_fd=/dev/fd/9; \
         target_real=$(readlink -f -- \"$target_fd\"); \
         case \"$target_real/\" in \"$work_real/\"*) ;; *) echo 'source destination escapes the sandbox work directory' >&2; exit 64 ;; esac; \
         test -z \"$(find \"$target_fd\" -mindepth 1 -print -quit)\" || {{ echo 'source destination must be an empty directory' >&2; exit 66; }}; \
         if [ \"$(id -u)\" -eq 0 ]; then chown root:root \"$target_fd\"; chmod 700 \"$target_fd\"; fi;"
    )
}

fn source_target_restore_script() -> &'static str {
    "if [ \"$(id -u)\" -eq 0 ] && [ -d /dev/fd/9 ] && id -u temps >/dev/null 2>&1; then chown \"$(id -u temps):$(id -g temps)\" /dev/fd/9; chmod 755 /dev/fd/9; fi"
}

pub(super) fn source_import_staging_dir(
    backend: temps_sandbox_providers::SandboxBackend,
    internal_id: i32,
) -> String {
    let private_root = match backend {
        temps_sandbox_providers::SandboxBackend::Local => "/tmp",
        temps_sandbox_providers::SandboxBackend::Docker => "/run/temps-source-import",
        temps_sandbox_providers::SandboxBackend::Firecracker
        | temps_sandbox_providers::SandboxBackend::Microsandbox => "/root",
    };
    format!("{private_root}/.temps-source-import-{internal_id}")
}

pub(super) fn source_import_staging_prepare_script(staging_dir: &str) -> String {
    let staging_dir = shell_escape_service(staging_dir);
    let require_hard_limit = if staging_dir.starts_with("/run/temps-source-import/") {
        "grep -q ' /run/temps-source-import ' /proc/self/mountinfo || { echo 'secure source-import staging is unavailable; rebuild this sandbox before importing' >&2; exit 69; }; "
    } else if staging_dir.starts_with("/run/secrets/") {
        "grep -q ' /run/secrets ' /proc/self/mountinfo || { echo 'secure source-import credential storage is unavailable; rebuild this sandbox before importing' >&2; exit 69; }; "
    } else {
        // Local and Firecracker do not currently expose a provider-enforced,
        // size-bounded staging filesystem. Sampling `du` after extraction is
        // not a quota and can exhaust the host/VM disk between samples, so
        // reject the import instead of pretending the soft monitor is safe.
        "echo 'secure source-import staging is unavailable for this sandbox backend' >&2; exit 69; "
    };
    format!(
        "{require_hard_limit}find {staging_dir} -depth -delete 2>/dev/null || true; mkdir -m 700 -p {staging_dir}; \
         test ! -L {staging_dir} || {{ echo 'source staging directory must not be a symbolic link' >&2; exit 64; }};"
    )
}

pub(super) fn source_import_staging_finalize_script(staging_dir: &str) -> String {
    let staging_dir = shell_escape_service(staging_dir);
    format!(
        "target_fd=/dev/fd/9; test -d \"$target_fd\" || exit 67; \
         cd -P -- \"$target_fd\"; \
         test -z \"$(find . -mindepth 1 -print -quit)\" || {{ echo 'source destination changed during import' >&2; exit 66; }}; \
         cp -a {staging_dir}/. . || {{ find . -mindepth 1 -depth -delete 2>/dev/null || true; exit 67; }}; \
         find {staging_dir} -depth -delete; \
         if id -u temps >/dev/null 2>&1; then \
           chown -R \"$(id -u temps):$(id -g temps)\" . || {{ find . -mindepth 1 -depth -delete 2>/dev/null || true; exit 67; }}; \
           chmod 755 .; \
         fi"
    )
}

/// Execute Git under an in-container deadline while continuously enforcing
/// the import quota. The outer Tokio timeout only drops a future; this inner
/// process is what actually terminates Git and removes a partial checkout.
pub(super) fn source_import_command_script(target_dir: &str, command: &str) -> String {
    let target_dir = shell_escape_service(target_dir);
    let command = shell_escape_service(command);
    format!(
        "set +e; timeout -k 5 {timeout_secs} sh -c {command} & import_pid=$!; limit_exceeded=0; \
         while kill -0 \"$import_pid\" 2>/dev/null; do \
           entry_count=$(find {target_dir} -mindepth 1 -print 2>/dev/null | head -n {entry_probe} | wc -l | tr -d ' '); \
           byte_count=$(du -sk {target_dir} 2>/dev/null | awk '{{print $1 * 1024}}'); byte_count=${{byte_count:-0}}; \
           if [ \"$entry_count\" -gt {max_entries} ] || [ \"$byte_count\" -gt {max_bytes} ]; then \
             limit_exceeded=1; kill -TERM \"$import_pid\" 2>/dev/null || true; break; \
           fi; sleep 1; \
         done; \
         wait \"$import_pid\"; import_status=$?; set -e; \
         if [ \"$limit_exceeded\" -eq 1 ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import exceeds the 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi; \
         if [ \"$import_status\" -ne 0 ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; exit \"$import_status\"; \
         fi",
        timeout_secs = SOURCE_IMPORT_TIMEOUT.as_secs(),
        entry_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_entries = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

pub(super) fn git_metadata_cleanup_script(target_dir: &str, strip_git_metadata: bool) -> String {
    if strip_git_metadata {
        format!(
            "; find {git_dir} -depth -delete",
            git_dir = shell_escape_service(&Path::new(target_dir).join(".git").to_string_lossy())
        )
    } else {
        String::new()
    }
}

pub(super) fn source_import_bounds_script(target_dir: &str) -> String {
    let target_dir = shell_escape_service(target_dir);
    format!(
        "; file_count=$(find {target_dir} -mindepth 1 -print | head -n {file_probe} | wc -l | tr -d ' '); \
         byte_count=$(du -sk {target_dir} | awk '{{print $1 * 1024}}'); \
         if [ \"$file_count\" -gt {max_files} ] || [ \"$byte_count\" -gt {max_bytes} ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import exceeds the 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi",
        file_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_files = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

/// Enforce the quota across the durable workspace plus the staged import.
/// Checking before finalization lets us reject only the new staging tree and
/// preserve files that already belong to the workspace.
pub(super) fn source_import_aggregate_bounds_script(work_dir: &str, staging_dir: &str) -> String {
    let work_dir = shell_escape_service(work_dir);
    let staging_dir = shell_escape_service(staging_dir);
    format!(
        "; existing_count=$(find {work_dir} -mindepth 1 -print | head -n {entry_probe} | wc -l | tr -d ' '); \
         staged_count=$(find {staging_dir} -mindepth 1 -print | head -n {entry_probe} | wc -l | tr -d ' '); \
         existing_bytes=$(du -sk {work_dir} | awk '{{print $1 * 1024}}'); \
         staged_bytes=$(du -sk {staging_dir} | awk '{{print $1 * 1024}}'); \
         if [ $((existing_count + staged_count)) -gt {max_entries} ] || \
            [ $((existing_bytes + staged_bytes)) -gt {max_bytes} ]; then \
           find {staging_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import would exceed the aggregate 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi",
        entry_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_entries = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}
