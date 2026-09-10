// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE delivery_profiles (
  id serial PRIMARY KEY,
  name text NOT NULL UNIQUE,
  provider_kind text NOT NULL CHECK (provider_kind IN ('direct','cloudflare')),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE project_delivery_settings (
  project_id integer PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  default_profile_id integer REFERENCES delivery_profiles(id) ON DELETE RESTRICT,
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE environment_delivery_settings (
  environment_id integer PRIMARY KEY REFERENCES environments(id) ON DELETE CASCADE,
  profile_id integer REFERENCES delivery_profiles(id) ON DELETE RESTRICT,
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE domain_delivery_bindings (
  id serial PRIMARY KEY,
  hostname text NOT NULL,
  project_id integer NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
  environment_id integer NOT NULL REFERENCES environments(id) ON DELETE RESTRICT,
  custom_domain_id integer NOT NULL REFERENCES project_custom_domains(id) ON DELETE RESTRICT,
  profile_id integer NOT NULL REFERENCES delivery_profiles(id) ON DELETE RESTRICT,
  profile_source text NOT NULL,
  dns_provider_id integer NOT NULL REFERENCES dns_providers(id) ON DELETE RESTRICT,
  zone text NOT NULL,
  origin_target text NOT NULL,
  record_type text NOT NULL,
  proxied boolean NOT NULL,
  status text NOT NULL,
  last_error text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  applied_at timestamptz,
  CONSTRAINT uq_domain_delivery_bindings_hostname UNIQUE (hostname)
);
CREATE INDEX idx_domain_delivery_bindings_project ON domain_delivery_bindings(project_id);
CREATE TABLE domain_delivery_previews (
  id uuid PRIMARY KEY,
  project_id integer NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  actor_user_id integer NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  request jsonb NOT NULL,
  plan jsonb NOT NULL,
  config_fingerprint text NOT NULL,
  status text NOT NULL,
  last_error text,
  expires_at timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  applied_at timestamptz
);
CREATE INDEX idx_domain_delivery_previews_project_created ON domain_delivery_previews(project_id, created_at DESC);
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP TABLE IF EXISTS domain_delivery_previews;
DROP TABLE IF EXISTS domain_delivery_bindings;
DROP TABLE IF EXISTS environment_delivery_settings;
DROP TABLE IF EXISTS project_delivery_settings;
DROP TABLE IF EXISTS delivery_profiles;
"#,
            )
            .await?;
        Ok(())
    }
}
