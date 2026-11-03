// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-sandbox egress proxy sidecar: its script, environment and
//! policy labels, and creating, replacing and parking it.

use super::*;

pub(super) fn sandbox_container_environment(
    mut environment: HashMap<String, String>,
    docker_network: &str,
) -> Vec<String> {
    if is_managed_sandbox_network(docker_network) {
        let proxy = format!("http://{SANDBOX_EGRESS_PROXY_ALIAS}:{SANDBOX_EGRESS_PROXY_PORT}");
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            environment.insert(key.to_string(), proxy.clone());
        }
        let no_proxy = "localhost,127.0.0.1,::1,temps-sandbox-egress-proxy";
        environment.insert("NO_PROXY".to_string(), no_proxy.to_string());
        environment.insert("no_proxy".to_string(), no_proxy.to_string());
    }

    let mut entries = environment
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    entries.sort_unstable();
    entries
}

pub(super) fn sandbox_egress_proxy_name(container_name: &str) -> String {
    format!("{SANDBOX_EGRESS_PROXY_PREFIX}{container_name}")
}

pub(super) fn sandbox_egress_proxy_extra_hosts() -> Vec<String> {
    vec![SANDBOX_HOST_GATEWAY.to_string()]
}

pub(super) fn container_has_extra_host(
    container: &bollard::models::ContainerInspectResponse,
    expected: &str,
) -> bool {
    container
        .host_config
        .as_ref()
        .and_then(|config| config.extra_hosts.as_ref())
        .is_some_and(|hosts| hosts.iter().any(|host| host == expected))
}

fn container_has_environment_value(
    container: &bollard::models::ContainerInspectResponse,
    expected_key: &str,
    expected_value: &str,
) -> bool {
    container
        .config
        .as_ref()
        .and_then(|config| config.env.as_ref())
        .is_some_and(|environment| {
            environment.iter().any(|entry| {
                entry
                    .split_once('=')
                    .is_some_and(|(key, value)| key == expected_key && value == expected_value)
            })
        })
}

/// The value of `key` in the container's environment, when it is set.
fn container_environment_value<'a>(
    container: &'a bollard::models::ContainerInspectResponse,
    key: &str,
) -> Option<&'a str> {
    container
        .config
        .as_ref()
        .and_then(|config| config.env.as_ref())
        .and_then(|environment| {
            environment.iter().find_map(|entry| {
                entry
                    .split_once('=')
                    .and_then(|(name, value)| (name == key).then_some(value))
            })
        })
}

/// A sandbox's egress policy in the form the proxy sidecar receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidecarEgressPolicy {
    /// Canonical JSON of the normalized policy.
    pub(super) json: String,
    /// SHA-256 of `json`, hex encoded.
    pub(super) digest: String,
}

impl SidecarEgressPolicy {
    pub(super) fn new(sandbox: &str, policy: &SandboxEgressPolicy) -> Result<Self, AgentError> {
        let policy = policy
            .normalized()
            .map_err(|error| AgentError::Validation {
                message: format!("sandbox '{sandbox}': {error}"),
            })?;
        let json = serde_json::to_string(&policy).map_err(|error| {
            AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("serialize the egress policy of sandbox '{sandbox}': {error}"),
            }
        })?;
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(json.as_bytes()));
        Ok(Self { json, digest })
    }

    /// Whether `container` is a sidecar created for exactly this policy and
    /// sandbox.
    pub(super) fn is_carried_by(
        &self,
        container: &bollard::models::ContainerInspectResponse,
        sandbox: &str,
    ) -> bool {
        container
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .and_then(|labels| labels.get(SANDBOX_EGRESS_RULES_LABEL))
            .is_some_and(|digest| *digest == self.digest)
            && container_has_environment_value(container, SANDBOX_EGRESS_RULES_ENV, &self.json)
            && container_has_environment_value(container, SANDBOX_NAME_ENV, sandbox)
    }
}

/// The policy an existing egress sidecar enforces: `internet` when it
/// carries none (it predates egress policies). A policy that no longer
/// parses is an error rather than `internet`, so a sandbox whose rules
/// cannot be read never silently gets the whole internet back.
pub(super) fn egress_policy_of_sidecar(
    proxy_name: &str,
    container: &bollard::models::ContainerInspectResponse,
) -> Result<SandboxEgressPolicy, AgentError> {
    let Some(raw) = container_environment_value(container, SANDBOX_EGRESS_RULES_ENV) else {
        return Ok(SandboxEgressPolicy::internet());
    };
    serde_json::from_str::<SandboxEgressPolicy>(raw)
        .map_err(|error| error.to_string())
        .and_then(|policy| policy.normalized().map_err(|error| error.to_string()))
        .map_err(|reason| AgentError::SandboxProviderUnavailable {
            provider: "docker".to_string(),
            reason: format!(
                "the egress policy on sandbox egress proxy '{proxy_name}' cannot be read \
                 ({reason}); re-apply the sandbox's egress policy to replace it"
            ),
        })
}

/// A recovered network-enabled container must satisfy the current policy,
/// not merely still be running. This makes upgrades fail closed: a container
/// left on the legacy outward-routed bridge is recreated by the registry and
/// retains its named home/workspace volumes, instead of bypassing the new
/// boundary for the rest of its lifetime.
pub(super) fn recovered_container_matches_egress_policy(
    container: &bollard::models::ContainerInspectResponse,
    container_name: &str,
) -> bool {
    let network_mode = container
        .host_config
        .as_ref()
        .and_then(|config| config.network_mode.as_deref());
    let networks = container
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref());
    if network_mode == Some("none") {
        // Docker may explicitly report its built-in null network. It has no
        // external connectivity; any additional network remains forbidden.
        return networks.is_none_or(|networks| networks.keys().all(|name| name == "none"));
    }

    let expected_network = sandbox_network_name(container_name);
    let networks_match = networks.is_some_and(|networks| {
        networks.contains_key(&expected_network)
            && networks
                .keys()
                .all(|name| name == &expected_network || name.starts_with("temps-sandbox-data-"))
    });
    if !networks_match {
        return false;
    }

    let proxy = format!("http://{SANDBOX_EGRESS_PROXY_ALIAS}:{SANDBOX_EGRESS_PROXY_PORT}");
    ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"]
        .into_iter()
        .all(|key| container_has_environment_value(container, key, &proxy))
        && ["NO_PROXY", "no_proxy"].into_iter().all(|key| {
            container_has_environment_value(
                container,
                key,
                "localhost,127.0.0.1,::1,temps-sandbox-egress-proxy",
            )
        })
}

pub(super) fn sandbox_mcp_relay_url(
    control_plane_url: &str,
    registered_url: &str,
) -> Option<String> {
    let expected_prefix = format!(
        "{}/api/ai/sandbox-tools/",
        control_plane_url.trim_end_matches('/')
    );
    let bridge_id = registered_url
        .strip_prefix(&expected_prefix)?
        .strip_suffix("/mcp")?;
    (bridge_id.len() == 32
        && bridge_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then(|| format!("{SANDBOX_MCP_RELAY_BASE_URL}/{bridge_id}"))
}

/// The egress proxy is the only container with a NIC on both the internal
/// sandbox network and this ordinary outbound bridge.
pub(super) const SANDBOX_EGRESS_NETWORK: &str = "temps-sandbox-egress-v1";
const SANDBOX_EGRESS_PROXY_PREFIX: &str = "temps-sandbox-egress-proxy-v2-";
pub(super) const SANDBOX_EGRESS_PROXY_ALIAS: &str = "temps-sandbox-egress-proxy";
pub(super) const SANDBOX_EGRESS_PROXY_PORT: u16 = 3128;
const SANDBOX_CONTROL_PLANE_URL_ENV: &str = "TEMPS_CONTROL_PLANE_URL";
pub(super) const SANDBOX_HOST_GATEWAY: &str = "host.docker.internal:host-gateway";
/// A direct, capability-only route exposed by the per-sandbox egress proxy.
/// It is deliberately included in `NO_PROXY`: requests go to the sidecar
/// itself, whose reverse-proxy handler accepts only model relay paths.
pub const SANDBOX_MODEL_RELAY_BASE_URL: &str =
    "http://temps-sandbox-egress-proxy:3128/.temps/model-relay";
pub(super) const SANDBOX_EGRESS_POLICY_LABEL: &str = "sh.temps.sandbox-egress-policy";
pub(super) const SANDBOX_EGRESS_POLICY_VERSION: &str = "1";
/// The sandbox's normalized [`SandboxEgressPolicy`], as JSON, in the egress
/// proxy's environment. A sidecar without it enforces `internet`, which is
/// what every sidecar created before egress policies existed did.
pub(super) const SANDBOX_EGRESS_RULES_ENV: &str = "TEMPS_SANDBOX_EGRESS_POLICY";
/// SHA-256 (hex) of that JSON on the sidecar, so a policy change shows up as
/// drift and `ensure_egress_proxy` replaces the sidecar.
pub(super) const SANDBOX_EGRESS_RULES_LABEL: &str = "sh.temps.sandbox-egress-rules";
/// The sandbox a sidecar serves, so its refusals name it.
pub(super) const SANDBOX_NAME_ENV: &str = "TEMPS_SANDBOX_NAME";

pub(super) const SANDBOX_MCP_RELAY_BASE_URL: &str =
    "http://temps-sandbox-egress-proxy:3128/.temps/mcp";

/// Small CONNECT/HTTP forward proxy used as the sandbox's only internet
/// route. It applies the sandbox's egress policy (`TEMPS_SANDBOX_EGRESS_POLICY`,
/// the normalized [`SandboxEgressPolicy`] as JSON): in `internet` mode any
/// public destination on ports 80/443, in `restricted` mode only the hosts and
/// CIDR blocks a rule names, on that rule's ports, and a name no rule could
/// allow is refused before it is even resolved. It resolves the destination
/// itself, rejects the request when *any* answer is private/non-routable
/// (which closes DNS-rebinding fallbacks, and holds in every mode), and
/// connects to the already-validated IP rather than resolving the hostname a
/// second time. A refusal is a 403 naming the sandbox and destination, and
/// one rate-limited JSON line on stdout. The Temps relays (model, MCP, git)
/// are served by the proxy itself and work whatever the policy.
///
/// The process runs non-root in a read-only, capability-free container. It
/// carries no credentials and does not terminate TLS.
pub(super) const SANDBOX_EGRESS_PROXY_SCRIPT: &str = r#"
const http = require("http");
const https = require("https");
const net = require("net");
const dns = require("dns").promises;

const proxyAuthority = "temps-sandbox-egress-proxy:3128";
const controlPlane = new URL(process.env.TEMPS_CONTROL_PLANE_URL);
if (!['http:', 'https:'].includes(controlPlane.protocol) ||
    controlPlane.username || controlPlane.password) {
  throw new Error("TEMPS_CONTROL_PLANE_URL must be an HTTP(S) origin without credentials");
}
const sandboxName = process.env.TEMPS_SANDBOX_NAME || "unknown";

function isPrivateAddress(address) {
  if (address.startsWith("::ffff:")) address = address.slice(7);
  if (net.isIPv4(address)) {
    const octets = address.split(".").map(Number);
    return octets[0] === 0 || octets[0] === 10 || octets[0] === 127 ||
      (octets[0] === 100 && octets[1] >= 64 && octets[1] <= 127) ||
      (octets[0] === 169 && octets[1] === 254) ||
      (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31) ||
      (octets[0] === 192 && octets[1] === 0 && octets[2] === 0) ||
      (octets[0] === 192 && octets[1] === 0 && octets[2] === 2) ||
      (octets[0] === 192 && octets[1] === 88 && octets[2] === 99) ||
      (octets[0] === 192 && octets[1] === 168) ||
      (octets[0] === 198 && (octets[1] === 18 || octets[1] === 19)) ||
      (octets[0] === 198 && octets[1] === 51 && octets[2] === 100) ||
      (octets[0] === 203 && octets[1] === 0 && octets[2] === 113) ||
      octets[0] >= 224;
  }
  const normalized = address.toLowerCase();
  // Only the global-unicast 2000::/3 range is eligible. This conservative
  // rule also rejects IPv4-mapped, loopback, link-local, ULA, multicast,
  // documentation, and transition ranges.
  return (!normalized.startsWith("2") && !normalized.startsWith("3")) ||
    normalized.startsWith("2001:2:") || normalized.startsWith("2001:db8:");
}

// ---- egress policy (begin) ----
// The sandbox's egress policy, set by its owner and validated by Temps:
// `internet` allows any public destination on ports 80 and 443;
// `restricted` allows only destinations a rule names. Private destinations
// are refused in both modes, before any rule is consulted.

function normalizeHost(value) {
  let host = String(value).trim().toLowerCase();
  if (host.startsWith("[") && host.endsWith("]")) host = host.slice(1, -1);
  if (host.endsWith(".")) host = host.slice(0, -1);
  return host;
}

function canonicalAddress(address) {
  const lowered = String(address).toLowerCase();
  return lowered.startsWith("::ffff:") && net.isIPv4(lowered.slice(7)) ? lowered.slice(7) : lowered;
}

function loadEgressPolicy(raw) {
  if (raw === undefined || raw === "") return { mode: "internet", rules: [] };
  const parsed = JSON.parse(raw);
  if (parsed.mode !== "internet" && parsed.mode !== "restricted") {
    throw new Error(`unknown egress policy mode ${JSON.stringify(parsed.mode)}`);
  }
  const rules = (parsed.rules || []).map((rule) => {
    const listed = Array.isArray(rule.ports) && rule.ports.length > 0 ? rule.ports : [80, 443];
    const ports = new Set(listed.map(Number));
    if (typeof rule.host === "string") return { host: normalizeHost(rule.host), ports };
    if (typeof rule.cidr === "string") {
      const slash = rule.cidr.lastIndexOf("/");
      const address = canonicalAddress(slash === -1 ? rule.cidr : rule.cidr.slice(0, slash));
      const family = net.isIPv4(address) ? "ipv4" : "ipv6";
      const prefix = slash === -1 ? (family === "ipv4" ? 32 : 128) : Number(rule.cidr.slice(slash + 1));
      const blocks = new net.BlockList();
      blocks.addSubnet(address, prefix, family);
      return { cidr: rule.cidr, family, blocks, ports };
    }
    throw new Error("egress rule names neither a host nor a cidr");
  });
  if (parsed.mode === "internet" && rules.length > 0) {
    throw new Error("egress mode internet takes no rules");
  }
  return { mode: parsed.mode, rules };
}

function hostMatches(pattern, host) {
  if (pattern.startsWith("*.")) {
    const suffix = pattern.slice(1);
    return host.length > suffix.length && host.endsWith(suffix);
  }
  return pattern === host;
}

function addressInRule(rule, address) {
  if (rule.blocks === undefined) return false;
  const candidate = canonicalAddress(address);
  const family = net.isIPv4(candidate) ? "ipv4" : "ipv6";
  return rule.family === family && rule.blocks.check(candidate, family);
}

// Whether a destination could be allowed at all, decided before any DNS
// lookup: a restricted sandbox cannot use name resolution to reach (or leak
// data to) a name no rule allows.
function mayResolve(policy, hostname, port) {
  if (policy.mode === "internet") return [80, 443].includes(port);
  const host = normalizeHost(hostname);
  const literal = net.isIP(host) !== 0;
  return policy.rules.some((rule) => rule.ports.has(port) &&
    (rule.cidr !== undefined || (!literal && hostMatches(rule.host, host))));
}

// The decision for a destination the sandbox named as `hostname`, resolved
// to `answers` ([{ address, family }]). Returns { address } to connect to,
// or { denied: "private" | "policy" }.
function egressDecision(policy, hostname, port, answers) {
  if (answers.length === 0 || answers.some(({ address }) => isPrivateAddress(address))) {
    return { denied: "private" };
  }
  const ordered = [
    ...answers.filter(({ family }) => family === 4),
    ...answers.filter(({ family }) => family !== 4),
  ];
  if (policy.mode === "internet") {
    return [80, 443].includes(port) ? { address: ordered[0].address } : { denied: "policy" };
  }
  const host = normalizeHost(hostname);
  const literal = net.isIP(host) !== 0;
  const rules = policy.rules.filter((rule) => rule.ports.has(port));
  if (!literal && rules.some((rule) => rule.host !== undefined && hostMatches(rule.host, host))) {
    return { address: ordered[0].address };
  }
  const match = ordered.find(({ address }) => rules.some((rule) => addressInRule(rule, address)));
  return match ? { address: match.address } : { denied: "policy" };
}
// ---- egress policy (end) ----

// A malformed policy stops the proxy: the sandbox then has no route out at
// all, never a wider one.
const egressPolicy = loadEgressPolicy(process.env.TEMPS_SANDBOX_EGRESS_POLICY);

class Denied extends Error {
  constructor(reason, destination) {
    super(`egress to ${destination} denied (${reason})`);
    this.reason = reason;
    this.destination = destination;
  }
}

function formatDestination(hostname, port) {
  const host = normalizeHost(hostname);
  return net.isIPv6(host) ? `[${host}]:${port}` : `${host}:${port}`;
}

function denialExplanation(reason) {
  switch (reason) {
    case "policy":
      return egressPolicy.mode === "restricted"
        ? "the sandbox's egress policy does not allow it (mode restricted: only the listed hosts and CIDR blocks, on their ports)"
        : "the sandbox's egress policy does not allow it (mode internet: ports 80 and 443 only)";
    case "private":
      return "private, loopback, link-local and metadata addresses are never reachable from a sandbox";
    case "unresolvable":
      return "the name did not resolve";
    default:
      return "only plain http:// requests and CONNECT tunnels can leave the sandbox";
  }
}

const denialLog = { windowStart: 0, logged: 0, suppressed: 0 };
function logDenial(method, reason, destination) {
  const now = Date.now();
  if (now - denialLog.windowStart >= 10_000) {
    if (denialLog.suppressed > 0) {
      process.stdout.write(JSON.stringify({
        level: "warn", event: "sandbox_egress_denied_suppressed", sandbox: sandboxName,
        suppressed: denialLog.suppressed, time: new Date(now).toISOString(),
      }) + "\n");
    }
    denialLog.windowStart = now;
    denialLog.logged = 0;
    denialLog.suppressed = 0;
  }
  if (denialLog.logged >= 20) {
    denialLog.suppressed += 1;
    return;
  }
  denialLog.logged += 1;
  process.stdout.write(JSON.stringify({
    level: "warn", event: "sandbox_egress_denied", sandbox: sandboxName, destination,
    method, reason, policy_mode: egressPolicy.mode, time: new Date(now).toISOString(),
  }) + "\n");
}

function denial(error, fallbackDestination, method) {
  const reason = error instanceof Denied ? error.reason : "invalid";
  const destination = error instanceof Denied ? error.destination : fallbackDestination;
  logDenial(method, reason, destination);
  return `sandbox ${sandboxName}: egress to ${destination} denied: ${denialExplanation(reason)}\n`;
}

function refuseRequest(request, response, error) {
  const body = denial(error, String(request.url).slice(0, 256), request.method);
  if (!response.headersSent) {
    response.writeHead(403, {
      "content-type": "text/plain; charset=utf-8",
      "content-length": Buffer.byteLength(body),
    });
  }
  response.end(body);
}

// Admit an outbound destination: the policy, then DNS, then the policy and
// the private-address check against what the name resolved to. Returns the
// address to connect to, so the name is never resolved a second time.
async function admit(hostname, port) {
  const host = normalizeHost(hostname);
  const destination = formatDestination(host, port);
  if (!mayResolve(egressPolicy, host, port)) throw new Denied("policy", destination);
  let answers;
  try {
    answers = await dns.lookup(host, { all: true, verbatim: true });
  } catch (_) {
    throw new Denied("unresolvable", destination);
  }
  const decision = egressDecision(egressPolicy, host, port, answers);
  if (decision.denied) throw new Denied(decision.denied, destination);
  return decision.address;
}

function parseAuthority(value, defaultPort) {
  const authority = String(value);
  const match = /^(\[[0-9A-Fa-f:.]+\]|[^\s:\/?#@\[\]]+)(?::([0-9]{1,5}))?$/.exec(authority);
  if (!match) throw new Denied("invalid", authority.slice(0, 256));
  const port = match[2] === undefined ? defaultPort : Number(match[2]);
  if (port < 1 || port > 65535) throw new Denied("invalid", authority.slice(0, 256));
  const parsed = new URL(`http://${match[1]}`);
  return { hostname: normalizeHost(parsed.hostname), port };
}

function modelRelayTarget(request) {
  const incoming = new URL(request.url, `http://${request.headers.host || proxyAuthority}`);
  if (incoming.host !== proxyAuthority) return null;
  let match;
  if (request.method === "POST") {
    match = incoming.pathname.match(
      /^\/\.temps\/model-relay\/([a-f0-9]{32})\/(v1\/messages(?:\/count_tokens)?|responses|chat\/completions)$/
    );
    const isAnthropicMessages = match && match[2].startsWith("v1/messages");
    if (incoming.search && (!isAnthropicMessages || incoming.search !== "?beta=true")) {
      return null;
    }
  } else if (request.method === "GET") {
    match = incoming.pathname.match(
      /^\/\.temps\/model-relay\/([a-f0-9]{32})\/(models)$/
    );
    const queryKeys = [...incoming.searchParams.keys()];
    const clientVersion = incoming.searchParams.get("client_version");
    if (queryKeys.some((key) => key !== "client_version") ||
        queryKeys.filter((key) => key === "client_version").length > 1 ||
        (clientVersion !== null && !/^[A-Za-z0-9._+-]{1,64}$/.test(clientVersion))) {
      return null;
    }
  } else {
    return null;
  }
  if (!match) return null;
  const basePath = controlPlane.pathname.replace(/\/$/, "");
  const target = new URL(controlPlane.toString());
  target.pathname = `${basePath}/api/ai/sandbox-models/${match[1]}/${match[2]}`;
  target.search = incoming.search;
  return target;
}

function mcpTarget(request) {
  const incoming = new URL(request.url, `http://${request.headers.host || proxyAuthority}`);
  if (request.method !== "POST" || incoming.host !== proxyAuthority) return null;
  const match = incoming.pathname.match(/^\/\.temps\/mcp\/([a-f0-9]{32})$/);
  if (!match) return null;
  const basePath = controlPlane.pathname.replace(/\/$/, "");
  const target = new URL(controlPlane.toString());
  target.pathname = `${basePath}/api/ai/sandbox-tools/${match[1]}/mcp`;
  target.search = "";
  return target;
}

function gitRelayTarget(request) {
  const incoming = new URL(request.url, `http://${request.headers.host || proxyAuthority}`);
  if (incoming.host !== proxyAuthority) return null;
  let match;
  if (request.method === "GET") {
    match = incoming.pathname.match(
      /^\/\.temps\/git\/([a-f0-9]{32})\/([1-9][0-9]*)\/(info\/refs)$/
    );
    if (!match ||
        !["?service=git-upload-pack", "?service=git-receive-pack"].includes(incoming.search)) {
      return null;
    }
  } else if (request.method === "POST") {
    match = incoming.pathname.match(
      /^\/\.temps\/git\/([a-f0-9]{32})\/([1-9][0-9]*)\/(git-upload-pack|git-receive-pack)$/
    );
    if (!match || incoming.search) return null;
  } else {
    return null;
  }
  const basePath = controlPlane.pathname.replace(/\/$/, "");
  const target = new URL(controlPlane.toString());
  target.pathname = `${basePath}/api/git/sandbox-relay/${match[1]}/${match[2]}/${match[3]}`;
  target.search = incoming.search;
  return target;
}

function gitRelayAuthorized(_request) {
  // Fail closed until the control plane issues and the sidecar verifies a
  // credential that is independent of the relay URL and Host header.
  return false;
}

async function forwardGitRelay(request, response) {
  const target = gitRelayTarget(request);
  if (!target || !gitRelayAuthorized(request)) {
    throw new Error("Git relay target is not authorized");
  }
  await forward(request, response, target, true);
}

async function forward(request, response, target, allowPrivate) {
  const port = Number(target.port || (target.protocol === "http:" ? 80 : 443));
  let address;
  if (allowPrivate) {
    // A Temps relay: the control plane, whatever the sandbox's policy.
    const answer = (await dns.lookup(target.hostname, { all: true, verbatim: true }))[0];
    if (!answer) throw new Error("destination did not resolve");
    address = answer.address;
  } else {
    address = await admit(target.hostname, port);
  }
  const headers = { ...request.headers, host: target.host };
  delete headers["proxy-authorization"];
  delete headers["proxy-connection"];
  const transport = target.protocol === "https:" ? https : http;
  const upstream = transport.request({
    host: address,
    port,
    servername: normalizeHost(target.hostname),
    method: request.method,
    path: `${target.pathname}${target.search}`,
    headers,
  }, (upstreamResponse) => {
    // An upstream that drops mid-body must end this response, not the proxy.
    upstreamResponse.on("error", () => response.destroy());
    response.writeHead(upstreamResponse.statusCode || 502, upstreamResponse.headers);
    upstreamResponse.pipe(response);
  });
  upstream.on("error", () => {
    if (!response.headersSent) response.writeHead(502);
    response.end();
  });
  request.on("error", () => upstream.destroy());
  request.pipe(upstream);
}

async function handleRequest(request, response) {
  try {
    const incoming = new URL(request.url, `http://${request.headers.host || proxyAuthority}`);
    const relayTarget = modelRelayTarget(request);
    if (incoming.host === proxyAuthority &&
        incoming.pathname.startsWith("/.temps/model-relay/")) {
      if (!relayTarget) throw new Error("model relay target is not allowed");
      await forward(request, response, relayTarget, true);
      return;
    }
    if (incoming.host === proxyAuthority && incoming.pathname.startsWith("/.temps/mcp/")) {
      const target = mcpTarget(request);
      if (!target) throw new Error("MCP target is not allowed");
      await forward(request, response, target, true);
      return;
    }
    if (incoming.host === proxyAuthority && incoming.pathname.startsWith("/.temps/git/")) {
      await forwardGitRelay(request, response);
      return;
    }
    const target = new URL(request.url);
    if (target.protocol !== "http:" || target.username || target.password) {
      throw new Error("request target is not allowed");
    }
    await forward(request, response, target, false);
  } catch (error) {
    refuseRequest(request, response, error);
  }
}

const server = http.createServer(handleRequest);

server.on("connect", async (request, client, head) => {
  let destination = String(request.url).slice(0, 256);
  let upstream = null;
  // Listeners go on before anything that waits. Once "connect" fires the
  // HTTP server no longer handles errors on this socket, and a client that
  // resets it -- curl does, right after reading a 403 -- would otherwise
  // be an unhandled ECONNRESET that exits the proxy and cuts every
  // connection the sandbox has.
  client.on("error", () => upstream?.destroy());
  client.on("close", () => upstream?.destroy());
  client.setTimeout(60_000, () => client.destroy());
  try {
    const { hostname, port } = parseAuthority(request.url, 443);
    destination = formatDestination(hostname, port);
    const address = await admit(hostname, port);
    if (client.destroyed) return;
    upstream = net.connect({ host: address, port }, () => {
      client.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      if (head.length) upstream.write(head);
      client.pipe(upstream);
      upstream.pipe(client);
    });
    upstream.setTimeout(60_000, () => upstream.destroy());
    upstream.on("error", () => client.destroy());
  } catch (error) {
    const body = denial(error, destination, "CONNECT");
    client.end(
      "HTTP/1.1 403 Forbidden\r\n" +
      "Content-Type: text/plain; charset=utf-8\r\n" +
      `Content-Length: ${Buffer.byteLength(body)}\r\n` +
      "Connection: close\r\n\r\n" + body
    );
  }
});

server.headersTimeout = 10_000;
server.requestTimeout = 30_000;
server.keepAliveTimeout = 5_000;
server.on("clientError", (_, socket) => socket.destroy());
server.listen(3128, "0.0.0.0");
"#;

impl DockerSandboxProvider {
    /// Make sure `sandbox_container_name`'s egress proxy runs on both of its
    /// networks with the current script, image and egress policy, replacing
    /// it otherwise. Returns whether it was (re)created, which interrupts the
    /// sandbox's outbound connections.
    ///
    /// `desired` is the policy to enforce. `None` keeps the policy the
    /// existing proxy carries (a stopped one included: parking stops the
    /// proxy instead of removing it for this), and is `internet` only when
    /// the sandbox has no proxy at all. A proxy whose policy cannot be read
    /// is an error, never `internet`.
    pub(super) async fn ensure_egress_proxy(
        &self,
        sandbox_container_name: &str,
        sandbox_network: &str,
        control_plane_url: &str,
        desired: Option<&SandboxEgressPolicy>,
    ) -> Result<bool, AgentError> {
        let image = image_name_for_runtime("node");
        self.ensure_image_for_runtime("node").await?;
        let proxy_name = sandbox_egress_proxy_name(sandbox_container_name);

        let existing = match self
            .docker
            .inspect_container(
                &proxy_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => Some(info),
            Err(error) if docker_error_is_not_found(&error) => None,
            Err(error) => {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!("inspect managed sandbox egress proxy: {error}"),
                })
            }
        };
        let policy = match (desired, &existing) {
            (Some(policy), _) => policy.clone(),
            (None, Some(info)) => egress_policy_of_sidecar(&proxy_name, info)?,
            (None, None) => SandboxEgressPolicy::internet(),
        };
        let egress = SidecarEgressPolicy::new(sandbox_container_name, &policy)?;

        if let Some(info) = &existing {
            let image_matches = info
                .config
                .as_ref()
                .and_then(|config| config.image.as_deref())
                == Some(image.as_str());
            let policy_matches = info
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref())
                .and_then(|labels| labels.get(SANDBOX_EGRESS_POLICY_LABEL))
                .is_some_and(|value| value == SANDBOX_EGRESS_POLICY_VERSION);
            let networks = info
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref());
            let networks_match = networks.is_some_and(|networks| {
                networks.contains_key(SANDBOX_EGRESS_NETWORK)
                    && networks.contains_key(sandbox_network)
                    && networks.len() == 2
            });
            let command_matches = info
                .config
                .as_ref()
                .and_then(|config| config.cmd.as_ref())
                .is_some_and(|command| {
                    command == &vec!["-e".to_string(), SANDBOX_EGRESS_PROXY_SCRIPT.to_string()]
                });
            let control_plane_matches = container_has_environment_value(
                info,
                SANDBOX_CONTROL_PLANE_URL_ENV,
                control_plane_url,
            );
            let host_gateway_matches = container_has_extra_host(info, SANDBOX_HOST_GATEWAY);
            let rules_match = egress.is_carried_by(info, sandbox_container_name);
            // A stopped proxy is always recreated rather than started: it
            // was parked with its sandbox, and its endpoint still names the
            // network released then.
            let running = info.state.as_ref().and_then(|state| state.running) == Some(true);
            if image_matches
                && policy_matches
                && networks_match
                && command_matches
                && control_plane_matches
                && host_gateway_matches
                && rules_match
                && running
            {
                return Ok(false);
            }

            self.docker
                .remove_container(
                    &proxy_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|source| AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!("replace drifted sandbox egress proxy: {source}"),
                })?;
        }

        self.create_egress_proxy_container(
            &proxy_name,
            sandbox_container_name,
            control_plane_url,
            &egress,
        )
        .await?;

        // A proxy that cannot be connected or started is left created: it
        // reaches nothing, and it keeps the sandbox's egress policy for the
        // next attempt instead of letting that one fall back to `internet`.
        self.connect_container_to_network(
            &proxy_name,
            sandbox_network,
            Some(SANDBOX_EGRESS_PROXY_ALIAS),
        )
        .await?;

        self.docker
            .start_container(
                &proxy_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|source| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("start managed sandbox egress proxy: {source}"),
            })?;
        tracing::info!(
            container = proxy_name,
            sandbox = sandbox_container_name,
            egress_mode = policy.mode.as_str(),
            egress_rules = policy.rules.len(),
            "Managed sandbox egress proxy is ready"
        );
        Ok(true)
    }

    /// Create (without starting) the egress proxy container `proxy_name` for
    /// `sandbox_container_name`, attached only to the outbound bridge.
    async fn create_egress_proxy_container(
        &self,
        proxy_name: &str,
        sandbox_container_name: &str,
        control_plane_url: &str,
        egress: &SidecarEgressPolicy,
    ) -> Result<(), AgentError> {
        let mut tmpfs = HashMap::new();
        tmpfs.insert("/tmp".to_string(), "size=16m,mode=1777".to_string());
        let body = bollard::models::ContainerCreateBody {
            image: Some(image_name_for_runtime("node")),
            user: Some(format!("{SANDBOX_UID}:{SANDBOX_GID}")),
            entrypoint: Some(vec!["node".to_string()]),
            cmd: Some(vec![
                "-e".to_string(),
                SANDBOX_EGRESS_PROXY_SCRIPT.to_string(),
            ]),
            env: Some(vec![
                format!("{SANDBOX_CONTROL_PLANE_URL_ENV}={control_plane_url}"),
                format!("{SANDBOX_EGRESS_RULES_ENV}={}", egress.json),
                format!("{SANDBOX_NAME_ENV}={sandbox_container_name}"),
            ]),
            labels: Some(HashMap::from([
                (
                    SANDBOX_EGRESS_POLICY_LABEL.to_string(),
                    SANDBOX_EGRESS_POLICY_VERSION.to_string(),
                ),
                (
                    SANDBOX_NETWORK_OWNER_LABEL.to_string(),
                    sandbox_container_name.to_string(),
                ),
                (
                    SANDBOX_EGRESS_RULES_LABEL.to_string(),
                    egress.digest.clone(),
                ),
            ])),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some(SANDBOX_EGRESS_NETWORK.to_string()),
                extra_hosts: Some(sandbox_egress_proxy_extra_hosts()),
                cap_drop: Some(vec!["ALL".to_string()]),
                security_opt: Some(vec!["no-new-privileges:true".to_string()]),
                readonly_rootfs: Some(true),
                tmpfs: Some(tmpfs),
                memory: Some(128 * 1024 * 1024),
                memory_swap: Some(128 * 1024 * 1024),
                pids_limit: Some(64),
                nano_cpus: Some(250_000_000),
                ulimits: Some(vec![bollard::models::ResourcesUlimits {
                    name: Some("nofile".to_string()),
                    soft: Some(256),
                    hard: Some(256),
                }]),
                init: Some(true),
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
                    maximum_retry_count: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(proxy_name)
                        .build(),
                ),
                body,
            )
            .await
            .map_err(|source| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("create managed sandbox egress proxy: {source}"),
            })?;
        Ok(())
    }

    /// Apply `policy` to a sandbox that is not running: replace its parked
    /// proxy with a created, never-started one carrying the new policy,
    /// which the proxy recreated when the sandbox starts inherits.
    pub(super) async fn park_egress_policy(
        &self,
        sandbox_container_name: &str,
        policy: &SandboxEgressPolicy,
    ) -> Result<(), AgentError> {
        let egress = SidecarEgressPolicy::new(sandbox_container_name, policy)?;
        self.ensure_image_for_runtime("node").await?;
        self.ensure_managed_bridge(SANDBOX_EGRESS_NETWORK, false)
            .await?;
        let proxy_name = sandbox_egress_proxy_name(sandbox_container_name);
        match self
            .docker
            .remove_container(
                &proxy_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(()) => {}
            Err(error) if docker_error_is_not_found(&error) => {}
            Err(source) => {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!(
                        "replace the parked egress proxy of sandbox '{sandbox_container_name}': {source}"
                    ),
                })
            }
        }
        self.create_egress_proxy_container(
            &proxy_name,
            sandbox_container_name,
            &self.config.control_plane_url,
            &egress,
        )
        .await
    }
}
