//! Provider catalog — single source of truth for how each AI CLI is
//! installed, authenticated, and seeded inside a sandbox container.
//!
//! Adding a new provider only requires:
//!   1. Append a `ProviderCatalogEntry` to [`PROVIDER_CATALOG`].
//!   2. Implement `AiCliProvider` in a new module under `ai_cli/`.
//!   3. Register it in [`super::create_provider`].
//!
//! No DB migrations, no UI changes, no schema bumps.

/// How the credential bytes should be delivered to the CLI inside the
/// sandbox. Each variant maps to a distinct seeding strategy in
/// `session_manager::seed_provider_credentials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialFormat {
    /// Single-line secret (API key) injected as an env var via `~/.env`.
    /// The catalog's `env_var` field names the variable.
    ApiKey,
    /// OAuth-style JSON credential file (Claude Code's
    /// `~/.claude/.credentials.json` shape). The decrypted bytes are wrapped
    /// in the canonical envelope before writing to `seed_path`.
    OauthToken,
    /// Arbitrary file body (OpenCode's `auth.json`, future providers' config
    /// files). Decrypted bytes are written verbatim to `seed_path`.
    ConfigFile,
}

/// Single auth flavor a provider supports. Most providers expose just one;
/// Claude exposes both (subscription OAuth and direct API key).
#[derive(Debug, Clone)]
pub struct AuthFlavor {
    /// Stable identifier stored in `ProviderConfig.auth_type`.
    pub id: &'static str,
    /// Human-readable label for the settings UI.
    pub label: &'static str,
    /// One-line explanation shown beneath the flavor toggle.
    pub description: &'static str,
    /// How to interpret `credentials_encrypted` for this flavor.
    pub format: CredentialFormat,
    /// Env-var name (used when `format == ApiKey`). Empty for other formats.
    pub env_var: &'static str,
    /// Path inside the sandbox where the credential file is written, **relative
    /// to the sandbox user's home dir** (used when `format != ApiKey`). Empty
    /// for `ApiKey` flavors. Stored relative so a future image with a
    /// different non-root user only requires editing
    /// `crate::sandbox::user::SANDBOX_HOME`. Use [`AuthFlavor::seed_path`] to
    /// resolve to an absolute path at the call site.
    pub seed_path_rel: &'static str,
}

impl AuthFlavor {
    /// Absolute path inside the sandbox where the credential file is written.
    /// Returns an empty string for `ApiKey` flavors that have no seed path.
    pub fn seed_path(&self) -> String {
        if self.seed_path_rel.is_empty() {
            String::new()
        } else {
            format!(
                "{}/{}",
                crate::sandbox::user::SANDBOX_HOME,
                self.seed_path_rel
            )
        }
    }
}

/// Static description of an AI CLI provider — install command, auth options,
/// UI metadata. Lives here so `session_manager`, the settings UI, and the
/// smoke-test handler all read from one place.
#[derive(Debug, Clone)]
pub struct ProviderCatalogEntry {
    /// Stable id stored in settings (`claude_cli`, `codex_cli`, `opencode`).
    pub id: &'static str,
    /// Display name for UI cards.
    pub name: &'static str,
    /// Shell command users run to install the CLI on their host.
    pub install_command: &'static str,
    /// Shell command users run to authenticate the CLI on their host.
    pub auth_command: &'static str,
    /// Auth flavors this provider supports, in display order. The first entry
    /// is the recommended default for new installs.
    pub auth_flavors: &'static [AuthFlavor],
    /// Model identifiers this provider accepts, in display order. The first
    /// entry is the recommended default. Empty when the provider doesn't
    /// expose model selection (e.g. OpenCode delegates model choice to its
    /// own per-session config). The settings UI renders these in the model
    /// dropdown for the *active* provider only.
    pub models: &'static [&'static str],
}

impl ProviderCatalogEntry {
    /// Look up an auth flavor by id. Returns `None` if the id isn't valid for
    /// this provider — caller should treat that as a configuration error.
    pub fn flavor(&self, id: &str) -> Option<&AuthFlavor> {
        self.auth_flavors.iter().find(|f| f.id == id)
    }

    /// Default auth flavor for this provider (the first entry). Used when a
    /// settings row was migrated from the legacy schema and the user hasn't
    /// picked a flavor yet.
    pub fn default_flavor(&self) -> &AuthFlavor {
        // SAFETY: every catalog entry must declare at least one flavor. This
        // is enforced by the `catalog_invariants` test below.
        &self.auth_flavors[0]
    }
}

/// All providers Temps knows how to install, authenticate, and seed.
///
/// Order matters: the settings UI renders providers in this order, and the
/// smoke-test endpoint iterates this list when checking which CLIs are
/// installed.
pub const PROVIDER_CATALOG: &[ProviderCatalogEntry] = &[
    ProviderCatalogEntry {
        id: "claude_cli",
        name: "Claude Code",
        install_command: "curl -fsSL https://claude.ai/install.sh | bash",
        auth_command: "claude setup-token",
        auth_flavors: &[
            AuthFlavor {
                id: "subscription",
                label: "Subscription (OAuth)",
                description:
                    "Claude Max/Pro — paste the OAuth token from `claude setup-token`.",
                format: CredentialFormat::OauthToken,
                env_var: "",
                seed_path_rel: ".claude/.credentials.json",
            },
            AuthFlavor {
                id: "api_key",
                label: "API Key",
                description: "Pay-per-use Anthropic API key (sk-ant-...).",
                format: CredentialFormat::ApiKey,
                env_var: "ANTHROPIC_API_KEY",
                seed_path_rel: "",
            },
        ],
        // Model IDs the Claude CLI accepts. Short aliases (`sonnet`/`opus`/
        // `haiku`) always pin to the latest release in that tier; the dated
        // IDs let users opt into a specific snapshot.
        models: &[
            "sonnet",
            "opus",
            "haiku",
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-haiku-4-5",
        ],
    },
    ProviderCatalogEntry {
        id: "codex_cli",
        name: "Codex (OpenAI)",
        install_command: "bun add -g @openai/codex",
        auth_command: "codex login",
        auth_flavors: &[
            AuthFlavor {
                id: "subscription",
                label: "Subscription (Sign in with ChatGPT)",
                description:
                    "ChatGPT Plus/Pro/Team/Enterprise — run `codex login` on your host, then paste the contents of `~/.codex/auth.json` here.",
                format: CredentialFormat::ConfigFile,
                env_var: "",
                seed_path_rel: ".codex/auth.json",
            },
            AuthFlavor {
                id: "api_key",
                label: "OpenAI API Key",
                description: "Pay-per-use OpenAI API key (sk-...).",
                format: CredentialFormat::ApiKey,
                env_var: "OPENAI_API_KEY",
                seed_path_rel: "",
            },
        ],
        // Model IDs the Codex CLI exposes via its `Select Model and Effort`
        // picker (run `codex` then `/model`). Verified against the CLI's
        // interactive menu — GPT-5.4 is the current frontier family, with
        // the `-codex` variants tuned for coding and `-max` trading latency
        // for depth. The `5.1` family is kept as a cheaper/faster fallback.
        // `gpt-5-codex` is kept as a legacy option but fails on many
        // ChatGPT accounts ("model not supported"), so it's not the default.
        models: &[
            "gpt-5.4",
            "gpt-5.4-codex",
            "gpt-5.4-codex-max",
            "gpt-5.2",
            "gpt-5.1-codex",
            "gpt-5.1-codex-mini",
            "gpt-5-codex",
        ],
    },
    ProviderCatalogEntry {
        id: "opencode",
        name: "OpenCode",
        install_command: "curl -fsSL https://opencode.ai/install | bash",
        auth_command: "opencode auth add",
        auth_flavors: &[AuthFlavor {
            id: "config_file",
            label: "Auth Config File",
            description:
                "Paste the contents of `~/.local/share/opencode/auth.json` from a host where you've already run `opencode auth add`.",
            format: CredentialFormat::ConfigFile,
            env_var: "",
            seed_path_rel: ".local/share/opencode/auth.json",
        }],
        // OpenCode picks its own model from `~/.config/opencode/config.json`
        // (or runtime `--model provider/id`). Leaving this empty tells the
        // settings UI to hide the model dropdown for OpenCode and surface a
        // hint that model selection lives in the OpenCode config instead.
        models: &[],
    },
];

/// Look up a provider by id. Returns `None` for unknown ids — callers
/// should reject those as a misconfiguration rather than silently fall back.
pub fn find_provider(id: &str) -> Option<&'static ProviderCatalogEntry> {
    PROVIDER_CATALOG.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every catalog entry must have a unique id and at least one auth
    /// flavor, otherwise `default_flavor()` will panic.
    #[test]
    fn catalog_invariants() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for entry in PROVIDER_CATALOG {
            assert!(!entry.id.is_empty(), "provider catalog has empty id");
            assert!(
                seen.insert(entry.id),
                "duplicate provider id in catalog: {}",
                entry.id
            );
            assert!(
                !entry.auth_flavors.is_empty(),
                "provider {} has no auth flavors",
                entry.id
            );
            for flavor in entry.auth_flavors {
                assert!(
                    !flavor.id.is_empty(),
                    "provider {} has flavor with empty id",
                    entry.id
                );
                if matches!(flavor.format, CredentialFormat::ApiKey) {
                    assert!(
                        !flavor.env_var.is_empty(),
                        "provider {} flavor {} declares ApiKey but no env_var",
                        entry.id,
                        flavor.id
                    );
                } else {
                    assert!(
                        !flavor.seed_path_rel.is_empty(),
                        "provider {} flavor {} needs a seed_path_rel for non-ApiKey format",
                        entry.id,
                        flavor.id
                    );
                }
            }
        }
    }

    #[test]
    fn find_provider_returns_known_ids() {
        assert!(find_provider("claude_cli").is_some());
        assert!(find_provider("codex_cli").is_some());
        assert!(find_provider("opencode").is_some());
        assert!(find_provider("nope").is_none());
    }

    #[test]
    fn claude_subscription_is_first_flavor() {
        let claude = find_provider("claude_cli").expect("claude_cli in catalog");
        assert_eq!(claude.default_flavor().id, "subscription");
    }

    #[test]
    fn codex_supports_subscription_and_api_key() {
        let codex = find_provider("codex_cli").expect("codex_cli in catalog");
        // Subscription is the recommended default — it's first in the list.
        assert_eq!(codex.default_flavor().id, "subscription");
        assert!(matches!(
            codex.default_flavor().format,
            CredentialFormat::ConfigFile
        ));
        let api_key = codex.flavor("api_key").expect("api_key flavor exists");
        assert_eq!(api_key.env_var, "OPENAI_API_KEY");
    }
}
