// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::compose::test_support::executor_with_checks_disabled;

/// Compose files whose only policy violations are the two rules rewritten
/// here, so every one of them must deploy after normalization.
const REWRITABLE_FILES: &[&str] = &[
    // The shape almost every published file has.
    "services:\n  web:\n    image: web:1\n    ports:\n      - \"8080:80\"\n",
    // Several services, several entries, protocols and a bare container port.
    "services:\n  web:\n    image: web:1\n    container_name: site\n    ports:\n      - \"80:80\"\n      - \"443:443\"\n      - \"53:53/udp\"\n  db:\n    image: db:16\n    container_name: site_db\n    ports:\n      - 5432\n",
    // Long form, with an explicit wildcard host address.
    "services:\n  api:\n    image: api:1\n    ports:\n      - target: 8000\n        published: 8000\n        host_ip: 0.0.0.0\n        protocol: tcp\n",
    // Declared project networks, in both the list and the mapping form.
    "services:\n  cache:\n    image: cache:7\n    container_name: cache_node\n    networks: [backend]\n    ports:\n      - \"6379:6379\"\n  app:\n    image: app:1\n    container_name: cache_app\n    networks:\n      backend:\n        aliases: [api]\nnetworks:\n  backend: {}\n",
    // A port range and an anchor the model inherits from.
    "x-common: &common\n  ports:\n    - \"8000-8002:8000-8002\"\nservices:\n  worker:\n    <<: *common\n    image: worker:1\n    container_name: worker_1\n",
];

fn normalize(content: &str) -> NormalizedCompose {
    executor_with_checks_disabled(&[])
        .normalize_for_policy(content)
        .unwrap()
}

fn parse(content: &str) -> YamlValue {
    yaml_serde::from_str(content).unwrap()
}

fn with_ports(entry: &str) -> String {
    format!("services:\n  web:\n    image: web:1\n    ports:\n      {entry}\n")
}

fn first_port(content: &str) -> YamlValue {
    parse(content)["services"]["web"]["ports"][0].clone()
}

#[test]
fn short_form_ports_are_rewritten_to_an_ephemeral_loopback_port() {
    for (entry, expected) in [
        ("- \"8080:80\"", "127.0.0.1::80"),
        ("- \"0.0.0.0:8080:80\"", "127.0.0.1::80"),
        ("- \"192.168.1.10:8080:80\"", "127.0.0.1::80"),
        ("- \"8080:80/udp\"", "127.0.0.1::80/udp"),
        ("- \"0.0.0.0:5353:53/UDP\"", "127.0.0.1::53/UDP"),
        ("- \"[::1]:8080:80\"", "127.0.0.1::80"),
        ("- \"[::]:8080:80/tcp\"", "127.0.0.1::80/tcp"),
        // A bare container port publishes on every interface too.
        ("- \"3000\"", "127.0.0.1::3000"),
        ("- 5432", "127.0.0.1::5432"),
        // A host port that is not a port at all: Compose reads `127.0.0.1`
        // as the host port here, so the entry is still not loopback-bound.
        ("- \"127.0.0.1:80\"", "127.0.0.1::80"),
        // Ranges keep their container side.
        ("- \"8000-8010:8000-8010\"", "127.0.0.1::8000-8010"),
        ("- \"9000-9002\"", "127.0.0.1::9000-9002"),
    ] {
        let normalized = normalize(&with_ports(entry));
        assert_eq!(
            first_port(&normalized.content).as_str(),
            Some(expected),
            "{entry}"
        );
        let [rewrite] = normalized.rewrites.as_slice() else {
            panic!("{entry}: expected exactly one rewrite, got {normalized:?}");
        };
        assert_eq!(rewrite.check, PolicyCheck::PublishedPorts);
        assert_eq!(rewrite.service, "web");
        assert_eq!(rewrite.field, "ports");
        assert!(rewrite.after.contains(expected), "{:?}", rewrite);
        // The author's entry is named, so the log says what changed.
        assert!(
            rewrite
                .before
                .contains(entry.trim_start_matches("- ").trim_matches('"')),
            "{rewrite:?}"
        );
    }
}

#[test]
fn ports_already_bound_to_loopback_keep_their_host_port() {
    for entry in [
        "- \"127.0.0.1:8080:80\"",
        "- \"127.0.0.1::80\"",
        "- \"127.0.0.1:5353:53/udp\"",
        "- target: 80\n        published: 8080\n        host_ip: 127.0.0.1",
    ] {
        let content = with_ports(entry);
        let normalized = normalize(&content);
        assert!(normalized.rewrites.is_empty(), "{entry}: {normalized:?}");
        // Nothing changed, so the file keeps its original text.
        assert_eq!(normalized.content, content);
    }
}

#[test]
fn port_entries_the_rewrite_cannot_parse_are_left_for_the_policy() {
    for entry in [
        // A variable survived resolution: the policy names it instead.
        "- \"${PORT}:80\"",
        "- \"8080:${PORT}\"",
        // Not a port, and not a protocol Docker publishes.
        "- \"8080:http\"",
        "- \"8080:80/sctp\"",
        "- \"8080:0\"",
        "- \"8080:70000\"",
        "- \"8000-8010-8020\"",
        // Neither a scalar nor a long-form mapping.
        "- [8080, 80]",
    ] {
        let content = with_ports(entry);
        let normalized = normalize(&content);
        assert!(normalized.rewrites.is_empty(), "{entry}: {normalized:?}");
        assert_eq!(normalized.content, content);
        assert!(
            executor_with_checks_disabled(&[])
                .preflight_validate(&normalized.content, None)
                .is_err(),
            "{entry} must still be rejected"
        );
    }
}

#[test]
fn long_form_ports_get_a_loopback_host_ip_and_lose_the_fixed_host_port() {
    let normalized = normalize(
        "services:\n  web:\n    image: web:1\n    ports:\n      - target: 80\n        published: 8080\n        host_ip: 0.0.0.0\n        protocol: tcp\n",
    );
    let port = first_port(&normalized.content);
    assert_eq!(port["host_ip"].as_str(), Some("127.0.0.1"));
    assert_eq!(port["target"].as_u64(), Some(80));
    assert_eq!(port["protocol"].as_str(), Some("tcp"));
    assert!(port.get("published").is_none(), "{port:?}");
    let [rewrite] = normalized.rewrites.as_slice() else {
        panic!("expected one rewrite, got {normalized:?}");
    };
    assert_eq!(rewrite.check, PolicyCheck::PublishedPorts);
    assert!(rewrite.before.contains("published: 8080"), "{rewrite:?}");
    assert!(rewrite.before.contains("host_ip: 0.0.0.0"), "{rewrite:?}");
    assert!(rewrite.after.contains("host_ip: 127.0.0.1"), "{rewrite:?}");
    assert!(!rewrite.after.contains("published"), "{rewrite:?}");
}

#[test]
fn long_form_ports_without_a_host_address_are_bound_to_loopback() {
    // No `host_ip` at all: Compose publishes on every interface.
    let normalized =
        normalize("services:\n  web:\n    image: web:1\n    ports:\n      - target: 80\n");
    let port = first_port(&normalized.content);
    assert_eq!(port["host_ip"].as_str(), Some("127.0.0.1"));
    assert_eq!(normalized.rewrites.len(), 1);
}

#[test]
fn container_name_becomes_an_alias_on_the_default_network() {
    let normalized = normalize("services:\n  db:\n    image: db:16\n    container_name: app_db\n");
    let service = parse(&normalized.content)["services"]["db"].clone();
    assert!(service.get("container_name").is_none(), "{service:?}");
    assert_eq!(
        service["networks"]["default"]["aliases"][0].as_str(),
        Some("app_db")
    );
    let [rewrite] = normalized.rewrites.as_slice() else {
        panic!("expected one rewrite, got {normalized:?}");
    };
    assert_eq!(rewrite.check, PolicyCheck::ContainerName);
    assert_eq!(rewrite.field, "container_name");
    assert!(rewrite.before.contains("app_db"), "{rewrite:?}");
    assert!(rewrite.after.contains("alias 'app_db'"), "{rewrite:?}");
    assert!(rewrite.after.contains("default"), "{rewrite:?}");
}

#[test]
fn container_name_aliases_every_project_owned_network_the_service_joins() {
    let normalized = normalize(
        "services:\n  db:\n    image: db:16\n    container_name: app_db\n    networks:\n      - backend\n      - frontend\nnetworks:\n  backend: {}\n  frontend:\n    driver: bridge\n",
    );
    let networks = parse(&normalized.content)["services"]["db"]["networks"].clone();
    for network in ["backend", "frontend", "default"] {
        assert_eq!(
            networks[network]["aliases"][0].as_str(),
            Some("app_db"),
            "{network}: {networks:?}"
        );
    }
    let [rewrite] = normalized.rewrites.as_slice() else {
        panic!("expected one rewrite, got {normalized:?}");
    };
    assert!(
        rewrite.after.contains("backend, frontend, default"),
        "{rewrite:?}"
    );
}

#[test]
fn container_name_keeps_the_mapping_form_and_settings_a_service_already_declared() {
    let normalized = normalize(
        "services:\n  db:\n    image: db:16\n    container_name: app_db\n    networks:\n      backend:\n        aliases: [primary]\n      shared:\n        aliases: [public_db]\nnetworks:\n  backend: {}\n  shared:\n    external: true\n",
    );
    let networks = parse(&normalized.content)["services"]["db"]["networks"].clone();
    // The project's own network carries both the author's alias and the name.
    assert_eq!(networks["backend"]["aliases"][0].as_str(), Some("primary"));
    assert_eq!(networks["backend"]["aliases"][1].as_str(), Some("app_db"));
    // The external network keeps exactly what the author wrote.
    assert_eq!(networks["shared"]["aliases"][0].as_str(), Some("public_db"));
    assert_eq!(
        networks["shared"]["aliases"].as_sequence().unwrap().len(),
        1
    );
    assert_eq!(networks["default"]["aliases"][0].as_str(), Some("app_db"));
}

#[test]
fn container_name_is_never_aliased_on_an_external_or_shared_network() {
    let shared = temps_core::NETWORK_NAME.as_str();
    let content = format!(
        "services:\n  db:\n    image: db:16\n    container_name: app_db\n    networks: [outside, {shared}, renamed]\nnetworks:\n  outside:\n    external: true\n  {shared}: {{}}\n  renamed:\n    name: {shared}\n",
    );
    let normalized = normalize(&content);
    let networks = parse(&normalized.content)["services"]["db"]["networks"].clone();
    for network in ["outside", shared, "renamed"] {
        assert!(
            networks[network].get("aliases").is_none(),
            "{network} must not carry the alias: {networks:?}"
        );
    }
    // Only the project's own default network answers to the old name.
    assert_eq!(networks["default"]["aliases"][0].as_str(), Some("app_db"));
    let [rewrite] = normalized.rewrites.as_slice() else {
        panic!("expected one rewrite, got {normalized:?}");
    };
    assert_eq!(rewrite.after, "a network alias 'app_db' on default");
}

#[test]
fn container_name_is_dropped_without_an_alias_when_no_owned_network_can_carry_it() {
    for (content, why) in [
        (
            // `networks` cannot be set next to `network_mode`.
            "services:\n  db:\n    image: db:16\n    container_name: app_db\n    network_mode: none\n",
            "network_mode",
        ),
        (
            // A name that is not a DNS label cannot be answered.
            "services:\n  db:\n    image: db:16\n    container_name: \"app db/1\"\n",
            "not a label",
        ),
        (
            // Even the default network belongs to someone else here.
            "services:\n  db:\n    image: db:16\n    container_name: app_db\nnetworks:\n  default:\n    external: true\n",
            "external default",
        ),
    ] {
        let normalized = normalize(content);
        let service = parse(&normalized.content)["services"]["db"].clone();
        assert!(service.get("container_name").is_none(), "{why}: {service:?}");
        assert!(
            service
                .get("networks")
                .and_then(|networks| networks.get("default"))
                .is_none(),
            "{why}: {service:?}"
        );
        let [rewrite] = normalized.rewrites.as_slice() else {
            panic!("{why}: expected one rewrite, got {normalized:?}");
        };
        assert_eq!(rewrite.check, PolicyCheck::ContainerName, "{why}");
        assert!(rewrite.after.is_empty(), "{why}: {rewrite:?}");
        let message = rewrite.message();
        assert!(message.contains("removed `container_name`"), "{why}: {message}");
        // The log must not claim the name still resolves when it does not.
        assert!(
            message.contains("could not be kept as a network alias"),
            "{why}: {message}"
        );
        assert!(!message.contains("stays resolvable"), "{why}: {message}");
    }
}

#[test]
fn a_non_string_container_name_is_removed_and_reported() {
    let normalized = normalize("services:\n  db:\n    image: db:16\n    container_name: 42\n");
    let [rewrite] = normalized.rewrites.as_slice() else {
        panic!("expected one rewrite, got {normalized:?}");
    };
    assert!(rewrite.before.contains("non-string"), "{rewrite:?}");
    assert!(parse(&normalized.content)["services"]["db"]
        .get("container_name")
        .is_none());
}

#[test]
fn a_disabled_check_is_not_rewritten() {
    let content =
        "services:\n  db:\n    image: db:16\n    container_name: app_db\n    ports:\n      - \"5432:5432\"\n";
    // Both relaxed: the policy accepts the file as written, so there is
    // nothing to narrow.
    let both =
        executor_with_checks_disabled(&[PolicyCheck::PublishedPorts, PolicyCheck::ContainerName])
            .normalize_for_policy(content)
            .unwrap();
    assert!(both.rewrites.is_empty(), "{both:?}");
    assert_eq!(both.content, content);

    // One relaxed: only the enforced rule is rewritten.
    let ports_only = executor_with_checks_disabled(&[PolicyCheck::ContainerName])
        .normalize_for_policy(content)
        .unwrap();
    assert_eq!(ports_only.rewrites.len(), 1);
    assert_eq!(ports_only.rewrites[0].check, PolicyCheck::PublishedPorts);
    assert!(ports_only.content.contains("container_name: app_db"));

    let names_only = executor_with_checks_disabled(&[PolicyCheck::PublishedPorts])
        .normalize_for_policy(content)
        .unwrap();
    assert_eq!(names_only.rewrites.len(), 1);
    assert_eq!(names_only.rewrites[0].check, PolicyCheck::ContainerName);
    assert!(names_only.content.contains("5432:5432"));
}

#[test]
fn a_file_with_nothing_to_narrow_is_returned_verbatim() {
    for content in [
        "",
        "   \n",
        "services:\n  web:\n    image: web:1\n",
        "name: stack\n",
        // No `services:` mapping at all.
        "services: []\n",
    ] {
        let normalized = normalize(content);
        assert!(normalized.rewrites.is_empty(), "{content:?}");
        assert_eq!(normalized.content, content);
    }
}

#[test]
fn the_strict_policy_accepts_every_rewritten_model() {
    let executor = executor_with_checks_disabled(&[]);
    for content in REWRITABLE_FILES {
        // Each file is rejected as written.
        let error = executor.preflight_validate(content, None).unwrap_err();
        assert!(
            matches!(
                error,
                ComposeError::SecurityPolicyViolation {
                    check: Some(PolicyCheck::PublishedPorts | PolicyCheck::ContainerName),
                    ..
                }
            ),
            "{content}: {error}"
        );
        // And accepted after normalization, by the unchanged policy.
        let normalized = normalize(content);
        assert!(!normalized.rewrites.is_empty(), "{content}");
        if let Err(error) = executor.preflight_validate(&normalized.content, None) {
            panic!(
                "the policy rejected a rewritten model: {error}\n{}",
                normalized.content
            );
        }
        // Normalization is a fixed point: a model Temps already rewrote is
        // not rewritten again on the next deploy.
        let again = normalize(&normalized.content);
        assert!(again.rewrites.is_empty(), "{}", normalized.content);
        assert_eq!(again.content, normalized.content);
    }
}

#[test]
fn every_rewrite_message_names_the_check_the_service_and_the_reason() {
    let normalized = normalize(
        "services:\n  web:\n    image: web:1\n    container_name: site\n    ports:\n      - \"8080:80\"\n",
    );
    assert_eq!(normalized.rewrites.len(), 2);
    let messages: Vec<String> = normalized
        .rewrites
        .iter()
        .map(ComposeRewrite::message)
        .collect();
    let ports = messages
        .iter()
        .find(|message| message.contains("published_ports"))
        .expect("a published-ports rewrite");
    assert!(ports.contains("service 'web'"), "{ports}");
    assert!(ports.contains("\"8080:80\""), "{ports}");
    assert!(ports.contains("\"127.0.0.1::80\""), "{ports}");
    assert!(ports.contains("bypass the Temps proxy"), "{ports}");
    assert!(ports.contains("public in the project settings"), "{ports}");

    let name = messages
        .iter()
        .find(|message| message.contains("container_name"))
        .expect("a container-name rewrite");
    assert!(name.contains("daemon-global"), "{name}");
    assert!(name.contains("network alias 'site' on default"), "{name}");
    assert!(
        name.contains("never added on the shared Temps network"),
        "{name}"
    );
}

#[test]
fn project_owned_networks_exclude_external_and_shared_ones() {
    let shared = temps_core::NETWORK_NAME.as_str();
    let owned = ComposeExecutor::project_owned_networks(&parse(&format!(
        "networks:\n  backend: {{}}\n  frontend:\n    driver: bridge\n  legacy:\n    external: false\n  outside:\n    external: true\n  {shared}: {{}}\n  renamed:\n    name: {shared}\n",
    )));
    assert!(owned.contains("default"), "{owned:?}");
    assert!(owned.contains("backend"), "{owned:?}");
    assert!(owned.contains("frontend"), "{owned:?}");
    assert!(owned.contains("legacy"), "{owned:?}");
    assert!(!owned.contains("outside"), "{owned:?}");
    assert!(!owned.contains(shared), "{owned:?}");
    assert!(!owned.contains("renamed"), "{owned:?}");

    // No top-level block at all: only the implicit default network.
    let implicit = ComposeExecutor::project_owned_networks(&parse("services: {web: {image: w}}"));
    assert_eq!(implicit.len(), 1);
    assert!(implicit.contains("default"));
}
