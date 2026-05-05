//! IPC protocol between the helper (uid 1000) and the daemon (uid 1001)
//! over a Unix socket.
//!
//! Wire format: one JSON object per line. Helper writes one
//! [`IpcRequest`], daemon writes one [`IpcResponse`], connection closes.
//! Keep it simple — there's no pipelining, no streaming; every git op
//! gets a fresh socket connection.

use serde::{Deserialize, Serialize};

use crate::Operation;

/// Helper → daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcRequest {
    /// `git ... get` — daemon should mint a fresh credential and return
    /// it. The bulk of traffic on this socket.
    Get {
        host: String,
        owner: String,
        repo: String,
        operation: Operation,
    },
    /// `git ... store` — git wants us to remember a credential it just
    /// used successfully. Daemon ignores the store payload (we mint
    /// fresh creds every time, so there's nothing to remember) and
    /// returns success. Important: do NOT let user code inject creds
    /// into our pool by sending a `store` with attacker-controlled
    /// values.
    Store,
    /// `git ... erase` — git wants us to forget a credential. Daemon
    /// invalidates any cache entry it might have for this repo so the
    /// next `get` re-mints rather than returning the stale one.
    Erase {
        host: String,
        owner: String,
        repo: String,
    },
}

/// Daemon → helper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcResponse {
    /// Success on `get`. Helper writes these straight back to git.
    Credential {
        username: String,
        /// The token. Treat as opaque; the helper logs nothing about it.
        password: String,
    },
    /// Success on `store`/`erase`, or a successful `get` for which the
    /// daemon has nothing to return (in which case the helper should
    /// fall through silently and let git try the next helper or fail).
    Ok,
    /// Daemon refuses to serve this request. `reason` is a short
    /// human-readable string, written by the helper to stderr so the
    /// user sees *why* git's auth failed rather than a silent 401.
    Refused { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JSON-line wire shape for `get` requests must round-trip cleanly,
    /// because both endpoints depend on serde for deserialization. A
    /// shape change here is a breaking protocol change — the test
    /// freezes the contract.
    #[test]
    fn get_request_json_shape() {
        let req = IpcRequest::Get {
            host: "github.com".into(),
            owner: "acme".into(),
            repo: "web".into(),
            operation: Operation::Fetch,
        };
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["kind"], "get");
        assert_eq!(v["host"], "github.com");
        assert_eq!(v["operation"], "fetch");

        let parsed: IpcRequest = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, req);
    }

    /// `Refused` responses must surface the reason — silent refusals
    /// would force ops to dig through daemon logs to figure out auth
    /// failures.
    #[test]
    fn refused_response_carries_reason() {
        let resp = IpcResponse::Refused {
            reason: "cross-project request denied".into(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["kind"], "refused");
        assert_eq!(v["reason"], "cross-project request denied");
    }
}
