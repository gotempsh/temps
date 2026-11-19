// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stored and composed addresses which take precedence over project custom domains.
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, FromQueryResult, Statement};
use std::collections::{HashMap, HashSet};
use temps_core::public_hostname::{base_domain, PublicHostnameStrategy};
use temps_core::public_hostname_resolver::match_strategy;
use temps_entities::preset::{compose_public_route_labels, PresetConfig};

#[derive(Debug, Clone, Copy)]
pub enum GeneratedAddress {
    Environment,
    Tunnel,
    Deployment,
}

#[derive(FromQueryResult)]
struct StoredAddress {
    domain: String,
    tunnel: bool,
}

#[derive(FromQueryResult)]
struct EnvironmentAddress {
    id: i32,
    subdomain: String,
    env_slug: String,
    project_slug: String,
    deployment: Option<String>,
    preset_config: Option<PresetConfig>,
}

#[derive(FromQueryResult)]
struct Tunnel {
    environment_id: i32,
    domain: String,
}

#[derive(FromQueryResult)]
struct DomainStrategy {
    domain: String,
    generated_hostname_mode: String,
}

/// Resolve only the requested normalized hosts. Page through environment slugs
/// because shortening a generated DNS label with a hash is not reversible.
pub async fn generated_address_conflicts<C: ConnectionTrait>(
    conn: &C,
    hosts: &HashSet<String>,
    preview: &str,
) -> Result<HashMap<String, GeneratedAddress>, DbErr> {
    let mut conflicts = HashMap::new();
    if hosts.is_empty() {
        return Ok(conflicts);
    }
    let base = base_domain(preview);
    let suffix = format!(".{base}");
    let mut candidates: Vec<String> = hosts.iter().cloned().collect();
    for host in hosts {
        if let Some(label) = host.strip_suffix(&suffix).filter(|s| !s.contains('.')) {
            candidates.push(label.to_owned());
        }
    }
    let stored = StoredAddress::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT LOWER(RTRIM(domain, '.')) AS domain, generated_by_connector_id IS NOT NULL AS tunnel
         FROM environment_domains WHERE LOWER(RTRIM(domain, '.')) = ANY($1)",
        [candidates.into()],
    )).all(conn).await?;
    for row in stored {
        let host = if row.domain.contains('.') {
            row.domain
        } else {
            format!("{}{suffix}", row.domain)
        };
        if hosts.contains(&host) {
            conflicts.insert(
                host,
                if row.tunnel {
                    GeneratedAddress::Tunnel
                } else {
                    GeneratedAddress::Environment
                },
            );
        }
    }
    let service_hosts = hosts
        .iter()
        .any(|h| h.split('.').next().is_some_and(|l| l.contains("--")));
    let internal_hosts = hosts.iter().any(|host| host.ends_with(".temps.local"));
    if !internal_hosts && !service_hosts && !hosts.iter().any(|h| h.ends_with(&suffix)) {
        return Ok(conflicts);
    }
    let strategy = if service_hosts {
        let modes = DomainStrategy::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT domain, generated_hostname_mode FROM dns_managed_domains",
        ))
        .all(conn)
        .await?
        .into_iter()
        .map(|m| {
            (
                m.domain,
                PublicHostnameStrategy::from_db_str(&m.generated_hostname_mode),
            )
        })
        .collect();
        match_strategy(&modes, preview)
    } else {
        PublicHostnameStrategy::Standard
    };
    let mut after = 0;
    loop {
        let rows = EnvironmentAddress::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT e.id, e.subdomain, e.slug AS env_slug, p.slug AS project_slug, d.slug AS deployment,
                    CASE WHEN $2 AND e.current_deployment_id IS NOT NULL THEN p.preset_config END AS preset_config
             FROM environments e JOIN projects p ON p.id = e.project_id
             LEFT JOIN deployments d ON d.id = e.current_deployment_id
             WHERE e.deleted_at IS NULL AND e.id > $1 ORDER BY e.id LIMIT 128",
            [after.into(), service_hosts.into()],
        ))
        .all(conn)
        .await?;
        let Some(last) = rows.last() else { break };
        let mut tunnels: HashMap<i32, Vec<String>> = HashMap::new();
        if service_hosts {
            let ids: Vec<i32> = rows
                .iter()
                .filter(|e| matches!(e.preset_config, Some(PresetConfig::DockerCompose(_))))
                .map(|e| e.id)
                .collect();
            if !ids.is_empty() {
                let statement = Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT environment_id, LOWER(RTRIM(domain, '.')) AS domain
                     FROM environment_domains WHERE environment_id = ANY($1)
                     AND generated_by_connector_id IS NOT NULL AND audience = 'public'",
                    [ids.into()],
                );
                for tunnel in Tunnel::find_by_statement(statement).all(conn).await? {
                    tunnels
                        .entry(tunnel.environment_id)
                        .or_default()
                        .push(tunnel.domain);
                }
            }
        }
        after = last.id;
        let done = rows.len() < 128;
        let mut record = |host: String, kind| {
            if hosts.contains(&host) {
                conflicts.insert(host, kind);
            }
        };
        for row in rows {
            record(
                PublicHostnameStrategy::Standard.environment_hostname(preview, &row.subdomain),
                GeneratedAddress::Environment,
            );
            if internal_hosts {
                if let Some(alias) =
                    temps_core::internal_access::environment_alias_name(&row.subdomain)
                {
                    record(alias, GeneratedAddress::Environment);
                }
                record(
                    format!("{}.{}.temps.local", row.env_slug, row.project_slug)
                        .to_ascii_lowercase(),
                    GeneratedAddress::Environment,
                );
            }
            if let Some(slug) = row.deployment {
                record(
                    PublicHostnameStrategy::Standard.deployment_hostname(preview, &slug),
                    GeneratedAddress::Deployment,
                );
            }
            if let Some(PresetConfig::DockerCompose(config)) = row.preset_config {
                for label in compose_public_route_labels(&config.public_ports) {
                    record(
                        strategy.service_hostname(preview, &row.subdomain, &label),
                        GeneratedAddress::Environment,
                    );
                    for tunnel in tunnels.get(&row.id).into_iter().flatten() {
                        if let Some((prefix, zone)) = tunnel.split_once('.') {
                            record(
                                PublicHostnameStrategy::Flat.service_hostname(zone, prefix, &label),
                                GeneratedAddress::Tunnel,
                            );
                        }
                    }
                }
            }
        }
        if done {
            break;
        }
    }
    Ok(conflicts)
}
