// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The token the proxy presents to the local preview gateway
//! (`X-Temps-Preview-Token`).
//!
//! The source of truth is the settings row, `preview_gateway.shared_secret`:
//! the gateway supervisor generates it on first boot, persists it there, and
//! configures the gateway container with it. The proxy already holds those
//! settings in `ConfigService`'s in-memory cache (reloaded on `NOTIFY`, 5 s
//! TTL as a safety net), so reading the secret from them costs nothing per
//! request and picks up a rotated secret through the same reload path as
//! every other setting.
//!
//! The supervisor also exports the secret to the process environment
//! (`PREVIEW_GATEWAY_SHARED_SECRET`). That copy is only consulted when the
//! settings value is empty — the degraded start-up where the supervisor could
//! not persist the secret and configured the gateway with an in-memory or
//! legacy-file value — and even then it is re-read at most once per
//! [`ENV_FALLBACK_REFRESH`], never per request.

use arc_swap::ArcSwap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Environment variable the preview gateway supervisor exports.
const PREVIEW_GATEWAY_SECRET_ENV: &str = "PREVIEW_GATEWAY_SHARED_SECRET";

/// How long a read of the environment fallback is reused.
const ENV_FALLBACK_REFRESH: Duration = Duration::from_secs(30);

/// A resolved gateway token, borrowed from the settings or shared from the
/// cached environment fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewToken<'a> {
    Settings(&'a str),
    Environment(Arc<str>),
}

impl PreviewToken<'_> {
    pub fn as_str(&self) -> &str {
        match self {
            PreviewToken::Settings(token) => token,
            PreviewToken::Environment(token) => token,
        }
    }
}

#[derive(Debug)]
struct EnvSnapshot {
    value: Option<Arc<str>>,
    read_at: Option<Instant>,
}

type EnvReader = Box<dyn Fn() -> Option<String> + Send + Sync>;

/// Resolves the preview gateway token without touching the environment on
/// the request path.
pub struct PreviewGatewayToken {
    env_fallback: ArcSwap<EnvSnapshot>,
    read_env: EnvReader,
    refresh: Duration,
}

impl Default for PreviewGatewayToken {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PreviewGatewayToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret itself.
        f.debug_struct("PreviewGatewayToken")
            .field(
                "env_fallback_set",
                &self.env_fallback.load().value.is_some(),
            )
            .finish()
    }
}

impl PreviewGatewayToken {
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap fallback for the preview gateway secret: cached in an ArcSwap and re-read at most every ENV_FALLBACK_REFRESH, never per request"
    )]
    pub fn new() -> Self {
        Self::with_env_reader(
            Box::new(|| std::env::var(PREVIEW_GATEWAY_SECRET_ENV).ok()),
            ENV_FALLBACK_REFRESH,
        )
    }

    fn with_env_reader(read_env: EnvReader, refresh: Duration) -> Self {
        Self {
            env_fallback: ArcSwap::from_pointee(EnvSnapshot {
                value: None,
                read_at: None,
            }),
            read_env,
            refresh,
        }
    }

    /// The token to send: the settings value when set, otherwise the cached
    /// environment fallback; `None` when neither has one.
    pub fn resolve<'a>(&self, settings_secret: &'a str) -> Option<PreviewToken<'a>> {
        if !settings_secret.is_empty() {
            return Some(PreviewToken::Settings(settings_secret));
        }
        self.env_fallback().map(PreviewToken::Environment)
    }

    fn env_fallback(&self) -> Option<Arc<str>> {
        let snapshot = self.env_fallback.load();
        let fresh = snapshot
            .read_at
            .is_some_and(|read_at| read_at.elapsed() < self.refresh);
        if fresh {
            return snapshot.value.clone();
        }
        // Concurrent requests may both re-read here; that is a bounded,
        // harmless duplicate (once per refresh window), not a per-request read.
        let value: Option<Arc<str>> = (self.read_env)()
            .map(|secret| secret.trim().to_string())
            .filter(|secret| !secret.is_empty())
            .map(Arc::from);
        self.env_fallback.store(Arc::new(EnvSnapshot {
            value: value.clone(),
            read_at: Some(Instant::now()),
        }));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn counting_reader(
        value: &'static Mutex<Option<String>>,
        reads: Arc<AtomicUsize>,
    ) -> EnvReader {
        Box::new(move || {
            reads.fetch_add(1, Ordering::SeqCst);
            value.lock().unwrap().clone()
        })
    }

    #[test]
    fn settings_secret_wins_and_never_reads_the_environment() {
        static ENV: Mutex<Option<String>> = Mutex::new(None);
        *ENV.lock().unwrap() = Some("from-env".to_string());
        let reads = Arc::new(AtomicUsize::new(0));
        let token = PreviewGatewayToken::with_env_reader(
            counting_reader(&ENV, Arc::clone(&reads)),
            Duration::from_secs(60),
        );

        for _ in 0..100 {
            let resolved = token.resolve("from-settings").unwrap();
            assert_eq!(resolved, PreviewToken::Settings("from-settings"));
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn environment_fallback_is_read_once_per_window() {
        static ENV: Mutex<Option<String>> = Mutex::new(None);
        *ENV.lock().unwrap() = Some(" from-env \n".to_string());
        let reads = Arc::new(AtomicUsize::new(0));
        let token = PreviewGatewayToken::with_env_reader(
            counting_reader(&ENV, Arc::clone(&reads)),
            Duration::from_secs(60),
        );

        for _ in 0..100 {
            assert_eq!(token.resolve("").unwrap().as_str(), "from-env");
        }
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn environment_fallback_picks_up_a_late_export() {
        // The supervisor exports the secret after the proxy starts serving.
        static ENV: Mutex<Option<String>> = Mutex::new(None);
        let reads = Arc::new(AtomicUsize::new(0));
        let token = PreviewGatewayToken::with_env_reader(
            counting_reader(&ENV, Arc::clone(&reads)),
            Duration::from_millis(10),
        );

        assert_eq!(token.resolve(""), None);
        *ENV.lock().unwrap() = Some("late".to_string());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(token.resolve("").unwrap().as_str(), "late");
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn empty_everywhere_resolves_to_none() {
        static ENV: Mutex<Option<String>> = Mutex::new(None);
        *ENV.lock().unwrap() = Some("   ".to_string());
        let token = PreviewGatewayToken::with_env_reader(
            counting_reader(&ENV, Arc::new(AtomicUsize::new(0))),
            Duration::from_secs(60),
        );
        assert_eq!(token.resolve(""), None);
    }

    #[test]
    fn debug_output_does_not_contain_the_secret() {
        static ENV: Mutex<Option<String>> = Mutex::new(None);
        *ENV.lock().unwrap() = Some("super-secret-value".to_string());
        let token = PreviewGatewayToken::with_env_reader(
            counting_reader(&ENV, Arc::new(AtomicUsize::new(0))),
            Duration::from_secs(60),
        );
        let _ = token.resolve("");
        assert!(!format!("{token:?}").contains("super-secret-value"));
    }
}
