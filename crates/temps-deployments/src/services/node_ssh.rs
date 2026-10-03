// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add a server over SSH (ADR 048 D2c): the control plane connects to the
//! server with credentials the operator supplies for this one operation,
//! checks the host key the operator confirmed, makes sure `temps` and Docker
//! are there, and runs the pairing command (D2b) and the agent service on it.
//!
//! Every remote command is fixed text: nothing the operator types is
//! interpolated into a shell. The pairing code and a sudo password travel on
//! the command's stdin, never in its arguments.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;
use zeroize::Zeroizing;

/// How long connecting and the key exchange may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Short commands (checks) may take this long.
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
/// Installing `temps` downloads a release.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// How long `temps join --pair` may wait on the server for the control plane
/// to reach it.
pub const PAIRING_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The longest one step can go without a new step: "installing temps" runs
/// three checks around the installer. Whoever decides that an enrollment's
/// process is gone must wait longer than this.
pub const LONGEST_STEP: Duration = {
    let install = CHECK_TIMEOUT.as_secs() * 3 + INSTALL_TIMEOUT.as_secs();
    let pairing = PAIRING_TIMEOUT.as_secs();
    Duration::from_secs(if install > pairing { install } else { pairing })
};
/// Lines of remote output kept per command for error messages.
const TAIL_LINES: usize = 12;
/// Longest line of remote output kept; the rest of a longer line is dropped.
const LINE_LIMIT: usize = 16 * 1024;
/// Remote output kept per command: its newest lines, up to the size of the
/// enrollment's log.
const OUTPUT_LIMIT: usize = 64 * 1024;
/// Stands in for the lines dropped from the start of a command's output.
pub const OUTPUT_TRUNCATED: &str = "[output truncated]";
/// Ends a line that was cut at `LINE_LIMIT`.
const LINE_TRUNCATED: &str = "[line truncated]";
/// Where the official installer puts the binary for root (`sudo -H`).
const ROOT_INSTALL_PATH: &str = "/root/.temps/bin/temps";
pub const INSTALL_COMMAND: &str = "curl -fsSL https://temps.sh/install.sh | bash";

/// A server's SSH host key, as the operator confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    /// e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// `SHA256:<base64>`, as `ssh-keygen -lf` prints it.
    pub fingerprint: String,
}

/// How to log in. Held only for the operation.
pub enum SshAuth {
    Password(Zeroizing<String>),
    PrivateKey {
        key: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
    },
    /// The SSH agent of the `temps serve` process (`SSH_AUTH_SOCK`).
    Agent,
}

/// Never prints the credentials.
impl std::fmt::Debug for SshAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SshAuth::{}", self.method())
    }
}

impl SshAuth {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Password(_) => "password",
            Self::PrivateKey { .. } => "private_key",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("could not connect to {address}: {reason}")]
    Connect { address: SocketAddr, reason: String },
    #[error(
        "the server's host key is {presented}, not the {expected} you confirmed. If the server \
         was reinstalled, check the new key with its administrator and start again; otherwise \
         someone may be intercepting the connection"
    )]
    HostKeyChanged { expected: String, presented: String },
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    Remote(String),
    #[error("the SSH session failed: {0}")]
    Session(String),
}

/// What happened with the agent once the node joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    /// Installed as `temps-agent.service`: restarts on failure and at boot.
    Service,
    /// Started in the background: no service manager, so it does not come
    /// back after a reboot.
    Detached,
}

/// Where the enrollment reports progress. The step names are shown as they
/// are, and the console lists them in order (`SSH_ENROLLMENT_STEPS` in
/// `web/src/lib/wireguard-mesh.ts`): keep the two in step.
pub trait Progress: Send + Sync {
    fn step(&self, step: &str);
    fn log(&self, line: &str);
}

/// Accept the host key the operator confirmed (or learn it, when there is
/// none yet) and remember what was presented.
struct Pinning {
    expected: Option<String>,
    presented: Arc<Mutex<Option<HostKey>>>,
}

impl client::Handler for Pinning {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_key.public_key();
        let presented = HostKey {
            algorithm: key.algorithm().as_str().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        };
        let accepted = accepts(self.expected.as_deref(), &presented.fingerprint);
        if let Ok(mut slot) = self.presented.lock() {
            *slot = Some(presented);
        }
        Ok(accepted)
    }
}

/// Whether to go on with a server that presented the host key `presented`.
/// `expected` is `None` only when reading the key for the operator to
/// confirm (`host_key`), which never logs in. Everything that logs in passes
/// the fingerprint the operator confirmed, and only that exact key is
/// accepted.
fn accepts(expected: Option<&str>, presented: &str) -> bool {
    expected.is_none_or(|expected| expected == presented)
}

fn config() -> Arc<client::Config> {
    Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(120)),
        keepalive_interval: Some(Duration::from_secs(15)),
        ..Default::default()
    })
}

async fn connect(
    address: SocketAddr,
    expected: Option<&str>,
) -> Result<(client::Handle<Pinning>, HostKey), SshError> {
    let presented = Arc::new(Mutex::new(None));
    let handler = Pinning {
        expected: expected.map(str::to_string),
        presented: presented.clone(),
    };
    let connected =
        tokio::time::timeout(CONNECT_TIMEOUT, client::connect(config(), address, handler))
            .await
            .map_err(|_| SshError::Connect {
                address,
                reason: format!(
                    "no SSH answer within {}s; check the address and that port {} is open to this \
                 control plane",
                    CONNECT_TIMEOUT.as_secs(),
                    address.port()
                ),
            })?;
    let seen = presented.lock().ok().and_then(|slot| slot.clone());
    match (connected, seen, expected) {
        (Ok(handle), Some(key), _) => Ok((handle, key)),
        (Err(_), Some(key), Some(expected)) if !accepts(Some(expected), &key.fingerprint) => {
            Err(SshError::HostKeyChanged {
                expected: expected.to_string(),
                presented: key.fingerprint,
            })
        }
        (Ok(_), None, _) => Err(SshError::Session(
            "the server presented no host key".to_string(),
        )),
        (Err(error), _, _) => Err(SshError::Connect {
            address,
            reason: error.to_string(),
        }),
    }
}

/// The server's host key, for the operator to confirm before anything else
/// happens. Does not log in.
pub async fn host_key(address: SocketAddr) -> Result<HostKey, SshError> {
    let (handle, key) = connect(address, None).await?;
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(key)
}

/// What to enroll and how.
pub struct Enrollment {
    pub address: SocketAddr,
    pub user: String,
    pub auth: SshAuth,
    /// The fingerprint the operator confirmed.
    pub host_key_fingerprint: String,
    /// The `tpair1.` code; secret.
    pub pairing_code: Zeroizing<String>,
    /// How long the pairing command may wait for the control plane.
    pub pairing_timeout: Duration,
}

/// Run the enrollment to the point where the node has joined and its agent
/// runs.
pub async fn enroll(request: Enrollment, progress: &dyn Progress) -> Result<AgentMode, SshError> {
    progress.step("connecting");
    let (mut handle, _) = connect(request.address, Some(&request.host_key_fingerprint)).await?;
    progress.log(&format!(
        "Connected to {}; host key {} matches.",
        request.address, request.host_key_fingerprint
    ));

    progress.step("authenticating");
    authenticate(&mut handle, &request.user, &request.auth).await?;
    progress.log(&format!(
        "Logged in as {} ({}).",
        request.user,
        request.auth.method()
    ));

    let remote = Remote {
        handle: &handle,
        sudo_password: match &request.auth {
            SshAuth::Password(password) => Some(password.clone()),
            _ => None,
        },
        redactor: Redactor::for_enrollment(&request),
        progress,
    };

    progress.step("checking the server");
    let uname = remote.check("uname -sm").await?;
    if !uname.starts_with("Linux") {
        return Err(SshError::Remote(format!(
            "the server runs {}; nodes must run Linux",
            uname.trim()
        )));
    }
    let privilege = remote.privilege(&request.user).await?;
    progress.log(&format!(
        "{}; commands run {}.",
        uname.trim(),
        privilege.describe()
    ));
    let docker = remote
        .run_quiet(
            &privilege,
            "docker info --format '{{.ServerVersion}}'",
            CHECK_TIMEOUT,
        )
        .await?;
    if docker.code != 0 {
        return Err(SshError::Remote(
            "Docker is not installed or not running on the server. Install it \
             (https://docs.docker.com/engine/install/), then try again."
                .to_string(),
        ));
    }
    progress.log(&format!(
        "Docker {} is running.",
        docker.lines.join(" ").trim()
    ));

    progress.step("installing temps");
    let temps = remote.temps_binary(&privilege).await?;

    progress.step("pairing");
    let mut stdin = Zeroizing::new(String::new());
    stdin.push_str(&request.pairing_code);
    stdin.push('\n');
    let joined = remote
        .run(
            &privilege,
            &format!("{temps} join --pair -"),
            Some(stdin),
            request.pairing_timeout,
        )
        .await?;
    if joined.code != 0 {
        return Err(SshError::Remote(format!(
            "`temps join --pair` failed on the server (exit {}):\n{}",
            joined.code,
            joined.tail()
        )));
    }

    progress.step("starting the agent");
    let systemd = remote
        .run(
            &privilege,
            "test -d /run/systemd/system",
            None,
            CHECK_TIMEOUT,
        )
        .await?
        .code
        == 0;
    let mode = if systemd {
        let installed = remote
            .run(
                &privilege,
                &format!("{temps} agent service install"),
                None,
                CHECK_TIMEOUT,
            )
            .await?;
        if installed.code != 0 {
            return Err(SshError::Remote(format!(
                "the node joined, but its agent service could not be installed (exit {}):\n{}",
                installed.code,
                installed.tail()
            )));
        }
        AgentMode::Service
    } else {
        progress.log(
            "This server does not run systemd: starting the agent in the background. It will \
             not come back after a reboot until you run `temps agent` under a supervisor.",
        );
        let started = remote
            .run(
                &privilege,
                &format!(
                    "sh -c 'nohup setsid {temps} agent >>/var/log/temps-agent.log 2>&1 </dev/null &'"
                ),
                None,
                CHECK_TIMEOUT,
            )
            .await?;
        if started.code != 0 {
            return Err(SshError::Remote(format!(
                "the node joined, but its agent could not be started (exit {}):\n{}",
                started.code,
                started.tail()
            )));
        }
        AgentMode::Detached
    };
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(mode)
}

async fn authenticate(
    handle: &mut client::Handle<Pinning>,
    user: &str,
    auth: &SshAuth,
) -> Result<(), SshError> {
    let session = |error: russh::Error| SshError::Session(error.to_string());
    let result = match auth {
        SshAuth::Password(password) => handle
            .authenticate_password(user, password.as_str())
            .await
            .map_err(session)?,
        SshAuth::PrivateKey { key, passphrase } => {
            let key = russh::keys::decode_secret_key(key, passphrase.as_ref().map(|p| p.as_str()))
                .map_err(|error| {
                    SshError::Auth(format!(
                        "the private key could not be read ({error}); paste the whole key file, \
                         and its passphrase if it has one"
                    ))
                })?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(session)?
                .flatten();
            handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(session)?
        }
        SshAuth::Agent => {
            let mut agent = russh::keys::agent::client::AgentClient::connect_env()
                .await
                .map_err(|error| {
                    SshError::Auth(format!(
                        "this control plane has no SSH agent to use ({error}); start `temps serve` \
                         with SSH_AUTH_SOCK set, or use a password or private key"
                    ))
                })?;
            let identities = agent
                .request_identities()
                .await
                .map_err(|error| SshError::Auth(format!("the SSH agent failed: {error}")))?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(session)?
                .flatten();
            let mut accepted = false;
            for identity in identities {
                let key = identity.public_key().into_owned();
                let result = handle
                    .authenticate_publickey_with(user, key, hash, &mut agent)
                    .await
                    .map_err(|error| SshError::Auth(format!("the SSH agent failed: {error}")))?;
                if result.success() {
                    accepted = true;
                    break;
                }
            }
            if !accepted {
                return Err(SshError::Auth(format!(
                    "the server accepted none of the SSH agent's keys for {user}"
                )));
            }
            return Ok(());
        }
    };
    if result.success() {
        Ok(())
    } else {
        Err(SshError::Auth(format!(
            "the server refused the {} for {user}",
            match auth {
                SshAuth::Password(_) => "password",
                SshAuth::PrivateKey { .. } => "private key",
                SshAuth::Agent => "agent's keys",
            }
        )))
    }
}

/// How commands get root.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Privilege {
    Root,
    /// `sudo -n`: no password needed.
    Sudo,
    /// `sudo -S`: the login password on stdin.
    SudoWithPassword,
}

impl Privilege {
    fn describe(&self) -> &'static str {
        match self {
            Self::Root => "as root",
            Self::Sudo => "through sudo",
            Self::SudoWithPassword => "through sudo with the login password",
        }
    }

    /// `command` with this privilege; `-H` so root's home (and its
    /// `~/.temps`) is used.
    fn wrap(&self, command: &str) -> String {
        match self {
            Self::Root => command.to_string(),
            Self::Sudo => format!("sudo -n -H {command}"),
            // -k: ignore cached credentials, so sudo always reads the
            // password line and never leaves it for the command's stdin.
            Self::SudoWithPassword => format!("sudo -k -S -p '' -H {command}"),
        }
    }
}

struct Output {
    code: u32,
    lines: Vec<String>,
}

impl Output {
    fn tail(&self) -> String {
        let start = self.lines.len().saturating_sub(TAIL_LINES);
        self.lines[start..].join("\n")
    }
}

struct Remote<'a> {
    handle: &'a client::Handle<Pinning>,
    sudo_password: Option<Zeroizing<String>>,
    /// Applied to every line of remote output before it is logged or kept.
    redactor: Redactor,
    progress: &'a dyn Progress,
}

/// Masks the secrets an enrollment holds wherever they appear in the
/// server's output. That output is stored in the enrollment's log, which
/// anyone who can read settings sees.
///
/// Matching ignores whitespace, in the output and in the secrets, and runs
/// across line ends: a secret the server printed in pieces (wrapped by a
/// terminal, broken by `\r\n` or blank lines, an indented continuation, or
/// written in several chunks) still matches, and every piece of it is masked.
/// [`Collector`] holds lines back until that can be decided.
struct Redactor {
    /// Each secret's characters other than whitespace.
    patterns: Vec<Zeroizing<Vec<char>>>,
    /// The longest pattern's length.
    longest: usize,
}

const REDACTED: &str = "[redacted]";
/// Secrets with fewer characters than this (whitespace aside) are not
/// masked: too short to tell from ordinary output.
const MIN_SECRET_CHARS: usize = 4;
/// Lines held back at most while the output may be in the middle of a
/// secret; past it (or `HELD_BYTES`) the unfinished secret is masked and the
/// lines released.
const HELD_LINES: usize = 64;
const HELD_BYTES: usize = 2 * LINE_LIMIT;

impl Redactor {
    fn for_enrollment(request: &Enrollment) -> Self {
        let mut secrets = vec![request.pairing_code.clone()];
        if let Ok(code) = temps_wireguard::pairing::PairingCode::decode(&request.pairing_code) {
            secrets.push(Zeroizing::new(code.secret));
            secrets.push(Zeroizing::new(code.join_token));
            // Not a secret, but only the pairing code carries it here: a
            // precaution, so nothing from the code shows in the log.
            secrets.push(Zeroizing::new(code.ca_fingerprint));
        }
        match &request.auth {
            SshAuth::Password(password) => secrets.push(password.clone()),
            SshAuth::PrivateKey { key, passphrase } => {
                secrets.extend(
                    key.lines()
                        .map(str::trim)
                        .filter(|line| line.len() >= 16 && !line.starts_with("-----"))
                        .map(|line| Zeroizing::new(line.to_string())),
                );
                secrets.extend(passphrase.clone());
            }
            SshAuth::Agent => {}
        }
        Self::new(secrets)
    }

    fn new(secrets: Vec<Zeroizing<String>>) -> Self {
        let patterns: Vec<Zeroizing<Vec<char>>> = secrets
            .iter()
            .map(|secret| Zeroizing::new(significant(secret).map(|(_, c)| c).collect()))
            .filter(|pattern: &Zeroizing<Vec<char>>| pattern.len() >= MIN_SECRET_CHARS)
            .collect();
        let longest = patterns
            .iter()
            .map(|pattern| pattern.len())
            .max()
            .unwrap_or(0);
        Self { patterns, longest }
    }

    /// Which characters of `text` are a secret's: every occurrence of every
    /// secret, overlapping ones included.
    fn covered(&self, text: &[char]) -> Vec<bool> {
        let mut covered = vec![false; text.len()];
        for start in 0..text.len() {
            for pattern in &self.patterns {
                if text[start..].starts_with(pattern.as_slice()) {
                    covered[start..start + pattern.len()].fill(true);
                }
            }
        }
        covered
    }

    /// Where a secret that `text` ends in the middle of starts: the first
    /// position from which the rest of `text` begins a secret without
    /// finishing it.
    fn unfinished(&self, text: &[char]) -> Option<usize> {
        let from = text.len().saturating_sub(self.longest.saturating_sub(1));
        (from..text.len()).find(|&start| {
            let rest = &text[start..];
            self.patterns
                .iter()
                .any(|pattern| pattern.len() > rest.len() && pattern.starts_with(rest))
        })
    }
}

/// The characters of `text` matching looks at (all but whitespace), with
/// their byte offsets.
fn significant(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    text.char_indices().filter(|(_, c)| !c.is_whitespace())
}

/// A complete line of output not released yet, as received, and which of its
/// characters (those `significant` yields) are a secret's.
struct Held {
    text: String,
    covered: Vec<bool>,
}

impl Held {
    fn new(text: String) -> Self {
        let covered = vec![false; significant(&text).count()];
        Self { text, covered }
    }

    /// The line with each run of secret characters (and the whitespace
    /// inside the run) replaced by `REDACTED`.
    fn masked(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        let mut copied = 0;
        let mut run: Option<(usize, usize)> = None;
        for ((at, c), &secret) in significant(&self.text).zip(&self.covered) {
            if secret {
                let end = at + c.len_utf8();
                run = Some(run.map_or((at, end), |(start, _)| (start, end)));
            } else if let Some((start, end)) = run.take() {
                out.push_str(&self.text[copied..start]);
                out.push_str(REDACTED);
                copied = end;
            }
        }
        if let Some((start, end)) = run {
            out.push_str(&self.text[copied..start]);
            out.push_str(REDACTED);
            copied = end;
        }
        out.push_str(&self.text[copied..]);
        out
    }
}

/// Why lines are being released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    /// A line ended: release the lines no secret can still reach into.
    Line,
    /// A line was cut at `LINE_LIMIT` and its rest is dropped, so a secret it
    /// ends in the middle of is never finished: mask it, release everything.
    Cut,
    /// The output ended: release everything, masking a secret it ends in
    /// the middle of once `MIN_SECRET_CHARS` of it are there.
    End,
}

/// Splits a command's output into lines as it arrives, with every line
/// redacted, and keeps a bounded amount of it: lines longer than
/// `LINE_LIMIT` are cut, and past `OUTPUT_LIMIT` the oldest lines are
/// dropped (errors show the output's last lines).
///
/// A secret can continue on the next line, so a line ending in what may be
/// the start of one is held back (with the lines after it) until the output
/// shows whether it is: no line is released, streamed or kept, before every
/// secret it holds a piece of is known and masked. What is held is bounded
/// (`HELD_LINES`, `HELD_BYTES`); past it the unfinished secret is masked
/// and remembered (`carry`), so its continuation is masked when it comes.
struct Collector<'r> {
    redactor: &'r Redactor,
    /// The line being received.
    pending: Vec<u8>,
    /// Dropping the rest of a line that was cut.
    skipping: bool,
    /// Complete lines not released yet.
    held: Vec<Held>,
    /// The start of a secret in lines already released (and masked there),
    /// whitespace removed: what the next lines may continue.
    carry: Zeroizing<Vec<char>>,
    lines: VecDeque<String>,
    /// Bytes in `lines`.
    kept: usize,
    truncated: bool,
}

impl<'r> Collector<'r> {
    fn new(redactor: &'r Redactor) -> Self {
        Self {
            redactor,
            pending: Vec::new(),
            skipping: false,
            held: Vec::new(),
            carry: Zeroizing::new(Vec::new()),
            lines: VecDeque::new(),
            kept: 0,
            truncated: false,
        }
    }

    /// Take a chunk of output; the lines it released, to stream.
    fn push(&mut self, data: &[u8]) -> Vec<String> {
        let mut released = Vec::new();
        for segment in data.split_inclusive(|byte| *byte == b'\n') {
            let ends_line = segment.last() == Some(&b'\n');
            if self.skipping {
                self.skipping = !ends_line;
                continue;
            }
            let content = if ends_line {
                &segment[..segment.len() - 1]
            } else {
                segment
            };
            let room = LINE_LIMIT - self.pending.len();
            if content.len() > room {
                self.pending.extend_from_slice(&content[..room]);
                let mut line = printable(&self.pending);
                self.pending.clear();
                // The cut can split a character, which shows as U+FFFD.
                let end = line.trim_end_matches('\u{FFFD}').len();
                line.truncate(end);
                self.held.push(Held::new(line));
                released.extend(self.release(Release::Cut));
                self.skipping = !ends_line;
                continue;
            }
            self.pending.extend_from_slice(content);
            if ends_line {
                let line = printable(&self.pending);
                self.pending.clear();
                self.held.push(Held::new(line));
                released.extend(self.release(Release::Line));
            }
        }
        released
    }

    /// Once the output ended: the lines still held, and the last line when
    /// the output did not end with a newline.
    fn flush(&mut self) -> Vec<String> {
        if !self.skipping && !self.pending.is_empty() {
            let line = printable(&self.pending);
            self.held.push(Held::new(line));
        }
        self.pending.clear();
        self.skipping = false;
        self.release(Release::End)
    }

    /// Mask what is known to be secret in the held lines and release those
    /// no secret can still reach into.
    fn release(&mut self, why: Release) -> Vec<String> {
        // Matched against: the carried start of a secret, then every held
        // line's characters.
        let carried = self.carry.len();
        let mut text = Zeroizing::new(self.carry.to_vec());
        let mut line_of = vec![None; carried];
        for (index, held) in self.held.iter().enumerate() {
            for (_, c) in significant(&held.text) {
                text.push(c);
                line_of.push(Some(index));
            }
        }
        let covered = self.redactor.covered(&text);
        let unfinished = self.redactor.unfinished(&text);
        let overflow = self.held.len() > HELD_LINES
            || self.held.iter().map(|held| held.text.len()).sum::<usize>() > HELD_BYTES;
        let mask_from = match why {
            Release::Line if overflow => unfinished,
            Release::Line => None,
            Release::Cut => unfinished,
            Release::End => unfinished.filter(|&start| text.len() - start >= MIN_SECRET_CHARS),
        };
        let mut position = carried;
        for held in &mut self.held {
            for secret in &mut held.covered {
                *secret |= covered[position] || mask_from.is_some_and(|start| position >= start);
                position += 1;
            }
        }
        let (keep_from, carry) = match (why, unfinished) {
            (Release::Line, Some(start)) if overflow => (self.held.len(), text[start..].to_vec()),
            (Release::Line, Some(start)) => match line_of[start] {
                Some(line) => (line, Vec::new()),
                None => (0, text[start..carried].to_vec()),
            },
            _ => (self.held.len(), Vec::new()),
        };
        self.carry = Zeroizing::new(carry);
        let mut released: Vec<String> = self
            .held
            .drain(..keep_from)
            .map(|held| held.masked())
            .collect();
        if why == Release::Cut {
            if let Some(line) = released.last_mut() {
                line.push(' ');
                line.push_str(LINE_TRUNCATED);
            }
        }
        for line in &released {
            self.keep(line);
        }
        released
    }

    fn keep(&mut self, line: &str) {
        self.kept += line.len();
        self.lines.push_back(line.to_string());
        while self.kept > OUTPUT_LIMIT && self.lines.len() > 1 {
            if let Some(dropped) = self.lines.pop_front() {
                self.kept -= dropped.len();
                self.truncated = true;
            }
        }
    }

    fn into_lines(self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.lines.len() + 1);
        if self.truncated {
            lines.push(OUTPUT_TRUNCATED.to_string());
        }
        lines.extend(self.lines);
        lines
    }
}

impl Remote<'_> {
    /// Run a check without privilege; its stdout on success.
    async fn check(&self, command: &str) -> Result<String, SshError> {
        let output = self.exec(command, None, CHECK_TIMEOUT, false).await?;
        if output.code != 0 {
            return Err(SshError::Remote(format!(
                "`{command}` failed on the server (exit {}):\n{}",
                output.code,
                output.tail()
            )));
        }
        Ok(output.lines.join("\n"))
    }

    /// How to run commands as root for `user`.
    async fn privilege(&self, user: &str) -> Result<Privilege, SshError> {
        if self.check("id -u").await?.trim() == "0" {
            return Ok(Privilege::Root);
        }
        if self
            .exec("sudo -n true", None, CHECK_TIMEOUT, false)
            .await?
            .code
            == 0
        {
            return Ok(Privilege::Sudo);
        }
        let Some(password) = &self.sudo_password else {
            return Err(SshError::Remote(format!(
                "{user} is not root and sudo asks for a password. Log in as root, give {user} \
                 passwordless sudo, or log in with {user}'s password so it can be used for sudo."
            )));
        };
        let mut stdin = Zeroizing::new(password.to_string());
        stdin.push('\n');
        if self
            .exec("sudo -k -S -p '' true", Some(stdin), CHECK_TIMEOUT, false)
            .await?
            .code
            == 0
        {
            return Ok(Privilege::SudoWithPassword);
        }
        Err(SshError::Remote(format!(
            "{user} cannot use sudo with its password on this server. Log in as root or as a \
             user that can."
        )))
    }

    /// Run `command` as root, with `stdin` after the sudo password if one is
    /// needed.
    async fn run(
        &self,
        privilege: &Privilege,
        command: &str,
        stdin: Option<Zeroizing<String>>,
        timeout: Duration,
    ) -> Result<Output, SshError> {
        self.run_as_root(privilege, command, stdin, timeout, true)
            .await
    }

    /// `run` without copying the output to the log.
    async fn run_quiet(
        &self,
        privilege: &Privilege,
        command: &str,
        timeout: Duration,
    ) -> Result<Output, SshError> {
        self.run_as_root(privilege, command, None, timeout, false)
            .await
    }

    async fn run_as_root(
        &self,
        privilege: &Privilege,
        command: &str,
        stdin: Option<Zeroizing<String>>,
        timeout: Duration,
        stream: bool,
    ) -> Result<Output, SshError> {
        let stdin = match (privilege, &self.sudo_password) {
            (Privilege::SudoWithPassword, Some(password)) => {
                let mut combined = Zeroizing::new(password.to_string());
                combined.push('\n');
                if let Some(stdin) = stdin {
                    combined.push_str(&stdin);
                }
                Some(combined)
            }
            _ => stdin,
        };
        self.exec(&privilege.wrap(command), stdin, timeout, stream)
            .await
    }

    /// The absolute path of a `temps` that can pair, installing it if the
    /// server has none.
    async fn temps_binary(&self, privilege: &Privilege) -> Result<String, SshError> {
        let locate = format!(
            "sh -c 'command -v temps || {{ test -x {ROOT_INSTALL_PATH} && echo {ROOT_INSTALL_PATH}; }}'"
        );
        let mut found = self.run_quiet(privilege, &locate, CHECK_TIMEOUT).await?;
        if found.code != 0 {
            self.progress
                .log("temps is not installed; running the official installer.");
            let installed = self
                .run(
                    privilege,
                    &format!("bash -c '{INSTALL_COMMAND}'"),
                    None,
                    INSTALL_TIMEOUT,
                )
                .await?;
            if installed.code != 0 {
                return Err(SshError::Remote(format!(
                    "installing temps failed (exit {}):\n{}",
                    installed.code,
                    installed.tail()
                )));
            }
            found = self.run_quiet(privilege, &locate, CHECK_TIMEOUT).await?;
        }
        let path = found
            .lines
            .iter()
            .map(|line| line.trim())
            .find(|line| line.starts_with('/'))
            .filter(|path| is_safe_path(path))
            .ok_or_else(|| {
                SshError::Remote(
                    "temps is not on the server's PATH after installing it; install it by hand \
                     and try again"
                        .to_string(),
                )
            })?
            .to_string();
        let pairs = self
            .run_quiet(
                privilege,
                &format!("sh -c '\"$0\" join --help | grep -q -- --pair' {path}"),
                CHECK_TIMEOUT,
            )
            .await?;
        if pairs.code != 0 {
            return Err(SshError::Remote(format!(
                "{path} is too old to pair with this control plane. Upgrade it on the server \
                 ({INSTALL_COMMAND}), then try again."
            )));
        }
        self.progress.log(&format!("Using {path}."));
        Ok(path)
    }

    async fn exec(
        &self,
        command: &str,
        stdin: Option<Zeroizing<String>>,
        timeout: Duration,
        stream: bool,
    ) -> Result<Output, SshError> {
        let session = |error: russh::Error| SshError::Session(error.to_string());
        let mut channel = self.handle.channel_open_session().await.map_err(session)?;
        channel.exec(true, command).await.map_err(session)?;
        // stdin carries secrets (the sudo password, the pairing code): it is
        // sent and never logged or kept.
        if let Some(stdin) = stdin {
            channel
                .data_bytes(stdin.as_bytes().to_vec())
                .await
                .map_err(session)?;
        }
        channel.eof().await.map_err(session)?;
        let mut code = None;
        let mut output = Collector::new(&self.redactor);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let message = tokio::time::timeout_at(deadline, channel.wait())
                .await
                .map_err(|_| {
                    SshError::Remote(format!(
                        "the server did not finish within {}s",
                        timeout.as_secs()
                    ))
                })?;
            let Some(message) = message else { break };
            match message {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                    for line in output.push(&data) {
                        if stream {
                            self.progress.log(&line);
                        }
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
        for line in output.flush() {
            if stream {
                self.progress.log(&line);
            }
        }
        Ok(Output {
            // A command killed by a signal reports no status.
            code: code.unwrap_or(255),
            lines: output.into_lines(),
        })
    }
}

/// A line of remote output as text: terminal escape sequences (colours) and
/// other control characters removed.
fn printable(line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ parameters, then a final byte in @..~.
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out.trim_end().to_string()
}

/// Whether `path` can be used in a fixed command without quoting.
fn is_safe_path(path: &str) -> bool {
    path.starts_with('/')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privileges_wrap_commands_without_touching_them() {
        assert_eq!(
            Privilege::Root.wrap("temps join --pair -"),
            "temps join --pair -"
        );
        assert_eq!(
            Privilege::Sudo.wrap("temps join --pair -"),
            "sudo -n -H temps join --pair -"
        );
        assert_eq!(
            Privilege::SudoWithPassword.wrap("true"),
            "sudo -k -S -p '' -H true"
        );
    }

    #[test]
    fn only_plain_absolute_paths_reach_a_command() {
        assert!(is_safe_path("/usr/local/bin/temps"));
        assert!(is_safe_path("/root/.temps/bin/temps"));
        for unsafe_path in ["temps", "/tmp/a b", "/tmp/$(id)", "/tmp/a;rm", "/tmp/`x`"] {
            assert!(!is_safe_path(unsafe_path), "{unsafe_path}");
        }
    }

    #[test]
    fn secrets_are_masked_in_output() {
        let redactor = Redactor::new(vec![
            Zeroizing::new("hunter2-password".to_string()),
            Zeroizing::new("tpair1.abcdef".to_string()),
            Zeroizing::new("tpair1.abcdef.longer".to_string()),
            Zeroizing::new("abc".to_string()),
        ]);
        assert_eq!(
            collect(&redactor, b"sudo: hunter2-password was wrong\n"),
            vec!["sudo: [redacted] was wrong"]
        );
        assert_eq!(
            collect(&redactor, b"code tpair1.abcdef.longer and tpair1.abcdef"),
            vec!["code [redacted] and [redacted]"]
        );
        // Too short to mask safely.
        assert_eq!(collect(&redactor, b"abc"), vec!["abc"]);
    }

    #[test]
    fn output_lines_lose_colours_and_control_characters() {
        assert_eq!(
            printable(b"\x1b[2m2026\x1b[0m \x1b[32m INFO\x1b[0m up\r\n"),
            "2026  INFO up"
        );
        assert_eq!(printable(b"a\x07b\tc"), "ab\tc");
    }

    #[test]
    fn the_output_tail_keeps_the_last_lines() {
        let output = Output {
            code: 1,
            lines: (0..20).map(|n| n.to_string()).collect(),
        };
        assert_eq!(output.tail().lines().count(), TAIL_LINES);
        assert!(output.tail().ends_with("19"));
    }

    const FINGERPRINT: &str = "SHA256:gGcMqXcQ/xFPz1fOY6ZhqcN0Un244iBB3Zkg5AcToJw";

    #[test]
    fn reading_the_host_key_accepts_any_key() {
        assert!(accepts(None, FINGERPRINT));
        assert!(accepts(None, "SHA256:anything"));
    }

    #[test]
    fn enrolling_accepts_only_the_confirmed_key() {
        assert!(accepts(Some(FINGERPRINT), FINGERPRINT));
    }

    #[test]
    fn enrolling_refuses_any_other_key() {
        let other = "SHA256:hGcMqXcQ/xFPz1fOY6ZhqcN0Un244iBB3Zkg5AcToJw";
        assert!(!accepts(Some(FINGERPRINT), other));
        // No prefix, case or whitespace leniency.
        assert!(!accepts(Some(FINGERPRINT), &FINGERPRINT.to_lowercase()));
        assert!(!accepts(Some(FINGERPRINT), &format!("{FINGERPRINT} ")));
        assert!(!accepts(Some(FINGERPRINT), &FINGERPRINT[..20]));
        assert!(!accepts(Some(FINGERPRINT), ""));
    }

    #[test]
    fn a_step_can_never_outlast_the_longest_step() {
        assert!(LONGEST_STEP >= PAIRING_TIMEOUT);
        assert!(LONGEST_STEP >= INSTALL_TIMEOUT + CHECK_TIMEOUT * 3);
    }

    // ── Redaction ────────────────────────────────────────────────────────

    const PASSWORD: &str = "correct-horse-battery";
    const PASSPHRASE: &str = "key-passphrase-9";

    fn pairing_code() -> (temps_wireguard::pairing::PairingCode, Zeroizing<String>) {
        use base64::Engine;
        use temps_wireguard::pairing::{PairingCode, PairingId, PairingSecret};
        let code = PairingCode {
            id: PairingId::generate().unwrap().to_base64url(),
            secret: PairingSecret::generate().unwrap().to_base64url(),
            name: "worker-1".into(),
            control_plane_public_key: base64::engine::general_purpose::STANDARD.encode([7u8; 32]),
            control_plane_endpoint: None,
            control_plane_address: "10.201.0.1".parse().unwrap(),
            node_address: "10.201.0.5".parse().unwrap(),
            node_endpoint: "198.51.100.7:51820".parse().unwrap(),
            prefix_len: 24,
            listen_port: 51820,
            node_api_port: 51820,
            ca_fingerprint: "3f9c".repeat(16),
            join_token: "tjoin_7Qm2vX9kLp4RtY8wZc3N".into(),
            expires_at: 4_102_444_800,
        };
        let encoded = Zeroizing::new(code.encode().unwrap());
        (code, encoded)
    }

    /// A real OpenSSH private key, generated from a fixed seed.
    fn private_key_pem() -> Zeroizing<String> {
        let key = russh::keys::PrivateKey::from(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[9; 32]),
        );
        key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap()
    }

    fn enrollment(auth: SshAuth, pairing_code: Zeroizing<String>) -> Enrollment {
        Enrollment {
            address: "198.51.100.7:22".parse().unwrap(),
            user: "deploy".into(),
            auth,
            host_key_fingerprint: FINGERPRINT.into(),
            pairing_code,
            pairing_timeout: PAIRING_TIMEOUT,
        }
    }

    /// Every secret an enrollment holds, in the forms a server could echo.
    fn secrets_of(
        code: &temps_wireguard::pairing::PairingCode,
        encoded: &str,
        auth: &SshAuth,
    ) -> Vec<String> {
        let mut secrets = vec![
            encoded.to_string(),
            code.secret.clone(),
            code.join_token.clone(),
            code.ca_fingerprint.clone(),
        ];
        match auth {
            SshAuth::Password(password) => secrets.push(password.to_string()),
            SshAuth::PrivateKey { key, passphrase } => {
                secrets.extend(
                    key.lines()
                        .filter(|line| !line.starts_with("-----"))
                        .map(str::to_string),
                );
                secrets.extend(passphrase.as_ref().map(|p| p.to_string()));
            }
            SshAuth::Agent => {}
        }
        secrets
    }

    fn assert_masked(lines: &[String], secrets: &[String]) {
        let text = lines.join("\n");
        for secret in secrets {
            assert!(
                !text.contains(secret.as_str()),
                "{secret:?} leaked into:\n{text}"
            );
        }
    }

    #[test]
    fn every_secret_of_a_password_enrollment_is_masked() {
        let (code, encoded) = pairing_code();
        let request = enrollment(
            SshAuth::Password(Zeroizing::new(PASSWORD.into())),
            encoded.clone(),
        );
        let redactor = Redactor::for_enrollment(&request);
        let secrets = secrets_of(&code, &encoded, &request.auth);
        for secret in &secrets {
            let lines = collect(
                &redactor,
                format!("error: bad value '{secret}' here").as_bytes(),
            );
            assert_eq!(
                lines,
                vec!["error: bad value '[redacted]' here"],
                "{secret}"
            );
        }
    }

    #[test]
    fn every_secret_of_a_private_key_enrollment_is_masked() {
        let (code, encoded) = pairing_code();
        let request = enrollment(
            SshAuth::PrivateKey {
                key: private_key_pem(),
                passphrase: Some(Zeroizing::new(PASSPHRASE.into())),
            },
            encoded.clone(),
        );
        let redactor = Redactor::for_enrollment(&request);
        let secrets = secrets_of(&code, &encoded, &request.auth);
        // The PEM body has several lines, and each one is masked.
        assert!(secrets.len() > 4 + 3);
        for secret in &secrets {
            let lines = collect(&redactor, format!("> {secret}\n").as_bytes());
            assert_eq!(lines, vec!["> [redacted]"], "{secret}");
        }
    }

    #[test]
    fn a_secret_cut_off_by_the_end_of_the_output_is_masked() {
        let redactor = Redactor::new(vec![Zeroizing::new("tpair1.abcdefghijkl".to_string())]);
        assert_eq!(
            collect(&redactor, b"code: tpair1.abcd"),
            vec!["code: [redacted]"]
        );
        // The whole secret is masked as usual.
        assert_eq!(
            collect(&redactor, b"tpair1.abcdefghijkl and more\n"),
            vec!["[redacted] and more"]
        );
        // Too little of it to tell from ordinary text, which is kept.
        assert_eq!(collect(&redactor, b"connect\n"), vec!["connect"]);
        assert_eq!(collect(&redactor, b"plain text"), vec!["plain text"]);
    }

    /// `output` as a command's whole output: the lines streamed, checked to
    /// be the lines kept.
    fn collect(redactor: &Redactor, output: &[u8]) -> Vec<String> {
        collect_chunks(redactor, &[output])
    }

    fn collect_chunks(redactor: &Redactor, chunks: &[&[u8]]) -> Vec<String> {
        let mut collector = Collector::new(redactor);
        let mut streamed = Vec::new();
        for chunk in chunks {
            streamed.extend(collector.push(chunk));
        }
        streamed.extend(collector.flush());
        assert_eq!(collector.into_lines(), streamed);
        streamed
    }

    /// What is left of `lines` once whitespace and every `REDACTED` are
    /// removed: no character of a masked secret may be in it.
    fn unmasked(lines: &[String]) -> String {
        let text: String = lines
            .concat()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        text.replace(REDACTED, "")
    }

    /// The ways a server's output can break a secret in two.
    const BREAKS: [&str; 8] = [
        "\n",
        "\r\n",
        "\n\n\n",
        "\n    ",
        "  \r\n\t",
        "\r",
        "\x1b[0m\n\x1b[32m",
        " ",
    ];

    #[test]
    fn a_secret_on_one_line_is_masked_and_the_rest_kept() {
        let redactor = Redactor::new(vec![Zeroizing::new("correct horse battery".to_string())]);
        assert_eq!(
            collect(&redactor, b"one\nsudo: correct horse battery: wrong\ntwo\n"),
            vec!["one", "sudo: [redacted]: wrong", "two"]
        );
    }

    #[test]
    fn a_secret_split_across_lines_is_masked_in_every_line() {
        let secret = "tpair1.Zm9vYmFyLWJhei1xdXV4";
        let redactor = Redactor::new(vec![Zeroizing::new(secret.to_string())]);
        for split in 1..secret.len() {
            for line_break in BREAKS {
                let output = format!(
                    "before {}{line_break}{} after\n",
                    &secret[..split],
                    &secret[split..]
                );
                let lines = collect(&redactor, output.as_bytes());
                assert_eq!(unmasked(&lines), "beforeafter", "{output:?} -> {lines:?}");
            }
        }
        // A secret across three lines.
        let lines = collect(&redactor, b"before tpair1.Zm9v\nYmFyLWJh\nei1xdXV4 after\n");
        assert_eq!(
            lines,
            vec!["before [redacted]", "[redacted]", "[redacted] after"]
        );
    }

    #[test]
    fn a_secret_split_across_chunks_is_masked() {
        let secret = "hunter2-password";
        let redactor = Redactor::new(vec![Zeroizing::new(secret.to_string())]);
        for output in [
            format!("before {secret} after\n"),
            "before hunter2-\r\npassword after\n".to_string(),
        ] {
            let output = output.as_bytes();
            for cut in 0..=output.len() {
                let lines = collect_chunks(&redactor, &[&output[..cut], &output[cut..]]);
                assert_eq!(unmasked(&lines), "beforeafter", "cut at {cut}: {lines:?}");
            }
            // A byte at a time.
            let bytes: Vec<&[u8]> = output.chunks(1).collect();
            assert_eq!(unmasked(&collect_chunks(&redactor, &bytes)), "beforeafter");
        }
    }

    /// The guarantee: whichever way the server's output breaks any secret of
    /// a real enrollment (a line break, `\r\n`, blank lines, indentation,
    /// colours, chunks), not one of its characters is left in the lines.
    #[test]
    fn no_piece_of_any_enrollment_secret_survives_a_split() {
        let (code, encoded) = pairing_code();
        for auth in [
            SshAuth::Password(Zeroizing::new(PASSWORD.into())),
            SshAuth::PrivateKey {
                key: private_key_pem(),
                passphrase: Some(Zeroizing::new(PASSPHRASE.into())),
            },
        ] {
            let request = enrollment(auth, encoded.clone());
            let redactor = Redactor::for_enrollment(&request);
            for secret in secrets_of(&code, &encoded, &request.auth) {
                for split in (1..secret.len()).filter(|&at| secret.is_char_boundary(at)) {
                    for line_break in BREAKS {
                        let output = format!(
                            "before {}{line_break}{} after\n",
                            &secret[..split],
                            &secret[split..]
                        );
                        let output = output.as_bytes();
                        let half = output.len() / 2;
                        let lines = collect_chunks(&redactor, &[&output[..half], &output[half..]]);
                        assert_eq!(unmasked(&lines), "beforeafter", "{lines:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_line_is_held_only_while_it_may_end_in_a_secret() {
        let redactor = Redactor::new(vec![Zeroizing::new("tpair1.abcdef".to_string())]);
        let mut output = Collector::new(&redactor);
        // Nothing that could start a secret: released at once.
        assert_eq!(output.push(b"plain\n"), vec!["plain"]);
        // Could go on as the secret: held until the next line says.
        assert!(output.push(b"code tpair1.ab\n").is_empty());
        assert_eq!(
            output.push(b"cdef done\n"),
            vec!["code [redacted]", "[redacted] done"]
        );
        // It did not: released as it was.
        assert!(output.push(b"code tpa\n").is_empty());
        assert_eq!(output.push(b"nothing\n"), vec!["code tpa", "nothing"]);
        assert!(output.flush().is_empty());
    }

    #[test]
    fn what_is_held_back_is_bounded_and_the_secret_still_masked() {
        let redactor = Redactor::new(vec![Zeroizing::new("tpair1.abcdef".to_string())]);
        let mut output = Collector::new(&redactor);
        let mut lines = output.push(b"code tpair1.ab\n");
        // Blank lines do not finish the secret, nor show it is not one.
        for _ in 0..10 * HELD_LINES {
            lines.extend(output.push(b"\n"));
            assert!(output.held.len() <= HELD_LINES + 1);
        }
        assert!(!lines.is_empty(), "held lines were released");
        assert_eq!(lines[0], "code [redacted]");
        // The rest of the secret, after the start was released, is masked too.
        lines.extend(output.push(b"cdef done\n"));
        lines.extend(output.flush());
        assert_eq!(lines.last().map(String::as_str), Some("[redacted] done"));
        assert_eq!(unmasked(&lines), "codedone");
    }

    // ── Bounded output ───────────────────────────────────────────────────

    #[test]
    fn lines_split_across_chunks_are_joined() {
        let redactor = Redactor::new(vec![Zeroizing::new("hunter2-password".to_string())]);
        let mut output = Collector::new(&redactor);
        assert!(output.push(b"first li").is_empty());
        assert_eq!(output.push(b"ne\nsecond hunter2-"), vec!["first line"]);
        assert_eq!(output.push(b"password\nlast"), vec!["second [redacted]"]);
        assert_eq!(output.flush(), vec!["last"]);
        assert_eq!(
            output.into_lines(),
            vec!["first line", "second [redacted]", "last"]
        );
    }

    #[test]
    fn a_line_longer_than_the_limit_is_cut_and_the_rest_dropped() {
        let redactor = Redactor::new(vec![]);
        let mut output = Collector::new(&redactor);
        let long = vec![b'x'; LINE_LIMIT * 3];
        // Arrives in pieces, without a newline for a long time.
        let mut completed = Vec::new();
        for chunk in long.chunks(1000) {
            completed.extend(output.push(chunk));
        }
        assert!(output.pending.is_empty());
        completed.extend(output.push(b"yyy\nnext\n"));
        assert_eq!(completed.len(), 2);
        assert_eq!(
            completed[0],
            format!("{} {LINE_TRUNCATED}", "x".repeat(LINE_LIMIT))
        );
        assert_eq!(completed[1], "next");
        assert!(output.flush().is_empty());
    }

    #[test]
    fn a_secret_straddling_the_cut_is_masked() {
        let secret = "tpair1.SECRETSECRETSECRET";
        let redactor = Redactor::new(vec![Zeroizing::new(secret.to_string())]);
        let mut output = Collector::new(&redactor);
        let mut line = "a".repeat(LINE_LIMIT - 10).into_bytes();
        line.extend_from_slice(secret.as_bytes());
        line.push(b'\n');
        let completed = output.push(&line);
        assert_eq!(completed.len(), 1);
        assert!(!completed[0].contains("tpair1.SEC"), "{}", completed[0]);
        assert!(completed[0].ends_with(&format!("{REDACTED} {LINE_TRUNCATED}")));
    }

    #[test]
    fn output_past_the_limit_keeps_its_newest_lines() {
        let redactor = Redactor::new(vec![]);
        let mut output = Collector::new(&redactor);
        let line = "y".repeat(1000);
        for n in 0..200 {
            output.push(format!("{n} {line}\n").as_bytes());
        }
        assert!(output.kept <= OUTPUT_LIMIT);
        let lines = output.into_lines();
        assert_eq!(lines[0], OUTPUT_TRUNCATED);
        assert!(lines.last().unwrap().starts_with("199 "));
        let kept: usize = lines[1..].iter().map(String::len).sum();
        assert!(kept <= OUTPUT_LIMIT);
        assert!(kept > OUTPUT_LIMIT - 2 * 1004);
    }

    #[test]
    fn output_within_the_limit_is_kept_whole() {
        let redactor = Redactor::new(vec![]);
        let mut output = Collector::new(&redactor);
        output.push(b"a\nb\n");
        assert_eq!(output.into_lines(), vec!["a", "b"]);
    }

    // ── Against an SSH server on loopback ────────────────────────────────

    mod loopback {
        use std::collections::HashMap;
        use std::net::SocketAddr;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use russh::keys::HashAlg;
        use russh::server::{self, Auth, Msg, Session};
        use russh::{Channel, ChannelId};

        pub const PASSWORD: &str = "loopback-password";

        /// How the server answers one command.
        #[derive(Clone, Default)]
        pub struct Reply {
            pub code: u32,
            pub stdout: Vec<u8>,
            pub stderr: Vec<u8>,
        }

        impl Reply {
            pub fn ok(stdout: &str) -> Self {
                Self {
                    code: 0,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: Vec::new(),
                }
            }
            pub fn failed() -> Self {
                Self {
                    code: 1,
                    ..Default::default()
                }
            }
        }

        type Script = Arc<dyn Fn(&str, &[u8]) -> Reply + Send + Sync>;

        #[derive(Default)]
        pub struct Seen {
            pub auth_attempts: AtomicUsize,
            /// Every command run, with the stdin it got, in order.
            pub commands: Mutex<Vec<(String, Vec<u8>)>>,
        }

        impl Seen {
            pub fn auth_attempts(&self) -> usize {
                self.auth_attempts.load(Ordering::SeqCst)
            }
            pub fn commands(&self) -> Vec<(String, String)> {
                self.commands
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(command, stdin)| {
                        (command.clone(), String::from_utf8_lossy(stdin).into_owned())
                    })
                    .collect()
            }
        }

        pub struct Server {
            pub address: SocketAddr,
            pub fingerprint: String,
            pub seen: Arc<Seen>,
        }

        /// An SSH server on 127.0.0.1 that accepts `PASSWORD` and answers
        /// commands with `script`, once the client has sent their stdin.
        pub async fn start(
            script: impl Fn(&str, &[u8]) -> Reply + Send + Sync + 'static,
        ) -> Server {
            let key = russh::keys::PrivateKey::from(
                russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]),
            );
            let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
            let config = Arc::new(server::Config {
                keys: vec![key],
                auth_rejection_time: Duration::ZERO,
                auth_rejection_time_initial: Some(Duration::ZERO),
                ..Default::default()
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let seen = Arc::new(Seen::default());
            let script: Script = Arc::new(script);
            let serving = seen.clone();
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let handler = Handler {
                        seen: serving.clone(),
                        script: script.clone(),
                        running: HashMap::new(),
                    };
                    let config = config.clone();
                    tokio::spawn(async move {
                        if let Ok(session) = server::run_stream(config, socket, handler).await {
                            let _ = session.await;
                        }
                    });
                }
            });
            Server {
                address,
                fingerprint,
                seen,
            }
        }

        struct Handler {
            seen: Arc<Seen>,
            script: Script,
            running: HashMap<ChannelId, (String, Vec<u8>)>,
        }

        impl Handler {
            fn attempt(&self) {
                self.seen.auth_attempts.fetch_add(1, Ordering::SeqCst);
            }
        }

        impl server::Handler for Handler {
            type Error = russh::Error;

            async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
                self.attempt();
                Ok(Auth::reject())
            }

            async fn auth_password(
                &mut self,
                _user: &str,
                password: &str,
            ) -> Result<Auth, Self::Error> {
                self.attempt();
                Ok(if password == PASSWORD {
                    Auth::Accept
                } else {
                    Auth::reject()
                })
            }

            async fn auth_publickey_offered(
                &mut self,
                _user: &str,
                _key: &russh::keys::PublicKey,
            ) -> Result<Auth, Self::Error> {
                self.attempt();
                Ok(Auth::reject())
            }

            async fn auth_publickey(
                &mut self,
                _user: &str,
                _key: &russh::keys::PublicKey,
            ) -> Result<Auth, Self::Error> {
                self.attempt();
                Ok(Auth::reject())
            }

            async fn channel_open_session(
                &mut self,
                _channel: Channel<Msg>,
                reply: server::ChannelOpenHandle,
                _session: &mut Session,
            ) -> Result<(), Self::Error> {
                reply.accept().await;
                Ok(())
            }

            async fn exec_request(
                &mut self,
                channel: ChannelId,
                data: &[u8],
                session: &mut Session,
            ) -> Result<(), Self::Error> {
                self.running.insert(
                    channel,
                    (String::from_utf8_lossy(data).into_owned(), Vec::new()),
                );
                session.channel_success(channel)
            }

            async fn data(
                &mut self,
                channel: ChannelId,
                data: &[u8],
                _session: &mut Session,
            ) -> Result<(), Self::Error> {
                if let Some((_, stdin)) = self.running.get_mut(&channel) {
                    stdin.extend_from_slice(data);
                }
                Ok(())
            }

            async fn channel_eof(
                &mut self,
                channel: ChannelId,
                session: &mut Session,
            ) -> Result<(), Self::Error> {
                let Some((command, stdin)) = self.running.remove(&channel) else {
                    return Ok(());
                };
                let reply = (self.script)(&command, &stdin);
                self.seen.commands.lock().unwrap().push((command, stdin));
                if !reply.stdout.is_empty() {
                    session.data(channel, reply.stdout)?;
                }
                if !reply.stderr.is_empty() {
                    session.extended_data(channel, 1, reply.stderr)?;
                }
                session.exit_status_request(channel, reply.code)?;
                session.eof(channel)?;
                session.close(channel)
            }
        }
    }

    use loopback::Reply;

    #[derive(Default)]
    struct Recorded {
        lines: Mutex<Vec<String>>,
    }

    impl Progress for Recorded {
        fn step(&self, _step: &str) {}
        fn log(&self, line: &str) {
            self.lines.lock().unwrap().push(line.to_string());
        }
    }

    async fn logged_in(server: &loopback::Server) -> client::Handle<Pinning> {
        let (mut handle, key) = connect(server.address, Some(&server.fingerprint))
            .await
            .unwrap();
        assert_eq!(key.fingerprint, server.fingerprint);
        authenticate(
            &mut handle,
            "deploy",
            &SshAuth::Password(Zeroizing::new(loopback::PASSWORD.into())),
        )
        .await
        .unwrap();
        handle
    }

    fn remote<'a>(
        handle: &'a client::Handle<Pinning>,
        sudo_password: Option<&str>,
        redactor: Redactor,
        progress: &'a Recorded,
    ) -> Remote<'a> {
        Remote {
            handle,
            sudo_password: sudo_password.map(|p| Zeroizing::new(p.to_string())),
            redactor,
            progress,
        }
    }

    #[tokio::test]
    async fn reading_the_host_key_returns_the_servers_key_without_logging_in() {
        let server = loopback::start(|_, _| Reply::ok("")).await;
        let key = host_key(server.address).await.unwrap();
        assert_eq!(key.fingerprint, server.fingerprint);
        assert_eq!(key.algorithm, "ssh-ed25519");
        assert_eq!(server.seen.auth_attempts(), 0);
    }

    #[tokio::test]
    async fn another_host_key_stops_the_connection_before_any_login() {
        let server = loopback::start(|_, _| Reply::ok("")).await;
        let confirmed = "SHA256:hGcMqXcQ/xFPz1fOY6ZhqcN0Un244iBB3Zkg5AcToJw";
        let error = connect(server.address, Some(confirmed))
            .await
            .err()
            .expect("the connection must be refused");
        match error {
            SshError::HostKeyChanged {
                expected,
                presented,
            } => {
                assert_eq!(expected, confirmed);
                assert_eq!(presented, server.fingerprint);
            }
            other => panic!("expected HostKeyChanged, got {other:?}"),
        }
        // Give the server a moment to process anything the client sent.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(server.seen.auth_attempts(), 0);
        assert!(server.seen.commands().is_empty());
    }

    #[tokio::test]
    async fn the_confirmed_host_key_lets_the_login_go_ahead() {
        let server = loopback::start(|_, _| Reply::ok("")).await;
        let _handle = logged_in(&server).await;
        assert_eq!(server.seen.auth_attempts(), 1);
    }

    #[tokio::test]
    async fn root_needs_no_sudo() {
        let server = loopback::start(|command, _| match command {
            "id -u" => Reply::ok("0\n"),
            _ => Reply::failed(),
        })
        .await;
        let handle = logged_in(&server).await;
        let progress = Recorded::default();
        let remote = remote(
            &handle,
            Some(loopback::PASSWORD),
            Redactor::new(vec![]),
            &progress,
        );
        assert_eq!(remote.privilege("root").await.unwrap(), Privilege::Root);
        assert_eq!(server.seen.commands().len(), 1);
    }

    #[tokio::test]
    async fn passwordless_sudo_is_used_when_there_is_one() {
        let server = loopback::start(|command, _| match command {
            "id -u" => Reply::ok("1000\n"),
            "sudo -n true" => Reply::ok(""),
            _ => Reply::failed(),
        })
        .await;
        let handle = logged_in(&server).await;
        let progress = Recorded::default();
        let remote = remote(
            &handle,
            Some(loopback::PASSWORD),
            Redactor::new(vec![]),
            &progress,
        );
        assert_eq!(remote.privilege("deploy").await.unwrap(), Privilege::Sudo);
    }

    #[tokio::test]
    async fn sudo_with_the_login_password_gets_it_on_stdin() {
        let server = loopback::start(|command, stdin| match command {
            "id -u" => Reply::ok("1000\n"),
            "sudo -k -S -p '' true" if stdin == format!("{}\n", loopback::PASSWORD).as_bytes() => {
                Reply::ok("")
            }
            _ => Reply::failed(),
        })
        .await;
        let handle = logged_in(&server).await;
        let progress = Recorded::default();
        let remote = remote(
            &handle,
            Some(loopback::PASSWORD),
            Redactor::new(vec![]),
            &progress,
        );
        assert_eq!(
            remote.privilege("deploy").await.unwrap(),
            Privilege::SudoWithPassword
        );
        let commands = server.seen.commands();
        assert_eq!(
            commands.last().unwrap(),
            &(
                "sudo -k -S -p '' true".to_string(),
                format!("{}\n", loopback::PASSWORD)
            )
        );
        // The password is never part of a command line.
        assert!(commands
            .iter()
            .all(|(command, _)| !command.contains(loopback::PASSWORD)));
    }

    #[tokio::test]
    async fn sudo_asking_for_a_password_without_one_is_explained() {
        let server = loopback::start(|command, _| match command {
            "id -u" => Reply::ok("1000\n"),
            _ => Reply::failed(),
        })
        .await;
        let handle = logged_in(&server).await;
        let progress = Recorded::default();
        let remote = remote(&handle, None, Redactor::new(vec![]), &progress);
        let error = remote.privilege("deploy").await.unwrap_err();
        assert!(
            matches!(&error, SshError::Remote(message) if message.contains("sudo asks for a password")),
            "{error}"
        );
        // Without a password, `sudo -S` is never tried.
        assert_eq!(server.seen.commands().len(), 2);
    }

    #[tokio::test]
    async fn a_password_sudo_refuses_is_explained() {
        let server = loopback::start(|command, _| match command {
            "id -u" => Reply::ok("1000\n"),
            _ => Reply::failed(),
        })
        .await;
        let handle = logged_in(&server).await;
        let progress = Recorded::default();
        let remote = remote(&handle, Some("wrong"), Redactor::new(vec![]), &progress);
        let error = remote.privilege("deploy").await.unwrap_err();
        assert!(
            matches!(&error, SshError::Remote(message) if message.contains("cannot use sudo")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn commands_as_root_get_the_sudo_password_first_then_their_stdin() {
        let server = loopback::start(|_, _| Reply::ok("")).await;
        let handle = logged_in(&server).await;
        let progress = Recorded::default();
        let remote = remote(
            &handle,
            Some(loopback::PASSWORD),
            Redactor::new(vec![]),
            &progress,
        );
        let payload = || Some(Zeroizing::new("tpair1.code\n".to_string()));

        remote
            .run(
                &Privilege::SudoWithPassword,
                "temps join --pair -",
                payload(),
                CHECK_TIMEOUT,
            )
            .await
            .unwrap();
        remote
            .run(&Privilege::SudoWithPassword, "true", None, CHECK_TIMEOUT)
            .await
            .unwrap();
        remote
            .run(
                &Privilege::Sudo,
                "temps join --pair -",
                payload(),
                CHECK_TIMEOUT,
            )
            .await
            .unwrap();
        remote
            .run(
                &Privilege::Root,
                "temps join --pair -",
                payload(),
                CHECK_TIMEOUT,
            )
            .await
            .unwrap();

        assert_eq!(
            server.seen.commands(),
            vec![
                (
                    "sudo -k -S -p '' -H temps join --pair -".to_string(),
                    format!("{}\ntpair1.code\n", loopback::PASSWORD)
                ),
                (
                    "sudo -k -S -p '' -H true".to_string(),
                    format!("{}\n", loopback::PASSWORD)
                ),
                (
                    "sudo -n -H temps join --pair -".to_string(),
                    "tpair1.code\n".to_string()
                ),
                (
                    "temps join --pair -".to_string(),
                    "tpair1.code\n".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn secrets_are_masked_on_stdout_and_stderr() {
        let (code, encoded) = pairing_code();
        for auth in [
            SshAuth::Password(Zeroizing::new(loopback::PASSWORD.into())),
            SshAuth::PrivateKey {
                key: private_key_pem(),
                passphrase: Some(Zeroizing::new(PASSPHRASE.into())),
            },
        ] {
            let secrets = secrets_of(&code, &encoded, &auth);
            let echoed: String = secrets
                .iter()
                .map(|secret| format!("\x1b[31mvalue:\x1b[0m {secret}\n"))
                .collect();
            let server = loopback::start(move |_, _| Reply {
                code: 1,
                stdout: echoed.as_bytes().to_vec(),
                stderr: echoed.as_bytes().to_vec(),
            })
            .await;
            let handle = logged_in(&server).await;
            let request = enrollment(auth, encoded.clone());
            let progress = Recorded::default();
            let remote = remote(&handle, None, Redactor::for_enrollment(&request), &progress);
            let output = remote
                .run(&Privilege::Root, "temps join --pair -", None, CHECK_TIMEOUT)
                .await
                .unwrap();
            // Every line from both streams was kept, and none shows a secret.
            assert_eq!(output.lines.len(), secrets.len() * 2);
            assert!(output.lines.iter().all(|line| line == "value: [redacted]"));
            assert_masked(&output.lines, &secrets);
            let logged = progress.lines.lock().unwrap().clone();
            assert_eq!(logged.len(), secrets.len() * 2);
            assert_masked(&logged, &secrets);
            assert_masked(&[output.tail()], &secrets);
        }
    }
}
