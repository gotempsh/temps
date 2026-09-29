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
/// Lines of remote output kept per command for error messages.
const TAIL_LINES: usize = 12;
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
        // `expected` is `None` only when reading the key for the operator to
        // confirm (`host_key`), which never logs in. Everything that logs in
        // passes the fingerprint the operator confirmed.
        let accepted = self
            .expected
            .as_deref()
            .is_none_or(|expected| expected == presented.fingerprint);
        if let Ok(mut slot) = self.presented.lock() {
            *slot = Some(presented);
        }
        Ok(accepted)
    }
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
        (Err(_), Some(key), Some(expected)) if key.fingerprint != expected => {
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
struct Redactor {
    secrets: Vec<Zeroizing<String>>,
}

const REDACTED: &str = "[redacted]";

impl Redactor {
    fn for_enrollment(request: &Enrollment) -> Self {
        let mut secrets = vec![request.pairing_code.clone()];
        if let Ok(code) = temps_wireguard::pairing::PairingCode::decode(&request.pairing_code) {
            secrets.push(Zeroizing::new(code.secret));
            secrets.push(Zeroizing::new(code.join_token));
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

    fn new(mut secrets: Vec<Zeroizing<String>>) -> Self {
        // Too short to mask without mangling ordinary output; the longest
        // first, so one secret containing another is masked whole.
        secrets.retain(|secret| secret.len() >= 4);
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        Self { secrets }
    }

    fn apply(&self, line: String) -> String {
        self.secrets.iter().fold(line, |line, secret| {
            if line.contains(secret.as_str()) {
                line.replace(secret.as_str(), REDACTED)
            } else {
                line
            }
        })
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
        let mut pending = Vec::new();
        let mut lines = Vec::new();
        let collect = |pending: &mut Vec<u8>, lines: &mut Vec<String>, flush: bool| {
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                lines.push(self.redactor.apply(printable(&line)));
            }
            if flush && !pending.is_empty() {
                lines.push(self.redactor.apply(printable(pending)));
                pending.clear();
            }
        };
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
                    pending.extend_from_slice(&data);
                    let before = lines.len();
                    collect(&mut pending, &mut lines, false);
                    if stream {
                        for line in &lines[before..] {
                            self.progress.log(line);
                        }
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
        let before = lines.len();
        collect(&mut pending, &mut lines, true);
        if stream {
            for line in &lines[before..] {
                self.progress.log(line);
            }
        }
        Ok(Output {
            // A command killed by a signal reports no status.
            code: code.unwrap_or(255),
            lines,
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
            redactor.apply("sudo: hunter2-password was wrong".to_string()),
            "sudo: [redacted] was wrong"
        );
        assert_eq!(
            redactor.apply("code tpair1.abcdef.longer and tpair1.abcdef".to_string()),
            "code [redacted] and [redacted]"
        );
        // Too short to mask safely.
        assert_eq!(redactor.apply("abc".to_string()), "abc");
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
}
