// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preview URLs: password, access mode, domain and share links.

use super::*;

/// The outcome of [`SandboxService::set_preview_access`]: the updated row
/// plus what it replaced, for the caller's audit record.
#[derive(Debug, Clone)]
pub struct PreviewAccessChange {
    pub previous_access: SandboxPreviewAccess,
    pub previous_allowed_cidrs: Vec<String>,
    pub sandbox: sandboxes::Model,
}

impl SandboxService {
    /// Compute preview URL parts once per call. Platform settings can
    /// change while the server is live (admin edits), so we don't cache.
    pub async fn preview_parts(&self) -> PreviewUrlParts {
        preview_urls::load(&self.platform_config).await
    }

    // ── Preview password ────────────────────────────────────────────────

    /// Set (or rotate) the preview password for a sandbox. The plaintext
    /// is hashed with argon2id and only the hash + last-4 hint are stored.
    /// Returns the hint to the caller — the plaintext is never persisted
    /// or echoed back (the caller already has it).
    ///
    /// Rotating an existing password invalidates every live preview cookie
    /// immediately: the proxy folds a digest of the argon2 hash into the
    /// cookie payload, so a new hash = a new fingerprint = every existing
    /// cookie fails verification.
    pub async fn set_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
        plaintext: &str,
    ) -> Result<String, SandboxError> {
        crate::services::preview_password::validate(plaintext)
            .map_err(|message| SandboxError::Validation { message })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let hp = crate::services::preview_password::hash_password(plaintext).map_err(|reason| {
            SandboxError::PasswordHashFailed {
                sandbox_id: public_id_value.to_string(),
                reason,
            }
        })?;
        let sandbox_id = row.id;
        let previous_access = row.preview_access;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(Some(hp.hash));
        active.preview_password_hint = Set(Some(hp.hint.clone()));
        // Setting a password is choosing the password wall.
        active.preview_access = Set(SandboxPreviewAccess::Password);
        active.update(self.db.as_ref()).await?;
        self.record_event(
            sandbox_id,
            "preview_password_set",
            Some(serde_json::json!({
                "previous_access": previous_access.as_str(),
                "access": SandboxPreviewAccess::Password.as_str(),
            })),
        )
        .await;
        Ok(hp.hint)
    }

    /// Remove the preview password. A sandbox in `password` mode returns to
    /// `temps_login` -- never to an open preview. Idempotent: clearing an
    /// already-unset password is a no-op, not an error. Returns the access
    /// mode the sandbox has afterwards.
    pub async fn clear_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<SandboxPreviewAccess, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let had_password = row.preview_password_hash.is_some();
        let previous_access = row.preview_access;
        let access = access_after_clearing_password(previous_access);
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(None);
        active.preview_password_hint = Set(None);
        active.preview_access = Set(access);
        active.update(self.db.as_ref()).await?;
        if had_password {
            self.record_event(
                sandbox_id,
                "preview_password_cleared",
                Some(serde_json::json!({
                    "previous_access": previous_access.as_str(),
                    "access": access.as_str(),
                })),
            )
            .await;
        }
        Ok(access)
    }

    /// Change who may open the sandbox's preview URLs and from which
    /// networks.
    ///
    /// `allowed_cidrs` replaces the stored allow-list (empty = any address)
    /// after validation and normalisation. `password` is refused while no
    /// preview password is set: the mode would admit nobody, and the caller
    /// almost certainly meant to set one first. The proxy picks a change up
    /// within its sandbox lookup cache TTL (30 s).
    pub async fn set_preview_access(
        &self,
        public_id_value: &str,
        user_id: i32,
        access: SandboxPreviewAccess,
        allowed_cidrs: &[String],
    ) -> Result<PreviewAccessChange, SandboxError> {
        let allowed_cidrs = crate::services::preview_access::normalize_allowed_cidrs(allowed_cidrs)
            .map_err(|message| SandboxError::Validation {
                message: format!(
                    "Invalid preview allow-list for sandbox {public_id_value}: {message}"
                ),
            })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if access == SandboxPreviewAccess::Password && row.preview_password_hash.is_none() {
            return Err(SandboxError::Validation {
                message: format!(
                    "Sandbox {public_id_value} has no preview password, so `password` access \
                     would admit nobody. Set one first with PUT \
                     /v1/sandboxes/{public_id_value}/preview-password (which also switches \
                     the sandbox to `password` access)."
                ),
            });
        }
        let previous_access = row.preview_access;
        let previous_cidrs = row.preview_allowed_cidrs.clone();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_access = Set(access);
        active.preview_allowed_cidrs = Set(allowed_cidrs.clone());
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(
            sandbox_id,
            "preview_access_changed",
            Some(serde_json::json!({
                "previous_access": previous_access.as_str(),
                "access": access.as_str(),
                "previous_allowed_cidrs": previous_cidrs,
                "allowed_cidrs": allowed_cidrs,
            })),
        )
        .await;
        Ok(PreviewAccessChange {
            previous_access,
            previous_allowed_cidrs: previous_cidrs,
            sandbox: updated,
        })
    }

    /// Read-only preview renewal preflight. A stale `running` row is not enough:
    /// the provider runtime must still be alive. This never wakes a workspace.
    pub async fn preview_ready_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if row.status != "running" || self.registry.get(row.id, public_id_value).await.is_err() {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: "unavailable".into(),
                operation: "renew preview access while the workspace is running".into(),
            });
        }
        Ok(row)
    }

    // ── Preview URL (`sandbox.domain(port)`) ─────────────────────────────

    /// Resolve the public URL for a port inside the sandbox. Returns the
    /// same `ws-<id>-<port>.<domain>` hostname the preview gateway already
    /// routes for workspace sessions, so standalone sandboxes don't
    /// require any gateway changes.
    ///
    /// Validation: `port` must be in [1, 65535]. Port `0` is rejected
    /// because the gateway matches exact numbers — surfacing a URL with
    /// `port=0` would be useless.
    pub async fn domain(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
    ) -> Result<String, SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        // Ownership + validity check. The numeric id is intentionally
        // discarded — the preview URL never embeds it. No wake: building a
        // URL string does not need the container running.
        let _ = self.resolve_id_no_wake(public_id_value, user_id).await?;
        let parts = self.preview_parts().await;
        Ok(parts.url_for(public_id_value, port))
    }

    /// Build a shareable preview link that carries its own authorization.
    ///
    /// `domain()` returns the bare preview URL, which is useless to anyone who
    /// does not already hold the preview password — so sharing a protected
    /// preview meant sharing the password itself, which then cannot be revoked
    /// for one recipient. This returns that URL plus a minted `session_grant`
    /// aimed at the login bridge: the recipient's browser exchanges it for the
    /// ordinary preview cookie and lands on `path`.
    ///
    /// The grant is scoped to this one sandbox, expires, and is never
    /// forwarded to the sandbox itself.
    pub async fn preview_share_link(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
        path: &str,
        ttl: Duration,
    ) -> Result<(String, u64), SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        // No wake: minting a share link reads the row's password hash.
        let (row, _) = self.resolve_id_no_wake(public_id_value, user_id).await?;
        // A share link stands in for the password, so it only exists while
        // the password wall is the sandbox's access mode.
        let password_hash = row
            .preview_password_hash
            .as_deref()
            .filter(|_| row.preview_access == SandboxPreviewAccess::Password)
            .ok_or_else(|| SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: "preview password disabled".into(),
                operation: "mint an expiring preview link; set a preview password first".into(),
            })?;

        let now = SystemTime::now();
        let (grant, expires_at) = temps_core::encode_preview_session_grant(
            &self.cookie_crypto,
            public_id_value,
            password_hash,
            ttl,
            now,
        )
        .map_err(|source| SandboxError::PreviewGrantFailed {
            sandbox_id: public_id_value.to_string(),
            source,
        })?;
        let granted_ttl = ttl.min(temps_core::PREVIEW_SESSION_GRANT_MAX_TTL);

        let next = temps_core::sanitize_preview_next(path);

        let base = self.preview_parts().await.url_for(public_id_value, port);
        let url = format!(
            "{}/__temps/preview/login?grant=1&next={}#session_grant={}",
            base.trim_end_matches('/'),
            url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>(),
            url::form_urlencoded::byte_serialize(grant.as_bytes()).collect::<String>(),
        );
        self.record_event(
            row.id,
            "preview_share_link_created",
            Some(serde_json::json!({
                "actor_user_id": user_id,
                "port": port,
                "path": next,
                "ttl_seconds": granted_ttl.as_secs(),
                "expires_at": expires_at,
            })),
        )
        .await;
        Ok((url, expires_at))
    }
}
