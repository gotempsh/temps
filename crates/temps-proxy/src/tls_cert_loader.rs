use anyhow::Result;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_database::DbConnection;
use std::io::BufReader;
use std::sync::Arc;
use temps_entities::domains;
use tracing::{debug, warn};

/// Certificate loader that fetches certificates from the database
pub struct CertificateLoader {
    db: Arc<DbConnection>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl CertificateLoader {
    pub fn new(db: Arc<DbConnection>, encryption_service: Arc<temps_core::EncryptionService>) -> Self {
        Self { db, encryption_service }
    }

    /// Load certificate for a given SNI hostname
    /// Supports both exact matches and wildcard certificates
    pub async fn load_certificate(
        &self,
        sni: &str,
    ) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>> {
        debug!("Loading certificate for SNI: {}", sni);

        // Try exact match first
        if let Some(cert_data) = self.find_certificate(sni).await? {
            return Ok(Some(cert_data));
        }

        // Try wildcard match
        if let Some(wildcard_domain) = self.get_wildcard_domain(sni) {
            debug!("Trying wildcard certificate for: {}", wildcard_domain);
            if let Some(cert_data) = self.find_certificate(&wildcard_domain).await? {
                return Ok(Some(cert_data));
            }
        }

        warn!("No certificate found for SNI: {}", sni);
        Ok(None)
    }

    /// Find certificate in database by domain
    async fn find_certificate(
        &self,
        domain: &str,
    ) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>> {
        let domain_entity = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain))
            .filter(domains::Column::Status.eq("active"))
            .one(self.db.as_ref())
            .await?;

        if let Some(domain) = domain_entity {
            // Check if we have both certificate and private key
            if let (Some(cert_pem), Some(encrypted_key_pem)) = (domain.certificate, domain.private_key) {
                debug!("Found certificate for domain: {}", domain.domain);

                // Decrypt the private key
                let key_pem = self.encryption_service
                    .decrypt_string(&encrypted_key_pem)
                    .map_err(|e| anyhow::anyhow!("Failed to decrypt private key for domain {}: {}", domain.domain, e))?;

                // Parse certificates
                let certs = self.parse_certificates(cert_pem.as_bytes())?;
                let key = self.parse_private_key(key_pem.as_bytes())?;

                return Ok(Some((certs, key)));
            } else {
                warn!("Domain {} found but missing certificate or key", domain.domain);
            }
        }

        Ok(None)
    }

    /// Get wildcard domain from subdomain (e.g., "api.example.com" -> "*.example.com")
    fn get_wildcard_domain(&self, domain: &str) -> Option<String> {
        let parts: Vec<&str> = domain.split('.').collect();
        if parts.len() >= 2 {
            let base_domain = parts[1..].join(".");
            Some(format!("*.{}", base_domain))
        } else {
            None
        }
    }

    /// Parse PEM certificates into rustls format
    fn parse_certificates(&self, pem_bytes: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
        let mut reader = BufReader::new(pem_bytes);
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Failed to parse certificates: {}", e))?;

        if certs.is_empty() {
            return Err(anyhow::anyhow!("No certificates found in PEM"));
        }

        Ok(certs)
    }

    /// Parse PEM private key into rustls format
    fn parse_private_key(&self, pem_bytes: &[u8]) -> Result<PrivateKeyDer<'static>> {
        let mut reader = BufReader::new(pem_bytes);

        loop {
            match rustls_pemfile::read_one(&mut reader)
                .map_err(|e| anyhow::anyhow!("Failed to parse private key: {}", e))?
            {
                Some(rustls_pemfile::Item::Pkcs1Key(key)) => return Ok(key.into()),
                Some(rustls_pemfile::Item::Pkcs8Key(key)) => return Ok(key.into()),
                Some(rustls_pemfile::Item::Sec1Key(key)) => return Ok(key.into()),
                None => break,
                _ => {}
            }
        }

        Err(anyhow::anyhow!("No valid private key found in PEM"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::DbConnection;

    #[test]
    fn test_wildcard_domain_extraction() {
        // Create a dummy encryption service for testing
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .expect("Failed to create encryption service")
        );
        let loader = CertificateLoader::new(
            Arc::new(DbConnection::default()),
            encryption_service,
        );

        assert_eq!(
            loader.get_wildcard_domain("api.example.com"),
            Some("*.example.com".to_string())
        );

        assert_eq!(
            loader.get_wildcard_domain("www.sub.example.com"),
            Some("*.sub.example.com".to_string())
        );

        assert_eq!(
            loader.get_wildcard_domain("example.com"),
            Some("*.com".to_string())
        );
    }
}
