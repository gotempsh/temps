// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Custom hostnames for public status pages (#1412).
//!
//! - `status_page_domains`: one row per hostname attached to a status page.
//!   `hostname` is stored normalized (lowercase ASCII/punycode, no trailing
//!   dot) and is unique across all pages.
//! - `domain_id` points at the `domains` row that carries the ACME order and,
//!   once ownership is proven, the certificate. Temps creates that row for the
//!   binding and owns it exclusively, so a trigger deletes it whenever the
//!   binding goes away or is re-pointed — on every path, including cascades
//!   from page or project deletion — together with the certificate copy in
//!   `tls_acme_certificates`. That stops both serving and renewal of the
//!   certificate and leaves no stored copy behind.
//! - Removing a hostname, or deleting its page or project, leaves a
//!   *tombstone*: `status_page_id` becomes NULL and `detached_at` is set. A
//!   tombstone keeps answering 404 for the hostname (instead of letting it fall
//!   through to the console) until the name is attached again or claimed by an
//!   application, which always takes precedence over a tombstone.
//! - `certificate_operation_until` is a lease: a verify or renew claims it
//!   with one conditional UPDATE and clears it when done, so two certificate
//!   operations on one hostname never overlap (one would replace or delete
//!   the other's ACME order). It holds no lock or connection while the CA is
//!   called, works across processes, and expires on its own if a process
//!   dies mid-operation.
//! - Route-table NOTIFY triggers make every proxy (in-process or a separate
//!   `temps proxy`) reload when a binding, a page, or the certificate state of
//!   a bound `domains` row changes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE IF NOT EXISTS status_page_domains (
    id SERIAL PRIMARY KEY,
    status_page_id INTEGER REFERENCES status_pages(id) ON DELETE SET NULL,
    hostname VARCHAR(253) NOT NULL,
    verification_method VARCHAR(16) NOT NULL DEFAULT 'http-01',
    domain_id INTEGER REFERENCES domains(id) ON DELETE SET NULL,
    detached_at TIMESTAMPTZ,
    certificate_operation_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT status_page_domains_method_check
        CHECK (verification_method IN ('http-01', 'dns-01')),
    CONSTRAINT status_page_domains_hostname_normalized
        CHECK (hostname = lower(hostname) AND hostname NOT LIKE '%.' AND hostname NOT LIKE '%*%'),
    CONSTRAINT status_page_domains_tombstone_shape
        CHECK ((status_page_id IS NULL) = (detached_at IS NOT NULL)
               AND (status_page_id IS NOT NULL OR domain_id IS NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_status_page_domains_hostname
    ON status_page_domains (hostname);

CREATE INDEX IF NOT EXISTS idx_status_page_domains_page
    ON status_page_domains (status_page_id, id) WHERE status_page_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uq_status_page_domains_domain
    ON status_page_domains (domain_id) WHERE domain_id IS NOT NULL;

-- Detaching a binding (directly, or through ON DELETE SET NULL when its page
-- is deleted) turns it into a tombstone and releases its certificate.
CREATE OR REPLACE FUNCTION status_page_domain_tombstone()
RETURNS trigger AS $$
BEGIN
    IF NEW.status_page_id IS NULL THEN
        NEW.detached_at := COALESCE(NEW.detached_at, now());
        NEW.domain_id := NULL;
    ELSE
        NEW.detached_at := NULL;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS status_page_domains_tombstone ON status_page_domains;
CREATE TRIGGER status_page_domains_tombstone
    BEFORE INSERT OR UPDATE ON status_page_domains
    FOR EACH ROW EXECUTE FUNCTION status_page_domain_tombstone();

-- Delete a certificate a status-page binding owns, from both places ACME
-- issuance stores it: the `domains` row (served and renewed) and the
-- `tls_acme_certificates` copy, which has no foreign key to that row. The
-- hostname is unique to the binding (attaching refuses a name any `domains`
-- row already uses), so the copy is matched by name, as
-- `DomainService::delete_domain` does. The application calls this for an
-- order it created but could not link, so there is one cleanup path.
CREATE OR REPLACE FUNCTION release_status_page_certificate(certificate_id INTEGER)
RETURNS void AS $$
DECLARE
    released_hostname TEXT;
BEGIN
    DELETE FROM domains WHERE id = certificate_id RETURNING domain INTO released_hostname;
    IF released_hostname IS NOT NULL THEN
        DELETE FROM tls_acme_certificates WHERE lower(domain) = lower(released_hostname);
    END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION status_page_domain_release_certificate()
RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.domain_id IS NOT NULL THEN
            PERFORM release_status_page_certificate(OLD.domain_id);
        END IF;
        RETURN OLD;
    END IF;
    IF OLD.domain_id IS NOT NULL AND OLD.domain_id IS DISTINCT FROM NEW.domain_id THEN
        PERFORM release_status_page_certificate(OLD.domain_id);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS status_page_domains_release_certificate ON status_page_domains;
CREATE TRIGGER status_page_domains_release_certificate
    AFTER UPDATE OR DELETE ON status_page_domains
    FOR EACH ROW EXECUTE FUNCTION status_page_domain_release_certificate();

-- Only changes that affect routing reload the route table; the certificate
-- operation lease is written on every verify and renew and does not.
DROP TRIGGER IF EXISTS status_page_domains_route_table_changes ON status_page_domains;
CREATE TRIGGER status_page_domains_route_table_changes
    AFTER INSERT OR DELETE OR UPDATE OF status_page_id, hostname, domain_id
    ON status_page_domains
    FOR EACH STATEMENT EXECUTE FUNCTION notify_route_table_change();

DROP TRIGGER IF EXISTS status_pages_route_table_changes ON status_pages;
CREATE TRIGGER status_pages_route_table_changes
    AFTER INSERT OR UPDATE OR DELETE ON status_pages
    FOR EACH STATEMENT EXECUTE FUNCTION notify_route_table_change();

CREATE OR REPLACE FUNCTION notify_status_page_certificate_change()
RETURNS trigger AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM status_page_domains WHERE domain_id = NEW.id) THEN
        PERFORM pg_notify('route_table_changes', '');
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS domains_status_page_route_changes ON domains;
CREATE TRIGGER domains_status_page_route_changes
    AFTER UPDATE OF status, certificate ON domains
    FOR EACH ROW EXECUTE FUNCTION notify_status_page_certificate_change();
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
-- Dropping the table does not fire its row triggers. Release the
-- certificates the bindings own first, or they would stay served and keep
-- being renewed with nothing left that owns them.
DO $$
BEGIN
    IF to_regclass('status_page_domains') IS NOT NULL THEN
        PERFORM release_status_page_certificate(domain_id)
        FROM status_page_domains
        WHERE domain_id IS NOT NULL;
    END IF;
END;
$$;
DROP TRIGGER IF EXISTS domains_status_page_route_changes ON domains;
DROP FUNCTION IF EXISTS notify_status_page_certificate_change();
DROP TRIGGER IF EXISTS status_pages_route_table_changes ON status_pages;
DROP TABLE IF EXISTS status_page_domains;
DROP FUNCTION IF EXISTS status_page_domain_release_certificate();
DROP FUNCTION IF EXISTS release_status_page_certificate(INTEGER);
DROP FUNCTION IF EXISTS status_page_domain_tombstone();
"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    /// The tables and trigger function this migration builds on, reduced to
    /// the columns it touches.
    const DEPENDENCIES: &str = r#"
CREATE TABLE projects (id SERIAL PRIMARY KEY);
CREATE TABLE status_pages (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    is_published BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE TABLE domains (
    id SERIAL PRIMARY KEY,
    domain TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    certificate TEXT
);
CREATE TABLE tls_acme_certificates (id SERIAL PRIMARY KEY, domain TEXT NOT NULL);
CREATE FUNCTION notify_route_table_change() RETURNS trigger AS $$
BEGIN
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
INSERT INTO projects DEFAULT VALUES;
INSERT INTO status_pages (project_id, is_published) VALUES (1, TRUE);
"#;

    async fn count(db: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
        db.query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .expect("count query")
            .expect("count row")
            .try_get::<i64>("", "n")
            .expect("decode count")
    }

    /// Issue a certificate for `hostname` the way ACME does (the `domains`
    /// row plus its `tls_acme_certificates` copy) and bind it to page 1.
    async fn issue(db: &sea_orm::DatabaseConnection, hostname: &str) {
        db.execute_unprepared(&format!(
            "INSERT INTO domains (domain, certificate) VALUES ('{hostname}', 'pem');
             INSERT INTO tls_acme_certificates (domain) VALUES ('{hostname}');
             INSERT INTO status_page_domains (status_page_id, hostname, domain_id)
             SELECT 1, '{hostname}', id FROM domains WHERE domain = '{hostname}';"
        ))
        .await
        .expect("issue certificate");
    }

    async fn certificates_for(db: &sea_orm::DatabaseConnection, hostname: &str) -> (i64, i64) {
        (
            count(
                db,
                &format!("SELECT COUNT(*) AS n FROM domains WHERE domain = '{hostname}'"),
            )
            .await,
            count(
                db,
                &format!(
                    "SELECT COUNT(*) AS n FROM tls_acme_certificates WHERE domain = '{hostname}'"
                ),
            )
            .await,
        )
    }

    #[tokio::test]
    async fn released_and_rolled_back_certificates_leave_nothing_behind() {
        let container = match GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "migration-test")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                let message = error.to_string().to_lowercase();
                if message.contains("socket")
                    || message.contains("connection refused")
                    || message.contains("permission denied")
                {
                    eprintln!(
                        "Skipping status page domains migration test: Docker unavailable: {error}"
                    );
                    return;
                }
                panic!("status page domains migration database startup failed: {error}");
            }
        };
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Postgres port");
        let db = Database::connect(format!(
            "postgres://postgres:migration-test@127.0.0.1:{port}/postgres"
        ))
        .await
        .expect("connect");
        db.execute_unprepared(DEPENDENCIES)
            .await
            .expect("create dependencies");
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.expect("apply migration");

        // Detaching a binding (removing the hostname) releases both stores.
        issue(&db, "removed.example.org").await;
        assert_eq!(certificates_for(&db, "removed.example.org").await, (1, 1));
        db.execute_unprepared(
            "UPDATE status_page_domains SET status_page_id = NULL WHERE hostname = 'removed.example.org'",
        )
        .await
        .expect("detach");
        assert_eq!(certificates_for(&db, "removed.example.org").await, (0, 0));

        // Releasing (unpublish) and deleting the binding do too.
        issue(&db, "released.example.org").await;
        db.execute_unprepared(
            "UPDATE status_page_domains SET domain_id = NULL WHERE hostname = 'released.example.org'",
        )
        .await
        .expect("release");
        assert_eq!(certificates_for(&db, "released.example.org").await, (0, 0));
        issue(&db, "deleted.example.org").await;
        db.execute_unprepared(
            "DELETE FROM status_page_domains WHERE hostname = 'deleted.example.org'",
        )
        .await
        .expect("delete binding");
        assert_eq!(certificates_for(&db, "deleted.example.org").await, (0, 0));

        // The application's cleanup of an order it could not link.
        db.execute_unprepared(
            "INSERT INTO domains (domain, certificate) VALUES ('unlinked.example.org', 'pem');
             INSERT INTO tls_acme_certificates (domain) VALUES ('unlinked.example.org');",
        )
        .await
        .expect("unlinked order");
        db.execute_unprepared(
            "SELECT release_status_page_certificate(id) FROM domains WHERE domain = 'unlinked.example.org'",
        )
        .await
        .expect("release unlinked order");
        assert_eq!(certificates_for(&db, "unlinked.example.org").await, (0, 0));

        // Rolling the migration back releases what the bindings still own,
        // and nothing else.
        issue(&db, "owned.example.org").await;
        db.execute_unprepared(
            "INSERT INTO domains (domain, certificate) VALUES ('operator.example.org', 'pem');
             INSERT INTO tls_acme_certificates (domain) VALUES ('operator.example.org');",
        )
        .await
        .expect("operator certificate");
        Migration.down(&manager).await.expect("roll back");
        assert_eq!(certificates_for(&db, "owned.example.org").await, (0, 0));
        assert_eq!(
            certificates_for(&db, "operator.example.org").await,
            (1, 1),
            "certificates managed under Domains are untouched"
        );
        assert!(!manager
            .has_table("status_page_domains")
            .await
            .expect("table check"));

        // And it re-applies cleanly.
        Migration.up(&manager).await.expect("re-apply migration");
    }
}
