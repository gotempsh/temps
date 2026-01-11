/*!
# Temps Infrastructure Crate

This crate provides infrastructure-related services and HTTP endpoints for the Temps platform.
It includes platform detection, network diagnostics, and service access information.

## Features

- **Platform Detection**: Detect OS, architecture, and supported platforms
- **Network Information**: Public and private IP address discovery
- **Access Mode Detection**: Determine how the service is being accessed (local, direct, NAT, Cloudflare tunnel)
- **HTTP Routes**: Ready-to-use Axum routes for infrastructure endpoints

## Usage

### Basic Service Usage

```rust,no_run
use temps_infra::{PlatformInfoService, DnsService};
use bollard::Docker;

# async fn example() -> anyhow::Result<()> {
let docker = Docker::connect_with_local_defaults()?;
let platform_service = PlatformInfoService::new(docker);

let platform_info = platform_service.get_platform_info().await?;
println!("Platform: {}", platform_info.platforms[0]);

let dns_service = DnsService::new();
let dns_result = dns_service.lookup_a_records("example.com").await?;
println!("A records: {:?}", dns_result.records);
# Ok(())
# }
```

### HTTP Routes Integration

```rust,no_run
use temps_infra::{
    routes::configure_routes,
    routes::{InfraAppState, DnsAppState},
    PlatformInfoService,
    DnsService
};
use axum::Router;
use std::sync::Arc;
use bollard::Docker;

#[derive(Clone)]
struct AppState {
    platform_service: PlatformInfoService,
    dns_service: DnsService,
}

impl InfraAppState for AppState {
    fn platform_info_service(&self) -> &PlatformInfoService {
        &self.platform_service
    }
}

impl DnsAppState for AppState {
    fn dns_service(&self) -> &DnsService {
        &self.dns_service
    }
}

# async fn example() -> anyhow::Result<()> {
let docker = Docker::connect_with_local_defaults()?;

let app_state = Arc::new(AppState {
    platform_service: PlatformInfoService::new(docker),
    dns_service: DnsService::new(),
});

let router: Router = Router::new()
    .merge(configure_routes::<AppState>())
    .with_state(app_state);
# Ok(())
# }
```

## API Endpoints

- `GET /.well-known/temps.json` - Platform compatibility information
- `GET /platform/public-ip` - Server's public IP address
- `GET /platform/private-ip` - Server's private IP addresses
- `GET /platform/access-info` - Service access mode and configuration
- `GET /dns/lookup?domain=<domain>` - DNS A record lookup for a domain
*/

pub mod plugin;
pub mod routes;
pub mod services;
pub mod types;

// Re-export commonly used types and services
pub use plugin::{InfraPlugin, InfraState};
pub use routes::{configure_routes, DnsApiDoc, DnsAppState, InfraAppState, PlatformInfoApiDoc};
pub use services::{DnsService, PlatformInfoService};
pub use types::{
    DnsLookupError, DnsLookupRequest, DnsLookupResponse, NetworkInterface, PlatformInfo,
    PrivateIpInfo, PublicIpInfo, ServerMode, ServiceAccessInfo,
};
