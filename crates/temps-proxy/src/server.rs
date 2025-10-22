use crate::config::*;
use crate::proxy::LoadBalancer;
use crate::service::lb_service::LbService;
use crate::services::*;
use crate::tls_cert_loader::CertificateLoader;
use crate::traits::*;
use anyhow::Result;
use pingora::server::RunArgs;
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::listeners::TlsAccept;
use pingora_core::protocols::tls::TlsRef;
use pingora_core::server::configuration::Opt;
use pingora_openssl::pkey::PKey;
use pingora_openssl::ssl::NameType;
use pingora_openssl::x509::X509;
use pingora_proxy::http_proxy_service;
use std::sync::Arc;
use temps_core::plugin::{ServiceRegistrationContext, TempsPlugin};
use temps_database::DbConnection;
use temps_routes::CachedPeerTable;
use tracing::{debug, info};

use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

/// Dynamic certificate callback for TLS
struct DynamicCertLoader {
    cert_loader: Arc<CertificateLoader>,
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
            }
            Err(e) => {
                debug!("Error loading certificate for {}: {}", sni, e);
            }
        }
    }
}

/// Setup plugin system and register all necessary services for the proxy
async fn setup_proxy_plugins(db: Arc<DbConnection>) -> Result<ServiceRegistrationContext> {
    // Create registration context - it will create its own registry
    let context = ServiceRegistrationContext::new();

    // Register core services that plugins depend on
    context.register_service(db.clone());

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
pub fn setup_proxy_server(
    db: Arc<DbConnection>,
    proxy_config: ProxyConfig,
    crypto: Arc<temps_core::CookieCrypto>,
    encryption_service: Arc<temps_core::EncryptionService>,
    route_table: Arc<CachedPeerTable>,
    shutdown_signal: Box<dyn ProxyShutdownSignal>,
) -> Result<()> {
    // Setup plugin system (async operation in sync context)
    let context = tokio::runtime::Runtime::new()?.block_on(setup_proxy_plugins(db.clone()))?;

    // Create service implementations
    let lb_service = Arc::new(LbService::new(db.clone()));
    let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
        Arc::new(proxy_config.clone()),
        lb_service,
        route_table.clone(),
    )) as Arc<dyn UpstreamResolver>;

    // Get IP service from plugin registry
    let ip_service = context.require_service::<temps_geo::IpAddressService>();

    let request_logger = Arc::new(RequestLoggerImpl::new(
        LoggingConfig::default(),
        db.clone(),
        ip_service.clone(),
    )) as Arc<dyn RequestLogger>;

    let proxy_log_service = Arc::new(crate::service::proxy_log_service::ProxyLogService::new(
        db.clone(),
        ip_service.clone(),
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

    // Clone db for TLS certificate loader before moving into LoadBalancer

    // Create the main load balancer
    let lb = LoadBalancer::new(
        upstream_resolver,
        request_logger,
        proxy_log_service,
        project_context_resolver,
        visitor_manager,
        session_manager,
        crypto,
        db.clone(),
    );

    // Setup Pingora server
    let opt = Opt {
        daemon: false,
        ..Default::default()
    };

    let mut server = pingora_core::server::Server::new(opt)?;
    server.bootstrap();

    // Create HTTP proxy service
    let mut proxy_service = http_proxy_service(&server.configuration, lb);
    proxy_service.add_tcp(&proxy_config.address);
    // Add TLS if configured
    if let Some(ref tls_address) = proxy_config.tls_address {
        debug!("Adding TLS service on {}", tls_address);

        // Create certificate loader for dynamic SNI resolution
        let cert_loader = Arc::new(CertificateLoader::new(
            db.clone(),
            encryption_service.clone(),
        ));

        // Create TLS callback handler
        let tls_callbacks: Box<dyn TlsAccept + Send + Sync> =
            Box::new(DynamicCertLoader { cert_loader });

        // Create TLS settings with dynamic certificate callback
        let tls_settings = TlsSettings::with_callbacks(tls_callbacks)
            .map_err(|e| anyhow::anyhow!("Failed to create TLS settings: {}", e))?;

        proxy_service.add_tls_with_settings(tls_address, None, tls_settings);
        debug!("TLS listener configured on {}", tls_address);
    }

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
) -> Result<LoadBalancer> {
    // Setup plugin system (async operation in sync context)
    let context = tokio::runtime::Runtime::new()?.block_on(setup_proxy_plugins(db.clone()))?;

    // Create service implementations
    let lb_service = Arc::new(LbService::new(db.clone()));
    let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
        Arc::new(proxy_config.clone()),
        lb_service,
        route_table.clone(),
    )) as Arc<dyn UpstreamResolver>;

    // Get IP service from plugin registry
    let ip_service = context.require_service::<temps_geo::IpAddressService>();

    let request_logger = Arc::new(RequestLoggerImpl::new(
        LoggingConfig::default(),
        db.clone(),
        ip_service.clone(),
    )) as Arc<dyn RequestLogger>;

    let proxy_log_service = Arc::new(crate::service::proxy_log_service::ProxyLogService::new(
        db.clone(),
        ip_service.clone(),
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
        request_logger,
        proxy_log_service,
        project_context_resolver,
        visitor_manager,
        session_manager,
        crypto,
        db,
    );

    Ok(lb)
}
