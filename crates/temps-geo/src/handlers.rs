// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem},
    ProblemDetails,
};
use utoipa::{OpenApi, ToSchema};

use crate::settings_service::GeoSettingsService;
use crate::{GeoIpError, GeoIpService, GeoLocation};
use temps_core::GeoSettings;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_ip_geolocation,
        get_geo_database_status,
    ),
    components(schemas(
        GeoLocationResponse,
        GeoDatabaseStatusResponse,
    )),
    tags(
        (name = "geo", description = "Geolocation API endpoints")
    )
)]
pub struct ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub geo_ip_service: Arc<GeoIpService>,
    /// Reads `AppSettings::geo`, so the status endpoint reports the admin's
    /// current policy and the refresh job's recorded metadata from one place.
    /// It can say whether a MaxMind license key is configured; it never
    /// returns the key.
    pub geo_settings_service: Arc<GeoSettingsService>,
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/geo/status", get(get_geo_database_status))
        .route("/geo/{ip}", get(get_ip_geolocation))
}

impl From<GeoIpError> for Problem {
    fn from(error: GeoIpError) -> Self {
        match error {
            GeoIpError::NotFound(ref message) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("IP Not Found")
                .with_detail(message.clone()),
            GeoIpError::Settings(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Geolocation Settings Unavailable")
                .with_detail(error.to_string()),
            GeoIpError::LicenseKey(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("MaxMind License Key Error")
                .with_detail(error.to_string()),
            GeoIpError::DatabaseError(_)
            | GeoIpError::IoError(_)
            | GeoIpError::Other(_)
            | GeoIpError::InvalidSourceUrl { .. }
            | GeoIpError::DownloadFailed { .. }
            | GeoIpError::ArchiveInvalid { .. }
            | GeoIpError::InvalidDatabase { .. }
            | GeoIpError::WriteFailed { .. }
            | GeoIpError::ReloadFailed { .. }
            | GeoIpError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Geolocation Error")
                .with_detail(error.to_string()),
        }
    }
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
        (status = 401, description = "Authentication required", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "IP address not found in database", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_ip_geolocation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(ip_str): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);

    // Parse IP address
    let ip = ip_str.parse().map_err(|_| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid IP Address")
            .with_detail(format!("'{}' is not a valid IP address", ip_str))
    })?;

    // Geolocate the IP
    let location = state.geo_ip_service.geolocate(ip).await?;

    let response = GeoLocationResponse::from((ip_str, location));
    Ok(Json(response))
}

/// Freshness of the geolocation database backing this instance
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GeoDatabaseStatusResponse {
    /// Where the loaded database was downloaded from: `maxmind_official` when
    /// a license key is configured, `bundled_github` otherwise. `null` when no
    /// refresh has run yet (the database was provisioned by the operator).
    #[schema(example = "maxmind_official")]
    pub source: Option<String>,
    /// Whether a MaxMind license key is configured. The key itself is never
    /// returned, logged, or persisted.
    #[schema(example = true)]
    pub license_key_configured: bool,
    /// When new database bytes were last installed (ISO 8601, UTC)
    #[schema(example = "2026-09-01T03:14:00Z")]
    pub last_refreshed_at: Option<String>,
    /// MaxMind `build_epoch` of the loaded database (Unix seconds)
    #[schema(example = 1_767_225_600i64)]
    pub build_epoch: Option<i64>,
    /// When MaxMind built the loaded data (ISO 8601, UTC)
    #[schema(example = "2026-09-01T02:00:00Z")]
    pub build_time: Option<String>,
    /// Age in days of the loaded data, from `build_time` when known and from
    /// `last_refreshed_at` otherwise
    #[schema(example = 3i64)]
    pub age_days: Option<i64>,
    /// `ok` or `error` for the most recent refresh attempt; `null` when none
    /// has run yet
    #[schema(example = "ok")]
    pub last_check_status: Option<String>,
    /// When a refresh was last attempted, successful or not (ISO 8601, UTC)
    #[schema(example = "2026-09-17T03:14:00Z")]
    pub last_check_at: Option<String>,
    /// Redacted reason the last refresh failed, so an operator can act on it
    /// without reading server logs
    #[schema(example = "Geo database download from source 'maxmind_official' failed: HTTP 401")]
    pub last_error: Option<String>,
    /// Age after which this instance treats the database, and cached IP
    /// lookups, as stale
    #[schema(example = 30i64)]
    pub stale_after_days: i64,
    /// Whether `age_days` has passed `stale_after_days`, or the last check
    /// failed
    #[schema(example = false)]
    pub is_stale: bool,
    /// How often the scheduled refresh job runs
    #[schema(example = 24i64)]
    pub refresh_interval_hours: u64,
}

/// Render a timestamp the way every Temps API does: ISO 8601, UTC, `Z` suffix.
pub fn to_iso8601(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl GeoDatabaseStatusResponse {
    /// Merge the persisted metadata with what the process actually has loaded.
    ///
    /// `live_build_epoch` wins over the stored value because it describes the
    /// database answering lookups right now -- they differ when an operator
    /// dropped a file in by hand, which is exactly the case where a stored-only
    /// answer would mislead.
    fn build(settings: GeoSettings, live_build_epoch: Option<u64>) -> Self {
        let effective = GeoSettings {
            build_epoch: live_build_epoch.or(settings.build_epoch),
            ..settings
        };

        let stale_after_days = i64::from(effective.effective_stale_lookup_days());
        let age_days = effective.age_days(chrono::Utc::now());
        let is_stale =
            effective.last_check_failed() || age_days.is_some_and(|days| days > stale_after_days);

        Self {
            source: effective.source.clone(),
            license_key_configured: effective.license_key_configured(),
            last_refreshed_at: effective.last_refreshed_at.map(to_iso8601),
            build_epoch: effective
                .build_epoch
                .and_then(|epoch| i64::try_from(epoch).ok()),
            build_time: effective.build_time().map(to_iso8601),
            age_days,
            last_check_status: effective.last_check_status.clone(),
            last_check_at: effective.last_check_at.map(to_iso8601),
            last_error: effective.last_error.clone(),
            stale_after_days,
            is_stale,
            refresh_interval_hours: u64::from(effective.effective_refresh_interval_hours()),
        }
    }
}

/// Get the freshness of the geolocation database
#[utoipa::path(
    get,
    path = "/geo/status",
    tag = "geo",
    responses(
        (status = 200, description = "Geolocation database status retrieved", body = GeoDatabaseStatusResponse),
        (status = 401, description = "Authentication required", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_geo_database_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    // `SettingsRead`, not `AnalyticsRead`: this is operator/infrastructure
    // state (whether a license key is configured, the refresh cadence, and a
    // `last_error` that can embed an absolute server path from a write
    // failure), and `GET /settings` already returns the same fields under this
    // permission. `AnalyticsRead` is held by Reader/ApiReader roles and by
    // deployment tokens, which have no business reading it. The `/geo/{ip}`
    // lookup below stays on `AnalyticsRead` -- that one really is analytics.
    permission_guard!(auth, SettingsRead);

    let settings = state.geo_settings_service.load().await?;
    let response = GeoDatabaseStatusResponse::build(settings, state.geo_ip_service.build_epoch());

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockGeoIpService;
    use temps_core::{GEO_CHECK_STATUS_ERROR, GEO_CHECK_STATUS_OK, GEO_SOURCE_MAXMIND_OFFICIAL};

    fn test_state() -> Arc<AppState> {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                .into_connection(),
        );
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .expect("valid test server config"),
        );
        Arc::new(AppState {
            geo_ip_service: Arc::new(GeoIpService::Mock(MockGeoIpService::new())),
            geo_settings_service: Arc::new(GeoSettingsService::new(
                Arc::new(temps_config::ConfigService::new(server_config, db)),
                Arc::new(
                    temps_core::EncryptionService::new(&"c".repeat(64))
                        .expect("encryption service"),
                ),
            )),
        })
    }

    fn test_auth_context() -> temps_auth::AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: Some("hashed".to_string()),
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        temps_auth::AuthContext::new_session(user, temps_auth::Role::Admin)
    }

    /// `/geo/status` sits under the same prefix as the `/geo/{ip}` wildcard.
    /// Router construction is where axum rejects an ambiguous pair, so this
    /// would otherwise only surface as a panic at server startup.
    #[test]
    fn routes_build_with_the_static_status_path_alongside_the_ip_wildcard() {
        let _router: Router<()> = configure_routes().with_state(test_state());
    }

    #[tokio::test]
    async fn test_get_ip_geolocation_success() {
        let result = get_ip_geolocation(
            RequireAuth(test_auth_context()),
            State(test_state()),
            Path("127.0.0.1".to_string()),
        )
        .await;
        assert!(result.is_ok(), "Should successfully geolocate localhost");
    }

    #[tokio::test]
    async fn test_get_ip_geolocation_invalid_ip() {
        let result = get_ip_geolocation(
            RequireAuth(test_auth_context()),
            State(test_state()),
            Path("not-an-ip".to_string()),
        )
        .await;
        assert!(result.is_err(), "Should fail with invalid IP");
    }

    #[tokio::test]
    async fn test_geo_status_reports_never_checked_instead_of_failing() {
        let result =
            get_geo_database_status(RequireAuth(test_auth_context()), State(test_state())).await;
        assert!(
            result.is_ok(),
            "An instance that has never refreshed must still report a status"
        );
    }

    fn epoch_days_ago(days: i64) -> Option<u64> {
        u64::try_from((chrono::Utc::now() - chrono::Duration::days(days)).timestamp()).ok()
    }

    #[test]
    fn status_response_marks_an_old_database_stale() {
        let settings = GeoSettings {
            // Ciphertext presence -- not the key -- is what the response
            // reports, so a placeholder blob is the right fixture here.
            maxmind_license_key_encrypted: Some("ciphertext".to_string()),
            refresh_interval_hours: Some(24),
            stale_lookup_days: Some(30),
            source: Some(GEO_SOURCE_MAXMIND_OFFICIAL.to_string()),
            build_epoch: epoch_days_ago(90),
            last_check_status: Some(GEO_CHECK_STATUS_OK.to_string()),
            ..GeoSettings::default()
        };

        let response = GeoDatabaseStatusResponse::build(settings, None);

        assert!(response.license_key_configured);
        assert_eq!(response.source.as_deref(), Some("maxmind_official"));
        assert_eq!(response.age_days, Some(90));
        assert_eq!(response.stale_after_days, 30);
        assert!(response.is_stale);
        assert_eq!(response.refresh_interval_hours, 24);
    }

    /// The response is built from the same struct that holds the ciphertext,
    /// so this pins the guarantee that neither form of the key escapes.
    #[test]
    fn status_response_never_serializes_the_license_key() {
        let settings = GeoSettings {
            maxmind_license_key: Some("plaintext-key".to_string()),
            maxmind_license_key_encrypted: Some("ciphertext-blob".to_string()),
            ..GeoSettings::default()
        };

        let response = GeoDatabaseStatusResponse::build(settings, None);
        let rendered = serde_json::to_string(&response).expect("render the status response");

        assert!(!rendered.contains("plaintext-key"));
        assert!(!rendered.contains("ciphertext-blob"));
        assert!(rendered.contains("\"license_key_configured\":true"));
    }

    #[test]
    fn status_response_is_stale_when_the_last_check_failed() {
        let settings = GeoSettings {
            build_epoch: epoch_days_ago(0),
            last_check_status: Some(GEO_CHECK_STATUS_ERROR.to_string()),
            last_error: Some("HTTP 401".to_string()),
            ..GeoSettings::default()
        };

        let response = GeoDatabaseStatusResponse::build(settings, None);

        assert!(!response.license_key_configured);
        assert_eq!(response.age_days, Some(0));
        assert!(response.is_stale);
        assert_eq!(response.last_error.as_deref(), Some("HTTP 401"));
    }

    #[test]
    fn status_response_prefers_the_loaded_databases_build_epoch() {
        let live_epoch = epoch_days_ago(1).expect("positive epoch");
        let settings = GeoSettings {
            build_epoch: epoch_days_ago(400),
            ..GeoSettings::default()
        };

        let response = GeoDatabaseStatusResponse::build(settings, Some(live_epoch));

        assert_eq!(response.build_epoch, i64::try_from(live_epoch).ok());
        assert_eq!(response.age_days, Some(1));
        assert!(!response.is_stale);
    }

    /// A tick that deliberately downloaded nothing (no license key) is not a
    /// failure, and the status has to carry the reason so the console, the CLI
    /// and `temps doctor` can explain *why* nothing is refreshing.
    #[test]
    fn status_response_reports_a_skipped_check_as_a_reason_not_an_error() {
        let settings = GeoSettings {
            build_epoch: epoch_days_ago(2),
            last_check_at: Some(chrono::Utc::now()),
            last_check_status: Some(
                temps_core::GEO_CHECK_STATUS_SKIPPED_NO_LICENSE_KEY.to_string(),
            ),
            ..GeoSettings::default()
        };

        let response = GeoDatabaseStatusResponse::build(settings, None);

        assert!(!response.license_key_configured);
        assert_eq!(
            response.last_check_status.as_deref(),
            Some(temps_core::GEO_CHECK_STATUS_SKIPPED_NO_LICENSE_KEY)
        );
        assert_eq!(response.last_error, None);
        assert!(
            !response.is_stale,
            "a recent database is not stale just because refreshes are unlicensed"
        );
    }

    #[test]
    fn status_response_reports_nothing_known_without_metadata() {
        let response = GeoDatabaseStatusResponse::build(GeoSettings::default(), None);

        assert_eq!(response.source, None);
        assert_eq!(response.age_days, None);
        assert_eq!(response.last_check_status, None);
        assert!(!response.is_stale);
        // The effective defaults, not zeros, so the UI never shows "refreshes
        // every 0 hours" on an instance that has not been configured.
        assert_eq!(
            response.stale_after_days,
            i64::from(temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS)
        );
        assert_eq!(
            response.refresh_interval_hours,
            u64::from(temps_core::DEFAULT_GEO_REFRESH_INTERVAL_HOURS)
        );
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
            asn_org: None,
            is_hosting_provider: None,
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
