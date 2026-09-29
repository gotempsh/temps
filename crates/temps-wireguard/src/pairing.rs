//! One-paste node pairing (ADR 048 D2b).
//!
//! A control plane that nodes cannot dial still enrolls a node that it can:
//! the operator enters the node's address in the control plane, runs the
//! resulting `temps join --pair <code>` on the node, and the control plane
//! fetches the node's WireGuard public key itself over the mesh UDP port,
//! before WireGuard owns that port. Private keys never leave their host:
//! only public keys cross the wire.
//!
//! The exchange, on the node's mesh port:
//!
//! ```text
//! control plane -> node   HELLO   { pairing id, nonce_cp, control-plane public key } (padded)
//! node -> control plane   OFFER   { pairing id, nonce_cp, nonce_node, node public key }
//! control plane -> node   CONFIRM { pairing id, nonce_node }
//!                   or    REJECT  { pairing id, nonce_node, reason }
//! ```
//!
//! The control plane confirms only once it has stored the node's key; it
//! rejects a key it will not accept (one another node already uses, or a
//! pairing that closed meanwhile) so the node can say why instead of
//! bringing up a mesh nobody peers with.
//!
//! Every message carries an HMAC-SHA256 under a key derived from the
//! pairing secret in the code, so each side proves it holds the code and
//! nothing can be swapped in transit. Nothing needs to be confidential:
//! only public keys are exchanged. The node answers nothing without a valid
//! MAC, and a HELLO is padded to be larger than the OFFER it causes, so the
//! port cannot be used for amplification.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as BASE64URL},
    Engine,
};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::time::Instant;

type HmacSha256 = Hmac<Sha256>;

const MAGIC: &[u8; 8] = b"TMPSPAIR";
const VERSION: u8 = 1;
const KIND_HELLO: u8 = 1;
const KIND_OFFER: u8 = 2;
const KIND_CONFIRM: u8 = 3;
const KIND_REJECT: u8 = 4;
const HEADER_LEN: usize = MAGIC.len() + 2 + PAIRING_ID_LEN;
const PAIRING_ID_LEN: usize = 16;
const NONCE_LEN: usize = 16;
const KEY_LEN: usize = 32;
const MAC_LEN: usize = 32;
/// HELLOs are padded to this size, above every reply's size.
const HELLO_LEN: usize = 160;
const OFFER_LEN: usize = HEADER_LEN + 2 * NONCE_LEN + KEY_LEN + MAC_LEN;
const CONFIRM_LEN: usize = HEADER_LEN + NONCE_LEN + MAC_LEN;
const REJECT_LEN: usize = HEADER_LEN + NONCE_LEN + 1 + MAC_LEN;
// A reply must never be larger than the HELLO that caused it.
const _: () = assert!(OFFER_LEN < HELLO_LEN && CONFIRM_LEN < HELLO_LEN && REJECT_LEN < HELLO_LEN);
const CODE_PREFIX: &str = "tpair1.";
const KDF_INFO: &[u8] = b"temps-pair-v1";

/// How often the control plane repeats a HELLO while it waits for an OFFER.
pub const HELLO_INTERVAL: Duration = Duration::from_secs(1);
/// How long a node keeps answering after its last OFFER when no CONFIRM
/// arrives: the control plane stops sending HELLOs once it has the key, so
/// silence this long means it got the OFFER and lost every confirmation.
pub const CONFIRM_GRACE: Duration = Duration::from_secs(20);
/// Confirmations (or rejections) sent per exchange (UDP may drop some).
const CONFIRM_REPEATS: usize = 3;

/// Why the control plane refused a node's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Another node, a pending pairing or the control plane already uses
    /// this key: the node's key file was copied from another machine.
    KeyInUse,
    /// The pairing was cancelled, expired or already completed.
    PairingClosed,
    /// A reason this build does not know.
    Other,
}

impl RejectReason {
    fn to_byte(self) -> u8 {
        match self {
            Self::KeyInUse => 1,
            Self::PairingClosed => 2,
            Self::Other => 0,
        }
    }

    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::KeyInUse,
            2 => Self::PairingClosed,
            _ => Self::Other,
        }
    }
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::KeyInUse => "this machine's WireGuard key already belongs to another node",
            Self::PairingClosed => "the pairing was cancelled, expired or already completed",
            Self::Other => "the control plane refused this machine's key",
        })
    }
}

/// Why [`initiate`]'s `accept` did not take the node's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRefusal {
    /// Tell the node, so it stops and says why.
    Reject(RejectReason),
    /// Say nothing (the node keeps answering) and try again later, e.g. the
    /// key could not be stored yet.
    Defer(String),
}

#[derive(Debug, Error)]
pub enum PairingError {
    #[error("the pairing code is invalid: {0}")]
    InvalidCode(String),
    #[error("the pairing code expired")]
    Expired,
    #[error("no valid pairing message arrived before the deadline")]
    TimedOut,
    #[error("the control plane rejected the pairing: {0}")]
    Rejected(RejectReason),
    #[error("the node's key was not recorded yet: {0}")]
    Deferred(String),
    #[error("pairing I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not generate pairing randomness: {0}")]
    Randomness(String),
}

/// What `temps join --pair` needs: everything to bring the node's end of the
/// mesh up with the control plane as its only peer, and to register over it.
/// Pasted onto the node, so it is a secret until used.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    /// Identifies the pending pairing (16 bytes, base64url).
    pub id: String,
    /// The 32-byte pairing secret (base64url).
    pub secret: String,
    /// The name the node registers under (its enrollment token is bound to it).
    pub name: String,
    pub control_plane_public_key: String,
    /// `None` when nodes cannot dial the control plane (it dials them).
    pub control_plane_endpoint: Option<String>,
    pub control_plane_address: Ipv4Addr,
    pub node_address: Ipv4Addr,
    /// The node's WireGuard endpoint as the operator entered it.
    pub node_endpoint: SocketAddr,
    pub prefix_len: u8,
    pub listen_port: u16,
    /// Where the control plane serves the node API on its mesh address.
    pub node_api_port: u16,
    /// SHA-256 fingerprint of the cluster CA the node API presents.
    pub ca_fingerprint: String,
    /// Single-use join token for registering over the mesh.
    pub join_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCode")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("control_plane_address", &self.control_plane_address)
            .field("node_address", &self.node_address)
            .field("node_endpoint", &self.node_endpoint)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl PairingCode {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("a pairing code always serializes");
        format!("{CODE_PREFIX}{}", BASE64URL.encode(json))
    }

    pub fn decode(value: &str) -> Result<Self, PairingError> {
        let body = value.trim().strip_prefix(CODE_PREFIX).ok_or_else(|| {
            PairingError::InvalidCode(format!("it must start with {CODE_PREFIX}"))
        })?;
        let json = BASE64URL
            .decode(body)
            .map_err(|error| PairingError::InvalidCode(error.to_string()))?;
        let code: Self = serde_json::from_slice(&json)
            .map_err(|error| PairingError::InvalidCode(error.to_string()))?;
        code.pairing_id()?;
        code.pairing_secret()?;
        decode_public_key(&code.control_plane_public_key)?;
        Ok(code)
    }

    pub fn pairing_id(&self) -> Result<PairingId, PairingError> {
        PairingId::from_base64url(&self.id)
    }

    pub fn pairing_secret(&self) -> Result<PairingSecret, PairingError> {
        PairingSecret::from_base64url(&self.secret)
    }

    pub fn is_expired(&self, now_unix: i64) -> bool {
        now_unix >= self.expires_at
    }
}

/// Identifies one pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingId([u8; PAIRING_ID_LEN]);

impl PairingId {
    pub fn generate() -> Result<Self, PairingError> {
        Ok(Self(random()?))
    }

    pub fn to_base64url(&self) -> String {
        BASE64URL.encode(self.0)
    }

    pub fn from_base64url(value: &str) -> Result<Self, PairingError> {
        Ok(Self(decode_fixed(value, "pairing id")?))
    }
}

/// The secret both ends prove they hold.
#[derive(Clone)]
pub struct PairingSecret([u8; 32]);

impl std::fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingSecret(..)")
    }
}

impl PairingSecret {
    pub fn generate() -> Result<Self, PairingError> {
        Ok(Self(random()?))
    }

    pub fn to_base64url(&self) -> String {
        BASE64URL.encode(self.0)
    }

    pub fn from_base64url(value: &str) -> Result<Self, PairingError> {
        Ok(Self(decode_fixed(value, "pairing secret")?))
    }

    /// The MAC key for one pairing: HKDF-SHA256 with the pairing id as salt.
    fn mac_key(&self, id: &PairingId) -> [u8; 32] {
        let mut key = [0u8; 32];
        hkdf::Hkdf::<Sha256>::new(Some(&id.0), &self.0)
            .expand(KDF_INFO, &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        key
    }
}

/// One side's view of a pairing: its id and MAC key.
#[derive(Clone)]
pub struct PairingSession {
    id: PairingId,
    mac_key: [u8; 32],
}

impl PairingSession {
    pub fn new(id: PairingId, secret: &PairingSecret) -> Self {
        Self {
            mac_key: secret.mac_key(&id),
            id,
        }
    }

    fn mac(&self) -> HmacSha256 {
        HmacSha256::new_from_slice(&self.mac_key).expect("HMAC takes a key of any length")
    }

    fn seal(&self, kind: u8, body: &[&[u8]], padded_len: usize) -> Vec<u8> {
        let mut message = Vec::with_capacity(padded_len.max(HEADER_LEN + MAC_LEN));
        message.extend_from_slice(MAGIC);
        message.push(VERSION);
        message.push(kind);
        message.extend_from_slice(&self.id.0);
        for part in body {
            message.extend_from_slice(part);
        }
        message.resize(padded_len.saturating_sub(MAC_LEN).max(message.len()), 0);
        let mut mac = self.mac();
        mac.update(&message);
        message.extend_from_slice(&mac.finalize().into_bytes());
        message
    }

    /// The body of a `kind` message of exactly `len` bytes, if its header
    /// names this pairing and its MAC verifies.
    fn open<'a>(&self, kind: u8, len: usize, message: &'a [u8]) -> Option<&'a [u8]> {
        if message.len() != len
            || &message[..MAGIC.len()] != MAGIC
            || message[MAGIC.len()] != VERSION
            || message[MAGIC.len() + 1] != kind
            || message[MAGIC.len() + 2..HEADER_LEN] != self.id.0
        {
            return None;
        }
        let (signed, tag) = message.split_at(len - MAC_LEN);
        let mut mac = self.mac();
        mac.update(signed);
        mac.verify_slice(tag).ok()?;
        Some(&signed[HEADER_LEN..])
    }

    fn hello(&self, nonce_cp: &[u8; NONCE_LEN], cp_public_key: &[u8; KEY_LEN]) -> Vec<u8> {
        self.seal(KIND_HELLO, &[nonce_cp, cp_public_key], HELLO_LEN)
    }

    /// `(nonce_cp, control-plane public key)`.
    fn open_hello(&self, message: &[u8]) -> Option<([u8; NONCE_LEN], [u8; KEY_LEN])> {
        let body = self.open(KIND_HELLO, HELLO_LEN, message)?;
        Some((
            body[..NONCE_LEN].try_into().ok()?,
            body[NONCE_LEN..NONCE_LEN + KEY_LEN].try_into().ok()?,
        ))
    }

    fn offer(
        &self,
        nonce_cp: &[u8; NONCE_LEN],
        nonce_node: &[u8; NONCE_LEN],
        node_public_key: &[u8; KEY_LEN],
    ) -> Vec<u8> {
        self.seal(
            KIND_OFFER,
            &[nonce_cp, nonce_node, node_public_key],
            OFFER_LEN,
        )
    }

    /// `(nonce_cp, nonce_node, node public key)`.
    fn open_offer(
        &self,
        message: &[u8],
    ) -> Option<([u8; NONCE_LEN], [u8; NONCE_LEN], [u8; KEY_LEN])> {
        let body = self.open(KIND_OFFER, OFFER_LEN, message)?;
        Some((
            body[..NONCE_LEN].try_into().ok()?,
            body[NONCE_LEN..2 * NONCE_LEN].try_into().ok()?,
            body[2 * NONCE_LEN..2 * NONCE_LEN + KEY_LEN]
                .try_into()
                .ok()?,
        ))
    }

    fn confirm(&self, nonce_node: &[u8; NONCE_LEN]) -> Vec<u8> {
        self.seal(KIND_CONFIRM, &[nonce_node], CONFIRM_LEN)
    }

    fn open_confirm(&self, message: &[u8]) -> Option<[u8; NONCE_LEN]> {
        let body = self.open(KIND_CONFIRM, CONFIRM_LEN, message)?;
        body[..NONCE_LEN].try_into().ok()
    }

    fn reject(&self, nonce_node: &[u8; NONCE_LEN], reason: RejectReason) -> Vec<u8> {
        self.seal(KIND_REJECT, &[nonce_node, &[reason.to_byte()]], REJECT_LEN)
    }

    /// `(nonce_node, reason)`.
    fn open_reject(&self, message: &[u8]) -> Option<([u8; NONCE_LEN], RejectReason)> {
        let body = self.open(KIND_REJECT, REJECT_LEN, message)?;
        Some((
            body[..NONCE_LEN].try_into().ok()?,
            RejectReason::from_byte(body[NONCE_LEN]),
        ))
    }
}

/// Node side: answer the control plane's HELLOs on `socket` (bound to the
/// mesh port) with this node's public key until a CONFIRM arrives, or until
/// [`CONFIRM_GRACE`] passes without another HELLO after an OFFER. Returns the
/// address the control plane paired from, or [`PairingError::Rejected`] when
/// the control plane refuses the key. Messages that do not verify are
/// ignored without a reply.
pub async fn respond(
    socket: &UdpSocket,
    session: &PairingSession,
    expected_control_plane_key: &str,
    node_public_key: &str,
    deadline: Instant,
) -> Result<SocketAddr, PairingError> {
    let expected_cp = decode_public_key(expected_control_plane_key)?;
    let node_key = decode_public_key(node_public_key)?;
    let nonce_node: [u8; NONCE_LEN] = random()?;
    let mut offered: Option<(SocketAddr, Instant)> = None;
    let mut buffer = [0u8; 512];
    loop {
        let wait_until = match offered {
            Some((_, at)) => (at + CONFIRM_GRACE).min(deadline),
            None => deadline,
        };
        let received = tokio::time::timeout_at(wait_until, socket.recv_from(&mut buffer)).await;
        let (len, from) = match received {
            Ok(result) => result?,
            Err(_) => {
                return match offered {
                    Some((from, _)) => Ok(from),
                    None => Err(PairingError::TimedOut),
                }
            }
        };
        let message = &buffer[..len];
        if let Some((nonce_cp, cp_key)) = session.open_hello(message) {
            if cp_key != expected_cp {
                continue;
            }
            socket
                .send_to(&session.offer(&nonce_cp, &nonce_node, &node_key), from)
                .await?;
            offered = Some((from, Instant::now()));
        } else if session.open_confirm(message) == Some(nonce_node) {
            return Ok(from);
        } else if let Some((nonce, reason)) = session.open_reject(message) {
            if nonce == nonce_node {
                return Err(PairingError::Rejected(reason));
            }
        }
    }
}

/// Control-plane side: send HELLOs to `node` every [`HELLO_INTERVAL`] until
/// a verified OFFER arrives or `deadline` passes, then hand the node's
/// WireGuard public key (base64, as WireGuard writes it) to `accept`. The
/// node is confirmed only when `accept` took the key, and told why when it
/// rejected it. Returns the accepted key.
pub async fn initiate<F, Fut>(
    node: SocketAddr,
    session: &PairingSession,
    control_plane_public_key: &str,
    deadline: Instant,
    accept: F,
) -> Result<String, PairingError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), KeyRefusal>>,
{
    let cp_key = decode_public_key(control_plane_public_key)?;
    let bind: SocketAddr = if node.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(bind).await?;
    let nonce_cp: [u8; NONCE_LEN] = random()?;
    let hello = session.hello(&nonce_cp, &cp_key);
    let mut buffer = [0u8; 512];
    loop {
        if Instant::now() >= deadline {
            return Err(PairingError::TimedOut);
        }
        socket.send_to(&hello, node).await?;
        let resend_at = (Instant::now() + HELLO_INTERVAL).min(deadline);
        while let Ok(received) =
            tokio::time::timeout_at(resend_at, socket.recv_from(&mut buffer)).await
        {
            let (len, from) = received?;
            if from != node {
                continue;
            }
            let Some((echoed, nonce_node, node_key)) = session.open_offer(&buffer[..len]) else {
                continue;
            };
            if echoed != nonce_cp {
                continue;
            }
            let node_key = BASE64.encode(node_key);
            let (reply, outcome) = match accept(node_key.clone()).await {
                Ok(()) => (session.confirm(&nonce_node), Ok(node_key)),
                Err(KeyRefusal::Reject(reason)) => (
                    session.reject(&nonce_node, reason),
                    Err(PairingError::Rejected(reason)),
                ),
                Err(KeyRefusal::Defer(why)) => return Err(PairingError::Deferred(why)),
            };
            for _ in 0..CONFIRM_REPEATS {
                socket.send_to(&reply, node).await?;
            }
            return outcome;
        }
    }
}

fn random<const N: usize>() -> Result<[u8; N], PairingError> {
    let mut bytes = [0u8; N];
    temps_core::ecies::fill_secure_random_bytes("generating pairing randomness", &mut bytes)
        .map_err(|error| PairingError::Randomness(error.to_string()))?;
    Ok(bytes)
}

fn decode_fixed<const N: usize>(value: &str, what: &str) -> Result<[u8; N], PairingError> {
    BASE64URL
        .decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| PairingError::InvalidCode(format!("the {what} is malformed")))
}

fn decode_public_key(value: &str) -> Result<[u8; KEY_LEN], PairingError> {
    BASE64
        .decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| PairingError::InvalidCode("a WireGuard public key is malformed".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (PairingSession, PairingId, PairingSecret) {
        let id = PairingId::generate().unwrap();
        let secret = PairingSecret::generate().unwrap();
        (PairingSession::new(id, &secret), id, secret)
    }

    fn key(byte: u8) -> String {
        BASE64.encode([byte; KEY_LEN])
    }

    #[test]
    fn a_hello_is_larger_than_any_reply() {
        let (session, _, _) = session();
        let hello = session.hello(&[1; NONCE_LEN], &[2; KEY_LEN]);
        assert_eq!(hello.len(), HELLO_LEN);
        assert_eq!(
            session
                .offer(&[1; NONCE_LEN], &[3; NONCE_LEN], &[4; KEY_LEN])
                .len(),
            OFFER_LEN
        );
        assert_eq!(
            session
                .reject(&[1; NONCE_LEN], RejectReason::KeyInUse)
                .len(),
            REJECT_LEN
        );
    }

    #[test]
    fn a_rejection_carries_its_reason_and_verifies_like_every_message() {
        let (session, _, _) = session();
        for reason in [RejectReason::KeyInUse, RejectReason::PairingClosed] {
            let reject = session.reject(&[5; NONCE_LEN], reason);
            assert_eq!(session.open_reject(&reject), Some(([5; NONCE_LEN], reason)));
            assert_eq!(session.open_confirm(&reject), None);
        }
        let (other_pairing, _, _) = self::session();
        let reject = session.reject(&[5; NONCE_LEN], RejectReason::KeyInUse);
        assert_eq!(other_pairing.open_reject(&reject), None);
        assert_eq!(RejectReason::from_byte(200), RejectReason::Other);
    }

    #[test]
    fn messages_verify_only_under_the_same_secret_and_pairing() {
        let (session, id, _) = session();
        let hello = session.hello(&[1; NONCE_LEN], &[2; KEY_LEN]);
        assert_eq!(
            session.open_hello(&hello),
            Some(([1; NONCE_LEN], [2; KEY_LEN]))
        );

        let other_secret = PairingSession::new(id, &PairingSecret::generate().unwrap());
        assert_eq!(other_secret.open_hello(&hello), None);
        let (other_pairing, _, _) = self::session();
        assert_eq!(other_pairing.open_hello(&hello), None);

        let mut tampered = hello.clone();
        tampered[HEADER_LEN + NONCE_LEN] ^= 1;
        assert_eq!(session.open_hello(&tampered), None);
        // A HELLO is not an OFFER or a CONFIRM, whatever its length.
        assert_eq!(session.open_offer(&hello), None);
        assert_eq!(session.open_confirm(&hello[..CONFIRM_LEN]), None);
    }

    #[test]
    fn a_code_round_trips_and_keeps_its_secret_out_of_debug_output() {
        let id = PairingId::generate().unwrap();
        let secret = PairingSecret::generate().unwrap();
        let code = PairingCode {
            id: id.to_base64url(),
            secret: secret.to_base64url(),
            name: "worker-1".into(),
            control_plane_public_key: key(7),
            control_plane_endpoint: None,
            control_plane_address: "10.201.0.1".parse().unwrap(),
            node_address: "10.201.0.5".parse().unwrap(),
            node_endpoint: "198.51.100.7:51820".parse().unwrap(),
            prefix_len: 24,
            listen_port: 51820,
            node_api_port: 51820,
            ca_fingerprint: "ab".repeat(32),
            join_token: "join-token".into(),
            expires_at: 100,
        };
        let encoded = code.encode();
        assert!(encoded.starts_with(CODE_PREFIX));
        assert_eq!(PairingCode::decode(&encoded).unwrap(), code);
        let debug = format!("{code:?}");
        assert!(!debug.contains(&code.secret) && !debug.contains("join-token"));
        assert!(code.is_expired(100) && !code.is_expired(99));
        assert!(matches!(
            PairingCode::decode("tpair1.not-json"),
            Err(PairingError::InvalidCode(_))
        ));
    }

    #[tokio::test]
    async fn the_control_plane_learns_the_node_key_and_the_node_learns_it_was_heard() {
        let (session, _, _) = session();
        let node_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let node_addr = node_socket.local_addr().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let node_session = session.clone();
        let node = tokio::spawn(async move {
            respond(&node_socket, &node_session, &key(7), &key(9), deadline).await
        });
        let learned = initiate(
            node_addr,
            &session,
            &key(7),
            deadline,
            |node_key| async move {
                assert_eq!(node_key, key(9));
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(learned, key(9));
        let paired_from = node.await.unwrap().unwrap();
        assert_eq!(paired_from.ip(), node_addr.ip());
    }

    #[tokio::test]
    async fn a_node_ignores_a_hello_from_another_control_plane_key() {
        let (session, _, _) = session();
        let node_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let node_addr = node_socket.local_addr().unwrap();
        let node_session = session.clone();
        let node = tokio::spawn(async move {
            respond(
                &node_socket,
                &node_session,
                &key(7),
                &key(9),
                Instant::now() + Duration::from_millis(1500),
            )
            .await
        });
        let result = initiate(
            node_addr,
            &session,
            &key(8),
            Instant::now() + Duration::from_millis(1200),
            |_| async { panic!("no OFFER can arrive for another control plane's key") },
        )
        .await;
        assert!(matches!(result, Err(PairingError::TimedOut)));
        assert!(matches!(node.await.unwrap(), Err(PairingError::TimedOut)));
    }

    #[tokio::test]
    async fn a_node_whose_key_is_refused_is_told_why_instead_of_confirmed() {
        let (session, _, _) = session();
        let node_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let node_addr = node_socket.local_addr().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let node_session = session.clone();
        let node = tokio::spawn(async move {
            respond(&node_socket, &node_session, &key(7), &key(9), deadline).await
        });
        let result = initiate(node_addr, &session, &key(7), deadline, |_| async {
            Err(KeyRefusal::Reject(RejectReason::KeyInUse))
        })
        .await;
        assert!(matches!(
            result,
            Err(PairingError::Rejected(RejectReason::KeyInUse))
        ));
        assert!(matches!(
            node.await.unwrap(),
            Err(PairingError::Rejected(RejectReason::KeyInUse))
        ));
    }

    #[tokio::test]
    async fn a_deferred_key_leaves_the_node_waiting_for_the_next_attempt() {
        let (session, _, _) = session();
        let node_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let node_addr = node_socket.local_addr().unwrap();
        let node_session = session.clone();
        let node = tokio::spawn(async move {
            respond(
                &node_socket,
                &node_session,
                &key(7),
                &key(9),
                Instant::now() + Duration::from_secs(10),
            )
            .await
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let deferred = initiate(node_addr, &session, &key(7), deadline, |_| async {
            Err(KeyRefusal::Defer("the database is unavailable".into()))
        })
        .await;
        assert!(matches!(deferred, Err(PairingError::Deferred(_))));
        assert!(
            !node.is_finished(),
            "a deferred key must not end the node's wait"
        );
        let learned = initiate(node_addr, &session, &key(7), deadline, |_| async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(learned, key(9));
        assert!(node.await.unwrap().is_ok());
    }
}
