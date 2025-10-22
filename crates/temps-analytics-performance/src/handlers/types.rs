use crate::services::service::PerformanceService;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub performance_service: Arc<PerformanceService>,
    pub route_table: Arc<temps_routes::CachedPeerTable>,
    pub ip_address_service: Arc<temps_geo::IpAddressService>,
}
