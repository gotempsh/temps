use sea_orm::{DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;
use std::net::IpAddr;
use temps_core::url_validation::{validate_ipv4, validate_ipv6, UrlValidationError};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            UPDATE custom_routes
            SET domain = lower(rtrim(btrim(domain), '.'))
            WHERE domain <> lower(rtrim(btrim(domain), '.'));

            ALTER TABLE custom_routes
                ADD COLUMN force_override boolean NOT NULL DEFAULT false;

            DO $$
            DECLARE duplicate_domain text;
            BEGIN
                SELECT domain INTO duplicate_domain
                FROM custom_routes
                GROUP BY domain
                HAVING count(*) > 1
                LIMIT 1;

                IF duplicate_domain IS NOT NULL THEN
                    RAISE EXCEPTION
                        'cannot add normalized custom-route uniqueness: duplicate domain "%"; remove the duplicate route and retry',
                        duplicate_domain;
                END IF;

                IF EXISTS (SELECT 1 FROM custom_routes WHERE port NOT BETWEEN 1 AND 65535) THEN
                    RAISE EXCEPTION
                        'cannot add custom-route port constraint: a route has a port outside 1..65535; correct it and retry';
                END IF;
            END $$;

            CREATE UNIQUE INDEX idx_custom_routes_domain_normalized_unique
                ON custom_routes ((lower(rtrim(btrim(domain), '.'))));

            ALTER TABLE custom_routes
                ADD CONSTRAINT chk_custom_routes_port_range
                CHECK (port BETWEEN 1 AND 65535);
            "#,
        )
        .await?;

        // Legacy rows predate upstream validation. Quarantine hostnames and
        // hard-blocked special-use literals so upgrading cannot leave a DNS
        // rebinding or metadata route active. Private/loopback literals are
        // preserved for backward compatibility; all retained IPv6 values are
        // canonicalized with brackets before the data plane sees them.
        let legacy_routes = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT id, host FROM custom_routes WHERE enabled = true".to_string(),
            ))
            .await?;
        for row in legacy_routes {
            let id: i32 = row.try_get("", "id")?;
            let host: String = row.try_get("", "host")?;
            match canonical_legacy_host(&host) {
                Some(canonical) => {
                    if canonical != host {
                        db.execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            "UPDATE custom_routes SET host = $1 WHERE id = $2",
                            [canonical.into(), id.into()],
                        ))
                        .await?;
                    }
                }
                None => {
                    db.execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "UPDATE custom_routes SET enabled = false WHERE id = $1",
                        [id.into()],
                    ))
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE custom_routes
                    DROP CONSTRAINT IF EXISTS chk_custom_routes_port_range;
                DROP INDEX IF EXISTS idx_custom_routes_domain_normalized_unique;
                ALTER TABLE custom_routes DROP COLUMN IF EXISTS force_override;
                "#,
            )
            .await?;
        Ok(())
    }
}

fn canonical_legacy_host(host: &str) -> Option<String> {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    let ip = unbracketed.parse::<IpAddr>().ok()?;
    let validation = match ip {
        IpAddr::V4(ip) => validate_ipv4(&ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(mapped) => validate_ipv4(&mapped),
            None => validate_ipv6(&ip),
        },
    };
    if !matches!(
        validation,
        Ok(()) | Err(UrlValidationError::PrivateIp) | Err(UrlValidationError::LoopbackIp)
    ) {
        return None;
    }
    Some(match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    })
}
