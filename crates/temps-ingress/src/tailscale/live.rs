// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The live [`TailscaleOps`]: Serve entries in Temps' database, the
//! cluster's OAuth client, and Services through the tailnet's API (ADR 065).

use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::Value;
use temps_entities::{ingress_connectors, reachability_runs, tailnet_serve_entries};
use tracing::info;

use super::run::TailscaleOps;
use super::{ServiceDefinition, ServiceHost, TailscaleCredential};
use crate::reachability::model::RunResource;
use crate::reachability::run::{
    unrecorded_hosts, ServeClaim, ServeEntryStatus, ServeRelease, ServeWish, ServiceDeletion,
    ServiceEntryHost, WriteError,
};
use crate::reachability::serve_entries;
use crate::reachability::service::{load_tailscale_credential, record_of, LiveOps};
use crate::tailscale;

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[async_trait]
impl TailscaleOps for LiveOps {
    async fn find_credential(&self, name: &str) -> Result<Option<RunResource>, String> {
        let db = self.deps.db.as_ref();
        Ok(ingress_connectors::Entity::find()
            .filter(ingress_connectors::Column::Name.eq(name))
            .filter(ingress_connectors::Column::Provider.eq(tailscale::KIND))
            .one(db)
            .await
            .map_err(text)?
            .map(|c| RunResource::TailscaleCredential {
                id: c.id,
                name: c.name,
            }))
    }

    async fn claim_serve_entry(&self, run_id: i32, wish: &ServeWish) -> Result<ServeClaim, String> {
        serve_entries::claim(self.deps.db.as_ref(), run_id, wish)
            .await
            .map_err(text)
    }

    async fn serve_reference(
        &self,
        run_id: i32,
        node_id: i32,
        address_id: &str,
    ) -> Result<Option<(i32, bool)>, String> {
        serve_entries::reference(self.deps.db.as_ref(), run_id, node_id, address_id)
            .await
            .map_err(text)
    }

    async fn serve_entry(
        &self,
        node_id: i32,
        address_id: &str,
    ) -> Result<Option<ServeEntryStatus>, String> {
        serve_entries::status(self.deps.db.as_ref(), node_id, address_id)
            .await
            .map_err(text)
    }

    async fn release_serve_entry(
        &self,
        run_id: i32,
        node_id: i32,
        address_id: &str,
    ) -> Result<ServeRelease, String> {
        serve_entries::release(self.deps.db.as_ref(), run_id, node_id, address_id)
            .await
            .map_err(text)
    }

    async fn forget_serve_entry(&self, entry_id: i32) -> Result<(), String> {
        serve_entries::forget(self.deps.db.as_ref(), entry_id)
            .await
            .map_err(text)
    }

    async fn save_tailscale_credential(
        &self,
        name: &str,
        credential: &TailscaleCredential,
    ) -> Result<i32, String> {
        let values: std::collections::BTreeMap<String, Value> = [
            (tailscale::FIELD_CLIENT_ID, &credential.client_id),
            (tailscale::FIELD_CLIENT_SECRET, &credential.client_secret),
            (tailscale::FIELD_TAILNET, &credential.tailnet),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), Value::String(value.clone())))
        .collect();
        self.deps
            .connectors
            .save_credential(tailscale::KIND, name, &values)
            .await
            .map_err(text)
    }

    async fn release_tailscale_credential(&self, id: i32) -> Result<Option<String>, String> {
        // Every Service setup reads the cluster's one credential.
        let in_use = tailnet_serve_entries::Entity::find()
            .filter(tailnet_serve_entries::Column::ServiceName.is_not_null())
            .one(self.deps.db.as_ref())
            .await
            .map_err(text)?;
        if let Some(entry) = in_use {
            return Ok(Some(format!(
                "{} still serves {} as a Tailscale Service",
                entry.node_name, entry.hostname
            )));
        }
        self.deps
            .connectors
            .delete_credential(id)
            .await
            .map_err(text)?;
        Ok(None)
    }

    async fn read_service(&self, name: &str) -> Result<Option<ServiceDefinition>, String> {
        let credential = self.tailscale().await?;
        self.tailnet
            .get_service(&credential, name)
            .await
            .map_err(text)
    }

    async fn create_service(&self, definition: &ServiceDefinition) -> Result<(), WriteError> {
        let credential = self.tailscale().await.map_err(WriteError::refused)?;
        self.tailnet
            .create_service(&credential, definition)
            .await
            .map_err(|error| WriteError {
                maybe_applied: error.maybe_applied(),
                message: error.to_string(),
            })
    }

    async fn service_hosts(&self, name: &str) -> Result<Vec<ServiceHost>, String> {
        let credential = self.tailscale().await?;
        self.tailnet
            .service_hosts(&credential, name)
            .await
            .map_err(text)
    }

    async fn service_entries(&self, name: &str) -> Result<Vec<ServiceEntryHost>, String> {
        serve_entries::for_service(self.deps.db.as_ref(), name)
            .await
            .map_err(text)
    }

    async fn service_creation(
        &self,
        name: &str,
        creator_run_id: i32,
    ) -> Result<Option<(ServiceDefinition, temps_core::DBDateTime)>, String> {
        let Some(row) = reachability_runs::Entity::find_by_id(creator_run_id)
            .one(self.deps.db.as_ref())
            .await
            .map_err(text)?
        else {
            return Ok(None);
        };
        Ok(record_of(&row)
            .journal
            .into_iter()
            .find_map(|entry| match entry.resource {
                Some(RunResource::TailscaleService {
                    name: created_name,
                    created: true,
                    creator_run_id: run,
                    definition,
                    ..
                }) if created_name == name && run == creator_run_id => {
                    Some((definition, entry.pending_since.unwrap_or(entry.at)))
                }
                _ => None,
            }))
    }

    async fn delete_service(
        &self,
        tailnet: &str,
        name: &str,
        creator_run_id: i32,
        shown: &ServiceDefinition,
    ) -> Result<ServiceDeletion, String> {
        use sea_orm::TransactionTrait;
        let credential = self.tailscale().await?;
        let txn = self.deps.db.begin().await.map_err(text)?;
        // ADR 065 D4: no entry or reference can be added for the Service
        // between these checks and the DELETE.
        serve_entries::lock_service(&txn, tailnet, name)
            .await
            .map_err(text)?;
        let entries = serve_entries::for_service(&txn, name).await.map_err(text)?;
        if let Some(user) = entries.first() {
            return Ok(ServiceDeletion::Kept(format!(
                "`{name}` is used again, by {} on {}",
                user.hostname, user.node_name
            )));
        }
        let hosts = self
            .tailnet
            .service_hosts(&credential, name)
            .await
            .map_err(text)?;
        if !hosts.is_empty() {
            return Ok(ServiceDeletion::Kept(format!(
                "`{name}` is advertised by {}",
                unrecorded_hosts(&hosts, &[], None).join(", ")
            )));
        }
        let current = match self
            .tailnet
            .get_service(&credential, name)
            .await
            .map_err(text)?
        {
            None => return Ok(ServiceDeletion::Gone),
            Some(current) => current,
        };
        // Still this run's, by this install's marker, immediately before
        // the DELETE: never a Service someone else made at the name.
        let install = self.deps.records.instance_id().await.map_err(text)?;
        if let Err(why) =
            crate::reachability::run::still_ours(&current, name, &install, creator_run_id)
        {
            return Ok(ServiceDeletion::Kept(why));
        }
        if let Some(why) = current.differs_from(shown) {
            return Ok(ServiceDeletion::Kept(format!(
                "{why} since you were asked, so Temps did not delete it"
            )));
        }
        self.tailnet
            .delete_service(&credential, name)
            .await
            .map_err(text)?;
        txn.commit().await.map_err(text)?;
        info!(%tailnet, service = %name, "deleted a Tailscale Service Temps created");
        Ok(ServiceDeletion::Deleted)
    }
}

impl LiveOps {
    /// The cluster's Tailscale OAuth client, for a run's API call.
    pub(crate) async fn tailscale(&self) -> Result<TailscaleCredential, String> {
        load_tailscale_credential(&self.deps.connectors)
            .await?
            .map(|(_, _, credential)| credential)
            .ok_or_else(|| {
                "This cluster has no Tailscale OAuth client any more. Add one by making the \
                 address reachable again."
                    .to_string()
            })
    }
}
