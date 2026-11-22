// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network policy for every kind (ADR-052 §4, ADR-055 phase 7).
//!
//! Additive only; nothing reads these tables unless a node is switched out
//! of `legacy`, so existing workloads change nothing:
//!
//! 1. **`nodes.network_policy_mode`** (`legacy | shadow | enforce`, default
//!    `legacy`). The control plane is node 0 and has no `nodes` row, so it
//!    gets the same column on the single-row `control_plane_node_settings`.
//! 2. **`network_policy_rules`**: one row per allow/deny rule, keyed by
//!    workload identity (`subject_kind`, `subject_id`), never by address.
//!    `origin = link` rows are owned by a link row (`link_table`, `link_id`)
//!    and are created and removed by triggers on `project_services`,
//!    `sandbox_services` and `environments`, so "connected" and "permitted"
//!    are written in the same statement and cannot drift apart. Operator
//!    rules are written by the API.
//! 3. **`network_policy_profiles`**: a subject's egress/ingress profile when
//!    it differs from its kind's default (sandboxes restricted, everything
//!    else open).
//! 4. **`network_policy_state.revision`**: bumped by statement triggers on
//!    every change a node's policy depends on, so a reconciler re-reads the
//!    rules only when the revision moved (O(changes), not a scan per tick).
//!
//! Existing links are backfilled as link rules.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const MODES: &str = "'legacy', 'shadow', 'enforce'";
const KINDS: &str = "'deployment', 'sandbox', 'service', 'function', 'job'";
const PROFILES: &str = "'open', 'restricted', 'none'";

fn up_sql() -> Vec<String> {
    vec![
        // 1. Modes.
        "ALTER TABLE nodes ADD COLUMN IF NOT EXISTS network_policy_mode TEXT NOT NULL DEFAULT 'legacy'"
            .to_string(),
        "ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_network_policy_mode_valid".to_string(),
        format!(
            "ALTER TABLE nodes ADD CONSTRAINT nodes_network_policy_mode_valid \
             CHECK (network_policy_mode IN ({MODES}))"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS control_plane_node_settings ( \
                 id SMALLINT PRIMARY KEY DEFAULT 0 CHECK (id = 0), \
                 network_policy_mode TEXT NOT NULL DEFAULT 'legacy' \
                     CHECK (network_policy_mode IN ({MODES})), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now())"
        ),
        "INSERT INTO control_plane_node_settings (id) VALUES (0) ON CONFLICT (id) DO NOTHING"
            .to_string(),
        // 2. Rules.
        format!(
            "CREATE TABLE IF NOT EXISTS network_policy_rules ( \
                 id BIGSERIAL PRIMARY KEY, \
                 subject_kind TEXT NOT NULL CHECK (subject_kind IN ({KINDS})), \
                 subject_id BIGINT NOT NULL, \
                 direction TEXT NOT NULL CHECK (direction IN ('egress', 'ingress')), \
                 peer_kind TEXT NOT NULL CHECK (peer_kind IN ('service', 'workload', 'cidr', 'host')), \
                 peer_ref TEXT NOT NULL CHECK (length(peer_ref) BETWEEN 1 AND 253), \
                 port_start INTEGER NULL, \
                 port_end INTEGER NULL, \
                 protocol TEXT NOT NULL DEFAULT 'tcp' CHECK (protocol IN ('tcp', 'udp', 'any')), \
                 action TEXT NOT NULL CHECK (action IN ('allow', 'deny')), \
                 origin TEXT NOT NULL CHECK (origin IN ('link', 'operator', 'system')), \
                 link_table TEXT NULL CHECK (link_table IN ('project_services', 'sandbox_services')), \
                 link_id BIGINT NULL, \
                 description TEXT NULL CHECK (length(description) <= 500), \
                 created_by INTEGER NULL REFERENCES users(id) ON DELETE SET NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 CONSTRAINT network_policy_rules_ports_valid CHECK ( \
                     (port_start IS NULL AND port_end IS NULL) OR \
                     (port_start BETWEEN 1 AND 65535 AND port_end BETWEEN port_start AND 65535)), \
                 CONSTRAINT network_policy_rules_link_owned CHECK ( \
                     (origin = 'link') = (link_table IS NOT NULL AND link_id IS NOT NULL)), \
                 CONSTRAINT network_policy_rules_host_is_egress CHECK ( \
                     peer_kind <> 'host' OR direction = 'egress'))"
        ),
        "CREATE INDEX IF NOT EXISTS network_policy_rules_subject \
             ON network_policy_rules (subject_kind, subject_id)"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS network_policy_rules_peer \
             ON network_policy_rules (peer_kind, peer_ref)"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS network_policy_rules_created_at \
             ON network_policy_rules (created_at DESC)"
            .to_string(),
        "CREATE UNIQUE INDEX IF NOT EXISTS network_policy_rules_link_unique \
             ON network_policy_rules (link_table, link_id, subject_kind, subject_id) \
             WHERE link_table IS NOT NULL"
            .to_string(),
        // 3. Profiles.
        format!(
            "CREATE TABLE IF NOT EXISTS network_policy_profiles ( \
                 subject_kind TEXT NOT NULL CHECK (subject_kind IN ({KINDS})), \
                 subject_id BIGINT NOT NULL, \
                 egress TEXT NULL CHECK (egress IN ({PROFILES})), \
                 ingress TEXT NULL CHECK (ingress IN ({PROFILES})), \
                 updated_by INTEGER NULL REFERENCES users(id) ON DELETE SET NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 PRIMARY KEY (subject_kind, subject_id))"
        ),
        // 4. Revision.
        "CREATE TABLE IF NOT EXISTS network_policy_state ( \
             id SMALLINT PRIMARY KEY DEFAULT 0 CHECK (id = 0), \
             revision BIGINT NOT NULL DEFAULT 0, \
             updated_at TIMESTAMPTZ NOT NULL DEFAULT now())"
            .to_string(),
        "INSERT INTO network_policy_state (id) VALUES (0) ON CONFLICT (id) DO NOTHING".to_string(),
        "CREATE OR REPLACE FUNCTION network_policy_bump_revision() RETURNS trigger AS $$ \
         BEGIN \
             UPDATE network_policy_state SET revision = revision + 1, updated_at = now() WHERE id = 0; \
             RETURN NULL; \
         END $$ LANGUAGE plpgsql"
            .to_string(),
        "CREATE TRIGGER network_policy_rules_revision \
             AFTER INSERT OR UPDATE OR DELETE ON network_policy_rules \
             FOR EACH STATEMENT EXECUTE FUNCTION network_policy_bump_revision()"
            .to_string(),
        "CREATE TRIGGER network_policy_profiles_revision \
             AFTER INSERT OR UPDATE OR DELETE ON network_policy_profiles \
             FOR EACH STATEMENT EXECUTE FUNCTION network_policy_bump_revision()"
            .to_string(),
        // A new sandbox or service changes the container-name → identity
        // map a node uses; status and activity updates do not.
        "CREATE TRIGGER sandboxes_network_policy_revision \
             AFTER INSERT OR DELETE ON sandboxes \
             FOR EACH STATEMENT EXECUTE FUNCTION network_policy_bump_revision()"
            .to_string(),
        "CREATE TRIGGER external_services_network_policy_revision \
             AFTER INSERT OR DELETE OR UPDATE OF name, service_type ON external_services \
             FOR EACH STATEMENT EXECUTE FUNCTION network_policy_bump_revision()"
            .to_string(),
        // Link rules: a project link allows every environment of the project
        // to reach the service; a sandbox link allows that sandbox.
        "CREATE OR REPLACE FUNCTION network_policy_project_service_link() RETURNS trigger AS $$ \
         BEGIN \
             IF TG_OP = 'INSERT' THEN \
                 INSERT INTO network_policy_rules \
                     (subject_kind, subject_id, direction, peer_kind, peer_ref, protocol, action, origin, link_table, link_id) \
                 SELECT 'deployment', e.id, 'egress', 'service', NEW.service_id::text, 'any', 'allow', 'link', \
                        'project_services', NEW.id \
                 FROM environments e WHERE e.project_id = NEW.project_id \
                 ON CONFLICT DO NOTHING; \
                 RETURN NEW; \
             END IF; \
             DELETE FROM network_policy_rules WHERE link_table = 'project_services' AND link_id = OLD.id; \
             RETURN OLD; \
         END $$ LANGUAGE plpgsql"
            .to_string(),
        "CREATE TRIGGER project_services_network_policy_link \
             AFTER INSERT OR DELETE ON project_services \
             FOR EACH ROW EXECUTE FUNCTION network_policy_project_service_link()"
            .to_string(),
        "CREATE OR REPLACE FUNCTION network_policy_environment_link() RETURNS trigger AS $$ \
         BEGIN \
             IF TG_OP = 'INSERT' THEN \
                 INSERT INTO network_policy_rules \
                     (subject_kind, subject_id, direction, peer_kind, peer_ref, protocol, action, origin, link_table, link_id) \
                 SELECT 'deployment', NEW.id, 'egress', 'service', ps.service_id::text, 'any', 'allow', 'link', \
                        'project_services', ps.id \
                 FROM project_services ps WHERE ps.project_id = NEW.project_id \
                 ON CONFLICT DO NOTHING; \
                 RETURN NEW; \
             END IF; \
             DELETE FROM network_policy_rules WHERE subject_kind = 'deployment' AND subject_id = OLD.id; \
             DELETE FROM network_policy_profiles WHERE subject_kind = 'deployment' AND subject_id = OLD.id; \
             RETURN OLD; \
         END $$ LANGUAGE plpgsql"
            .to_string(),
        "CREATE TRIGGER environments_network_policy_link \
             AFTER INSERT OR DELETE ON environments \
             FOR EACH ROW EXECUTE FUNCTION network_policy_environment_link()"
            .to_string(),
        "CREATE OR REPLACE FUNCTION network_policy_sandbox_service_link() RETURNS trigger AS $$ \
         BEGIN \
             IF TG_OP = 'INSERT' THEN \
                 INSERT INTO network_policy_rules \
                     (subject_kind, subject_id, direction, peer_kind, peer_ref, protocol, action, origin, link_table, link_id, created_by) \
                 VALUES ('sandbox', NEW.sandbox_id, 'egress', 'service', NEW.service_id::text, 'any', 'allow', 'link', \
                         'sandbox_services', NEW.id, NEW.created_by) \
                 ON CONFLICT DO NOTHING; \
                 RETURN NEW; \
             END IF; \
             DELETE FROM network_policy_rules WHERE link_table = 'sandbox_services' AND link_id = OLD.id; \
             RETURN OLD; \
         END $$ LANGUAGE plpgsql"
            .to_string(),
        "CREATE TRIGGER sandbox_services_network_policy_link \
             AFTER INSERT OR DELETE ON sandbox_services \
             FOR EACH ROW EXECUTE FUNCTION network_policy_sandbox_service_link()"
            .to_string(),
        // Removing a sandbox or a service removes the rules about it.
        "CREATE OR REPLACE FUNCTION network_policy_sandbox_removed() RETURNS trigger AS $$ \
         BEGIN \
             DELETE FROM network_policy_rules WHERE subject_kind = 'sandbox' AND subject_id = OLD.id; \
             DELETE FROM network_policy_rules \
                 WHERE peer_kind = 'workload' AND peer_ref = 'sandbox/' || OLD.id::text; \
             DELETE FROM network_policy_profiles WHERE subject_kind = 'sandbox' AND subject_id = OLD.id; \
             RETURN OLD; \
         END $$ LANGUAGE plpgsql"
            .to_string(),
        "CREATE TRIGGER sandboxes_network_policy_removed \
             AFTER DELETE ON sandboxes \
             FOR EACH ROW EXECUTE FUNCTION network_policy_sandbox_removed()"
            .to_string(),
        "CREATE OR REPLACE FUNCTION network_policy_service_removed() RETURNS trigger AS $$ \
         BEGIN \
             DELETE FROM network_policy_rules WHERE subject_kind = 'service' AND subject_id = OLD.id; \
             DELETE FROM network_policy_rules WHERE peer_kind = 'service' AND peer_ref = OLD.id::text; \
             DELETE FROM network_policy_rules \
                 WHERE peer_kind = 'workload' AND peer_ref = 'service/' || OLD.id::text; \
             DELETE FROM network_policy_profiles WHERE subject_kind = 'service' AND subject_id = OLD.id; \
             RETURN OLD; \
         END $$ LANGUAGE plpgsql"
            .to_string(),
        "CREATE TRIGGER external_services_network_policy_removed \
             AFTER DELETE ON external_services \
             FOR EACH ROW EXECUTE FUNCTION network_policy_service_removed()"
            .to_string(),
    ]
}

/// Existing links become link rules. Idempotent.
pub fn backfill_sql() -> Vec<String> {
    vec![
        "INSERT INTO network_policy_rules \
             (subject_kind, subject_id, direction, peer_kind, peer_ref, protocol, action, origin, link_table, link_id) \
         SELECT 'deployment', e.id, 'egress', 'service', ps.service_id::text, 'any', 'allow', 'link', \
                'project_services', ps.id \
         FROM project_services ps JOIN environments e ON e.project_id = ps.project_id \
         ON CONFLICT DO NOTHING"
            .to_string(),
        "INSERT INTO network_policy_rules \
             (subject_kind, subject_id, direction, peer_kind, peer_ref, protocol, action, origin, link_table, link_id, created_by) \
         SELECT 'sandbox', ss.sandbox_id, 'egress', 'service', ss.service_id::text, 'any', 'allow', 'link', \
                'sandbox_services', ss.id, ss.created_by \
         FROM sandbox_services ss \
         ON CONFLICT DO NOTHING"
            .to_string(),
    ]
}

fn down_sql() -> Vec<String> {
    vec![
        "DROP TRIGGER IF EXISTS external_services_network_policy_removed ON external_services",
        "DROP TRIGGER IF EXISTS sandboxes_network_policy_removed ON sandboxes",
        "DROP TRIGGER IF EXISTS sandbox_services_network_policy_link ON sandbox_services",
        "DROP TRIGGER IF EXISTS environments_network_policy_link ON environments",
        "DROP TRIGGER IF EXISTS project_services_network_policy_link ON project_services",
        "DROP TRIGGER IF EXISTS external_services_network_policy_revision ON external_services",
        "DROP TRIGGER IF EXISTS sandboxes_network_policy_revision ON sandboxes",
        "DROP TABLE IF EXISTS network_policy_profiles",
        "DROP TABLE IF EXISTS network_policy_rules",
        "DROP TABLE IF EXISTS network_policy_state",
        "DROP FUNCTION IF EXISTS network_policy_service_removed()",
        "DROP FUNCTION IF EXISTS network_policy_sandbox_removed()",
        "DROP FUNCTION IF EXISTS network_policy_sandbox_service_link()",
        "DROP FUNCTION IF EXISTS network_policy_environment_link()",
        "DROP FUNCTION IF EXISTS network_policy_project_service_link()",
        "DROP FUNCTION IF EXISTS network_policy_bump_revision()",
        "DROP TABLE IF EXISTS control_plane_node_settings",
        "ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_network_policy_mode_valid",
        "ALTER TABLE nodes DROP COLUMN IF EXISTS network_policy_mode",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for sql in up_sql().into_iter().chain(backfill_sql()) {
            conn.execute_unprepared(&sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for sql in down_sql() {
            conn.execute_unprepared(&sql).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_defaults_to_legacy_everywhere() {
        let sql = up_sql().join("\n");
        assert!(sql.contains("network_policy_mode TEXT NOT NULL DEFAULT 'legacy'"));
        assert_eq!(
            sql.matches("network_policy_mode TEXT NOT NULL DEFAULT 'legacy'")
                .count(),
            2,
            "nodes and node 0 both default to legacy"
        );
    }

    #[test]
    fn down_removes_everything_up_creates() {
        let up = up_sql().join("\n");
        let down = down_sql().join("\n");
        for object in [
            "network_policy_rules",
            "network_policy_profiles",
            "network_policy_state",
            "control_plane_node_settings",
            "network_policy_bump_revision",
            "network_policy_project_service_link",
            "network_policy_environment_link",
            "network_policy_sandbox_service_link",
            "network_policy_sandbox_removed",
            "network_policy_service_removed",
            "project_services_network_policy_link",
            "environments_network_policy_link",
            "sandbox_services_network_policy_link",
            "sandboxes_network_policy_removed",
            "external_services_network_policy_removed",
            "sandboxes_network_policy_revision",
            "external_services_network_policy_revision",
        ] {
            assert!(up.contains(object), "up creates {object}");
            assert!(down.contains(object), "down removes {object}");
        }
        assert!(down.contains("DROP COLUMN IF EXISTS network_policy_mode"));
    }
}
