// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PostgreSQL service configuration: input parameters, defaults, the image
//! allowlist and the parameter schema.

use super::*;
use crate::externalsvc::port_util::find_available_port;
use crate::externalsvc::ServiceConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Validate a PostgreSQL role/username before it is interpolated into a
/// shell command or SQL. Allows only `[A-Za-z0-9_]` (the realistic role-name
/// charset), non-empty, and at most 63 bytes.
///
/// Defense-in-depth: the upgrade orchestrator already `shell_escape`s the
/// username at every interpolation site, but rejecting shell/SQL
/// metacharacters (quotes, `$`, `;`, spaces, backticks, …) up front means a
/// crafted username can never reach a `sh -c` string or a `sed` program in
/// the first place. Mirrors [`PostgresService::validate_database_name`], but
/// permits uppercase since role names are commonly mixed-case.
pub(crate) fn validate_pg_username(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("PostgreSQL username cannot be empty".to_string());
    }
    if name.len() > 63 {
        return Err(format!(
            "PostgreSQL username '{name}' exceeds the 63 character limit"
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "PostgreSQL username '{name}' contains invalid characters; only \
             ASCII letters, digits, and underscores are allowed"
        ));
    }
    Ok(())
}

/// Input configuration for creating a PostgreSQL service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "PostgreSQL Configuration",
    description = "Configuration for PostgreSQL service"
)]
pub struct PostgresInputConfig {
    /// PostgreSQL host address
    #[serde(default = "default_host")]
    #[schemars(example = example_host(), default = "default_host")]
    pub host: String,

    /// PostgreSQL port (auto-assigned if not provided)
    #[schemars(example = example_port())]
    pub port: Option<String>,

    /// PostgreSQL database name
    #[serde(default = "default_database")]
    #[schemars(example = example_database(), default = "default_database")]
    pub database: String,

    /// PostgreSQL username
    #[serde(default = "default_username")]
    #[schemars(example = example_username(), default = "default_username")]
    pub username: String,

    /// PostgreSQL password (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(with = "Option<String>", example = example_password())]
    pub password: Option<String>,

    /// Maximum number of connections
    #[serde(
        default = "default_max_connections",
        deserialize_with = "deserialize_max_connections"
    )]
    #[schemars(
        example = example_max_connections(),
        default = "default_max_connections"
    )]
    pub max_connections: u32,

    /// SSL mode (disable, allow, prefer, require)
    #[serde(default = "default_ssl_mode")]
    #[schemars(example = example_ssl_mode(), default = "default_ssl_mode_string")]
    pub ssl_mode: Option<String>,

    /// Docker image to use (defaults to gotempsh/postgres-walg:18-bookworm, supports timescale/timescaledb-ha:pg18)
    #[serde(default = "default_docker_image")]
    #[schemars(example = example_docker_image(), default = "default_docker_image")]
    pub docker_image: Option<String>,

    /// Real Docker container name when this service was imported from an
    /// existing PostgreSQL-compatible container (set by
    /// `import_from_container`, never user-editable — omitted from the
    /// create form). Overrides the derived `postgres-{name}` container name
    /// so internal addressing targets the actual pre-existing container
    /// instead of a synthesized name that doesn't exist. Deserialized
    /// through `deserialize_optional_non_empty` as a second guard alongside
    /// `#[schemars(skip)]`, mirroring the MariaDB fix for the same class of
    /// bug (see `crates/temps-providers/src/externalsvc/mariadb.rs`).
    #[serde(default, deserialize_with = "deserialize_optional_non_empty")]
    #[schemars(skip)]
    pub container_name: Option<String>,
}

/// Internal runtime configuration for PostgreSQL service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    #[serde(deserialize_with = "deserialize_max_connections")]
    pub max_connections: u32,
    pub ssl_mode: Option<String>,
    pub docker_image: String,
    /// Real container name for imported services — see
    /// `PostgresInputConfig::container_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

impl PostgresConfig {
    /// A new managed service must never inherit an imported workload's
    /// physical container identity. Its own name identifies its new resources.
    pub(super) fn for_managed_clone(mut self) -> Self {
        self.container_name = None;
        self
    }
}

impl From<PostgresInputConfig> for PostgresConfig {
    fn from(input: PostgresInputConfig) -> Self {
        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(5432)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "5432".to_string())
            }),
            database: input.database,
            username: input.username,
            password: input.password.unwrap_or_else(generate_password),
            max_connections: input.max_connections,
            ssl_mode: input.ssl_mode,
            docker_image: input
                .docker_image
                .unwrap_or_else(|| "gotempsh/postgres-walg:18-bookworm".to_string()),
            container_name: input.container_name,
        }
    }
}

/// Treats a blank string the same as an absent value — see
/// `PostgresInputConfig::container_name`.
pub(super) fn deserialize_optional_non_empty<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

pub(super) fn deserialize_optional_password<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

/// Deserialize max_connections from either string or number
pub(super) fn deserialize_max_connections<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Deserialize};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(u32),
    }

    match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s.parse::<u32>().map_err(de::Error::custom),
        StringOrNumber::Number(n) => Ok(n),
    }
}

pub(super) fn default_host() -> String {
    "localhost".to_string()
}

pub(super) fn default_database() -> String {
    "postgres".to_string()
}

pub(super) fn default_username() -> String {
    "postgres".to_string()
}

pub fn generate_password() -> String {
    use rand::{distr::Alphanumeric, RngExt};
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

pub(super) fn default_max_connections() -> u32 {
    100
}

pub(super) fn default_ssl_mode() -> Option<String> {
    Some("disable".to_string())
}

pub(super) fn default_ssl_mode_string() -> String {
    "disable".to_string()
}

pub(super) fn default_docker_image() -> Option<String> {
    Some("gotempsh/postgres-walg:18-bookworm".to_string())
}

/// Every PostgreSQL-flavoured image the platform is allowed to pull and run.
///
/// This is an exact-match allowlist on purpose (never a substring/prefix test):
/// a service's `docker_image` comes from client-deserializable parameters, and
/// backup/restore helpers pull and execute it in sidecar containers, so a
/// permissive match is remote code execution.
///
/// Because it is exact, it must list *every* image any other part of the
/// product can put on a service, or that service becomes permanently
/// un-initializable — `get_postgres_config` runs this check on every read of
/// the config, so an unlisted image doesn't just block creation, it blocks
/// deploys of every app linked to the service. Three sources feed it:
///
/// - images this repo builds (`images/*-walg/Dockerfile`),
/// - images the console offers as upgrade targets
///   (`web/src/components/storage/UpgradeServiceDialog.tsx`),
/// - upstream images container discovery classifies as Postgres
///   (`services.rs`, which matches `postgres` / `timescaledb` / `pgvector`).
///
/// Adding an image to any of those without adding it here is the bug this
/// list keeps regressing into.
///
/// Operators can extend (never shrink) this set via
/// `TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES` — see
/// [`extra_allowed_postgres_docker_images`].
pub(super) const ALLOWED_POSTGRES_DOCKER_IMAGES: &[&str] = &[
    "gotempsh/postgres-walg:15-bookworm",
    "gotempsh/postgres-walg:16-bookworm",
    "gotempsh/postgres-walg:17-bookworm",
    "gotempsh/postgres-walg:18-bookworm",
    "gotempsh/pgvector-walg:pg17",
    "gotempsh/pgvector-walg:pg18",
    "gotempsh/timescaledb-walg:pg18",
    "pgvector/pgvector:pg15",
    "pgvector/pgvector:pg16",
    "pgvector/pgvector:pg17",
    "pgvector/pgvector:pg18",
    "timescale/timescaledb-ha:pg17",
    "timescale/timescaledb-ha:pg18",
];

/// Environment variable an operator sets to allow additional PostgreSQL images
/// (comma-separated). This is deliberately host-level rather than an API
/// setting: which images this machine may pull and execute is the operator's
/// policy, and keeping it off the API avoids adding a write surface that can
/// widen container execution.
///
/// Requires a restart to take effect, like the other process-wide operator
/// knobs documented in `docs/reference/environment-variables.md`.
pub(crate) const EXTRA_POSTGRES_IMAGES_ENV: &str = "TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES";

/// Parse the operator-provided image list.
///
/// Split on commas, trim, drop empties. Entries are kept verbatim otherwise —
/// matching stays exact, so an entry without a `:tag` or `@sha256:` digest can
/// never match a real service image. Those are returned separately as
/// `suspicious` so startup can warn about them instead of silently ignoring
/// them: a self-hosted operator who typo'd the variable gets told, rather than
/// re-reading the same rejection error with no idea why their entry did
/// nothing.
pub(super) fn parse_extra_allowed_images(raw: &str) -> (Vec<String>, Vec<String>) {
    let mut images = Vec::new();
    let mut suspicious = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // A tag/digest separator must appear after the final `/`, otherwise the
        // only `:` is a registry port (e.g. `registry.internal:5000/pg`).
        let name_start = entry.rfind('/').map_or(0, |i| i + 1);
        if !entry[name_start..].contains(':') && !entry.contains('@') {
            suspicious.push(entry.to_string());
        }
        images.push(entry.to_string());
    }
    (images, suspicious)
}

/// Operator-supplied additions to [`ALLOWED_POSTGRES_DOCKER_IMAGES`], read once
/// per process.
///
/// Additive by design: the built-in list stays the floor so a typo in this
/// variable cannot strand every existing Postgres service. `get_postgres_config`
/// validates on every read of a service's config, so shrinking the allowlist
/// would not merely block new services — it would make existing ones
/// permanently un-initializable and block deploys of every app linked to them.
pub(super) fn extra_allowed_postgres_docker_images() -> &'static [String] {
    static EXTRA: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    EXTRA.get_or_init(|| {
        #[allow(
            clippy::disallowed_methods,
            reason = "documented host-level operator policy: TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES (docs/reference/environment-variables.md)"
        )]
        let Ok(raw) = std::env::var(EXTRA_POSTGRES_IMAGES_ENV) else {
            return Vec::new();
        };
        let (images, suspicious) = parse_extra_allowed_images(&raw);
        if !suspicious.is_empty() {
            warn!(
                "{} contains {} entr{} without a tag or digest ({}). Matching is exact, so \
                 these can never match a service image — did you mean e.g. 'postgis/postgis:18-3.5'?",
                EXTRA_POSTGRES_IMAGES_ENV,
                suspicious.len(),
                if suspicious.len() == 1 { "y" } else { "ies" },
                suspicious.join(", ")
            );
        }
        if !images.is_empty() {
            info!(
                "{} allows {} additional PostgreSQL image(s): {}",
                EXTRA_POSTGRES_IMAGES_ENV,
                images.len(),
                images.join(", ")
            );
        }
        images
    })
}

/// Exact-match membership test over the built-in list plus the operator's
/// additions.
///
/// Split out from [`validate_postgres_docker_image`] so the composition is
/// testable without mutating process environment or racing the `OnceLock`.
pub(super) fn is_allowed_postgres_docker_image(docker_image: &str, extra: &[String]) -> bool {
    ALLOWED_POSTGRES_DOCKER_IMAGES.contains(&docker_image)
        || extra.iter().any(|allowed| allowed == docker_image)
}

pub(crate) fn validate_postgres_docker_image(docker_image: &str) -> Result<()> {
    if is_allowed_postgres_docker_image(docker_image, extra_allowed_postgres_docker_images()) {
        Ok(())
    } else {
        // Name the escape hatch. A self-hosted operator hitting this has no
        // support channel; without this sentence the error is a dead end that
        // reads as "temps cannot run your image" rather than "temps has not
        // been told it may".
        let extra = extra_allowed_postgres_docker_images();
        Err(anyhow::anyhow!(
            "PostgreSQL Docker image '{}' is not supported. Allowed images: {}{}. \
             To allow additional images on this instance, set {} to a comma-separated \
             list (e.g. {}=postgis/postgis:18-3.5) and restart temps.",
            docker_image,
            ALLOWED_POSTGRES_DOCKER_IMAGES.join(", "),
            if extra.is_empty() {
                String::new()
            } else {
                format!(
                    " (plus {} from {})",
                    extra.join(", "),
                    EXTRA_POSTGRES_IMAGES_ENV
                )
            },
            EXTRA_POSTGRES_IMAGES_ENV,
            EXTRA_POSTGRES_IMAGES_ENV
        ))
    }
}

// Schema example functions
pub(super) fn example_host() -> &'static str {
    "localhost"
}

pub(super) fn example_port() -> &'static str {
    "5432"
}

pub(super) fn example_database() -> &'static str {
    "myapp"
}

pub(super) fn example_username() -> &'static str {
    "postgres"
}

pub(super) fn example_password() -> &'static str {
    "your-secure-password"
}

pub(super) fn example_max_connections() -> u32 {
    10
}

pub(super) fn example_ssl_mode() -> &'static str {
    "disable"
}

pub(super) fn example_docker_image() -> &'static str {
    "gotempsh/postgres-walg:18-bookworm"
}

impl PostgresService {
    pub(super) fn get_postgres_config(
        &self,
        service_config: ServiceConfig,
    ) -> Result<PostgresConfig> {
        // Parse input config and transform to runtime config
        // First deserialize to PostgresInputConfig to apply defaults and custom handling
        let input_config: PostgresInputConfig =
            serde_json::from_value(service_config.parameters)
                .map_err(|e| anyhow::anyhow!("Failed to parse PostgreSQL configuration: {}", e))?;
        let postgres_config = PostgresConfig::from(input_config);
        // Imported-service metadata is client-deserializable and therefore
        // cannot be used as an authorization signal. Every image must remain
        // allowlisted because backup/restore helpers may pull and execute it
        // even when the primary database container already exists.
        validate_postgres_docker_image(&postgres_config.docker_image)?;
        Ok(postgres_config)
    }
}

/// Docker-free, static metadata about this engine.
///
/// The parameter schema is generated from the input-config type and
/// depends on nothing at runtime, so it must be reachable without
/// constructing a service instance — a control plane with no local
/// Docker daemon still has to serve it to the console.
impl PostgresService {
    /// JSON Schema describing this engine's creation parameters.
    pub fn parameter_schema() -> Option<serde_json::Value> {
        // Generate JSON Schema from PostgresInputConfig
        let schema = schemars::schema_for!(PostgresInputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // `PostgresInputConfig` can deserialize absent values with defaults, but
        // service creation deliberately requires callers to choose the database
        // and username explicitly (see `PostgresParameterStrategy`). Schemars
        // interprets serde defaults as "optional", so without this correction
        // the public service-type schema contradicts the creation validator.
        // The dashboard and AI both consume this schema; publishing the wrong
        // required set makes them learn by failing POST /external-services.
        schema_json["required"] = serde_json::json!(["database", "username"]);

        // Add metadata about which fields are editable
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable
                let editable = match key.as_str() {
                    "host" => false,           // Don't change host after creation
                    "port" => true,            // Port can be changed
                    "database" => false,       // Don't change database name after creation
                    "username" => false,       // Don't change username after creation
                    "password" => true,        // Password can be changed by user
                    "max_connections" => true, // Max connections can be adjusted
                    "ssl_mode" => true,        // SSL mode can be changed
                    "docker_image" => true,    // Docker image can be upgraded
                    _ => false,
                };

                if let Some(prop) = schema_json["properties"][&key].as_object_mut() {
                    prop.insert("x-editable".to_string(), serde_json::json!(editable));
                }
            }
        }

        Some(schema_json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_postgres_images_are_allowlisted() {
        assert!(validate_postgres_docker_image("gotempsh/postgres-walg:18-bookworm").is_ok());
        assert!(validate_postgres_docker_image("attacker/postgres:latest").is_err());
    }

    /// Every image the console offers as an upgrade target in
    /// `web/src/components/storage/UpgradeServiceDialog.tsx` must validate.
    /// These were offered in the UI while being rejected by the allowlist, so
    /// picking one was a guaranteed failure.
    #[test]
    fn console_upgrade_target_images_are_allowlisted() {
        for image in [
            "gotempsh/postgres-walg:18-bookworm",
            "gotempsh/postgres-walg:17-bookworm",
            "gotempsh/pgvector-walg:pg18",
            "gotempsh/pgvector-walg:pg17",
            "gotempsh/timescaledb-walg:pg18",
        ] {
            assert!(
                validate_postgres_docker_image(image).is_ok(),
                "console offers {image} as an upgrade target but it is not allowlisted"
            );
        }
    }

    /// `services.rs` classifies any discovered container whose image contains
    /// `pgvector` as a Postgres service, so importing one must not produce a
    /// service that can never be initialized.
    #[test]
    fn discovered_pgvector_images_are_allowlisted() {
        for image in [
            "pgvector/pgvector:pg15",
            "pgvector/pgvector:pg16",
            "pgvector/pgvector:pg17",
            "pgvector/pgvector:pg18",
        ] {
            assert!(
                validate_postgres_docker_image(image).is_ok(),
                "container discovery imports {image} as Postgres but it is not allowlisted"
            );
        }
    }

    /// The allowlist stays exact-match: a listed image must not make an
    /// arbitrary image sharing its repository or tag prefix acceptable.
    #[test]
    fn allowlist_does_not_match_by_prefix_or_repository() {
        for image in [
            "pgvector/pgvector:latest",
            "pgvector/pgvector",
            "attacker/pgvector:pg18",
            "gotempsh/pgvector-walg:pg18-evil",
            "timescale/timescaledb-ha:pg18-attacker",
        ] {
            assert!(
                validate_postgres_docker_image(image).is_err(),
                "{image} must not be accepted by the exact-match allowlist"
            );
        }
    }

    #[test]
    fn extra_images_env_is_split_trimmed_and_compacted() {
        let (images, suspicious) = parse_extra_allowed_images(
            "  postgis/postgis:18-3.5 ,,registry.internal:5000/team/pg:18 , ",
        );
        assert_eq!(
            images,
            vec![
                "postgis/postgis:18-3.5".to_string(),
                "registry.internal:5000/team/pg:18".to_string(),
            ]
        );
        // A registry port is not a tag separator, but both entries here are
        // properly tagged, so neither should be flagged.
        assert!(suspicious.is_empty(), "unexpected warnings: {suspicious:?}");
    }

    #[test]
    fn extra_images_env_empty_or_blank_yields_nothing() {
        assert_eq!(parse_extra_allowed_images("").0, Vec::<String>::new());
        assert_eq!(
            parse_extra_allowed_images("  , ,, ").0,
            Vec::<String>::new()
        );
    }

    /// An entry with no tag can never match, since comparison is exact. It is
    /// still accepted verbatim, but the operator is warned rather than left to
    /// wonder why their variable did nothing.
    #[test]
    fn extra_images_env_flags_entries_without_tag_or_digest() {
        let (images, suspicious) =
            parse_extra_allowed_images("postgis/postgis,registry.internal:5000/team/pg");
        assert_eq!(images.len(), 2);
        assert_eq!(
            suspicious,
            vec![
                "postgis/postgis".to_string(),
                // ':5000' is a registry port, not a tag -- must still be flagged.
                "registry.internal:5000/team/pg".to_string(),
            ]
        );

        let (_, ok) = parse_extra_allowed_images(
            "postgis/postgis:18-3.5,gotempsh/postgres-walg@sha256:abc123",
        );
        assert!(
            ok.is_empty(),
            "tagged/digested entries must not warn: {ok:?}"
        );
    }

    /// End-to-end over the parse + membership composition: a value the operator
    /// puts in the env var makes an otherwise-rejected image acceptable, and
    /// only that exact image.
    #[test]
    fn extra_images_env_admits_exactly_the_operator_listed_image() {
        let (extra, _) = parse_extra_allowed_images("postgis/postgis:18-3.5");

        assert!(
            !is_allowed_postgres_docker_image("postgis/postgis:18-3.5", &[]),
            "image must be rejected without the operator override"
        );
        assert!(
            is_allowed_postgres_docker_image("postgis/postgis:18-3.5", &extra),
            "operator-listed image must be accepted"
        );

        // Still exact-match: the override admits one image, not a repository.
        for near_miss in [
            "postgis/postgis:latest",
            "postgis/postgis",
            "postgis/postgis:18-3.5-evil",
            "attacker/postgis:18-3.5",
        ] {
            assert!(
                !is_allowed_postgres_docker_image(near_miss, &extra),
                "{near_miss} must not be admitted by an exact-match override"
            );
        }
    }

    /// The operator knob extends the allowlist; it must never be able to shrink
    /// it, or a typo would strand every existing Postgres service.
    #[test]
    fn extra_images_env_cannot_remove_builtin_images() {
        // Built-ins are checked before the env list is even consulted.
        for image in ALLOWED_POSTGRES_DOCKER_IMAGES {
            assert!(
                validate_postgres_docker_image(image).is_ok(),
                "{image} must stay allowed regardless of {EXTRA_POSTGRES_IMAGES_ENV}"
            );
        }
    }

    /// The rejection message must name the env var -- a self-hosted operator
    /// has no support channel, so a bare "not supported" is a dead end.
    #[test]
    fn rejection_message_points_at_the_operator_override() {
        // Deliberately an image no operator would ever allowlist, so this test
        // cannot flip if TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES happens to be set
        // in the environment running the suite.
        let err = validate_postgres_docker_image("temps-test/never-allowlisted:0")
            .expect_err("unlisted image must be rejected")
            .to_string();
        assert!(
            err.contains(EXTRA_POSTGRES_IMAGES_ENV),
            "rejection message must mention {EXTRA_POSTGRES_IMAGES_ENV}, got: {err}"
        );
    }

    #[test]
    fn test_postgres_input_config_default_values() {
        let config = PostgresInputConfig {
            host: default_host(),
            port: None,
            database: default_database(),
            username: default_username(),
            password: None,
            max_connections: default_max_connections(),
            ssl_mode: default_ssl_mode(),
            docker_image: None,
            container_name: None,
        };

        let runtime_config: PostgresConfig = config.into();

        assert_eq!(runtime_config.host, "localhost");
        assert_eq!(runtime_config.database, "postgres");
        assert_eq!(runtime_config.username, "postgres");
        assert_eq!(runtime_config.max_connections, 100);
        assert_eq!(
            runtime_config.docker_image,
            "gotempsh/postgres-walg:18-bookworm"
        );
        assert!(runtime_config.password.len() >= 16); // Auto-generated password
    }

    #[test]
    fn test_postgres_input_config_custom_docker_image() {
        let config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "mydb".to_string(),
            username: "myuser".to_string(),
            password: Some("mypass".to_string()),
            max_connections: 50,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("timescale/timescaledb-ha:pg18".to_string()),
            container_name: None,
        };

        let runtime_config: PostgresConfig = config.into();

        assert_eq!(runtime_config.docker_image, "timescale/timescaledb-ha:pg18");
    }

    #[test]
    fn test_container_name_is_not_a_user_input() {
        // container_name is derived from the service name at creation time
        // (`postgres-{name}`), never supplied by the client — same as
        // MariaDB (see mariadb.rs's identical test). The create form is
        // generated from this schema, so the field must not appear in it.
        let schema = serde_json::to_value(schemars::schema_for!(PostgresInputConfig)).unwrap();
        assert!(
            !schema.to_string().contains("container_name"),
            "container_name leaked into the PostgreSQL create schema"
        );
    }

    #[test]
    fn test_validate_pg_username_accepts_realistic_names() {
        assert!(validate_pg_username("postgres").is_ok());
        assert!(validate_pg_username("appuser").is_ok());
        assert!(validate_pg_username("My_User123").is_ok());
        assert!(validate_pg_username("_svc").is_ok());
        assert!(validate_pg_username(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn test_validate_pg_username_rejects_shell_and_sed_injection() {
        // Empty / too long.
        assert!(validate_pg_username("").is_err());
        assert!(validate_pg_username(&"a".repeat(64)).is_err());
        // The exact break-out vector for the phase_restore sed filter: a
        // single quote closes the sed program's quoting.
        assert!(validate_pg_username("admin'").is_err());
        assert!(validate_pg_username("a'$(id)#").is_err());
        // Other shell/SQL metacharacters.
        assert!(validate_pg_username("u;drop").is_err());
        assert!(validate_pg_username("a b").is_err());
        assert!(validate_pg_username("a`b`").is_err());
        assert!(validate_pg_username("a$b").is_err());
        assert!(validate_pg_username("a-b").is_err());
    }
}
