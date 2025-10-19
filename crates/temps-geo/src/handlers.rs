use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_core::{
    problemdetails::{self, Problem},
    ProblemDetails,
};
use utoipa::{OpenApi, ToSchema};

use crate::{GeoIpService, GeoLocation};

#[derive(OpenApi)]
#[openapi(
    paths(
        get_ip_geolocation,
    ),
    components(schemas(
        GeoLocationResponse,
    )),
    tags(
        (name = "geo", description = "Geolocation API endpoints")
    )
)]
pub struct ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub geo_ip_service: Arc<GeoIpService>,
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route("/geo/{ip}", get(get_ip_geolocation))
}

/// Response containing geolocation information for an IP address
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GeoLocationResponse {
    /// IP address that was geolocated
    #[schema(example = "8.8.8.8")]
    pub ip: String,
    /// Country name
    #[schema(example = "United States")]
    pub country: Option<String>,
    /// ISO country code (2 letters)
    #[schema(example = "US")]
    pub country_code: Option<String>,
    /// City name
    #[schema(example = "Mountain View")]
    pub city: Option<String>,
    /// Latitude coordinate
    #[schema(example = 37.386)]
    pub latitude: Option<f64>,
    /// Longitude coordinate
    #[schema(example = -122.0838)]
    pub longitude: Option<f64>,
    /// Region/state name
    #[schema(example = "California")]
    pub region: Option<String>,
    /// Timezone identifier
    #[schema(example = "America/Los_Angeles")]
    pub timezone: Option<String>,
    /// Whether the IP is in the European Union
    #[schema(example = false)]
    pub is_eu: bool,
}

impl From<(String, GeoLocation)> for GeoLocationResponse {
    fn from((ip, location): (String, GeoLocation)) -> Self {
        Self {
            ip,
            country: location.country,
            country_code: location.country_code,
            city: location.city,
            latitude: location.latitude,
            longitude: location.longitude,
            region: location.region,
            timezone: location.timezone,
            is_eu: location.is_eu,
        }
    }
}

/// Get geolocation information for an IP address
#[utoipa::path(
    get,
    path = "/geo/{ip}",
    tag = "geo",
    params(
        ("ip" = String, Path, description = "IP address to geolocate (IPv4 or IPv6)")
    ),
    responses(
        (status = 200, description = "Geolocation information retrieved", body = GeoLocationResponse),
        (status = 400, description = "Invalid IP address", body = ProblemDetails),
        (status = 404, description = "IP address not found in database", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    )
)]
pub async fn get_ip_geolocation(
    State(state): State<Arc<AppState>>,
    Path(ip_str): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    // Parse IP address
    let ip = ip_str.parse().map_err(|_| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid IP Address")
            .with_detail(format!("'{}' is not a valid IP address", ip_str))
    })?;

    // Geolocate the IP
    let location = state
        .geo_ip_service
        .geolocate(ip)
        .await
        .map_err(|e| match e {
            crate::GeoIpError::NotFound(msg) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("IP Not Found")
                .with_detail(msg),
            _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Geolocation Error")
                .with_detail(e.to_string()),
        })?;

    let response = GeoLocationResponse::from((ip_str, location));
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockGeoIpService;

    #[tokio::test]
    async fn test_get_ip_geolocation_success() {
        let geo_service = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let state = Arc::new(AppState {
            geo_ip_service: geo_service,
        });

        let result = get_ip_geolocation(State(state), Path("127.0.0.1".to_string())).await;
        assert!(result.is_ok(), "Should successfully geolocate localhost");
    }

    #[tokio::test]
    async fn test_get_ip_geolocation_invalid_ip() {
        let geo_service = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let state = Arc::new(AppState {
            geo_ip_service: geo_service,
        });

        let result = get_ip_geolocation(State(state), Path("not-an-ip".to_string())).await;
        assert!(result.is_err(), "Should fail with invalid IP");
    }

    #[tokio::test]
    async fn test_geolocation_response_from_location() {
        let location = GeoLocation {
            country: Some("United States".to_string()),
            country_code: Some("US".to_string()),
            city: Some("New York".to_string()),
            latitude: Some(40.7128),
            longitude: Some(-74.0060),
            region: Some("New York".to_string()),
            timezone: Some("America/New_York".to_string()),
            is_eu: false,
        };

        let response = GeoLocationResponse::from(("8.8.8.8".to_string(), location));

        assert_eq!(response.ip, "8.8.8.8");
        assert_eq!(response.country, Some("United States".to_string()));
        assert_eq!(response.country_code, Some("US".to_string()));
        assert_eq!(response.city, Some("New York".to_string()));
        assert_eq!(response.latitude, Some(40.7128));
        assert_eq!(response.longitude, Some(-74.0060));
        assert!(!response.is_eu);
    }
}
