use tracing::{info, debug};
use maxminddb::geoip2;
use rand::seq::SliceRandom;
use serde::Serialize;
use std::net::IpAddr;
use thiserror::Error;

#[derive(Debug, Serialize, Clone)]
pub struct GeoLocation {
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub city: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub region: Option<String>,
    pub timezone: Option<String>,
    pub is_eu: bool,
}

#[derive(Error, Debug)]
pub enum GeoIpError {
    #[error("Failed to open MaxMind database: {0}")]
    DatabaseError(#[from] maxminddb::MaxMindDBError),
    #[error("IP address not found in database")]
    NotFound(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Other error: {0}")]
    Other(String),
}

/// Sample cities for mock geolocation data
#[derive(Clone)]
struct MockCity {
    city: &'static str,
    region: &'static str,
    country: &'static str,
    country_code: &'static str,
    latitude: f64,
    longitude: f64,
    timezone: &'static str,
    is_eu: bool,
}

const MOCK_CITIES: &[MockCity] = &[
    MockCity {
        city: "New York",
        region: "New York",
        country: "United States",
        country_code: "US",
        latitude: 40.7128,
        longitude: -74.0060,
        timezone: "America/New_York",
        is_eu: false,
    },
    MockCity {
        city: "London",
        region: "England",
        country: "United Kingdom",
        country_code: "GB",
        latitude: 51.5074,
        longitude: -0.1278,
        timezone: "Europe/London",
        is_eu: false,
    },
    MockCity {
        city: "Paris",
        region: "Île-de-France",
        country: "France",
        country_code: "FR",
        latitude: 48.8566,
        longitude: 2.3522,
        timezone: "Europe/Paris",
        is_eu: true,
    },
    MockCity {
        city: "Tokyo",
        region: "Tokyo",
        country: "Japan",
        country_code: "JP",
        latitude: 35.6762,
        longitude: 139.6503,
        timezone: "Asia/Tokyo",
        is_eu: false,
    },
    MockCity {
        city: "Sydney",
        region: "New South Wales",
        country: "Australia",
        country_code: "AU",
        latitude: -33.8688,
        longitude: 151.2093,
        timezone: "Australia/Sydney",
        is_eu: false,
    },
    MockCity {
        city: "Berlin",
        region: "Berlin",
        country: "Germany",
        country_code: "DE",
        latitude: 52.5200,
        longitude: 13.4050,
        timezone: "Europe/Berlin",
        is_eu: true,
    },
    MockCity {
        city: "Toronto",
        region: "Ontario",
        country: "Canada",
        country_code: "CA",
        latitude: 43.6532,
        longitude: -79.3832,
        timezone: "America/Toronto",
        is_eu: false,
    },
    MockCity {
        city: "Singapore",
        region: "Singapore",
        country: "Singapore",
        country_code: "SG",
        latitude: 1.3521,
        longitude: 103.8198,
        timezone: "Asia/Singapore",
        is_eu: false,
    },
];

pub enum GeoIpService {
    MaxMind(MaxMindGeoIpService),
    Mock(MockGeoIpService),
}

impl GeoIpService {
    pub fn new() -> Result<Self, GeoIpError> {
        // Check if we should use mock service for local development
        let use_mock = std::env::var("TEMPS_GEO_MOCK")
            .unwrap_or_else(|_| "false".to_string())
            .to_lowercase() == "true";

        if use_mock {
            info!("Using mock GeoIP service for local development");
            Ok(Self::Mock(MockGeoIpService::new()))
        } else {
            let db_path = std::env::current_dir()?.join("GeoLite2-City.mmdb");
            debug!("Loading MaxMind database from: {:?}", db_path);
            let reader = maxminddb::Reader::open_readfile(db_path)?;
            Ok(Self::MaxMind(MaxMindGeoIpService { reader }))
        }
    }

    pub async fn geolocate(&self, ip: IpAddr) -> Result<GeoLocation, GeoIpError> {
        match self {
            Self::MaxMind(service) => service.geolocate(ip).await,
            Self::Mock(service) => service.geolocate(ip).await,
        }
    }
}

pub struct MaxMindGeoIpService {
    reader: maxminddb::Reader<Vec<u8>>,
}

impl MaxMindGeoIpService {
    pub async fn geolocate(&self, ip: IpAddr) -> Result<GeoLocation, GeoIpError> {
        info!("Geolocating IP: {}", ip);
        let city_result: Result<geoip2::City, maxminddb::MaxMindDBError> = self.reader.lookup(ip);

        match city_result {
            Ok(city) => {
                let country = city
                    .country
                    .as_ref()
                    .and_then(|c| c.names.as_ref())
                    .and_then(|n| n.get("en").cloned())
                    .map(|s| s.to_string());

                let country_code = city
                    .country
                    .as_ref()
                    .and_then(|c| c.iso_code.clone())
                    .map(|s| s.to_string());

                let city_name = city
                    .city
                    .as_ref()
                    .and_then(|c| c.names.as_ref())
                    .and_then(|n| n.get("en").cloned())
                    .map(|s| s.to_string());

                let region = city
                    .subdivisions
                    .as_ref()
                    .and_then(|subs| subs.first())
                    .and_then(|sub| sub.names.as_ref())
                    .and_then(|n| n.get("en").cloned())
                    .map(|s| s.to_string());

                let location = city.location.as_ref();
                let latitude = location.and_then(|l| l.latitude);
                let longitude = location.and_then(|l| l.longitude);
                let timezone = location.and_then(|l| l.time_zone.clone()).map(|s| s.to_string());

                let is_eu = city
                    .country
                    .as_ref()
                    .and_then(|c| c.is_in_european_union)
                    .unwrap_or(false);

                Ok(GeoLocation {
                    country,
                    country_code,
                    city: city_name,
                    latitude,
                    longitude,
                    region,
                    timezone,
                    is_eu,
                })
            }
            Err(e) => Err(GeoIpError::NotFound(format!("IP lookup failed: {}", e))),
        }
    }
}

/// Mock GeoIP service for local development
pub struct MockGeoIpService;

impl MockGeoIpService {
    pub fn new() -> Self {
        Self
    }

    pub async fn geolocate(&self, ip: IpAddr) -> Result<GeoLocation, GeoIpError> {
        // For localhost/private IPs, return random city data
        if ip.is_loopback() || Self::is_private_ip(&ip) {
            info!("Mock geolocating IP: {} (localhost/private)", ip);
            let mut rng = rand::thread_rng();
            let mock_city = MOCK_CITIES.choose(&mut rng)
                .ok_or_else(|| GeoIpError::Other("Failed to select mock city".to_string()))?;

            return Ok(GeoLocation {
                country: Some(mock_city.country.to_string()),
                country_code: Some(mock_city.country_code.to_string()),
                city: Some(mock_city.city.to_string()),
                latitude: Some(mock_city.latitude),
                longitude: Some(mock_city.longitude),
                region: Some(mock_city.region.to_string()),
                timezone: Some(mock_city.timezone.to_string()),
                is_eu: mock_city.is_eu,
            });
        }

        // For other IPs, return a generic response
        info!("Mock geolocating IP: {} (external)", ip);
        Ok(GeoLocation {
            country: Some("Unknown".to_string()),
            country_code: Some("XX".to_string()),
            city: Some("Unknown".to_string()),
            latitude: Some(0.0),
            longitude: Some(0.0),
            region: Some("Unknown".to_string()),
            timezone: Some("UTC".to_string()),
            is_eu: false,
        })
    }

    fn is_private_ip(ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => {
                ipv4.is_private() || ipv4.is_link_local()
            }
            IpAddr::V6(ipv6) => {
                ipv6.is_loopback() || ipv6.is_unique_local()
            }
        }
    }
}