// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_entities::ingress_publications;

use super::{
    normalize_host, Decision, HostnameView, IngressConnectorService, Plan, PlannedConnector,
};
use crate::error::IngressError;

impl Plan {
    pub(super) fn retry_dns_connector(
        &self,
        decision: &Decision,
        publication: Option<&ingress_publications::Model>,
    ) -> Option<&PlannedConnector> {
        let Decision::Publish(id) = decision else {
            return None;
        };
        let connector = self.connector(*id)?;
        (connector.writes_dns
            && connector.model.external_id.is_some()
            && connector.model.secret_encrypted.is_some()
            && publication.is_some_and(|row| row.connector_id == *id && row.state == "error"))
        .then_some(connector)
    }
}

impl IngressConnectorService {
    /// Retry the failed row the operator saw, only while the publication plan
    /// still assigns this host to that connector. A route change needs its own
    /// reconciliation, including any withdrawal of the old record.
    pub async fn retry_hostname_dns(
        &self,
        id: i32,
        hostname: &str,
    ) -> Result<HostnameView, IngressError> {
        let host = normalize_host(hostname)?;
        let unavailable = || IngressError::RetryUnavailable {
            hostname: host.clone(),
            connector_id: id,
        };
        let _guard = self.reconcile_lock.lock().await;
        let _connector = self.connector_locks().lock(id).await;
        let existing = ingress_publications::Entity::find()
            .filter(ingress_publications::Column::Hostname.eq(&host))
            .filter(ingress_publications::Column::ConnectorId.eq(id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(unavailable)?;
        let plan = self.plan().await?;
        let connector = plan
            .retry_dns_connector(&plan.decide(&host), Some(&existing))
            .ok_or_else(unavailable)?;
        let handle = self.handle_of(&connector.model)?.ok_or_else(unavailable)?;
        let outcome = match self.provider_for(&connector.model).await {
            Ok(provider) => {
                self.publish(
                    &connector.model,
                    connector.zone.as_deref(),
                    provider.as_ref(),
                    &handle,
                    &host,
                )
                .await
            }
            Err(error) => Err(error),
        };
        let saved = self
            .save_publication(id, &host, Some(&existing), outcome)
            .await?;
        Ok(plan.view(&host, Some(&saved)))
    }
}
