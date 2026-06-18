use crate::config::*;
use crate::proxy::LoadBalancer;
use crate::service::connection_filter_service::TcpConnectionFilter;
use crate::service::lb_service::LbService;
use crate::services::*;
use crate::tls_cert_loader::CertificateLoader;
use crate::traits::*;
use anyhow::Result;
use pingora::server::RunArgs;
use pingora_core::apps::HttpServerOptions;
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::listeners::TlsAccept;
use pingora_core::protocols::tls::TlsRef;
use pingora_core::server::configuration::Opt;
use pingora_openssl::pkey::PKey;
use pingora_openssl::ssl::NameType;
use pingora_openssl::x509::X509;
use pingora_proxy::ProxyServiceBuilder;
use std::any::Any;
use std::sync::Arc;
use temps_config::ServerConfig;
use temps_core::plugin::{ServiceRegistrationContext, TempsPlugin};
use temps_database::DbConnection;
use temps_routes::CachedPeerTable;
use tracing::{debug, info};

use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

/// TLS extension data stored in SslDigest via the new Pingora 0.8.0 SslDigestExtension.
///
/// This allows us to capture SNI hostname during the TLS handshake and make it
/// available to the HTTP proxy layer without recomputation.
#[derive(Debug, Clone)]
pub struct TlsExtensionData {
    /// SNI hostname captured during TLS handshake
    pub sni_hostname: String,
}

/// Dynamic certificate callback for TLS
struct DynamicCertLoader {
    cert_loader: Arc<CertificateLoader>,
    /// On-demand HTTP-01 TLS manager (ADR-018). When present and the SNI has no
    /// active cert, the callback asks it to provision one in the background and
    /// still returns `Ok(None)` immediately (Option B: fail-fast, do not block
    /// the handshake). `None` preserves the pre-ADR behavior exactly.
    on_demand_cert_manager: Option<Arc<crate::on_demand_cert::OnDemandCertManager>>,
}

#[async_trait]
impl TlsAccept for DynamicCertLoader {
    async fn certificate_callback(&self, ssl_ref: &mut TlsRef) -> () {
        use pingora_openssl::ext;
        use pingora_openssl::ssl::SslRef;

        // TlsRef is a type alias for SslRef when using OpenSSL
        // We need to cast it to access OpenSSL-specific methods
        let ssl: &mut SslRef = unsafe { std::mem::transmute(ssl_ref) };

        // Get SNI hostname from the SSL context and clone it to avoid borrow conflicts
        let sni = ssl
            .servername(NameType::HOST_NAME)
            .unwrap_or("default")
            .to_string();

        debug!("TLS callback for SNI: {}", sni);

        match self.cert_loader.load_certificate(&sni).await {
            Ok(Some((certs, key))) => {
                debug!("Loading {} certificate(s) for {}", certs.len(), sni);

                // Convert rustls certificates to OpenSSL X509
                for (i, cert_der) in certs.iter().enumerate() {
                    match X509::from_der(cert_der.as_ref()) {
                        Ok(cert) => {
                            if i == 0 {
                                // First certificate is the leaf certificate
                                if let Err(e) = ext::ssl_use_certificate(ssl, &cert) {
                                    debug!("Failed to set certificate for {}: {}", sni, e);
                                    return;
                                }
                            } else {
                                // Subsequent certificates are chain certificates
                                if let Err(e) = ext::ssl_add_chain_cert(ssl, &cert) {
                                    debug!(
                                        "Failed to add chain certificate {} for {}: {}",
                                        i, sni, e
                                    );
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            debug!("Failed to parse certificate {} for {}: {}", i, sni, e);
                            return;
                        }
                    }
                }

                // Convert rustls private key to OpenSSL PKey
                match PKey::private_key_from_der(key.secret_der()) {
                    Ok(pkey) => {
                        if let Err(e) = ext::ssl_use_private_key(ssl, &pkey) {
                            debug!("Failed to set private key for {}: {}", sni, e);
                            return;
                        }
                    }
                    Err(e) => {
                        debug!("Failed to parse private key for {}: {}", sni, e);
                        return;
                    }
                }

                debug!("Successfully configured TLS for {}", sni);
            }
            Ok(None) => {
                debug!("No certificate found for SNI: {}", sni);
                // ADR-018 on-demand TLS hook (Option B, §1): trigger background
                // issuance for allowlisted, stable, in-zone hostnames, then fall
                // through and return `Ok(None)` immediately. We never block the
                // handshake here — the first request fails fast; the client's
                // retry succeeds once the cert is `active`. The gate (zone +
                // route + dedup + backoff + rate caps) is O(1) and runs inline.
                // Peer IP isn't readily available in the OpenSSL TLS callback, so
                // the per-IP novelty limiter is passed `None`; the zone, route,
                // dedup, backoff, and global hourly-cap checks remain in force.
                if let Some(ref manager) = self.on_demand_cert_manager {
                    let _ = manager.try_enqueue(&sni, None);
                }
            }
            Err(e) => {
                debug!("Error loading certificate for {}: {}", sni, e);
            }
        }
    }

    /// Store SNI hostname in SslDigestExtension after handshake completes.
    ///
    /// This is a Pingora 0.8.0 feature that allows attaching custom data to the
    /// TLS digest, making it accessible from the HTTP proxy layer via
    /// `session.downstream_session.digest().ssl_digest.extension`.
    async fn handshake_complete_callback(
        &self,
        ssl: &TlsRef,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        use pingora_openssl::ssl::{NameType as SslNameType, SslRef};

        let ssl_ref: &SslRef = unsafe { std::mem::transmute(ssl) };

        let sni = ssl_ref
            .servername(SslNameType::HOST_NAME)
            .unwrap_or("unknown")
            .to_string();

        debug!("TLS handshake complete for SNI: {}", sni);

        Some(Arc::new(TlsExtensionData { sni_hostname: sni }))
    }
}

/// Setup plugin system and register all necessary services for the proxy
async fn setup_proxy_plugins(
    db: Arc<DbConnection>,
    config: Arc<ServerConfig>,
) -> Result<ServiceRegistrationContext> {
    // Create registration context - it will create its own registry
    let context = ServiceRegistrationContext::new();

    // Register core services that plugins depend on
    context.register_service(db.clone());

    // Register ConfigPlugin for configuration services
    let config_plugin = Box::new(temps_config::ConfigPlugin::new(config.clone()));
    config_plugin
        .register_services(&context)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to register ConfigPlugin: {}", e))?;

    // Register GeoPlugin for IP geolocation
    let geo_plugin = temps_geo::GeoPlugin::new();
    geo_plugin
        .register_services(&context)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to register GeoPlugin: {}", e))?;

    debug!("Proxy plugin system initialized");

    Ok(context)
}

/// Custom shutdown signal trait that callers can implement
pub trait ProxyShutdownSignal: Send + Sync {
    /// Wait for the shutdown signal to be triggered
    fn wait_for_signal(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Bridge between our custom trait and Pingora's ShutdownSignalWatch
struct ShutdownSignalBridge {
    signal: Box<dyn ProxyShutdownSignal>,
}

impl ShutdownSignalBridge {
    fn new(signal: Box<dyn ProxyShutdownSignal>) -> Self {
        Self { signal }
    }
}

#[async_trait]
impl pingora::server::ShutdownSignalWatch for ShutdownSignalBridge {
    async fn recv(&self) -> pingora::server::ShutdownSignal {
        self.signal.wait_for_signal().await;
        pingora::server::ShutdownSignal::FastShutdown
    }
}

/// Setup and configure the proxy server with all services
#[allow(clippy::too_many_arguments)]
pub fn setup_proxy_server(
    db: Arc<DbConnection>,
    proxy_config: ProxyConfig,
    crypto: Arc<temps_core::CookieCrypto>,
    encryption_service: Arc<temps_core::EncryptionService>,
    route_table: Arc<CachedPeerTable>,
    shutdown_signal: Box<dyn ProxyShutdownSignal>,
    config: Arc<ServerConfig>,
    on_demand_manager: Option<Arc<crate::on_demand::OnDemandManager>>,
    admin_gate: Option<temps_core::admin_gate::AdminGateHandle>,
) -> Result<()> {
    // Setup plugin system (async operation in sync context)
    let context = tokio::runtime::Runtime::new()?
        .block_on(setup_proxy_plugins(db.clone(), config.clone()))?;

    // Create service implementations
    let lb_service = Arc::new(LbService::new(db.clone()));
    let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
        Arc::new(proxy_config.clone()),
        lb_service,
        route_table.clone(),
    )) as Arc<dyn UpstreamResolver>;

    // Get services from plugin registry
    let ip_service = context.require_service::<temps_geo::IpAddressService>();
    let config_service = context.require_service::<temps_config::ConfigService>();

    // Select the proxy-log storage backend (ClickHouse when TEMPS_CLICKHOUSE_*
    // is configured, else TimescaleDB). The batch writer must use the SAME
    // backend the API read handlers use so writes and reads agree. The
    // ClickHouse client lives only in this background task — never the Pingora
    // hot path.
    let proxy_log_storage =
        crate::storage::build_proxy_log_storage(&config, db.clone(), ip_service.clone());

    // Create batch writer for proxy logs (bounded channel + background batch write)
    let (proxy_log_handle, proxy_log_writer) =
        crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
            db.clone(),
            ip_service.clone(),
            proxy_log_storage,
        );

    // Spawn the batch writer background task
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for proxy log batch writer");
        rt.block_on(proxy_log_writer.run());
    });

    let ip_access_control_service = Arc::new(
        crate::service::ip_access_control_service::IpAccessControlService::new(db.clone()),
    );

    let challenge_service = Arc::new(crate::service::challenge_service::ChallengeService::new(
        db.clone(),
    ));

    let project_context_resolver = Arc::new(ProjectContextResolverImpl::new(route_table.clone()))
        as Arc<dyn ProjectContextResolver>;

    let visitor_manager = Arc::new(VisitorManagerImpl::new(
        db.clone(),
        crypto.clone(),
        ip_service.clone(),
    )) as Arc<dyn VisitorManager>;

    let session_manager =
        Arc::new(SessionManagerImpl::new(db.clone(), crypto.clone())) as Arc<dyn SessionManager>;

    // Create path-keyed file store for static asset serving
    let cas_file_store: Arc<dyn temps_file_store::FileStore> = Arc::new(
        temps_file_store::fs_store::FsFileStore::new(config_service.data_dir().join("cas")),
    );

    // Create the main load balancer
    let mut lb = LoadBalancer::new(
        upstream_resolver,
        proxy_log_handle,
        project_context_resolver,
        visitor_manager,
        session_manager,
        crypto,
        db.clone(),
        config_service,
        ip_access_control_service,
        challenge_service,
        proxy_config.disable_https_redirect,
    );
    if let Some(gate) = admin_gate {
        lb = lb.with_admin_gate(gate);
    }

    // Wire up on-demand scale-to-zero if OnDemandManager was created.
    //
    // The sleeping-domain callback is registered by the caller (serve)
    // BEFORE the route listener's initial load runs, so the first load itself
    // populates sleeping domains and on-demand configs — there is no longer a
    // duplicate `load_routes()` here gating the proxy bind. The idle sweep is
    // likewise started by the caller alongside callback registration.
    if let Some(ref on_demand_manager) = on_demand_manager {
        lb = lb.with_on_demand_manager(Arc::clone(on_demand_manager));
        info!("On-demand scale-to-zero enabled");
    }

    // Wire path-keyed file store for static asset serving
    lb = lb.with_file_store(cas_file_store);
    info!("Path-keyed file store enabled");

    // Setup Pingora server with explicit configuration
    // Disable upgrade mode to avoid "Console API failed to start: channel closed" error
    let opt = Opt {
        upgrade: false,   // Don't try to upgrade from old process
        daemon: false,    // Don't daemonize (systemd handles this)
        nocapture: false, // Not running under cargo test
        test: false,      // Not in test mode
        conf: None,       // No config file path
    };

    let mut server = pingora_core::server::Server::new(Some(opt))?;
    server.bootstrap();

    // Create HTTP proxy service using the builder (Pingora 0.8.0)
    // Limit downstream connection reuse to prevent slow memory leaks from long-lived
    // keep-alive connections. Equivalent to nginx's keepalive_requests directive.
    let mut server_options = HttpServerOptions::default();
    server_options.keepalive_request_limit = Some(1024);

    let mut proxy_service = ProxyServiceBuilder::new(&server.configuration, lb)
        .name("Temps HTTP Proxy Service")
        .server_options(server_options)
        .build();

    proxy_service.add_tcp(&proxy_config.address);
    // Add TLS if configured
    if let Some(ref tls_address) = proxy_config.tls_address {
        debug!("Adding TLS service on {}", tls_address);

        // Create certificate loader for dynamic SNI resolution
        let cert_loader = Arc::new(CertificateLoader::new(
            db.clone(),
            encryption_service.clone(),
        ));

        // Create TLS callback handler. The on-demand cert manager (ADR-018) is
        // taken from the proxy config; `None` keeps the legacy fail-fast-only
        // behavior. tls_cert_loader stays a pure DB lookup — the manager lives
        // only in DynamicCertLoader, never in the loader (ADR §7).
        let tls_callbacks: Box<dyn TlsAccept + Send + Sync> = Box::new(DynamicCertLoader {
            cert_loader,
            on_demand_cert_manager: proxy_config.on_demand_cert_manager.clone(),
        });

        // Create TLS settings with dynamic certificate callback and HTTP/2 support
        let mut tls_settings = TlsSettings::with_callbacks(tls_callbacks)
            .map_err(|e| anyhow::anyhow!("Failed to create TLS settings: {}", e))?;

        // Enable HTTP/2 via ALPN (Application-Layer Protocol Negotiation)
        tls_settings.enable_h2();

        proxy_service.add_tls_with_settings(tls_address, None, tls_settings);
        debug!(
            "TLS listener configured on {} with HTTP/2 support",
            tls_address
        );
    }

    // Set TCP-level connection filter for early IP blocking (Pingora 0.8.0 feature)
    // This rejects blocked IPs at the TCP layer before TLS handshake or HTTP processing,
    // saving significant resources compared to HTTP-layer blocking.
    let tcp_filter = Arc::new(TcpConnectionFilter::new(db.clone()));
    proxy_service.set_connection_filter(tcp_filter);
    debug!("TCP connection filter configured for early IP blocking");

    server.add_service(proxy_service);

    info!("Starting proxy server on {}", proxy_config.address);
    if let Some(ref tls_addr) = proxy_config.tls_address {
        info!("TLS server will listen on {}", tls_addr);
    }

    let run_args = RunArgs {
        shutdown_signal: Box::new(ShutdownSignalBridge::new(shutdown_signal)),
    };
    server.run(run_args);

    Ok(())
}

/// Create a proxy service with the given configuration
pub fn create_proxy_service(
    db: Arc<DbConnection>,
    proxy_config: ProxyConfig,
    crypto: Arc<temps_core::CookieCrypto>,
    route_table: Arc<CachedPeerTable>,
    config: Arc<ServerConfig>,
) -> Result<LoadBalancer> {
    // Setup plugin system (async operation in sync context)
    let context = tokio::runtime::Runtime::new()?
        .block_on(setup_proxy_plugins(db.clone(), config.clone()))?;

    // Create service implementations
    let lb_service = Arc::new(LbService::new(db.clone()));
    let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
        Arc::new(proxy_config.clone()),
        lb_service,
        route_table.clone(),
    )) as Arc<dyn UpstreamResolver>;

    // Get services from plugin registry
    let ip_service = context.require_service::<temps_geo::IpAddressService>();
    let config_service = context.require_service::<temps_config::ConfigService>();

    // Select the proxy-log storage backend (ClickHouse when TEMPS_CLICKHOUSE_*
    // is configured, else TimescaleDB). The batch writer must use the SAME
    // backend the API read handlers use so writes and reads agree. The
    // ClickHouse client lives only in this background task — never the Pingora
    // hot path.
    let proxy_log_storage =
        crate::storage::build_proxy_log_storage(&config, db.clone(), ip_service.clone());

    // Create batch writer for proxy logs (bounded channel + background batch write)
    let (proxy_log_handle, proxy_log_writer) =
        crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
            db.clone(),
            ip_service.clone(),
            proxy_log_storage,
        );

    // Spawn the batch writer background task
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for proxy log batch writer");
        rt.block_on(proxy_log_writer.run());
    });

    let ip_access_control_service = Arc::new(
        crate::service::ip_access_control_service::IpAccessControlService::new(db.clone()),
    );

    let challenge_service = Arc::new(crate::service::challenge_service::ChallengeService::new(
        db.clone(),
    ));

    let project_context_resolver = Arc::new(ProjectContextResolverImpl::new(route_table.clone()))
        as Arc<dyn ProjectContextResolver>;

    let visitor_manager = Arc::new(VisitorManagerImpl::new(
        db.clone(),
        crypto.clone(),
        ip_service.clone(),
    )) as Arc<dyn VisitorManager>;

    let session_manager =
        Arc::new(SessionManagerImpl::new(db.clone(), crypto.clone())) as Arc<dyn SessionManager>;

    // Create the main load balancer
    let lb = LoadBalancer::new(
        upstream_resolver,
        proxy_log_handle,
        project_context_resolver,
        visitor_manager,
        session_manager,
        crypto,
        db,
        config_service,
        ip_access_control_service,
        challenge_service,
        proxy_config.disable_https_redirect,
    );

    Ok(lb)
}
