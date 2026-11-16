// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Strictly narrowing rewrites applied to a resolved Compose model before the
//! security policy judges it.
//!
//! Almost every Compose file in the wild publishes `"8080:80"` and names its
//! containers, and [`super::security_policy`] rejects both. Neither is a
//! setting Temps has to refuse outright: a published port can be moved to
//! loopback, where only the Temps proxy reaches it, and a container name can
//! become a network alias on the networks the project itself owns. Both
//! changes only ever *reduce* what the stack exposes and neither changes what
//! data a service can see, so Temps applies them and reports each one in the
//! deployment log instead of failing the deploy.
//!
//! [`ComposeExecutor::normalize_for_policy`] runs inside
//! [`ComposeExecutor::check_configuration`], after interpolation and before
//! validation, so the settings preview, the "Run compose check" action and
//! the deploy all judge the same rewritten model. The policy then validates
//! that model unchanged, as defense in depth: a rewrite that failed to
//! produce a compliant entry still fails the check with its own message.
//!
//! A check an administrator disabled for the project is not rewritten —
//! there is nothing to narrow in a file the policy already accepts.
//!
//! Only the project's own model is normalized. When a Compose CLI is present
//! an inline project override has already been merged into that model
//! ([`ComposeExecutor::resolve_configuration`]); without one the override is
//! validated on its own and is not rewritten.

use std::collections::BTreeSet;

use temps_entities::compose_security::ComposeSecurityCheck as PolicyCheck;
use yaml_serde::{Mapping, Value as YamlValue};

use super::{ComposeError, ComposeExecutor};

/// The network every Compose project creates and owns, and the one Temps'
/// generated network override attaches every service to.
const DEFAULT_NETWORK: &str = "default";

/// Host address every published port is bound to; see
/// [`ComposeExecutor::published_host_ip`] for the worker-node rebinding that
/// happens afterwards.
const LOOPBACK_HOST_IP: &str = "127.0.0.1";

/// Longest name kept as a network alias. Docker resolves an alias as a DNS
/// label, so a longer one would not be answerable anyway.
const MAX_ALIAS_LENGTH: usize = 63;

/// One narrowing rewrite [`ComposeExecutor::normalize_for_policy`] applied,
/// reported to the user: the deployment log warns about each one and the
/// Compose check report notes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeRewrite {
    /// The check the original value would have been rejected by.
    pub check: PolicyCheck,
    pub service: String,
    /// Compose key that changed, e.g. `ports` or `container_name`.
    pub field: String,
    /// The entry as the author wrote it (after variable substitution).
    pub before: String,
    /// What replaced it; empty when the key was removed outright.
    pub after: String,
}

impl ComposeRewrite {
    /// One deployment-log line: what changed, and why it was safe to change.
    pub fn message(&self) -> String {
        let change = if self.after.is_empty() {
            format!("removed `{}` ({})", self.field, self.before)
        } else {
            format!("rewrote `{}` {} to {}", self.field, self.before, self.after)
        };
        format!(
            "Compose security rewrite [check={}]: service '{}' — {change}. {}",
            self.check.as_str(),
            self.service,
            self.rationale()
        )
    }

    /// Why the change was safe, and what it means for the running stack. A
    /// removal that could not keep an equivalent says so: claiming the name
    /// still resolves when it does not would send someone debugging in the
    /// wrong direction.
    fn rationale(&self) -> &'static str {
        match (self.check, self.after.is_empty()) {
            (PolicyCheck::PublishedPorts, _) => {
                "A published port must bind to 127.0.0.1 so traffic cannot bypass the Temps \
                 proxy's TLS, authentication and IP rules; Docker now picks a free loopback \
                 host port instead of the fixed one. Select the service as public in the \
                 project settings so Temps routes traffic to it."
            }
            (PolicyCheck::ContainerName, false) => {
                "A container name is daemon-global, so it collides with other environments and \
                 projects and can take another stack's DNS name. The name stays resolvable \
                 inside this stack as a network alias on the networks the project owns; it is \
                 never added on the shared Temps network or on an external one, where another \
                 project could reach it."
            }
            (PolicyCheck::ContainerName, true) => {
                "A container name is daemon-global, so it collides with other environments and \
                 projects and can take another stack's DNS name. It could not be kept as a \
                 network alias here — the name is not a usable DNS label, or this service joins \
                 no network the project owns — so nothing in the stack can use it as a \
                 hostname. Address the service by its Compose service name instead."
            }
            // Only the two rules above are rewritten today. A new rewrite
            // adds its own sentence here rather than relying on this one.
            _ => {
                "Temps narrowed this setting so the file satisfies the Compose security policy; \
                 nothing it exposed before is exposed now."
            }
        }
    }
}

/// A normalized Compose model and the rewrites that produced it.
#[derive(Debug, Clone)]
pub struct NormalizedCompose {
    pub content: String,
    /// Empty when nothing was rewritten, in which case `content` is the input
    /// verbatim.
    pub rewrites: Vec<ComposeRewrite>,
}

impl ComposeExecutor {
    /// Narrow the settings in `compose_content` that the policy would reject
    /// but that Temps can safely fix (see the module documentation), and
    /// report each change.
    ///
    /// Returns the input verbatim when nothing changed, so a file that needs
    /// no rewrite keeps its formatting, comments and anchors for the later
    /// steps.
    pub(super) fn normalize_for_policy(
        &self,
        compose_content: &str,
    ) -> Result<NormalizedCompose, ComposeError> {
        let unchanged = || NormalizedCompose {
            content: compose_content.to_string(),
            rewrites: Vec::new(),
        };
        let rewrite_ports = self.enforced(PolicyCheck::PublishedPorts);
        let rewrite_container_name = self.enforced(PolicyCheck::ContainerName);
        if compose_content.trim().is_empty() || !(rewrite_ports || rewrite_container_name) {
            return Ok(unchanged());
        }

        let mut root: YamlValue = yaml_serde::from_str(compose_content).map_err(|error| {
            ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: error.to_string(),
            }
        })?;
        // Settings inherited from a YAML anchor are rewritten too, for the
        // same reason validation expands them: Compose applies them at
        // runtime, so leaving `<<` folded would hide a published port from
        // the rewrite and then fail the deploy on it.
        root.apply_merge()
            .map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to expand YAML merge keys: {error}"),
            })?;

        let owned_networks = Self::project_owned_networks(&root);
        let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
            return Ok(unchanged());
        };

        let mut rewrites = Vec::new();
        for (service_key, service_value) in services.iter_mut() {
            let Some(service_name) = service_key.as_str().map(str::to_string) else {
                // A non-string service key is rejected by the policy with its
                // own message; rewriting it would only hide that.
                continue;
            };
            let Some(service) = service_value.as_mapping_mut() else {
                continue;
            };
            if rewrite_ports {
                Self::rewrite_published_ports(&service_name, service, &mut rewrites);
            }
            if rewrite_container_name {
                Self::rewrite_container_name(
                    &service_name,
                    service,
                    &owned_networks,
                    &mut rewrites,
                );
            }
        }

        if rewrites.is_empty() {
            return Ok(unchanged());
        }
        let content =
            yaml_serde::to_string(&root).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to render the normalized compose model: {error}"),
            })?;
        Ok(NormalizedCompose { content, rewrites })
    }

    /// Bind every published port to loopback, dropping the fixed host port so
    /// Docker assigns a free one. An entry that already publishes on loopback
    /// keeps its host port: the policy accepts it as written.
    fn rewrite_published_ports(
        service_name: &str,
        service: &mut Mapping,
        rewrites: &mut Vec<ComposeRewrite>,
    ) {
        let Some(ports) = service
            .get_mut(YamlValue::String("ports".to_string()))
            .and_then(YamlValue::as_sequence_mut)
        else {
            // A missing or non-sequence `ports` is the policy's to report.
            return;
        };
        for entry in ports.iter_mut() {
            if Self::port_entry_is_loopback_only(entry) {
                continue;
            }
            let (before, after) = match entry {
                YamlValue::String(binding) => {
                    // A binding that still carries a variable cannot be
                    // parsed into host and container parts; the policy
                    // rejects it and names the variable.
                    if Self::contains_interpolation(binding) {
                        continue;
                    }
                    let Some(rewritten) = Self::loopback_short_binding(binding) else {
                        continue;
                    };
                    let before = format!("\"{binding}\"");
                    *binding = rewritten.clone();
                    (before, format!("\"{rewritten}\""))
                }
                YamlValue::Number(number) => {
                    // `ports: [8080]` publishes on every interface too.
                    let before = number.to_string();
                    let Some(rewritten) = Self::loopback_short_binding(&before) else {
                        continue;
                    };
                    *entry = YamlValue::String(rewritten.clone());
                    (before, format!("\"{rewritten}\""))
                }
                YamlValue::Mapping(binding) => {
                    let before = Self::render_port_mapping(binding);
                    binding.insert(
                        YamlValue::String("host_ip".to_string()),
                        YamlValue::String(LOOPBACK_HOST_IP.to_string()),
                    );
                    binding.remove(YamlValue::String("published".to_string()));
                    (before, Self::render_port_mapping(binding))
                }
                _ => continue,
            };
            rewrites.push(ComposeRewrite {
                check: PolicyCheck::PublishedPorts,
                service: service_name.to_string(),
                field: "ports".to_string(),
                before,
                after,
            });
        }
    }

    /// `[[ip:]host:]container[/protocol]` as `127.0.0.1::container[/protocol]`,
    /// or `None` when the entry is not a binding this can parse — the policy
    /// then rejects it as written rather than Temps guessing.
    fn loopback_short_binding(binding: &str) -> Option<String> {
        let binding = binding.trim();
        let (address, protocol) = match binding.rsplit_once('/') {
            Some((address, protocol)) => {
                // Only Docker's two transport protocols; anything else means
                // this is not the syntax assumed here.
                if !matches!(protocol.to_ascii_lowercase().as_str(), "tcp" | "udp") {
                    return None;
                }
                (address, Some(protocol))
            }
            None => (binding, None),
        };
        // An IPv6 host address is bracketed: `[::1]:8080:80`.
        let address = match address.rsplit_once("]:") {
            Some((_host_ip, rest)) => rest,
            None => address,
        };
        let container = address.rsplit(':').next()?.trim();
        if !Self::is_port_or_port_range(container) {
            return None;
        }
        Some(match protocol {
            Some(protocol) => format!("{LOOPBACK_HOST_IP}::{container}/{protocol}"),
            None => format!("{LOOPBACK_HOST_IP}::{container}"),
        })
    }

    /// A single container port or an inclusive range (`8000-8010`), both of
    /// which Compose accepts on the container side of a binding.
    fn is_port_or_port_range(text: &str) -> bool {
        let port = |text: &str| {
            !text.is_empty()
                && text.bytes().all(|byte| byte.is_ascii_digit())
                && text.parse::<u16>().is_ok_and(|port| port > 0)
        };
        let mut bounds = text.split('-');
        bounds.next().is_some_and(port)
            && match bounds.next() {
                None => true,
                Some(last) => port(last) && bounds.next().is_none(),
            }
    }

    /// A long-form port entry's scalar fields, for the rewrite report.
    fn render_port_mapping(binding: &Mapping) -> String {
        let fields: Vec<String> = binding
            .iter()
            .filter_map(|(key, value)| {
                Some(format!(
                    "{}: {}",
                    key.as_str()?,
                    Self::render_scalar(value)?
                ))
            })
            .collect();
        format!("{{{}}}", fields.join(", "))
    }

    fn render_scalar(value: &YamlValue) -> Option<String> {
        match value {
            YamlValue::String(text) => Some(text.clone()),
            YamlValue::Number(number) => Some(number.to_string()),
            YamlValue::Bool(flag) => Some(flag.to_string()),
            YamlValue::Null => Some("null".to_string()),
            _ => None,
        }
    }

    /// Drop `container_name` and keep the name reachable inside the stack as
    /// a network alias on the project's own networks.
    fn rewrite_container_name(
        service_name: &str,
        service: &mut Mapping,
        owned_networks: &BTreeSet<String>,
        rewrites: &mut Vec<ComposeRewrite>,
    ) {
        let key = YamlValue::String("container_name".to_string());
        let Some(value) = service.get(&key) else {
            return;
        };
        let name = value.as_str().map(str::to_string);
        service.remove(&key);

        let alias = name
            .as_deref()
            .filter(|name| Self::is_usable_network_alias(name));
        let targets = match alias {
            Some(_) => Self::alias_networks(service, owned_networks),
            None => Vec::new(),
        };
        let before = name.as_deref().map_or_else(
            || "a non-string value".to_string(),
            |name| format!("'{name}'"),
        );
        let after = match (alias, targets.as_slice()) {
            (Some(alias), [_, ..]) => {
                format!("a network alias '{alias}' on {}", targets.join(", "))
            }
            // Either the name cannot be a DNS alias, or the service joins
            // another container's namespace (`network_mode`), or every
            // network it is on belongs to someone else. The name is gone
            // either way, which is what the policy requires.
            _ => String::new(),
        };
        if let Some(alias) = alias {
            if !targets.is_empty() {
                Self::add_network_aliases(service, &targets, alias);
            }
        }
        rewrites.push(ComposeRewrite {
            check: PolicyCheck::ContainerName,
            service: service_name.to_string(),
            field: "container_name".to_string(),
            before,
            after,
        });
    }

    /// Whether `name` can be answered as a Docker DNS alias. Deliberately the
    /// Compose service-name character set ([`Self::is_valid_service_name`]),
    /// which is what a stack would have used as a hostname anyway.
    fn is_usable_network_alias(name: &str) -> bool {
        !name.is_empty() && name.len() <= MAX_ALIAS_LENGTH && Self::is_valid_service_name(name)
    }

    /// The networks a service's alias may go on: the project-owned ones it is
    /// attached to, plus the project default network, which Temps' generated
    /// network override attaches every service to.
    ///
    /// Never the shared Temps network or an external one: those are reachable
    /// from other projects, so an alias there would keep exactly the
    /// cross-project collision and impersonation this rewrite removes.
    fn alias_networks(service: &Mapping, owned_networks: &BTreeSet<String>) -> Vec<String> {
        // Compose rejects `networks` together with `network_mode`, so a
        // service that shares another namespace cannot carry an alias.
        if service.contains_key(YamlValue::String("network_mode".to_string())) {
            return Vec::new();
        }
        let mut targets: Vec<String> = Vec::new();
        let mut consider = |name: &str| {
            if owned_networks.contains(name) && !targets.iter().any(|target| target == name) {
                targets.push(name.to_string());
            }
        };
        match service.get(YamlValue::String("networks".to_string())) {
            Some(YamlValue::Sequence(declared)) => {
                for name in declared.iter().filter_map(YamlValue::as_str) {
                    consider(name);
                }
            }
            Some(YamlValue::Mapping(declared)) => {
                for name in declared.keys().filter_map(YamlValue::as_str) {
                    consider(name);
                }
            }
            _ => {}
        }
        consider(DEFAULT_NETWORK);
        targets
    }

    /// Add `alias` on each of `targets`, converting the short list form of
    /// `networks` into the mapping form that can carry aliases.
    fn add_network_aliases(service: &mut Mapping, targets: &[String], alias: &str) {
        let key = YamlValue::String("networks".to_string());
        let mut networks = match service.get(&key) {
            Some(YamlValue::Mapping(declared)) => declared.clone(),
            Some(YamlValue::Sequence(declared)) => {
                let mut mapping = Mapping::new();
                for name in declared.iter().filter_map(YamlValue::as_str) {
                    mapping.insert(YamlValue::String(name.to_string()), YamlValue::Null);
                }
                mapping
            }
            _ => Mapping::new(),
        };
        let aliases_key = YamlValue::String("aliases".to_string());
        for target in targets {
            let target_key = YamlValue::String(target.clone());
            let mut settings = match networks.get(&target_key) {
                Some(YamlValue::Mapping(settings)) => settings.clone(),
                _ => Mapping::new(),
            };
            let mut aliases = match settings.get(&aliases_key) {
                Some(YamlValue::Sequence(aliases)) => aliases.clone(),
                _ => Vec::new(),
            };
            if !aliases.iter().any(|entry| entry.as_str() == Some(alias)) {
                aliases.push(YamlValue::String(alias.to_string()));
            }
            settings.insert(aliases_key.clone(), YamlValue::Sequence(aliases));
            networks.insert(target_key, YamlValue::Mapping(settings));
        }
        service.insert(key, YamlValue::Mapping(networks));
    }

    /// Networks this project creates and that only its own services join: the
    /// implicit `default`, plus every top-level network the file declares
    /// without `external`. The shared Temps network is excluded even when the
    /// file declares it, and a `default` the file declares as external is
    /// dropped.
    pub(super) fn project_owned_networks(root: &YamlValue) -> BTreeSet<String> {
        let mut owned = BTreeSet::from([DEFAULT_NETWORK.to_string()]);
        let Some(networks) = root.get("networks").and_then(YamlValue::as_mapping) else {
            return owned;
        };
        let shared = temps_core::NETWORK_NAME.as_str();
        for (network_key, settings) in networks {
            let Some(name) = network_key.as_str() else {
                continue;
            };
            let settings = settings.as_mapping();
            // Anything but a literal `external: false` is treated as
            // external: a value this cannot read is not one to alias on.
            let external = settings.is_some_and(|settings| {
                settings
                    .get("external")
                    .is_some_and(|value| value.as_bool() != Some(false))
            });
            let is_shared = name == shared
                || settings
                    .and_then(|settings| settings.get("name"))
                    .and_then(YamlValue::as_str)
                    == Some(shared);
            if external || is_shared {
                owned.remove(name);
            } else {
                owned.insert(name.to_string());
            }
        }
        owned
    }
}

#[cfg(test)]
mod tests;
