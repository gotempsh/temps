// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-side TLS for the databases Temps manages (ADR 093).
//!
//! `managed_service_certificate_authorities` holds the managed-services CA,
//! separate from the cluster CA in `settings`: the cluster CA's leaves are
//! worker identities and must never verify as a database, nor the reverse.
//! One row per `purpose`; the unique index makes lazy creation race-safe
//! (`INSERT ... ON CONFLICT DO NOTHING`, then read back the winner). The key is
//! encrypted with `EncryptionService` and never leaves the control plane.
//!
//! `external_service_tls` is the per-service TLS setting and the server
//! certificate currently issued for it, one optional row per service. A table
//! rather than a column on `external_services`, because the certificate's
//! lifecycle (renewal by `not_after`, the encrypted key) is its own and the
//! renewal job (Phase 2) needs an index over it. **A service without a row is
//! `off`**: every existing service keeps exactly today's behaviour, no backfill.
//! New standalone PostgreSQL services get a row with `available` at creation.
//!
//! `down` drops both tables. It loses the CA and every issued certificate;
//! containers already configured with `ssl=on` keep serving the files in
//! their volumes until they are next recreated by the previous binary, which
//! does not set `ssl=on` and therefore stops offering TLS.

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
CREATE TABLE IF NOT EXISTS managed_service_certificate_authorities (
 id serial PRIMARY KEY,
 purpose varchar(64) NOT NULL,
 certificate_pem text NOT NULL,
 private_key_encrypted text NOT NULL,
 fingerprint_sha256 varchar(64) NOT NULL,
 not_before timestamptz NOT NULL,
 not_after timestamptz NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS managed_service_certificate_authorities_purpose
 ON managed_service_certificate_authorities(purpose);

CREATE TABLE IF NOT EXISTS external_service_tls (
 service_id integer PRIMARY KEY REFERENCES external_services(id) ON DELETE CASCADE,
 mode varchar(16) NOT NULL DEFAULT 'off'
  CONSTRAINT external_service_tls_mode_check CHECK (mode IN ('off','available','required')),
 authority_id integer REFERENCES managed_service_certificate_authorities(id) ON DELETE RESTRICT,
 certificate_pem text,
 private_key_encrypted text,
 fingerprint_sha256 varchar(64),
 subject_alt_names text,
 not_before timestamptz,
 not_after timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT external_service_tls_certificate_complete CHECK (
  (certificate_pem IS NULL AND private_key_encrypted IS NULL AND fingerprint_sha256 IS NULL
   AND subject_alt_names IS NULL AND not_before IS NULL AND not_after IS NULL AND authority_id IS NULL)
  OR
  (certificate_pem IS NOT NULL AND private_key_encrypted IS NOT NULL AND fingerprint_sha256 IS NOT NULL
   AND subject_alt_names IS NOT NULL AND not_before IS NOT NULL AND not_after IS NOT NULL AND authority_id IS NOT NULL)
 )
);
CREATE INDEX IF NOT EXISTS external_service_tls_not_after
 ON external_service_tls(not_after) WHERE not_after IS NOT NULL;
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
DROP TABLE IF EXISTS external_service_tls;
DROP TABLE IF EXISTS managed_service_certificate_authorities;
"#,
            )
            .await?;
        Ok(())
    }
}
