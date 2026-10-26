// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tailscale ways in for a private address (ADR 065): what the agent and the
//! control plane must agree on.
//!
//! The agent drives tailscaled on its own machine through the LocalAPI, and
//! only with the worker owner's consent (`--tailscale-serve` in
//! `agent.json`). It reports what it found ([`TailscaleReport`]), what it did
//! with each Serve entry the control plane declared ([`ServeEntryReport`]),
//! and its inside checks of the tailnet names it serves ([`TailnetCheck`]).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// tailscaled's LocalAPI socket on Linux.
pub const TAILSCALED_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";

/// The directory holding the per-address Serve sockets (ADR 065 D3), `0700`
/// and owned by root: only tailscaled (root) can connect to them. A Serve
/// handler whose target is a socket here is the agent's; every other one is
/// not.
pub const SERVE_SOCKET_DIR: &str = "/run/temps/serve";

/// The oldest tailscaled that can point Serve at a unix socket (v1.94.0).
pub const MIN_TAILSCALED: (u32, u32) = (1, 94);

/// What the worker owner runs to allow the agent to drive tailscaled.
pub const TAILSCALE_SERVE_COMMAND: &str =
    "temps agent ingress private --listen tailscale --tailscale-serve";

/// The capability an agent reports when it enforces `tailnet_users` (ADR 065
/// D6). A route with that audience is never sent to an agent without it.
pub const CAPABILITY_TAILNET_IDENTITY: &str = "tailnet_identity.v1";

/// The header an agent names its capabilities in on every route snapshot
/// request, comma-separated. The control plane decides what the snapshot may
/// carry from this request alone, never from an earlier report: a worker
/// rolled back to an older binary stops sending it on its next request.
pub const AGENT_CAPABILITIES_HEADER: &str = "x-temps-agent-capabilities";

/// Whether a capabilities header value names `capability`, exactly.
pub fn header_names_capability(value: Option<&str>, capability: &str) -> bool {
    value.is_some_and(|value| value.split(',').any(|item| item.trim() == capability))
}

/// The port every tailnet name is served on.
pub const SERVE_HTTPS_PORT: u16 = 443;

/// What the agent's inside check asks a Serve socket for (ADR 065 D5 step 5):
/// answered by the socket itself, never the app, and only to a request
/// without `X-Forwarded-For`, which tailscaled always sets.
pub const SERVE_PROBE_PATH: &str = "/.well-known/temps/serve-probe";

/// Set to the app by a Serve socket with the visitor's tailnet login (ADR
/// 065 D6). Removed from every incoming request on every listener.
pub const TAILNET_LOGIN_HEADER: &str = "x-temps-user-login";

/// Whether a request header carries tailnet identity (ADR 065 D3): every
/// `Tailscale-*` header and Temps' own login header. Lower-case `name`.
pub fn is_identity_header(name: &str) -> bool {
    name.starts_with("tailscale-") || name == TAILNET_LOGIN_HEADER
}

/// The identity headers tailscaled replaces on every request it serves
/// (`addTailscaleIdentityHeaders`): it deletes any client copy and sets its
/// own. Any other `Tailscale-User-*` name is the client's, not tailscaled's.
pub const SERVE_IDENTITY_HEADERS: [&str; 3] = [
    "tailscale-user-login",
    "tailscale-user-name",
    "tailscale-user-profile-pic",
];

/// Whether an incoming identity header is passed to the app: only the
/// [`SERVE_IDENTITY_HEADERS`] tailscaled sets, and only on a Serve socket.
/// Anything else that is not an identity header always passes.
pub fn forwards_header(name: &str, serve_socket: bool) -> bool {
    !is_identity_header(name) || (serve_socket && SERVE_IDENTITY_HEADERS.contains(&name))
}

/// The tailscaled resolver, asked directly by the inside check.
pub const MAGIC_DNS_RESOLVER: &str = "100.100.100.100";

/// Tailscale's IPv6 range, next to [`crate::worker_ingress::TAILSCALE_RANGE`].
pub const TAILSCALE_RANGE_V6: &str = "fd7a:115c:a1e0::/48";

/// The socket file for an address. Address ids are `custom:<id>` or
/// `environment:<id>`; the colon becomes `-` so the path never needs quoting
/// in a Serve target. `None` for anything else, so a malformed id can never
/// name a path outside [`SERVE_SOCKET_DIR`].
pub fn serve_socket_path(address_id: &str) -> Option<std::path::PathBuf> {
    let (kind, id) = address_id.split_once(':')?;
    if !matches!(kind, "custom" | "environment")
        || id.is_empty()
        || id.len() > 10
        || !id.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(std::path::Path::new(SERVE_SOCKET_DIR).join(format!("{kind}-{id}.sock")))
}

/// The Serve proxy target for an address: `unix:<socket path>`.
pub fn serve_target(address_id: &str) -> Option<String> {
    serve_socket_path(address_id).map(|path| format!("unix:{}", path.display()))
}

/// Whether a Serve proxy target is one of the agent's own sockets: the only
/// handlers it ever edits or removes (ADR 065 D2).
pub fn is_owned_target(target: &str) -> bool {
    is_owned_target_in(SERVE_SOCKET_DIR, target)
}

/// [`is_owned_target`] for sockets under `dir` (the agent's tests use a
/// temporary directory).
pub fn is_owned_target_in(dir: &str, target: &str) -> bool {
    target
        .strip_prefix("unix:")
        .and_then(|path| path.strip_prefix(dir.trim_end_matches('/')))
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| {
            !name.is_empty()
                && !name.contains('/')
                && name.ends_with(".sock")
                && !name.contains("..")
        })
}

/// `1.94.1-t0123…` → `(1, 94)`. `None` when it is not a version.
pub fn parse_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.trim().split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Whether this tailscaled can point Serve at a socket.
pub fn version_supported(version: &str) -> bool {
    parse_version(version).is_some_and(|v| v >= MIN_TAILSCALED)
}

/// Whether `ip` is a Tailscale node or Service address.
pub fn is_tailscale_ip(ip: std::net::IpAddr) -> bool {
    crate::worker_ingress::ip_in_range(ip, crate::worker_ingress::TAILSCALE_RANGE)
        || crate::worker_ingress::ip_in_range(ip, TAILSCALE_RANGE_V6)
}

/// A machine name or Service name as a DNS name: lower-case, no trailing dot.
pub fn normalize_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Whether `name` is usable as a Service name (`svc:` prefix optional): one
/// DNS label, lower-case letters, digits and `-`, not starting or ending with
/// `-`, at most 63 characters.
pub fn valid_service_label(name: &str) -> bool {
    let label = name.strip_prefix("svc:").unwrap_or(name);
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Where the agent stands with tailscaled, decided in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TailscaleState {
    /// The worker owner has not allowed it: the agent never opened the
    /// socket.
    #[default]
    NoConsent,
    /// The agent is not root, so tailscaled would refuse a socket target.
    NotRoot,
    /// No tailscaled socket on this machine.
    NoSocket,
    /// tailscaled answered something this agent does not understand.
    NotUnderstood,
    /// tailscaled older than [`MIN_TAILSCALED`].
    Unsupported,
    /// tailscaled answers, at a supported version.
    Ready,
}

/// What the agent found about its tailscaled (ADR 065 D2), reported with the
/// listener report. Only `consent`, `root` and `state` are meaningful
/// without consent: the agent never opens the socket then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct TailscaleReport {
    /// `--tailscale-serve` in `agent.json`.
    pub consent: bool,
    pub root: bool,
    /// The user the agent runs as, when not root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub state: TailscaleState,
    /// The failure message for `state`, or for a LocalAPI call that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `BackendState`: `Running`, `NeedsLogin`, `Stopped`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_state: Option<String>,
    /// `Self.DNSName`, normalized: `master.example.ts.net`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_name: Option<String>,
    /// `Self.ID`, the stable node id the Tailscale API lists a Service's
    /// hosts by (ADR 065 D4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// The MagicDNS suffix: `example.ts.net`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailnet: Option<String>,
    #[serde(default)]
    pub magic_dns: bool,
    /// `CertDomains`, normalized. HTTPS certificates are on when not empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cert_domains: Vec<String>,
    /// `Self.Tags`. A device signed in as a user has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The login the device is signed in as, when it has no tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// `Self.TailscaleIPs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// What holds `<machine>:443` in the Serve config that the agent did not
    /// write, in words (`http://127.0.0.1:3000`). `None` when free or the
    /// agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_443_held_by: Option<String>,
    /// Funnel is on for `<machine>:443`.
    #[serde(default)]
    pub port_443_funnel: bool,
    /// What the agent did with each declared Serve entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ServeEntryReport>,
    /// The inside checks of the tailnet names it serves (ADR 065 D5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<TailnetCheck>,
}

impl TailscaleReport {
    /// HTTPS certificates are on for the tailnet.
    pub fn https(&self) -> bool {
        !self.cert_domains.is_empty()
    }

    /// The machine can serve through Serve now: consent, root, a supported
    /// tailscaled that is running, MagicDNS and HTTPS on.
    pub fn serve_ready(&self) -> bool {
        self.consent
            && self.root
            && self.state == TailscaleState::Ready
            && self.backend_state.as_deref() == Some("Running")
            && self.magic_dns
            && self.https()
            && self.dns_name.is_some()
    }
}

/// What the agent did with one Serve entry (ADR 065 D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServeOutcome {
    /// Written, at `etag`.
    Applied,
    /// The slot holds something the agent did not write; nothing written.
    Conflict,
    /// The write failed; it may have landed.
    Error,
    /// Being removed: the agent removed its handler.
    Removed,
    /// Being removed: no handler of the agent's was there.
    Absent,
}

/// One entry's outcome, naming the row it is for: a report about an entry
/// that was since replaced is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServeEntryReport {
    pub entry_id: i32,
    pub address_id: String,
    pub outcome: ServeOutcome,
    /// The Serve config `Etag` after the agent's write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// What holds the slot, or the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Funnel is on for this entry's name, or a Funnel request reached its
    /// socket: the agent stopped serving it.
    #[serde(default)]
    pub funnel: bool,
}

/// How an inside check of a tailnet name came out (ADR 065 D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TailnetCheckOutcome {
    /// Every check passed.
    Reachable,
    /// tailscaled is not running or not supported.
    Tailscale,
    /// MagicDNS or HTTPS certificates are off, or the name is not covered.
    Certificates,
    /// The Serve entry is missing, changed, or not pointing at its socket.
    Entry,
    /// The name is on Funnel.
    Funnel,
    /// MagicDNS does not resolve the name.
    NotResolved,
    /// MagicDNS resolves it somewhere other than this worker or Service.
    ResolvesElsewhere,
    /// The socket is not bound, or does not route the address.
    NoRoute,
}

/// One inside check of a tailnet name, as the worker reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TailnetCheck {
    pub address_id: String,
    pub hostname: String,
    pub outcome: TailnetCheckOutcome,
    /// What happened, with what to do when it failed (ADR 065 D5's table).
    pub message: String,
}

/// The D5 failure messages, word for word, so the agent and the plan say
/// the same thing.
pub mod messages {
    pub fn no_socket(node: &str) -> String {
        format!(
            "Tailscale is not running on `{node}` (no `{}`). Install Tailscale and run `sudo \
             tailscale up`.",
            super::TAILSCALED_SOCKET
        )
    }

    pub fn not_logged_in(node: &str, state: &str) -> String {
        format!("Tailscale on `{node}` is `{state}`. Run `sudo tailscale up`.")
    }

    pub fn magic_dns_off(tailnet: &str) -> String {
        format!(
            "MagicDNS is off for tailnet `{tailnet}`. Turn it on at \
             login.tailscale.com/admin/dns, then turn on HTTPS certificates."
        )
    }

    pub fn https_off(tailnet: &str, name: &str) -> String {
        format!(
            "HTTPS certificates are off for tailnet `{tailnet}`, so Serve cannot give `{name}` a \
             certificate. Turn them on at login.tailscale.com/admin/dns. Machine names become \
             public in certificate logs."
        )
    }

    pub fn old_tailscaled(version: &str, node: &str) -> String {
        let (major, minor) = super::MIN_TAILSCALED;
        let shown = super::parse_version(version)
            .map(|(a, b)| format!("{a}.{b}"))
            .unwrap_or_else(|| version.to_string());
        format!(
            "tailscaled {shown} on `{node}` cannot send Serve traffic to a socket. Temps needs \
             {major}.{minor} or later."
        )
    }

    pub fn not_root(node: &str, user: &str) -> String {
        format!(
            "The Temps agent on `{node}` runs as `{user}`; Tailscale lets only root point Serve \
             at a socket."
        )
    }

    pub fn no_consent(node: &str) -> String {
        format!("Run `{}` on `{node}`.", super::TAILSCALE_SERVE_COMMAND)
    }

    pub fn not_tagged(node: &str, login: &str, machine_name: &str) -> String {
        format!(
            "`{node}` is signed in as {login}, and only tagged devices can host a Tailscale \
             Service. Run `sudo tailscale up --advertise-tags=tag:temps` (the tag must be in \
             your policy's `tagOwners`), or use the machine name `{machine_name}`."
        )
    }

    pub fn not_approved(service: &str, node: &str, tag: &str) -> String {
        format!(
            "`{service}` is advertised by `{node}` but not approved. Approve it at \
             login.tailscale.com/admin/services, or add `\"autoApprovers\": {{\"services\": \
             {{\"{service}\": [\"{tag}\"]}}}}` to your tailnet policy."
        )
    }

    pub fn slot_taken(name: &str, held_by: &str) -> String {
        format!(
            "`{name}` already serves `{held_by}` (set with `tailscale serve`, not by Temps). \
             Remove it with `tailscale serve --https=443 off`, or give this app a Service name."
        )
    }

    pub fn service_taken(service: &str) -> String {
        format!(
            "`{service}` already exists in your tailnet and Temps did not create it. Pick \
             another name."
        )
    }

    pub const CREDENTIAL_SCOPE: &str = "This Tailscale OAuth client cannot manage Services. \
         Create one with the `services` and `devices:core:read` scopes.";

    pub fn not_understood(version: &str, node: &str) -> String {
        let shown = super::parse_version(version)
            .map(|(a, b)| format!("{a}.{b}"))
            .unwrap_or_else(|| "1.x".to_string());
        format!(
            "tailscaled {shown} on `{node}` answered in a way this agent does not understand; \
             update Temps or report it"
        )
    }

    pub fn listener_exclusive(node: &str, listener: &str) -> String {
        format!(
            "`{node}` already serves private addresses on {listener}:443; give this app a \
             Service name instead"
        )
    }

    pub fn funnel(node: &str) -> String {
        format!(
            "This private address is also on Tailscale Funnel, which makes it public. Run \
             `tailscale funnel --https=443 off` on `{node}`."
        )
    }

    pub fn machine_name_cost(name: &str, node: &str) -> String {
        format!(
            "`{name}` becomes this app's address. Another app on `{node}` needs a Service name."
        )
    }

    /// Phase 2's plan, refusing the listener where Serve holds 443 (D1).
    pub fn serve_exclusive(node: &str, machine_name: &str, listener: &str) -> String {
        format!(
            "`{node}` serves `{machine_name}` with Tailscale Serve, which takes port 443 on its \
             Tailscale address before its private listener on {listener}:443 sees it. Give this \
             app a Service name instead, or bind the listener to another private address."
        )
    }

    /// A name under the tailnet that is not a machine's (D1).
    pub fn service_only(name: &str) -> String {
        format!(
            "`{name}` is not a machine's name in your tailnet, and only a Tailscale Service can \
             give an app any other name there. Use a machine's name, or a hostname on a domain \
             you own."
        )
    }

    /// A tagged device whose tags are not listed (D6, word for word).
    pub fn tagged_not_allowed(tags: &[String]) -> String {
        let tags = if tags.is_empty() {
            "untagged".to_string()
        } else {
            tags.iter()
                .map(|tag| format!("`{tag}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "This app admits only listed tailnet users. Requests from tagged devices ({tags}) \
             carry no user; ask the owner to add the tag."
        )
    }

    /// A signed-in tailnet user who is not listed.
    pub fn user_not_allowed(login: &str) -> String {
        format!(
            "This app admits only listed tailnet users, and `{login}` is not one of them. Ask \
             the owner to add you."
        )
    }

    /// A `tailnet_users` route with nobody on it: refused, never widened.
    pub const NOBODY_LISTED: &str = "This app admits only listed tailnet users, and nobody is \
         listed. Its owner can add people in Make it reachable.";

    /// The login header did not decode, or whois could not say who it is.
    pub const IDENTITY_UNREADABLE: &str = "This app admits only listed tailnet users, and \
         Temps could not tell who this request is from, so it is not admitted.";

    /// A request from the worker's own tailnet node: a process on the worker
    /// is not a tailnet user, whoever owns the machine.
    pub const SELF_NOT_ADMITTED: &str = "This app admits only listed tailnet users, and this \
         request came from the worker that serves it, which is not a tailnet user.";

    /// A tagged device shared in from another tailnet: its tags are named by
    /// that tailnet's admins, so no tag entry here can admit it.
    pub const SHARED_TAGGED_NOT_ADMITTED: &str = "This app admits only listed tailnet users, \
         and devices shared in from another tailnet are not admitted by tag. Use a device \
         signed in as a listed user instead.";

    /// A route held back from an agent without `tailnet_identity.v1`.
    pub fn update_agent(host: &str, node: &str) -> String {
        format!(
            "{host}: it admits only listed tailnet users, and the agent on `{node}` cannot \
             check who they are; update the agent on `{node}`"
        )
    }

    /// The plan, for `tailnet_users` on a listener that is not on Tailscale.
    pub fn users_need_tailnet(listener: &str) -> String {
        format!(
            "Only tailnet users can be listed, and the private listener on {listener} is not on \
             Tailscale. Bind it with `--listen tailscale`, or serve the app through a tailnet \
             name."
        )
    }

    /// The plan, for `tailnet_users` on a listener whose agent may not ask
    /// tailscaled who a visitor is.
    pub fn users_need_consent(node: &str) -> String {
        format!(
            "Temps asks Tailscale who each visitor is, which needs the worker owner's consent. \
             Run `{}` on `{node}`.",
            super::TAILSCALE_SERVE_COMMAND
        )
    }

    pub const NO_PRINCIPALS: &str =
        "List at least one tailnet user (`alice@example.com`) or tag (`tag:ci`).";
}

/// Most people and tags one address can list (D6).
pub const MAX_PRINCIPALS: usize = 64;

/// What a `tailnet_users` entry names (ADR 065 D6).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    /// A Tailscale login name: `alice@example.com`, `alice@github`.
    Login,
    /// A tag a tagged device carries: `tag:ci`.
    Tag,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::Login => "login",
            PrincipalKind::Tag => "tag",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "login" => Some(PrincipalKind::Login),
            "tag" => Some(PrincipalKind::Tag),
            _ => None,
        }
    }
}

/// One person or tag a `tailnet_users` address admits. `value` is always
/// ASCII-lower-cased: it is compared that way (D6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
pub struct TailnetPrincipal {
    pub kind: PrincipalKind,
    #[schema(example = "alice@example.com")]
    pub value: String,
}

impl TailnetPrincipal {
    /// What a person typed: `tag:<name>` is a tag, `<user>@<domain>` a login.
    pub fn parse(input: &str) -> Result<Self, String> {
        let value = input.trim().to_ascii_lowercase();
        if let Some(name) = value.strip_prefix("tag:") {
            let valid = !name.is_empty()
                && name.len() <= 64
                && name.starts_with(|c: char| c.is_ascii_alphanumeric())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            return if valid {
                Ok(Self {
                    kind: PrincipalKind::Tag,
                    value,
                })
            } else {
                Err(format!(
                    "`{}` is not a Tailscale tag: a tag is `tag:` and letters, digits or dashes",
                    input.trim()
                ))
            };
        }
        let valid = value.len() <= 320
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
            && value
                .split_once('@')
                .is_some_and(|(user, domain)| !user.is_empty() && !domain.is_empty());
        if valid {
            Ok(Self {
                kind: PrincipalKind::Login,
                value,
            })
        } else {
            Err(format!(
                "`{}` is not a Tailscale login (`alice@example.com`) or tag (`tag:ci`)",
                input.trim()
            ))
        }
    }

    /// Check a stored or requested entry the same way [`Self::parse`] does.
    pub fn normalized(&self) -> Result<Self, String> {
        let parsed = Self::parse(&self.value)?;
        if parsed.kind != self.kind {
            return Err(format!(
                "`{}` is a {}, not a {}",
                self.value,
                parsed.kind.as_str(),
                self.kind.as_str()
            ));
        }
        Ok(parsed)
    }
}

/// A list as stored: each entry checked, sorted, without duplicates, and no
/// longer than [`MAX_PRINCIPALS`]. Empty is allowed here; the plan refuses it.
pub fn normalize_principals(list: &[TailnetPrincipal]) -> Result<Vec<TailnetPrincipal>, String> {
    let mut out = list
        .iter()
        .map(TailnetPrincipal::normalized)
        .collect::<Result<Vec<_>, _>>()?;
    out.sort();
    out.dedup();
    if out.len() > MAX_PRINCIPALS {
        return Err(format!(
            "An address can list at most {MAX_PRINCIPALS} tailnet users and tags"
        ));
    }
    Ok(out)
}

/// Whether `principals` admits a visitor with this login (an untagged
/// device's user) or these tags. Exact compare after ASCII lower-casing; an
/// empty list admits nobody (D6).
pub fn admits(principals: &[TailnetPrincipal], login: Option<&str>, tags: &[String]) -> bool {
    principals.iter().any(|principal| match principal.kind {
        PrincipalKind::Login => login.is_some_and(|login| {
            !login.is_empty() && login.to_ascii_lowercase() == principal.value
        }),
        PrincipalKind::Tag => tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase() == principal.value),
    })
}

/// `Tailscale-User-Login` as tailscaled sets it: plain ASCII, or RFC 2047
/// Q-encoded words (`=?utf-8?q?...?=`) when the login is not. `None` for
/// anything else, which is refused rather than guessed at (D6).
pub fn decode_login_header(raw: &[u8]) -> Option<String> {
    if raw.is_empty() || !raw.is_ascii() {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?.trim();
    if text.is_empty() || text.chars().any(|c| c.is_control()) {
        return None;
    }
    if !text.contains("=?") {
        return Some(text.to_string());
    }
    let mut bytes = Vec::with_capacity(text.len());
    for word in text.split_ascii_whitespace() {
        let inner = word.strip_prefix("=?")?.strip_suffix("?=")?;
        let mut parts = inner.splitn(3, '?');
        let charset = parts.next()?;
        let encoding = parts.next()?;
        let encoded = parts.next()?;
        if !charset.eq_ignore_ascii_case("utf-8") || !encoding.eq_ignore_ascii_case("q") {
            return None;
        }
        let mut chars = encoded.bytes();
        while let Some(byte) = chars.next() {
            match byte {
                b'_' => bytes.push(b' '),
                b'=' => {
                    let hex = [chars.next()?, chars.next()?];
                    let hex = std::str::from_utf8(&hex).ok()?;
                    bytes.push(u8::from_str_radix(hex, 16).ok()?);
                }
                b'?' => return None,
                other => bytes.push(other),
            }
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    if decoded.is_empty() || decoded.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_paths_stay_in_the_serve_directory() {
        assert_eq!(
            serve_socket_path("custom:12").unwrap(),
            std::path::Path::new("/run/temps/serve/custom-12.sock")
        );
        assert_eq!(
            serve_target("environment:7").as_deref(),
            Some("unix:/run/temps/serve/environment-7.sock")
        );
        for bad in [
            "custom:../x",
            "custom:",
            "custom:1/2",
            "other:1",
            "custom",
            "custom:12345678901",
        ] {
            assert!(serve_socket_path(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn only_sockets_in_the_serve_directory_are_owned() {
        assert!(is_owned_target("unix:/run/temps/serve/custom-12.sock"));
        for foreign in [
            "http://127.0.0.1:3000",
            "unix:/run/temps/serve/../evil.sock",
            "unix:/run/temps/serve/sub/x.sock",
            "unix:/run/temps/servex/a.sock",
            "unix:/run/temps/serve/",
            "unix:/var/run/app.sock",
            "/run/temps/serve/custom-1.sock",
        ] {
            assert!(!is_owned_target(foreign), "{foreign}");
        }
    }

    #[test]
    fn versions_from_194_can_serve_to_a_socket() {
        assert!(version_supported("1.94.0"));
        assert!(version_supported("1.104.2-t5a2b1c-gdeadbeef"));
        assert!(version_supported("2.0.0"));
        assert!(!version_supported("1.92.3"));
        assert!(!version_supported("1.9"));
        assert!(!version_supported("unknown"));
        assert_eq!(parse_version("1.90.1-tabc"), Some((1, 90)));
    }

    #[test]
    fn messages_are_the_adr_table() {
        assert_eq!(
            messages::old_tailscaled("1.90.4", "master"),
            "tailscaled 1.90 on `master` cannot send Serve traffic to a socket. Temps needs \
             1.94 or later."
        );
        assert_eq!(
            messages::no_consent("master"),
            "Run `temps agent ingress private --listen tailscale --tailscale-serve` on `master`."
        );
        assert_eq!(
            messages::not_approved("svc:myapp", "master", "tag:temps"),
            "`svc:myapp` is advertised by `master` but not approved. Approve it at \
             login.tailscale.com/admin/services, or add `\"autoApprovers\": {\"services\": \
             {\"svc:myapp\": [\"tag:temps\"]}}` to your tailnet policy."
        );
        assert_eq!(
            messages::listener_exclusive("master", "100.101.102.103"),
            "`master` already serves private addresses on 100.101.102.103:443; give this app a \
             Service name instead"
        );
    }

    #[test]
    fn service_labels_are_one_dns_label() {
        assert!(valid_service_label("myapp-staging"));
        assert!(valid_service_label("svc:myapp"));
        for bad in ["", "MyApp", "-a", "a-", "a.b", "svc:", &"a".repeat(64)] {
            assert!(!valid_service_label(bad), "{bad}");
        }
    }

    #[test]
    fn a_report_is_ready_only_with_everything_on() {
        let ready = TailscaleReport {
            consent: true,
            root: true,
            state: TailscaleState::Ready,
            backend_state: Some("Running".into()),
            dns_name: Some("master.example.ts.net".into()),
            magic_dns: true,
            cert_domains: vec!["master.example.ts.net".into()],
            ..TailscaleReport::default()
        };
        assert!(ready.serve_ready());
        for broken in [
            TailscaleReport {
                consent: false,
                ..ready.clone()
            },
            TailscaleReport {
                root: false,
                ..ready.clone()
            },
            TailscaleReport {
                backend_state: Some("NeedsLogin".into()),
                ..ready.clone()
            },
            TailscaleReport {
                cert_domains: Vec::new(),
                ..ready.clone()
            },
            TailscaleReport {
                magic_dns: false,
                ..ready.clone()
            },
        ] {
            assert!(!broken.serve_ready());
        }
    }

    #[test]
    fn identity_headers_pass_only_on_a_serve_socket() {
        for name in [
            "tailscale-user-login",
            "tailscale-user-name",
            "tailscale-user-profile-pic",
        ] {
            assert!(forwards_header(name, true), "{name}");
            assert!(!forwards_header(name, false), "{name}");
        }
        // Funnel, anything else tailscaled may add, any `Tailscale-User-*`
        // it does not replace (a client could send it through tailscaled),
        // and Temps' own login header never reach the app from a request.
        for name in [
            "tailscale-user-role",
            "tailscale-user-groups",
            "tailscale-user-",
            "tailscale-funnel-request",
            "tailscale-app-capabilities",
            "tailscale-headers-info",
            TAILNET_LOGIN_HEADER,
        ] {
            assert!(!forwards_header(name, true), "{name}");
            assert!(!forwards_header(name, false), "{name}");
        }
        assert!(forwards_header("accept", false));
        assert!(forwards_header("x-tailscale-thing", false));
    }

    #[test]
    fn a_capability_counts_only_when_the_request_names_it_exactly() {
        let cap = CAPABILITY_TAILNET_IDENTITY;
        assert!(!header_names_capability(None, cap));
        assert!(!header_names_capability(Some(""), cap));
        assert!(!header_names_capability(Some("tailnet_identity.v2"), cap));
        assert!(!header_names_capability(Some("tailnet_identity.v1x"), cap));
        assert!(header_names_capability(Some("tailnet_identity.v1"), cap));
        assert!(header_names_capability(
            Some("other.v3, tailnet_identity.v1"),
            cap
        ));
    }

    fn login(value: &str) -> TailnetPrincipal {
        TailnetPrincipal::parse(value).unwrap()
    }

    #[test]
    fn principals_parse_lower_cased_and_refuse_what_is_neither() {
        assert_eq!(
            TailnetPrincipal::parse(" Alice@Example.COM ").unwrap(),
            TailnetPrincipal {
                kind: PrincipalKind::Login,
                value: "alice@example.com".into()
            }
        );
        assert_eq!(login("alice@github").kind, PrincipalKind::Login);
        assert_eq!(
            TailnetPrincipal::parse("tag:CI").unwrap(),
            TailnetPrincipal {
                kind: PrincipalKind::Tag,
                value: "tag:ci".into()
            }
        );
        for bad in [
            "",
            "alice",
            "@example.com",
            "alice@",
            "tag:",
            "tag:-x",
            "tag:a b",
            "a b@c",
        ] {
            assert!(TailnetPrincipal::parse(bad).is_err(), "{bad}");
        }
        // A stored login that is really a tag is not taken as a login.
        let wrong = TailnetPrincipal {
            kind: PrincipalKind::Login,
            value: "tag:ci".into(),
        };
        assert!(wrong.normalized().is_err());
    }

    #[test]
    fn a_list_is_sorted_deduplicated_and_bounded() {
        let list = vec![
            login("tag:ci"),
            login("Bob@example.com"),
            login("bob@example.com"),
        ];
        let normalized = normalize_principals(&list).unwrap();
        assert_eq!(normalized, vec![login("bob@example.com"), login("tag:ci")]);
        let many: Vec<_> = (0..=MAX_PRINCIPALS)
            .map(|i| login(&format!("user{i}@example.com")))
            .collect();
        assert!(normalize_principals(&many).is_err());
        assert_eq!(normalize_principals(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn admission_is_exact_after_ascii_lower_casing_and_empty_admits_nobody() {
        let list = vec![login("alice@example.com"), login("tag:ci")];
        assert!(admits(&list, Some("Alice@Example.com"), &[]));
        assert!(!admits(&list, Some("alice@example.co"), &[]));
        assert!(!admits(&list, Some("alice@example.com.evil"), &[]));
        assert!(!admits(&list, Some(""), &[]));
        assert!(admits(&list, None, &["tag:CI".to_string()]));
        assert!(!admits(&list, None, &["tag:server".to_string()]));
        // A login never matches a tag entry, nor a tag a login entry.
        assert!(!admits(&list, Some("tag:ci"), &[]));
        assert!(!admits(
            &[],
            Some("alice@example.com"),
            &["tag:ci".to_string()]
        ));
    }

    #[test]
    fn the_login_header_is_q_decoded_or_refused() {
        assert_eq!(
            decode_login_header(b"alice@example.com").as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(
            decode_login_header(b"=?utf-8?q?j=C3=B6rg@example.com?=").as_deref(),
            Some("j\u{f6}rg@example.com")
        );
        // Encoded words split by whitespace join without it.
        assert_eq!(
            decode_login_header(b"=?UTF-8?Q?j=C3=B6?= =?utf-8?q?rg@example.com?=").as_deref(),
            Some("j\u{f6}rg@example.com")
        );
        for bad in [
            &b""[..],
            b"=?utf-8?b?am9yZw==?=",
            b"=?iso-8859-1?q?j=F6rg?=",
            b"=?utf-8?q?j=C3?=",
            b"=?utf-8?q?j=ZZ?=",
            b"=?utf-8?q?a?= plain",
            b"=?utf-8?q?a=0Ab?=",
            b"j\xc3\xb6rg@example.com",
            b"=?utf-8?q?unterminated",
        ] {
            assert_eq!(decode_login_header(bad), None, "{bad:?}");
        }
    }
}
