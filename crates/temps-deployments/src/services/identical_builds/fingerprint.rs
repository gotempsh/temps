// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The identity of an image build (ADR 113).
//!
//! Two builds are identical only when everything that decides the image
//! matches: the repository and how it is fetched, the commit, the build
//! context and Dockerfile, the preset and its configuration, `.temps.yaml`,
//! the target platforms, where the build runs, the registry mirror, and the
//! value of every build-time variable the build can read. Getting this wrong
//! would deploy one project's build-time values (an API key, a public DSN)
//! inside another project's image, so every doubt widens the inputs and
//! makes builds *not* match:
//!
//! - A repository Dockerfile only reads the `ARG`s it declares (Docker
//!   ignores the rest) and the secrets its `RUN --mount=type=secret` steps
//!   name, so those values count. A Dockerfile that cannot be read
//!   ([`dockerfile_build_arg_names`] returns `None`) counts every value.
//! - A Dockerfile Temps generates from a preset declares every variable, so
//!   every value counts.
//! - Docker's predefined proxy arguments and the values that shape the
//!   generated `.npmrc` count whatever the Dockerfile declares.
//! - `BUILDKIT_*` arguments are BuildKit settings, not values the image can
//!   contain (the cache-mount namespace is one per project and environment);
//!   they are left out, as the custom-Dockerfile secret planner does.
//!
//! Values never leave this module: the result is a keyed hash (HMAC with a
//! subkey of the install's encryption key, see [`identify`]), so the stored
//! fingerprint cannot be brute-forced back into a low-entropy secret without
//! the key.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use temps_deployer::build_protocol::{dockerfile_build_arg_names, dockerfile_secret_mount_ids};

/// Docker's predefined build arguments: usable in `RUN` without an `ARG`.
const PREDEFINED_ARGS: &[&str] = &[
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "FTP_PROXY",
    "ftp_proxy",
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// Variables the build job turns into a generated `.npmrc` in the context.
const GENERATED_FILE_ARGS: &[&str] = &["NPM_TOKEN", "NPM_RC"];

/// Bumped whenever the encoding below changes, so fingerprints from an older
/// binary never match a newer one's.
const FINGERPRINT_VERSION: &str = "temps-identical-build-v1";

/// Where the repository comes from: two projects only build the same source
/// when they fetch the same repository the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    /// The git provider connection used to fetch it; `None` for a public
    /// repository fetched anonymously.
    pub git_provider_connection_id: Option<i32>,
    pub git_url: Option<String>,
    pub repo_owner: String,
    pub repo_name: String,
    /// Whether only the project's directory is checked out: files outside it
    /// are then missing from the context.
    pub pull_only_root_directory: bool,
}

impl SourceIdentity {
    /// `owner/name`, for log lines.
    pub fn repository(&self) -> String {
        format!("{}/{}", self.repo_owner, self.repo_name)
    }
}

/// The Dockerfile the build will use.
#[derive(Debug, Clone, Copy)]
pub enum DockerfileSource<'a> {
    /// The repository's own Dockerfile, as checked out.
    Repository { contents: &'a str },
    /// None in the repository: Temps generates one from the preset.
    Generated,
}

/// Who runs the build. A worker build receives only the values recorded as
/// public, so its image can differ from a control-plane build of the same
/// inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderKind {
    ControlPlane,
    Worker,
    Ci,
}

impl BuilderKind {
    fn as_str(self) -> &'static str {
        match self {
            BuilderKind::ControlPlane => "control-plane",
            BuilderKind::Worker => "worker",
            BuilderKind::Ci => "ci",
        }
    }
}

/// Everything that determines the image a build produces.
#[derive(Debug, Clone, Copy)]
pub struct BuildInputs<'a> {
    pub source: &'a SourceIdentity,
    /// The commit actually checked out.
    pub commit_sha: &'a str,
    pub build_context: &'a str,
    pub dockerfile_path: &'a str,
    pub dockerfile: DockerfileSource<'a>,
    /// The stored preset, and its configuration as canonical JSON.
    pub preset: Option<&'a str>,
    pub preset_config: Option<&'a str>,
    /// The checked-out `.temps.yaml`, which can override build settings.
    pub temps_yaml: Option<&'a [u8]>,
    /// Target platforms, in build order; the builder's native platform when
    /// the job did not name any.
    pub platforms: &'a [String],
    pub builder: BuilderKind,
    pub registry_mirror_prefix: Option<&'a str>,
    pub cache_from: &'a [String],
    pub build_args: &'a [(String, String)],
    pub buildkit_build_args: &'a [(String, String)],
    /// Names the planner recorded as public. Whether a value is public
    /// decides whether the builder receives it (a worker gets only public
    /// values, and a repository Dockerfile does not get a non-public value
    /// that would stay in the image), so two builds with the same value but
    /// a different visibility build different images.
    pub public_names: &'a BTreeSet<String>,
}

/// The keys a build coordinates on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildIdentity {
    /// Keyed hash of every input in [`BuildInputs`].
    pub fingerprint: String,
    /// Keyed hash of the repository and build path only (scope `source`).
    pub source_key: String,
    /// How many build-time values the fingerprint covers (names and values
    /// stay here).
    pub considered_values: usize,
}

/// Why a build has no identity and is built on its own.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotIdentifiable {
    #[error("the checked-out commit '{commit}' is not a full commit SHA")]
    CommitNotPinned { commit: String },
    #[error("the build has no repository to identify it by")]
    NoRepository,
}

/// A keyed hash: HMAC-SHA256 under a subkey of the install's encryption key
/// (`EncryptionService::derive_subkey`).
pub trait FingerprintKey: Send + Sync {
    fn keyed(&self, domain: &str) -> [u8; 32];
}

impl FingerprintKey for temps_core::EncryptionService {
    fn keyed(&self, domain: &str) -> [u8; 32] {
        self.derive_subkey(domain)
    }
}

/// Length-prefixed fields, so no two different inputs encode the same way.
struct Canonical(Sha256);

impl Canonical {
    fn new(kind: &str) -> Self {
        let mut canonical = Self(Sha256::new());
        canonical.field("version", FINGERPRINT_VERSION.as_bytes());
        canonical.field("kind", kind.as_bytes());
        canonical
    }

    fn field(&mut self, name: &str, value: &[u8]) {
        self.0.update((name.len() as u64).to_be_bytes());
        self.0.update(name.as_bytes());
        self.0.update((value.len() as u64).to_be_bytes());
        self.0.update(value);
    }

    fn optional(&mut self, name: &str, value: Option<&[u8]>) {
        match value {
            Some(value) => {
                self.field(name, b"some");
                self.field(name, value);
            }
            None => self.field(name, b"none"),
        }
    }

    fn list<'a>(&mut self, name: &str, values: impl ExactSizeIterator<Item = &'a str>) {
        self.field(name, &(values.len() as u64).to_be_bytes());
        for value in values {
            self.field(name, value.as_bytes());
        }
    }

    fn finish(self, key: &dyn FingerprintKey) -> String {
        let digest = hex::encode(self.0.finalize());
        hex::encode(key.keyed(&format!("{FINGERPRINT_VERSION}:{digest}")))
    }
}

fn is_full_commit_sha(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit())
}

fn normalized_path(path: &str) -> String {
    let trimmed = path.trim().trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() {
        ".".to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_buildkit_setting(name: &str) -> bool {
    name.to_ascii_uppercase().starts_with("BUILDKIT_")
}

/// The names whose values the build can read. `None`: all of them.
fn readable_names(dockerfile: DockerfileSource<'_>) -> Option<BTreeSet<String>> {
    match dockerfile {
        DockerfileSource::Generated => None,
        DockerfileSource::Repository { contents } => {
            let mut names = dockerfile_build_arg_names(contents)?;
            names.extend(dockerfile_secret_mount_ids(contents));
            names.extend(PREDEFINED_ARGS.iter().map(|name| name.to_string()));
            names.extend(GENERATED_FILE_ARGS.iter().map(|name| name.to_string()));
            Some(names)
        }
    }
}

/// The build-time values that decide the image, sorted by name.
fn considered_values<'a>(
    args: &'a [(String, String)],
    readable: Option<&BTreeSet<String>>,
) -> Vec<(&'a str, &'a str)> {
    let mut values: Vec<(&str, &str)> = args
        .iter()
        .filter(|(name, _)| {
            // A generated `.npmrc` carries these whatever the Dockerfile
            // declares, and the proxy arguments are read without one.
            GENERATED_FILE_ARGS.contains(&name.as_str())
                || PREDEFINED_ARGS.contains(&name.as_str())
                || (!is_buildkit_setting(name)
                    && readable.is_none_or(|names| names.contains(name.as_str())))
        })
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    values.sort();
    values.dedup();
    values
}

/// The identity of the build `inputs` describe, keyed with `key`.
pub fn identify(
    inputs: &BuildInputs<'_>,
    key: &dyn FingerprintKey,
) -> Result<BuildIdentity, NotIdentifiable> {
    let source = inputs.source;
    if source.repo_owner.trim().is_empty() || source.repo_name.trim().is_empty() {
        return Err(NotIdentifiable::NoRepository);
    }
    let commit = inputs.commit_sha.trim().to_ascii_lowercase();
    if !is_full_commit_sha(&commit) {
        return Err(NotIdentifiable::CommitNotPinned {
            commit: inputs.commit_sha.to_string(),
        });
    }
    let build_context = normalized_path(inputs.build_context);
    let dockerfile_path = normalized_path(inputs.dockerfile_path);
    let connection = source
        .git_provider_connection_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "anonymous".to_string());
    let git_url = source.git_url.as_deref().unwrap_or_default().trim();
    let owner = source.repo_owner.trim().to_ascii_lowercase();
    let name = source.repo_name.trim().to_ascii_lowercase();

    let source_key = {
        let mut canonical = Canonical::new("source");
        canonical.field("connection", connection.as_bytes());
        canonical.field("git_url", git_url.as_bytes());
        canonical.field("owner", owner.as_bytes());
        canonical.field("name", name.as_bytes());
        canonical.field("build_context", build_context.as_bytes());
        canonical.field("dockerfile_path", dockerfile_path.as_bytes());
        canonical.finish(key)
    };

    let readable = readable_names(inputs.dockerfile);
    let build_args = considered_values(inputs.build_args, readable.as_ref());
    let buildkit_args = considered_values(inputs.buildkit_build_args, readable.as_ref());

    let mut canonical = Canonical::new("build");
    canonical.field("connection", connection.as_bytes());
    canonical.field("git_url", git_url.as_bytes());
    canonical.field("owner", owner.as_bytes());
    canonical.field("name", name.as_bytes());
    canonical.field(
        "pull_only_root_directory",
        if source.pull_only_root_directory {
            b"true"
        } else {
            b"false"
        },
    );
    canonical.field("commit", commit.as_bytes());
    canonical.field("build_context", build_context.as_bytes());
    canonical.field("dockerfile_path", dockerfile_path.as_bytes());
    match inputs.dockerfile {
        DockerfileSource::Repository { contents } => {
            canonical.field("dockerfile", b"repository");
            canonical.field("dockerfile_contents", contents.as_bytes());
        }
        DockerfileSource::Generated => canonical.field("dockerfile", b"generated"),
    }
    canonical.optional("preset", inputs.preset.map(str::as_bytes));
    canonical.optional("preset_config", inputs.preset_config.map(str::as_bytes));
    canonical.optional("temps_yaml", inputs.temps_yaml);
    canonical.list(
        "platforms",
        inputs.platforms.iter().map(|platform| platform.as_str()),
    );
    canonical.field("builder", inputs.builder.as_str().as_bytes());
    canonical.optional(
        "registry_mirror_prefix",
        inputs.registry_mirror_prefix.map(str::as_bytes),
    );
    canonical.list("cache_from", inputs.cache_from.iter().map(String::as_str));
    for (label, values) in [("build_arg", &build_args), ("buildkit_arg", &buildkit_args)] {
        canonical.field(label, &(values.len() as u64).to_be_bytes());
        for (name, value) in values.iter() {
            canonical.field(label, name.as_bytes());
            canonical.field(label, value.as_bytes());
            let visibility: &[u8] = if inputs.public_names.contains(*name) {
                b"public"
            } else {
                b"non-public"
            };
            canonical.field(label, visibility);
        }
    }

    Ok(BuildIdentity {
        fingerprint: canonical.finish(key),
        source_key,
        considered_values: build_args.len() + buildkit_args.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestKey(&'static str);

    impl FingerprintKey for TestKey {
        fn keyed(&self, domain: &str) -> [u8; 32] {
            let mut hasher = Sha256::new();
            hasher.update(self.0.as_bytes());
            hasher.update(domain.as_bytes());
            hasher.finalize().into()
        }
    }

    const KEY: TestKey = TestKey("install-a");
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn source() -> SourceIdentity {
        SourceIdentity {
            git_provider_connection_id: Some(4),
            git_url: Some("https://git.example.test/acme/shop.git".into()),
            repo_owner: "acme".into(),
            repo_name: "shop".into(),
            pull_only_root_directory: false,
        }
    }

    struct Owned {
        source: SourceIdentity,
        dockerfile: Option<String>,
        platforms: Vec<String>,
        args: Vec<(String, String)>,
        cache_from: Vec<String>,
        public: BTreeSet<String>,
    }

    fn owned(dockerfile: Option<&str>, args: &[(&str, &str)]) -> Owned {
        Owned {
            source: source(),
            dockerfile: dockerfile.map(str::to_string),
            platforms: vec!["linux/amd64".into()],
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            cache_from: Vec::new(),
            public: BTreeSet::new(),
        }
    }

    impl Owned {
        fn inputs(&self) -> BuildInputs<'_> {
            BuildInputs {
                source: &self.source,
                commit_sha: COMMIT,
                build_context: "apps/web",
                dockerfile_path: "Dockerfile",
                dockerfile: match &self.dockerfile {
                    Some(contents) => DockerfileSource::Repository { contents },
                    None => DockerfileSource::Generated,
                },
                preset: Some("nextjs"),
                preset_config: None,
                temps_yaml: None,
                platforms: &self.platforms,
                builder: BuilderKind::ControlPlane,
                registry_mirror_prefix: None,
                cache_from: &self.cache_from,
                build_args: &self.args,
                buildkit_build_args: &[],
                public_names: &self.public,
            }
        }

        fn identify(&self) -> BuildIdentity {
            identify(&self.inputs(), &KEY).expect("identifiable")
        }
    }

    const DOCKERFILE: &str = "FROM node:22 AS build\nARG NEXT_PUBLIC_API_URL\nRUN --mount=type=secret,id=NPM_AUTH npm ci\nFROM node:22\nCOPY --from=build /app /app\n";

    #[test]
    fn the_same_inputs_give_the_same_identity() {
        let a = owned(Some(DOCKERFILE), &[("NEXT_PUBLIC_API_URL", "https://api")]);
        let b = owned(Some(DOCKERFILE), &[("NEXT_PUBLIC_API_URL", "https://api")]);
        assert_eq!(a.identify(), b.identify());
        // Order of the arguments does not matter.
        let c = owned(None, &[("A", "1"), ("B", "2")]);
        let d = owned(None, &[("B", "2"), ("A", "1")]);
        assert_eq!(c.identify(), d.identify());
    }

    #[test]
    fn a_different_build_time_value_gives_a_different_fingerprint() {
        // Generated Dockerfile: every variable is declared, so every value
        // counts -- secrets included.
        let a = owned(None, &[("DATABASE_PASSWORD", "one")]);
        let b = owned(None, &[("DATABASE_PASSWORD", "two")]);
        assert_ne!(a.identify().fingerprint, b.identify().fingerprint);
        // ...but the source key, which only groups queueing, is the same.
        assert_eq!(a.identify().source_key, b.identify().source_key);

        // A declared ARG in a repository Dockerfile.
        let a = owned(Some(DOCKERFILE), &[("NEXT_PUBLIC_API_URL", "https://a")]);
        let b = owned(Some(DOCKERFILE), &[("NEXT_PUBLIC_API_URL", "https://b")]);
        assert_ne!(a.identify().fingerprint, b.identify().fingerprint);

        // A secret the Dockerfile mounts, though never declared as ARG.
        let a = owned(Some(DOCKERFILE), &[("NPM_AUTH", "token-a")]);
        let b = owned(Some(DOCKERFILE), &[("NPM_AUTH", "token-b")]);
        assert_ne!(a.identify().fingerprint, b.identify().fingerprint);
    }

    #[test]
    fn the_same_value_marked_public_in_one_build_only_never_matches() {
        // The builder receives a public value but withholds a non-public
        // one the Dockerfile would keep in the image: different images.
        const FINAL_STAGE_ARG: &str = "FROM alpine\nARG BUILD_LABEL\nRUN echo $BUILD_LABEL\n";
        let public = owned(Some(FINAL_STAGE_ARG), &[("BUILD_LABEL", "blue")]);
        let mut public = public;
        public.public.insert("BUILD_LABEL".to_string());
        let secret = owned(Some(FINAL_STAGE_ARG), &[("BUILD_LABEL", "blue")]);
        assert_ne!(public.identify().fingerprint, secret.identify().fingerprint);
        // A public name the build does not read changes nothing.
        let mut unread = owned(Some(FINAL_STAGE_ARG), &[("BUILD_LABEL", "blue")]);
        unread.public.insert("SOMETHING_ELSE".to_string());
        assert_eq!(unread.identify().fingerprint, secret.identify().fingerprint);
    }

    #[test]
    fn a_variable_present_in_one_build_only_never_matches() {
        let a = owned(None, &[("API_KEY", "x")]);
        let b = owned(None, &[]);
        assert_ne!(a.identify().fingerprint, b.identify().fingerprint);
        // An empty value is still a value.
        let c = owned(None, &[("API_KEY", "")]);
        assert_ne!(b.identify().fingerprint, c.identify().fingerprint);
    }

    #[test]
    fn a_value_the_repository_dockerfile_cannot_read_does_not_split_builds() {
        let a = owned(Some(DOCKERFILE), &[("RUNTIME_ONLY_SECRET", "a")]);
        let b = owned(Some(DOCKERFILE), &[("RUNTIME_ONLY_SECRET", "b")]);
        assert_eq!(a.identify().fingerprint, b.identify().fingerprint);
        assert_eq!(a.identify().considered_values, 0);
    }

    #[test]
    fn values_that_reach_the_build_without_an_arg_always_count() {
        for name in ["HTTP_PROXY", "NPM_TOKEN", "NPM_RC"] {
            let a = owned(Some(DOCKERFILE), &[(name, "a")]);
            let b = owned(Some(DOCKERFILE), &[(name, "b")]);
            assert_ne!(a.identify().fingerprint, b.identify().fingerprint, "{name}");
        }
    }

    #[test]
    fn an_unreadable_dockerfile_counts_every_value() {
        let unreadable = "FROM alpine\nARG $DYNAMIC\n";
        let a = owned(Some(unreadable), &[("ANYTHING", "a")]);
        let b = owned(Some(unreadable), &[("ANYTHING", "b")]);
        assert_ne!(a.identify().fingerprint, b.identify().fingerprint);
    }

    #[test]
    fn buildkit_settings_are_not_image_inputs() {
        let a = owned(None, &[("BUILDKIT_CACHE_MOUNT_NS", "project-1")]);
        let b = owned(None, &[("BUILDKIT_CACHE_MOUNT_NS", "project-2")]);
        assert_eq!(a.identify().fingerprint, b.identify().fingerprint);
    }

    #[test]
    fn every_other_input_changes_the_fingerprint() {
        let base = owned(None, &[]);
        let fingerprint = base.identify().fingerprint;
        let other_source = SourceIdentity {
            repo_name: "other".into(),
            ..source()
        };
        let other_connection = SourceIdentity {
            git_provider_connection_id: Some(5),
            ..source()
        };
        let sparse = SourceIdentity {
            pull_only_root_directory: true,
            ..source()
        };
        let arm = vec!["linux/arm64".to_string()];
        let cache = vec!["registry.example.test/cache".to_string()];
        let b = base.inputs();
        let cases = [
            (
                "repository",
                BuildInputs {
                    source: &other_source,
                    ..b
                },
            ),
            (
                "connection",
                BuildInputs {
                    source: &other_connection,
                    ..b
                },
            ),
            (
                "sparse checkout",
                BuildInputs {
                    source: &sparse,
                    ..b
                },
            ),
            (
                "commit",
                BuildInputs {
                    commit_sha: "1123456789abcdef0123456789abcdef01234567",
                    ..b
                },
            ),
            (
                "context",
                BuildInputs {
                    build_context: "apps/admin",
                    ..b
                },
            ),
            (
                "dockerfile path",
                BuildInputs {
                    dockerfile_path: "Dockerfile.prod",
                    ..b
                },
            ),
            (
                "dockerfile",
                BuildInputs {
                    dockerfile: DockerfileSource::Repository { contents: "FROM a" },
                    ..b
                },
            ),
            (
                "preset",
                BuildInputs {
                    preset: Some("vite"),
                    ..b
                },
            ),
            (
                "preset config",
                BuildInputs {
                    preset_config: Some("{}"),
                    ..b
                },
            ),
            (
                "temps.yaml",
                BuildInputs {
                    temps_yaml: Some(b"build: {}"),
                    ..b
                },
            ),
            (
                "platform",
                BuildInputs {
                    platforms: &arm,
                    ..b
                },
            ),
            (
                "builder",
                BuildInputs {
                    builder: BuilderKind::Worker,
                    ..b
                },
            ),
            (
                "mirror",
                BuildInputs {
                    registry_mirror_prefix: Some("mirror.test"),
                    ..b
                },
            ),
            (
                "cache",
                BuildInputs {
                    cache_from: &cache,
                    ..b
                },
            ),
        ];
        for (label, inputs) in cases {
            let changed = identify(&inputs, &KEY).expect("identifiable").fingerprint;
            assert_ne!(changed, fingerprint, "{label}");
        }
    }

    #[test]
    fn the_source_key_ignores_commit_and_values_but_not_the_path() {
        let a = owned(None, &[("X", "1")]);
        let mut inputs = a.inputs();
        inputs.commit_sha = "1123456789abcdef0123456789abcdef01234567";
        let other_commit = identify(&inputs, &KEY).unwrap();
        assert_eq!(other_commit.source_key, a.identify().source_key);
        inputs.build_context = "apps/admin";
        assert_ne!(
            identify(&inputs, &KEY).unwrap().source_key,
            a.identify().source_key
        );
    }

    #[test]
    fn equivalent_spellings_of_paths_and_commits_match() {
        let a = owned(None, &[]);
        let mut inputs = a.inputs();
        inputs.build_context = "./apps/web/";
        inputs.commit_sha = "0123456789ABCDEF0123456789ABCDEF01234567";
        assert_eq!(identify(&inputs, &KEY).unwrap(), a.identify());
    }

    #[test]
    fn the_fingerprint_is_keyed_and_reveals_no_value() {
        let a = owned(None, &[("SECRET", "hunter2")]);
        let other = identify(&a.inputs(), &TestKey("install-b")).unwrap();
        assert_ne!(other.fingerprint, a.identify().fingerprint);
        assert!(!a.identify().fingerprint.contains("hunter2"));
        assert_eq!(a.identify().fingerprint.len(), 64);
    }

    #[test]
    fn a_build_without_a_pinned_commit_or_repository_has_no_identity() {
        let a = owned(None, &[]);
        for commit in ["main", "", "abc123", "refs/heads/main"] {
            let mut inputs = a.inputs();
            inputs.commit_sha = commit;
            assert!(
                matches!(
                    identify(&inputs, &KEY),
                    Err(NotIdentifiable::CommitNotPinned { .. })
                ),
                "{commit}"
            );
        }
        let anonymous = SourceIdentity {
            repo_owner: String::new(),
            ..source()
        };
        let mut inputs = a.inputs();
        inputs.source = &anonymous;
        assert_eq!(identify(&inputs, &KEY), Err(NotIdentifiable::NoRepository));
    }
}
