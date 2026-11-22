// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS name ownership (ADR-055 §10, phase 0).
//!
//! Three changes, in one migration so the zone is never half-converted:
//!
//! 1. **Deployment records move to the environment owner.** The stable name
//!    `<env>.<project>.temps.local` used to be owned by `deployments.id`, a
//!    new owner on every redeploy, which is why the registry had to let a
//!    writer take over another owner's row. Every `deployment` row is
//!    rewritten to `owner_kind = 'environment'`, `owner_id =
//!    deployments.environment_id`; rows whose deployment no longer exists
//!    are orphans and are deleted. `'deployment'` stays allowed by the CHECK
//!    constraint for one release, so a rolled-back binary can still publish.
//! 2. **`dns_name_claims`**: one owner per name, unique on the normalised
//!    (lower-case, no trailing dot) `fqdn`.
//! 3. **`dns_name_conflicts`**: owners that publish a name someone else
//!    holds. Nothing is removed for them; they are reported.
//!
//! ## Backfill winner
//!
//! Where rows of several owners share a name, the claim goes to, in order:
//! the owner kind (`service_member`/`service_role` first, then
//! `node`/`static`, then `environment`/`deployment`), the oldest row, the
//! lowest row id. Databases win because their records point at container
//! addresses that change on restart, so refusing their publish would strand
//! the name on a dead address; an environment's records point at node
//! proxies that never change on redeploy, so refusing its publish keeps it
//! resolving exactly as today. Every loser gets a conflict row.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Owner kinds accepted after this migration. `deployment` is legacy: kept
/// for one release so a rolled-back binary can still write it.
const OWNER_KINDS: &str =
    "'service_member', 'service_role', 'node', 'static', 'deployment', 'environment'";

/// Normalised name, matching `normalize_fqdn` in `temps-dns`.
const NORMALIZED_FQDN: &str = "lower(rtrim(fqdn, '.'))";

/// Rank of an owner kind in the backfill (lower wins).
const OWNER_RANK: &str = "CASE owner_kind \
     WHEN 'service_member' THEN 0 WHEN 'service_role' THEN 0 \
     WHEN 'node' THEN 1 WHEN 'static' THEN 1 \
     ELSE 2 END";

/// Claim every name in `service_endpoints` that has no claim yet, choosing
/// the winner deterministically, then record every other owner of a claimed
/// name as a conflict. Idempotent: running it twice changes nothing.
pub fn backfill_sql() -> Vec<String> {
    vec![
        format!(
            "INSERT INTO dns_name_claims (fqdn, owner_kind, owner_id, created_at, updated_at) \
             SELECT DISTINCT ON ({NORMALIZED_FQDN}) {NORMALIZED_FQDN}, owner_kind, owner_id, \
                    created_at, now() \
             FROM service_endpoints \
             ORDER BY {NORMALIZED_FQDN}, {OWNER_RANK}, created_at, id \
             ON CONFLICT (fqdn) DO NOTHING"
        ),
        "INSERT INTO dns_name_conflicts \
             (fqdn, owner_kind, owner_id, holder_kind, holder_id, first_seen_at, last_seen_at) \
         SELECT DISTINCT c.fqdn, se.owner_kind, se.owner_id, c.owner_kind, c.owner_id, now(), now() \
         FROM service_endpoints se \
         JOIN dns_name_claims c ON c.fqdn = lower(rtrim(se.fqdn, '.')) \
         WHERE se.owner_kind <> c.owner_kind OR se.owner_id <> c.owner_id \
         ON CONFLICT (fqdn, owner_kind, owner_id) DO NOTHING"
            .to_string(),
    ]
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let mut stmts = vec![
            "ALTER TABLE service_endpoints DROP CONSTRAINT IF EXISTS service_endpoints_owner_kind_valid"
                .to_string(),
            format!(
                "ALTER TABLE service_endpoints ADD CONSTRAINT service_endpoints_owner_kind_valid \
                 CHECK (owner_kind IN ({OWNER_KINDS}))"
            ),
            // Take the registry's lock first, like every writer of the zone.
            "SELECT current FROM dns_generation WHERE id = 1 FOR UPDATE".to_string(),
            "UPDATE service_endpoints se \
             SET owner_kind = 'environment', owner_id = d.environment_id, updated_at = now() \
             FROM deployments d \
             WHERE se.owner_kind = 'deployment' AND d.id = se.owner_id"
                .to_string(),
            "DELETE FROM service_endpoints WHERE owner_kind = 'deployment'".to_string(),
            format!(
                "CREATE TABLE IF NOT EXISTS dns_name_claims ( \
                     fqdn TEXT PRIMARY KEY, \
                     owner_kind TEXT NOT NULL CHECK (owner_kind IN ({OWNER_KINDS})), \
                     owner_id BIGINT NOT NULL, \
                     target_workload_id BIGINT NULL, \
                     created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                     updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                     CONSTRAINT dns_name_claims_fqdn_normalized \
                         CHECK (fqdn = lower(rtrim(fqdn, '.')) AND fqdn <> '') \
                 )"
            ),
            "CREATE INDEX IF NOT EXISTS dns_name_claims_owner \
             ON dns_name_claims (owner_kind, owner_id)"
                .to_string(),
            // The publish deletes leftovers by normalised name.
            "CREATE INDEX IF NOT EXISTS service_endpoints_fqdn_normalized \
             ON service_endpoints (lower(rtrim(fqdn, '.')))"
                .to_string(),
            "CREATE TABLE IF NOT EXISTS dns_name_conflicts ( \
                 id BIGSERIAL PRIMARY KEY, \
                 fqdn TEXT NOT NULL, \
                 owner_kind TEXT NOT NULL, \
                 owner_id BIGINT NOT NULL, \
                 holder_kind TEXT NOT NULL, \
                 holder_id BIGINT NOT NULL, \
                 first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 CONSTRAINT dns_name_conflicts_uniq UNIQUE (fqdn, owner_kind, owner_id) \
             )"
            .to_string(),
            "CREATE INDEX IF NOT EXISTS dns_name_conflicts_owner \
             ON dns_name_conflicts (owner_kind, owner_id)"
                .to_string(),
        ];
        stmts.extend(backfill_sql());
        // Resolvers re-read the zone: owners changed and orphans went away.
        stmts.push(
            "UPDATE dns_generation SET current = current + 1, updated_at = now() WHERE id = 1"
                .to_string(),
        );
        for s in stmts {
            conn.execute_unprepared(&s).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let stmts = [
            "DROP TABLE IF EXISTS dns_name_conflicts",
            "DROP TABLE IF EXISTS dns_name_claims",
            "DROP INDEX IF EXISTS service_endpoints_fqdn_normalized",
            "SELECT current FROM dns_generation WHERE id = 1 FOR UPDATE",
            // Back to the previous owner: the environment's current deployment.
            "UPDATE service_endpoints se \
             SET owner_kind = 'deployment', owner_id = e.current_deployment_id, updated_at = now() \
             FROM environments e \
             WHERE se.owner_kind = 'environment' AND e.id = se.owner_id \
               AND e.current_deployment_id IS NOT NULL",
            "DELETE FROM service_endpoints WHERE owner_kind = 'environment'",
            "ALTER TABLE service_endpoints DROP CONSTRAINT IF EXISTS service_endpoints_owner_kind_valid",
            "ALTER TABLE service_endpoints ADD CONSTRAINT service_endpoints_owner_kind_valid \
             CHECK (owner_kind IN ('service_member', 'service_role', 'node', 'static', 'deployment'))",
            "UPDATE dns_generation SET current = current + 1, updated_at = now() WHERE id = 1",
        ];
        for s in stmts {
            conn.execute_unprepared(s).await?;
        }
        Ok(())
    }
}
