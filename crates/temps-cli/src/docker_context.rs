// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Honour the active `docker context` the way the `docker` CLI does.
//!
//! Every Docker client in the process is a bollard client, and bollard only
//! looks at `DOCKER_HOST` and then the default `/var/run/docker.sock`. On
//! Docker Desktop alternatives (Colima, OrbStack, Rancher Desktop, rootless
//! Docker) the daemon lives elsewhere and is selected with `docker context
//! use`, so `docker ps` works while Temps reports "Socket not found:
//! /var/run/docker.sock" and the console never starts.
//!
//! At startup, before any thread exists, [`adopt_active_docker_context`]
//! resolves the active context and exports its endpoint as `DOCKER_HOST`
//! for this process. It is deliberately narrow: an explicit `DOCKER_HOST`
//! always wins, the `default` context is left alone, and only a local
//! `unix://` endpoint whose socket exists is adopted. A remote context
//! (`ssh://`, `tcp://` with TLS material) is never applied, because bollard
//! cannot use it and a working default socket must not be replaced by an
//! endpoint that fails.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A Docker context that was exported as `DOCKER_HOST` for this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedDockerContext {
    pub name: String,
    pub host: String,
}

/// Why the active context was not adopted. Only reported at debug level:
/// every variant is a normal configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerContextSkip {
    DockerHostSet,
    NoActiveContext,
    DefaultContext,
    MetadataUnreadable { name: String, path: PathBuf },
    NotLocalUnixSocket { name: String, host: String },
    SocketMissing { name: String, socket: PathBuf },
}

/// Inputs to the resolution, separated from the process environment so the
/// rules can be tested without mutating it.
struct ContextEnv<'a> {
    docker_host: Option<&'a str>,
    docker_context: Option<&'a str>,
    config_dir: Option<&'a Path>,
}

#[derive(serde::Deserialize)]
struct DockerConfigFile {
    #[serde(rename = "currentContext", default)]
    current_context: Option<String>,
}

#[derive(serde::Deserialize)]
struct ContextMeta {
    #[serde(rename = "Endpoints", default)]
    endpoints: ContextEndpoints,
}

#[derive(serde::Deserialize, Default)]
struct ContextEndpoints {
    #[serde(default)]
    docker: Option<ContextDockerEndpoint>,
}

#[derive(serde::Deserialize)]
struct ContextDockerEndpoint {
    #[serde(rename = "Host", default)]
    host: Option<String>,
}

/// Resolve the active Docker context and export it as `DOCKER_HOST`.
///
/// Must be called before the process starts any thread (it mutates the
/// environment), which is why the CLI calls it first thing in `run`.
pub fn adopt_active_docker_context() -> Result<AdoptedDockerContext, DockerContextSkip> {
    let docker_host = std::env::var("DOCKER_HOST").ok();
    let docker_context = std::env::var("DOCKER_CONTEXT").ok();
    let config_dir = docker_config_dir();

    let adopted = resolve(&ContextEnv {
        docker_host: docker_host.as_deref(),
        docker_context: docker_context.as_deref(),
        config_dir: config_dir.as_deref(),
    })?;
    std::env::set_var("DOCKER_HOST", &adopted.host);
    Ok(adopted)
}

/// `$DOCKER_CONFIG`, else `~/.docker`, matching the Docker CLI.
fn docker_config_dir() -> Option<PathBuf> {
    match std::env::var_os("DOCKER_CONFIG") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::home_dir().map(|home| home.join(".docker")),
    }
}

fn resolve(env: &ContextEnv<'_>) -> Result<AdoptedDockerContext, DockerContextSkip> {
    if env.docker_host.is_some_and(|host| !host.trim().is_empty()) {
        return Err(DockerContextSkip::DockerHostSet);
    }
    let config_dir = env.config_dir.ok_or(DockerContextSkip::NoActiveContext)?;

    let name = match env.docker_context.map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => current_context_from_config(config_dir).ok_or(DockerContextSkip::NoActiveContext)?,
    };
    if name == "default" {
        return Err(DockerContextSkip::DefaultContext);
    }

    let meta_path = context_meta_path(config_dir, &name);
    let host = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<ContextMeta>(&raw).ok())
        .and_then(|meta| meta.endpoints.docker)
        .and_then(|endpoint| endpoint.host)
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
        .ok_or_else(|| DockerContextSkip::MetadataUnreadable {
            name: name.clone(),
            path: meta_path.clone(),
        })?;

    let Some(socket) = host.strip_prefix("unix://").map(PathBuf::from) else {
        return Err(DockerContextSkip::NotLocalUnixSocket { name, host });
    };
    if !socket.exists() {
        return Err(DockerContextSkip::SocketMissing { name, socket });
    }

    Ok(AdoptedDockerContext { name, host })
}

fn current_context_from_config(config_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(config_dir.join("config.json")).ok()?;
    let config: DockerConfigFile = serde_json::from_str(&raw).ok()?;
    config
        .current_context
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// The Docker CLI stores each context under the hex SHA-256 of its name.
fn context_meta_path(config_dir: &Path, name: &str) -> PathBuf {
    let digest = Sha256::digest(name.as_bytes());
    let dir: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    config_dir
        .join("contexts")
        .join("meta")
        .join(dir)
        .join("meta.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().expect("create temp docker config dir"),
            }
        }

        fn config_dir(&self) -> &Path {
            self.dir.path()
        }

        fn set_current_context(&self, name: &str) {
            std::fs::write(
                self.config_dir().join("config.json"),
                format!(r#"{{"auths":{{}},"currentContext":"{name}"}}"#),
            )
            .expect("write config.json");
        }

        fn add_context(&self, name: &str, host: &str) {
            let path = context_meta_path(self.config_dir(), name);
            std::fs::create_dir_all(path.parent().expect("meta dir")).expect("create meta dir");
            std::fs::write(
                path,
                format!(
                    r#"{{"Name":"{name}","Metadata":{{}},"Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":false}}}}}}"#
                ),
            )
            .expect("write meta.json");
        }

        /// A file standing in for a daemon socket; only existence is checked.
        fn socket(&self, file: &str) -> PathBuf {
            let path = self.config_dir().join(file);
            std::fs::write(&path, b"").expect("create fake socket");
            path
        }

        fn resolve(
            &self,
            docker_host: Option<&str>,
            docker_context: Option<&str>,
        ) -> Result<AdoptedDockerContext, DockerContextSkip> {
            resolve(&ContextEnv {
                docker_host,
                docker_context,
                config_dir: Some(self.config_dir()),
            })
        }
    }

    #[test]
    fn meta_path_uses_sha256_of_the_context_name() {
        // Known digest of "colima", as written by `docker context create`.
        let path = context_meta_path(Path::new("/cfg"), "colima");
        assert!(path.starts_with("/cfg/contexts/meta"));
        let dir = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assert_eq!(
            dir,
            "f24fd3749c1368328e2b149bec149cb6795619f244c5b584e844961215dadd16"
        );
    }

    #[test]
    fn adopts_the_current_context_socket() {
        let fx = Fixture::new();
        let socket = fx.socket("docker.sock");
        let host = format!("unix://{}", socket.display());
        fx.add_context("colima", &host);
        fx.set_current_context("colima");

        assert_eq!(
            fx.resolve(None, None),
            Ok(AdoptedDockerContext {
                name: "colima".into(),
                host
            })
        );
    }

    #[test]
    fn docker_context_env_overrides_the_config_file() {
        let fx = Fixture::new();
        let socket = fx.socket("orb.sock");
        let host = format!("unix://{}", socket.display());
        fx.add_context("orbstack", &host);
        fx.set_current_context("default");

        assert_eq!(
            fx.resolve(None, Some("orbstack")).map(|c| c.name),
            Ok("orbstack".to_string())
        );
    }

    #[test]
    fn explicit_docker_host_always_wins() {
        let fx = Fixture::new();
        fx.set_current_context("colima");
        assert_eq!(
            fx.resolve(Some("unix:///var/run/docker.sock"), None),
            Err(DockerContextSkip::DockerHostSet)
        );
        // An empty DOCKER_HOST is treated as unset, like the Docker CLI, so
        // resolution moves on to the context (which has no metadata here).
        assert!(matches!(
            fx.resolve(Some(""), None),
            Err(DockerContextSkip::MetadataUnreadable { name, .. }) if name == "colima"
        ));
    }

    #[test]
    fn default_or_missing_context_is_left_alone() {
        let fx = Fixture::new();
        assert_eq!(
            fx.resolve(None, None),
            Err(DockerContextSkip::NoActiveContext)
        );
        fx.set_current_context("default");
        assert_eq!(
            fx.resolve(None, None),
            Err(DockerContextSkip::DefaultContext)
        );
    }

    #[test]
    fn remote_contexts_are_never_adopted() {
        let fx = Fixture::new();
        fx.add_context("remote", "ssh://deploy@build-host");
        fx.set_current_context("remote");
        assert_eq!(
            fx.resolve(None, None),
            Err(DockerContextSkip::NotLocalUnixSocket {
                name: "remote".into(),
                host: "ssh://deploy@build-host".into()
            })
        );
    }

    #[test]
    fn a_context_whose_socket_is_gone_is_not_adopted() {
        let fx = Fixture::new();
        let missing = fx.config_dir().join("stopped-vm.sock");
        fx.add_context("colima", &format!("unix://{}", missing.display()));
        fx.set_current_context("colima");
        assert_eq!(
            fx.resolve(None, None),
            Err(DockerContextSkip::SocketMissing {
                name: "colima".into(),
                socket: missing
            })
        );
    }

    #[test]
    fn unreadable_metadata_is_reported_not_fatal() {
        let fx = Fixture::new();
        fx.set_current_context("ghost");
        assert!(matches!(
            fx.resolve(None, None),
            Err(DockerContextSkip::MetadataUnreadable { name, .. }) if name == "ghost"
        ));
    }
}
