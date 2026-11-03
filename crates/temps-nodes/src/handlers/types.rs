// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use temps_core::AuditLogger;

use crate::services::node_pairing_admin::NodePairingAdminService;
use crate::services::node_public_ingress::NodePublicIngressService;
use crate::services::node_service::NodeService;
use crate::services::{NodeScheduler, ResolverCoverageService};

/// State of the authenticated node administration routes.
pub struct NodeAdminState {
    pub db: Arc<temps_database::DbConnection>,
    pub node_service: Arc<NodeService>,
    pub node_public_ingress_service: Arc<NodePublicIngressService>,
    pub node_scheduler: Arc<NodeScheduler>,
    pub resolver_coverage: Arc<ResolverCoverageService>,
    pub node_pairing_admin: Arc<NodePairingAdminService>,
    pub enrollment_token_service: Arc<temps_config::EnrollmentTokenService>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub config_service: Arc<temps_config::ConfigService>,
    /// Builds and pushes images on the control plane; the node API reports
    /// what it can target.
    pub image_builder: Arc<dyn temps_deployer::ImageBuilder>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Where node metrics are stored, when metrics collection is wired up.
    pub metrics_store: Option<Arc<dyn temps_metrics::MetricsStore>>,
    /// Step-up check for sensitive node operations.
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}
