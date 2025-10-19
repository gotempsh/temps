use std::sync::Arc;
use crate::services::service::SessionReplayService;
use temps_routes::CachedPeerTable;


#[derive(Clone)]
pub struct AppState {
    pub session_replay_service: Arc<SessionReplayService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
	pub route_table: Arc<CachedPeerTable>,
}
