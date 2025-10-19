use std::sync::Arc;
use crate::services::service::PerformanceService;

#[derive(Clone)]
pub struct AppState {
    pub performance_service: Arc<PerformanceService>,
    pub route_table: Arc<temps_routes::CachedPeerTable>,
    pub ip_address_service: Arc<temps_geo::IpAddressService>,
}
