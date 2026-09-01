// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Short-lived proof that a plugin is acting for a signed-in user.
//!
//! The platform channel lets a plugin call the platform's own HTTP API. That
//! is only safe if the platform knows *who* the call is for: without it, a
//! plugin would either act as nobody (and every `permission_guard!` would
//! fail) or as everybody (and the permission system would mean nothing).
//!
//! An *actor token* is the answer. The platform mints one when it proxies an
//! authenticated request to a plugin, alongside the `x-temps-*` identity
//! headers, and the plugin echoes it back on any channel call it makes while
//! serving that request. Because it is encrypted under the instance's auth
//! secret, a plugin cannot mint one for a user who never called it.
//!
//! Two properties are deliberate:
//!
//! - **Only the user id and a principal reference travel.** Role and
//!   permissions are re-read from the database when the token is redeemed,
//!   so a user demoted after the token was minted cannot keep the old access
//!   for the rest of its lifetime, and an API key's actual (possibly
//!   restricted) role/permissions are re-derived from the key row rather than
//!   inherited from `AuthContext::effective_role`.
//! - **The token names its plugin.** A token minted while serving plugin `a`
//!   is rejected on plugin `b`'s channel, so one plugin cannot replay
//!   another's credential to borrow its users.
//!
//! ## Why a principal, not just a user id (v2)
//!
//! v1 carried only `user_id`, and redemption rebuilt authority as a full
//! session (`AuthContext::new_session`) regardless of what actually
//! authenticated the proxied request. That was a privilege escalation for
//! anyone holding a role-restricted API key: the key's `role`/`permissions`
//! never reached the token, so a "read-only" key holder who triggered a
//! plugin call got the *user's* full role back. It also meant the token
//! outlived the session that produced it — signing out, or an admin
//! terminating sessions after a compromise, did nothing to a token already
//! in flight.
//!
//! v2 fixes both by binding the token to a concrete, re-checkable principal:
//! the browser session that authenticated the request (verified still valid
//! at redemption — not expired, not MFA-pending), or the API key that did
//! (verified still active, with its role/permissions re-read from the row
//! rather than trusted from the token). Anything that cannot be represented
//! this way — a CLI token today has no revocable backing row wired up in
//! this codebase — is refused at mint time rather than silently downgraded
//! to an unlinkable session-shaped grant.
//!
//! This lives in `temps-core` because both sides need it: the proxy mints,
//! the channel verifies, and neither should depend on the other.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::{CookieCrypto, CryptoError};

/// Domain-separation prefix. Keeps actor tokens from being interchangeable
/// with any other payload encrypted under the same key — notably preview
/// session grants, which use the same `CookieCrypto`.
///
/// Bumped from `plugin-actor-v1` alongside the principal-binding change:
/// tokens minted by an older build would otherwise deserialize as v2's
/// shorter, user-id-only shape and silently lose the principal check rather
/// than fail loudly. A version bump makes a mixed-build rollout fail closed
/// (`WrongVersion`) instead of ambiguously.
pub const PLUGIN_ACTOR_TOKEN_VERSION: &str = "plugin-actor-v2";

/// Default lifetime.
///
/// Long enough for a plugin to finish the work one request kicks off
/// (creating a project, an environment and a deployment in sequence), short
/// enough that a token captured from a plugin's memory is near-worthless.
pub const PLUGIN_ACTOR_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

/// Longest lifetime a minted token may be given.
pub const PLUGIN_ACTOR_TOKEN_MAX_TTL: Duration = Duration::from_secs(60 * 60);

/// Field separator inside the encrypted payload.
const SEP: char = '|';

/// The credential that authenticated the request the token was minted from.
///
/// Carried in the token as a tag + numeric id (`s<session_id>` /
/// `k<key_id>`) and re-verified against its backing row at redemption — the
/// id is a foreign key, never a secret, so storing it in the token leaks
/// nothing beyond what `x-temps-user-id` already does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorPrincipal {
    /// A browser session. Redemption must confirm the session still exists,
    /// is not expired, and is not MFA-pending — the same gate
    /// `verify_session_details` applies to a live cookie.
    Session { session_id: i32 },
    /// An API key. Redemption must confirm the key is still active and
    /// re-read its role/permissions from the row — never trust the ones the
    /// token was minted with, since the key may have been narrowed since.
    ApiKey { key_id: i32 },
}

impl ActorPrincipal {
    const SESSION_TAG: char = 's';
    const API_KEY_TAG: char = 'k';

    fn encode(self) -> String {
        match self {
            ActorPrincipal::Session { session_id } => {
                format!("{}{session_id}", Self::SESSION_TAG)
            }
            ActorPrincipal::ApiKey { key_id } => format!("{}{key_id}", Self::API_KEY_TAG),
        }
    }

    fn decode(field: &str) -> Option<Self> {
        let (tag, rest) = field.split_at_checked(1)?;
        let id: i32 = rest.parse().ok()?;
        match tag.chars().next()? {
            c if c == Self::SESSION_TAG => Some(ActorPrincipal::Session { session_id: id }),
            c if c == Self::API_KEY_TAG => Some(ActorPrincipal::ApiKey { key_id: id }),
            _ => None,
        }
    }
}

/// What a verified token authorises: the user, and the principal redemption
/// must independently re-check before honouring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedActor {
    pub user_id: i32,
    pub principal: ActorPrincipal,
}

#[derive(Debug, Error)]
pub enum PluginActorError {
    #[error(
        "Plugin name '{plugin}' contains the reserved field separator and cannot be bound \
         into an actor token"
    )]
    InvalidPluginName { plugin: String },

    #[error("Actor token expiry overflowed the system clock for plugin '{plugin}'")]
    ExpiryOverflow { plugin: String },

    #[error("System clock is before the Unix epoch while minting an actor token for '{plugin}'")]
    ClockBeforeUnixEpoch { plugin: String },

    #[error("Failed to encrypt an actor token for plugin '{plugin}': {source}")]
    Encryption {
        plugin: String,
        #[source]
        source: CryptoError,
    },

    #[error(
        "Plugin '{plugin}' presented an actor token that is not valid for this instance \
         (it did not decrypt under the instance auth secret)"
    )]
    Undecryptable { plugin: String },

    #[error(
        "Plugin '{plugin}' presented a malformed actor token: expected {expected} \
         '{SEP}'-separated fields, found {found}"
    )]
    Malformed {
        plugin: String,
        expected: usize,
        found: usize,
    },

    #[error(
        "Plugin '{plugin}' presented an actor token of version '{found}'; \
         this instance issues '{expected}'"
    )]
    WrongVersion {
        plugin: String,
        expected: &'static str,
        found: String,
    },

    #[error(
        "Plugin '{plugin}' presented an actor token minted for plugin '{minted_for}' — \
         a token may only be used by the plugin it was issued to"
    )]
    WrongPlugin { plugin: String, minted_for: String },

    #[error("Plugin '{plugin}' presented an actor token with an unreadable user id: '{found}'")]
    UnreadableUserId { plugin: String, found: String },

    #[error("Plugin '{plugin}' presented an actor token with an unreadable expiry: '{found}'")]
    UnreadableExpiry { plugin: String, found: String },

    #[error(
        "Plugin '{plugin}' presented an actor token with an unreadable principal: '{found}' \
         (expected 's<session_id>' or 'k<api_key_id>')"
    )]
    UnreadablePrincipal { plugin: String, found: String },

    #[error("Plugin '{plugin}' presented an actor token that expired {age_secs}s ago")]
    Expired { plugin: String, age_secs: u64 },
}

/// Mint a token letting `plugin` act as `user_id`, backed by `principal`.
///
/// `ttl` is clamped to [`PLUGIN_ACTOR_TOKEN_MAX_TTL`]. Returns the token and
/// its absolute expiry, so a caller can surface when it will lapse.
///
/// There is deliberately no way to mint a token without a principal. A
/// caller with an `AuthContext` it cannot express as one — a source with no
/// revocable backing row — should refuse to mint rather than fall back to
/// something unlinkable; see the module docs for why.
pub fn encode_plugin_actor_token(
    crypto: &CookieCrypto,
    plugin: &str,
    user_id: i32,
    principal: ActorPrincipal,
    ttl: Duration,
    now: SystemTime,
) -> Result<(String, u64), PluginActorError> {
    // A plugin name containing the separator could shift every later field,
    // letting a token claim a different user or an arbitrary expiry. Plugin
    // names are kebab-case in practice — refuse rather than rely on that.
    if plugin.contains(SEP) {
        return Err(PluginActorError::InvalidPluginName {
            plugin: plugin.to_string(),
        });
    }
    let exp = now
        .checked_add(ttl.min(PLUGIN_ACTOR_TOKEN_MAX_TTL))
        .ok_or_else(|| PluginActorError::ExpiryOverflow {
            plugin: plugin.to_string(),
        })?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PluginActorError::ClockBeforeUnixEpoch {
            plugin: plugin.to_string(),
        })?
        .as_secs();

    let principal = principal.encode();
    let payload = format!(
        "{PLUGIN_ACTOR_TOKEN_VERSION}{SEP}{plugin}{SEP}{user_id}{SEP}{exp}{SEP}{principal}"
    );
    let token = crypto
        .encrypt(&payload)
        .map_err(|source| PluginActorError::Encryption {
            plugin: plugin.to_string(),
            source,
        })?;
    Ok((token, exp))
}

/// Validate a token and return the actor it authorises.
///
/// Every failure names what was wrong: a plugin author debugging a rejected
/// call alone needs to tell "wrong instance" from "expired" from "not your
/// token", and a single boolean cannot.
///
/// This confirms only that the token was minted by this instance, for this
/// plugin, and has not expired. It does **not** confirm the principal is
/// still valid — the caller must independently re-check the session/API-key
/// row named by `VerifiedActor::principal` before treating the result as
/// authorization. That extra step is what makes revocation (sign-out,
/// deactivating a key, deleting a user) take effect immediately rather than
/// waiting out the token's TTL.
pub fn verify_plugin_actor_token(
    crypto: &CookieCrypto,
    token: &str,
    plugin: &str,
    now: SystemTime,
) -> Result<VerifiedActor, PluginActorError> {
    let payload = crypto
        .decrypt(token)
        .map_err(|_| PluginActorError::Undecryptable {
            plugin: plugin.to_string(),
        })?;

    let fields: Vec<&str> = payload.split(SEP).collect();
    if fields.len() != 5 {
        return Err(PluginActorError::Malformed {
            plugin: plugin.to_string(),
            expected: 5,
            found: fields.len(),
        });
    }
    if fields[0] != PLUGIN_ACTOR_TOKEN_VERSION {
        return Err(PluginActorError::WrongVersion {
            plugin: plugin.to_string(),
            expected: PLUGIN_ACTOR_TOKEN_VERSION,
            found: fields[0].to_string(),
        });
    }
    if fields[1] != plugin {
        return Err(PluginActorError::WrongPlugin {
            plugin: plugin.to_string(),
            minted_for: fields[1].to_string(),
        });
    }
    let user_id = fields[2]
        .parse::<i32>()
        .map_err(|_| PluginActorError::UnreadableUserId {
            plugin: plugin.to_string(),
            found: fields[2].to_string(),
        })?;
    let exp = fields[3]
        .parse::<u64>()
        .map_err(|_| PluginActorError::UnreadableExpiry {
            plugin: plugin.to_string(),
            found: fields[3].to_string(),
        })?;
    let principal =
        ActorPrincipal::decode(fields[4]).ok_or_else(|| PluginActorError::UnreadablePrincipal {
            plugin: plugin.to_string(),
            found: fields[4].to_string(),
        })?;

    let now_secs = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PluginActorError::ClockBeforeUnixEpoch {
            plugin: plugin.to_string(),
        })?
        .as_secs();
    if now_secs >= exp {
        return Err(PluginActorError::Expired {
            plugin: plugin.to_string(),
            age_secs: now_secs - exp,
        });
    }

    Ok(VerifiedActor { user_id, principal })
}

#[cfg(test)]
mod tests {
    use super::*;

    // `CookieCrypto` takes a raw 32-byte key or 64 hex chars; these are
    // exactly 32 bytes.
    fn crypto() -> CookieCrypto {
        CookieCrypto::new("actor-token-test-secret-key-0001").unwrap()
    }

    fn other_crypto() -> CookieCrypto {
        CookieCrypto::new("a-different-instance-auth-secret").unwrap()
    }

    const SESSION: ActorPrincipal = ActorPrincipal::Session { session_id: 7 };
    const API_KEY: ActorPrincipal = ActorPrincipal::ApiKey { key_id: 9 };

    #[test]
    fn a_minted_token_authorises_its_user_and_principal() {
        let c = crypto();
        let now = SystemTime::now();
        let (token, exp) = encode_plugin_actor_token(
            &c,
            "example-plugin",
            42,
            SESSION,
            PLUGIN_ACTOR_TOKEN_TTL,
            now,
        )
        .unwrap();
        assert!(exp > 0);
        let actor = verify_plugin_actor_token(&c, &token, "example-plugin", now).unwrap();
        assert_eq!(actor.user_id, 42);
        assert_eq!(actor.principal, SESSION);
    }

    /// An API-key-backed token round-trips its key id distinctly from a
    /// session id — the tag, not just the number, is what redemption keys
    /// its re-check on.
    #[test]
    fn an_api_key_principal_round_trips() {
        let c = crypto();
        let now = SystemTime::now();
        let (token, _) = encode_plugin_actor_token(
            &c,
            "example-plugin",
            42,
            API_KEY,
            PLUGIN_ACTOR_TOKEN_TTL,
            now,
        )
        .unwrap();
        let actor = verify_plugin_actor_token(&c, &token, "example-plugin", now).unwrap();
        assert_eq!(actor.principal, API_KEY);
    }

    /// The property that stops one plugin borrowing another's users.
    #[test]
    fn a_token_is_rejected_by_a_different_plugin() {
        let c = crypto();
        let now = SystemTime::now();
        let (token, _) = encode_plugin_actor_token(
            &c,
            "example-plugin",
            42,
            SESSION,
            PLUGIN_ACTOR_TOKEN_TTL,
            now,
        )
        .unwrap();
        let err = verify_plugin_actor_token(&c, &token, "some-other-plugin", now).unwrap_err();
        assert!(
            matches!(&err, PluginActorError::WrongPlugin { minted_for, .. } if minted_for == "example-plugin"),
            "{err}"
        );
    }

    /// The property that stops a token from one instance working on another.
    #[test]
    fn a_token_from_another_instance_does_not_decrypt() {
        let now = SystemTime::now();
        let (token, _) = encode_plugin_actor_token(
            &crypto(),
            "example-plugin",
            42,
            SESSION,
            PLUGIN_ACTOR_TOKEN_TTL,
            now,
        )
        .unwrap();
        let err =
            verify_plugin_actor_token(&other_crypto(), &token, "example-plugin", now).unwrap_err();
        assert!(
            matches!(err, PluginActorError::Undecryptable { .. }),
            "{err}"
        );
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let c = crypto();
        let minted_at = SystemTime::now();
        let (token, _) = encode_plugin_actor_token(
            &c,
            "example-plugin",
            42,
            SESSION,
            Duration::from_secs(30),
            minted_at,
        )
        .unwrap();
        let later = minted_at + Duration::from_secs(31);
        let err = verify_plugin_actor_token(&c, &token, "example-plugin", later).unwrap_err();
        assert!(matches!(err, PluginActorError::Expired { .. }), "{err}");
    }

    #[test]
    fn ttl_is_clamped_to_the_maximum() {
        let c = crypto();
        let now = SystemTime::now();
        let (_, exp) = encode_plugin_actor_token(
            &c,
            "example-plugin",
            42,
            SESSION,
            PLUGIN_ACTOR_TOKEN_MAX_TTL * 10,
            now,
        )
        .unwrap();
        let now_secs = now.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(exp <= now_secs + PLUGIN_ACTOR_TOKEN_MAX_TTL.as_secs());
    }

    /// A separator in the plugin name could shift the user-id field and let a
    /// token claim someone else.
    #[test]
    fn a_plugin_name_containing_the_separator_is_refused() {
        let err = encode_plugin_actor_token(
            &crypto(),
            "evil|example-plugin",
            42,
            SESSION,
            PLUGIN_ACTOR_TOKEN_TTL,
            SystemTime::now(),
        )
        .unwrap_err();
        assert!(
            matches!(err, PluginActorError::InvalidPluginName { .. }),
            "{err}"
        );
    }

    #[test]
    fn garbage_is_rejected_rather_than_panicking() {
        let err = verify_plugin_actor_token(
            &crypto(),
            "not-a-token",
            "example-plugin",
            SystemTime::now(),
        )
        .unwrap_err();
        assert!(
            matches!(err, PluginActorError::Undecryptable { .. }),
            "{err}"
        );
    }

    /// A v1 token (four fields, no principal) must not silently pass as if
    /// it carried one. The exact rejection reason is secondary — `Malformed`
    /// here, since the field count is checked before the version string —
    /// what matters is that a mixed-build rollout gets a loud, typed error
    /// rather than a token quietly redeemed with no principal to re-check.
    #[test]
    fn a_v1_shaped_token_is_rejected_rather_than_silently_accepted() {
        let c = crypto();
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 900;
        let v1_payload = format!("plugin-actor-v1{SEP}example-plugin{SEP}42{SEP}{now_secs}");
        let token = c.encrypt(&v1_payload).unwrap();
        let err =
            verify_plugin_actor_token(&c, &token, "example-plugin", SystemTime::now()).unwrap_err();
        assert!(matches!(&err, PluginActorError::Malformed { .. }), "{err}");
    }

    /// The principal field's tag must be one of the two known kinds — a
    /// digit that looks numeric but has no tag must not silently decode to
    /// some arbitrary variant.
    #[test]
    fn an_unrecognised_principal_tag_is_rejected() {
        assert_eq!(ActorPrincipal::decode("x7"), None);
        assert_eq!(ActorPrincipal::decode("s"), None);
        assert_eq!(ActorPrincipal::decode(""), None);
    }
}
