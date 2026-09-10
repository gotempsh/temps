// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Edge server orchestration.
//!
//! Starts the Pingora proxy, route sync loop, heartbeat loop, and eviction loop.

use crate::analytics;
use crate::cache::EdgeCache;
use crate::proxy::EdgeProxy;
use crate::route_table::{EdgeRouteTable, EdgeRoutesResponse};
use crate::{CacheStats, EdgeConfig, EdgeError};
use pingora_core::server::configuration::Opt;
use pingora_proxy::ProxyServiceBuilder;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Response from registration with the origin control plane.
#[derive(serde::Deserialize)]
struct RegisterResponse {
    id: i32,
}

/// Register this edge node with the origin control plane.
///
/// Generates an X25519 key pair for ECIES certificate encryption and sends
/// the public key to the origin. The private key is stored in the local config.
pub async fn register_with_origin(config: &mut EdgeConfig) -> Result<(), EdgeError> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| EdgeError::RegistrationFailed(format!("HTTP client error: {}", e)))?;

    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let node_name = if config.node_name.is_empty() {
        &hostname
    } else {
        &config.node_name
    };

    // Generate X25519 key pair for ECIES certificate encryption
    let secret = temps_core::ecies::generate_x25519_static_secret().map_err(|error| {
        EdgeError::RegistrationFailed(format!("X25519 key generation failed: {error}"))
    })?;
    let public = x25519_dalek::PublicKey::from(&secret);
    let private_key_b64 = BASE64.encode(secret.as_bytes());
    let public_key_b64 = BASE64.encode(public.as_bytes());

    let mut labels = serde_json::json!({"role": "edge"});
    if let Some(ref region) = config.region {
        labels["region"] = serde_json::Value::String(region.clone());
    }
    // Store the API address so origin knows where to query analytics
    labels["api_address"] = serde_json::Value::String(config.api_address.clone());

    let body = serde_json::json!({
        "name": node_name,
        "token": config.token,
        "join_token": config.token,
        "address": config.api_address,
        "private_address": config.api_address,
        "role": "edge",
        "labels": labels,
        "edge_public_key": public_key_b64,
    });

    let url = format!("{}/api/internal/nodes/register", config.origin_url);
    info!("Registering edge node with origin at {}", url);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.token))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            EdgeError::RegistrationFailed(format!(
                "Failed to connect to origin {}: {}",
                config.origin_url, e
            ))
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(EdgeError::RegistrationFailed(format!(
            "Origin returned {} — {}",
            status, body_text
        )));
    }

    let register_resp: RegisterResponse = resp.json().await.map_err(|e| {
        EdgeError::RegistrationFailed(format!("Failed to parse registration response: {}", e))
    })?;

    config.node_id = Some(register_resp.id);
    config.edge_private_key = Some(private_key_b64);
    info!(
        "Registered as edge node #{} ({})",
        register_resp.id, node_name
    );

    Ok(())
}

/// Fetch routes from the origin and update the route table and cert store.
async fn sync_routes(
    config: &EdgeConfig,
    table: &EdgeRouteTable,
    cert_store: &crate::tls::EdgeCertStore,
) -> Result<(), EdgeError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| EdgeError::RouteSync(format!("HTTP client error: {}", e)))?;

    let url = format!("{}/api/internal/edge/routes", config.origin_url);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", config.token))
        .send()
        .await
        .map_err(|e| EdgeError::RouteSync(format!("Failed to fetch routes: {}", e)))?;

    if !resp.status().is_success() {
        return Err(EdgeError::RouteSync(format!(
            "Origin returned {}",
            resp.status()
        )));
    }

    let routes: EdgeRoutesResponse = resp
        .json()
        .await
        .map_err(|e| EdgeError::RouteSync(format!("Failed to parse routes: {}", e)))?;

    let count = routes.routes.len();
    let version = routes.version;

    // Decrypt and update TLS certificates if present
    if let (Some(ref certs), Some(ref edge_pk)) = (&routes.certificates, &config.edge_private_key) {
        let updated = cert_store.update_from_sync(certs, edge_pk);
        if updated > 0 {
            info!(
                "Certificate sync: updated {} cert(s), {} total in store",
                updated,
                cert_store.len()
            );
        }
    }

    table.replace(routes);
    debug!("Route sync: {} routes, version {}", count, version);

    Ok(())
}

/// Send a heartbeat to the origin with cache stats.
async fn send_heartbeat(config: &EdgeConfig, cache_stats: CacheStats) {
    let node_id = match config.node_id {
        Some(id) => id,
        None => return,
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to create heartbeat client: {}", e);
            return;
        }
    };

    let url = format!(
        "{}/api/internal/nodes/{}/heartbeat",
        config.origin_url, node_id
    );

    let mut labels = serde_json::json!({"role": "edge"});
    if let Some(ref region) = config.region {
        labels["region"] = serde_json::Value::String(region.clone());
    }
    labels["api_address"] = serde_json::Value::String(config.api_address.clone());

    let body = serde_json::json!({
        "capacity": {
            "cache_hit_rate": cache_stats.hit_rate,
            "cache_disk_usage_bytes": cache_stats.disk_usage_bytes,
            "cache_entry_count": cache_stats.entry_count,
            "cache_hits": cache_stats.hit_count,
            "cache_misses": cache_stats.miss_count,
        },
        "labels": labels,
    });

    match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.token))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            debug!("Heartbeat sent (node #{})", node_id);
        }
        Ok(resp) => {
            warn!("Heartbeat failed: {}", resp.status());
        }
        Err(e) => {
            warn!("Heartbeat error: {}", e);
        }
    }
}

/// Spawn the background route sync loop.
fn spawn_route_sync_loop(
    config: EdgeConfig,
    table: Arc<EdgeRouteTable>,
    cert_store: Arc<crate::tls::EdgeCertStore>,
) {
    let interval = Duration::from_secs(config.route_sync_interval_secs);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = sync_routes(&config, &table, &cert_store).await {
                warn!("Route sync failed: {}", e);
            }
        }
    });
}

/// Spawn the background heartbeat loop.
fn spawn_heartbeat_loop(config: EdgeConfig, cache: Arc<EdgeCache>) {
    tokio::spawn(async move {
        let interval = Duration::from_secs(30);
        loop {
            tokio::time::sleep(interval).await;
            let stats = cache.stats();
            send_heartbeat(&config, stats).await;
        }
    });
}

/// Spawn the background cache eviction loop.
fn spawn_eviction_loop(cache: Arc<EdgeCache>) {
    tokio::spawn(async move {
        let interval = Duration::from_secs(60);
        loop {
            tokio::time::sleep(interval).await;
            cache.evict_if_needed().await;
        }
    });
}

/// Start the edge CDN server. This function blocks.
pub fn start_edge(config: EdgeConfig) -> Result<(), EdgeError> {
    // Initialize cache
    let cache = Arc::new(EdgeCache::new(
        &config.cache_dir,
        config.max_cache_size_mb * 1024 * 1024,
    ));

    // Initialize route table and certificate store
    let route_table = Arc::new(EdgeRouteTable::new());
    let cert_store = Arc::new(crate::tls::EdgeCertStore::new());

    // Create a tokio runtime for async initialization
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| EdgeError::ServerError(format!("Failed to create runtime: {}", e)))?;

    // Initial route sync (must succeed before we start serving)
    rt.block_on(async {
        info!("Performing initial route sync...");
        match sync_routes(&config, &route_table, &cert_store).await {
            Ok(()) => {
                info!(
                    "Initial route sync complete: {} routes loaded, {} certs",
                    route_table.len(),
                    cert_store.len()
                );
            }
            Err(e) => {
                error!("Initial route sync failed: {}", e);
                error!("Starting with empty route table — will retry in background");
            }
        }
    });

    // Initialize analytics pipeline (local SQLite via Sea-ORM)
    let analytics_db_path = config.cache_dir.join("analytics.db");
    let (analytics_handle, analytics_writer, analytics_store) = rt
        .block_on(analytics::create_analytics_pipeline(&analytics_db_path))
        .map_err(|e| EdgeError::ServerError(format!("Failed to open analytics DB: {}", e)))?;

    // Spawn background loops inside the runtime
    let config_clone = config.clone();
    let table_clone = route_table.clone();
    let cache_clone = cache.clone();
    let cache_evict = cache.clone();
    let cert_store_sync = cert_store.clone();
    rt.spawn(async move {
        spawn_route_sync_loop(config_clone.clone(), table_clone, cert_store_sync);
        spawn_heartbeat_loop(config_clone, cache_clone);
        spawn_eviction_loop(cache_evict);
    });

    // Spawn analytics batch writer (channel → SQLite)
    rt.spawn(analytics_writer.run());

    // Spawn edge analytics query API (Axum on separate port)
    let api_router = crate::api::build_router(analytics_store, cache.clone(), &config.token);
    let api_addr = config.api_address.clone();
    rt.spawn(async move {
        info!("Edge analytics API listening on {}", api_addr);
        let listener = match tokio::net::TcpListener::bind(&api_addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to bind edge API to {}: {}", api_addr, e);
                return;
            }
        };
        if let Err(e) = axum::serve(listener, api_router).await {
            error!("Edge API server error: {}", e);
        }
    });

    // Build the Pingora server
    let mut server = pingora::server::Server::new(Some(Opt::default()))
        .map_err(|e| EdgeError::ServerError(format!("Failed to create Pingora server: {}", e)))?;
    server.bootstrap();

    // Build the edge proxy — use proxy_origin_url if set, otherwise origin_url
    let proxy_url = config
        .proxy_origin_url
        .as_deref()
        .unwrap_or(&config.origin_url);
    let edge_proxy = EdgeProxy::new(
        proxy_url,
        &config.token,
        route_table,
        cache,
        analytics_handle,
        config.region.clone(),
    );

    // Create proxy service
    let mut proxy_service = ProxyServiceBuilder::new(&server.configuration, edge_proxy)
        .name("Temps Edge CDN Proxy")
        .build();

    // Add listen address
    let addr = &config.listen_address;
    info!("Edge proxy listening on {}", addr);
    proxy_service.add_tcp(addr);

    if let Some(ref tls_addr) = config.tls_listen_address {
        if cert_store.is_empty() {
            warn!(
                "TLS address {} configured but no certificates loaded yet — \
                 TLS will start serving once certs arrive via route sync",
                tls_addr
            );
        }

        let tls_callbacks: Box<dyn pingora_core::listeners::TlsAccept + Send + Sync> =
            Box::new(crate::tls::EdgeTlsAccept::new(cert_store));
        let mut tls_settings = pingora_core::listeners::tls::TlsSettings::with_callbacks(
            tls_callbacks,
        )
        .map_err(|e| EdgeError::ServerError(format!("Failed to create TLS settings: {}", e)))?;
        tls_settings.enable_h2();

        proxy_service.add_tls_with_settings(tls_addr, None, tls_settings);
        info!("Edge proxy TLS listening on {}", tls_addr);
    }

    server.add_service(proxy_service);

    info!(
        "Edge CDN node started — origin: {}, region: {}",
        config.origin_url,
        config.region.as_deref().unwrap_or("default")
    );

    server.run_forever();
}
