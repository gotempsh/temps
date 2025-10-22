use async_trait::async_trait;
use chrono::{Duration, Utc};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::*;
use std::sync::Arc;
use temps_core::{EncryptionService, UtcDateTime};
use temps_database::DbConnection;

use super::errors::RepositoryError;
use super::models::*;

#[async_trait]
pub trait CertificateRepository: Send + Sync {
    // Certificate operations
    async fn save_certificate(&self, cert: Certificate) -> Result<Certificate, RepositoryError>;
    async fn find_certificate_by_id(&self, id: i32)
        -> Result<Option<Certificate>, RepositoryError>;
    async fn find_certificate(&self, domain: &str) -> Result<Option<Certificate>, RepositoryError>;
    async fn find_certificate_for_sni(
        &self,
        sni: &str,
    ) -> Result<Option<Certificate>, RepositoryError>;
    async fn list_certificates(
        &self,
        filter: CertificateFilter,
    ) -> Result<Vec<Certificate>, RepositoryError>;
    async fn update_certificate_status(
        &self,
        domain: &str,
        status: CertificateStatus,
    ) -> Result<(), RepositoryError>;
    async fn delete_certificate(&self, domain: &str) -> Result<(), RepositoryError>;
    async fn find_expiring_certificates(
        &self,
        days: i32,
    ) -> Result<Vec<Certificate>, RepositoryError>;

    // DNS challenge operations
    async fn save_dns_challenge(&self, data: DnsChallengeData) -> Result<(), RepositoryError>;
    async fn find_dns_challenge(
        &self,
        domain: &str,
    ) -> Result<Option<DnsChallengeData>, RepositoryError>;
    async fn delete_dns_challenge(&self, domain: &str) -> Result<(), RepositoryError>;

    // HTTP challenge operations
    async fn save_http_challenge(&self, data: HttpChallengeData) -> Result<(), RepositoryError>;
    async fn find_http_challenge(
        &self,
        domain: &str,
    ) -> Result<Option<HttpChallengeData>, RepositoryError>;
    async fn delete_http_challenge(&self, domain: &str) -> Result<(), RepositoryError>;

    // ACME account operations
    async fn save_acme_account(&self, account: AcmeAccount) -> Result<(), RepositoryError>;
    async fn find_acme_account(
        &self,
        email: &str,
        environment: &str,
    ) -> Result<Option<AcmeAccount>, RepositoryError>;

    // ACME order operations
    async fn save_acme_order(&self, order: AcmeOrder) -> Result<AcmeOrder, RepositoryError>;
    async fn find_acme_order_by_domain(
        &self,
        domain_id: i32,
    ) -> Result<Option<AcmeOrder>, RepositoryError>;
    async fn find_acme_order_by_url(
        &self,
        order_url: &str,
    ) -> Result<Option<AcmeOrder>, RepositoryError>;
    async fn list_all_orders(&self) -> Result<Vec<AcmeOrder>, RepositoryError>;
    async fn update_acme_order_status(
        &self,
        order_url: &str,
        status: &str,
        error: Option<String>,
    ) -> Result<(), RepositoryError>;
    async fn delete_acme_order(&self, order_url: &str) -> Result<(), RepositoryError>;

    // TLS ACME certificate operations (for TLS-ALPN challenges)
    async fn save_tls_acme_certificate(
        &self,
        domain: &str,
        cert_pem: &str,
        key_pem: &str,
        expires_at: UtcDateTime,
    ) -> Result<(), RepositoryError>;
    async fn find_tls_acme_certificate(
        &self,
        domain: &str,
    ) -> Result<Option<(String, String)>, RepositoryError>;
    async fn delete_tls_acme_certificate(&self, domain: &str) -> Result<(), RepositoryError>;
}

pub struct DefaultCertificateRepository {
    db: Arc<DbConnection>,
    encryption_service: Arc<EncryptionService>,
}

impl DefaultCertificateRepository {
    pub fn new(db: Arc<DbConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Decrypt the private key from an encrypted certificate
    fn decrypt_certificate(&self, mut cert: Certificate) -> Result<Certificate, RepositoryError> {
        if !cert.private_key_pem.is_empty() {
            cert.private_key_pem = self
                .encryption_service
                .decrypt_string(&cert.private_key_pem)
                .map_err(|e| {
                    RepositoryError::Internal(format!("Failed to decrypt private key: {}", e))
                })?;
        }
        Ok(cert)
    }
}

#[async_trait]
impl CertificateRepository for DefaultCertificateRepository {
    async fn save_certificate(&self, cert: Certificate) -> Result<Certificate, RepositoryError> {
        use temps_entities::domains;

        // Encrypt the private key before saving
        let encrypted_private_key = if !cert.private_key_pem.is_empty() {
            self.encryption_service
                .encrypt_string(&cert.private_key_pem)
                .map_err(|e| {
                    RepositoryError::Internal(format!("Failed to encrypt private key: {}", e))
                })?
        } else {
            String::new()
        };

        // Create a modified certificate with encrypted key
        let mut cert_with_encrypted_key = cert.clone();
        cert_with_encrypted_key.private_key_pem = encrypted_private_key.clone();

        let active_model: domains::ActiveModel = (&cert_with_encrypted_key).into();

        let result = domains::Entity::insert(active_model.clone())
            .on_conflict(
                OnConflict::column(domains::Column::Domain)
                    .update_columns([
                        domains::Column::Certificate,
                        domains::Column::PrivateKey,
                        domains::Column::ExpirationTime,
                        domains::Column::LastRenewed,
                        domains::Column::Status,
                        domains::Column::UpdatedAt,
                        domains::Column::VerificationMethod,
                        domains::Column::IsWildcard,
                        domains::Column::LastError,
                        domains::Column::LastErrorType,
                    ])
                    .to_owned(),
            )
            .exec_with_returning(self.db.as_ref())
            .await?;

        // Return the original certificate with unencrypted key (for immediate use)
        let mut returned_cert: Certificate = result.into();
        returned_cert.private_key_pem = cert.private_key_pem;
        Ok(returned_cert)
    }

    async fn find_certificate(&self, domain: &str) -> Result<Option<Certificate>, RepositoryError> {
        use temps_entities::domains;

        let result = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?;

        match result {
            Some(entity) => {
                let cert: Certificate = entity.into();
                Ok(Some(self.decrypt_certificate(cert)?))
            }
            None => Ok(None),
        }
    }

    async fn find_certificate_by_id(
        &self,
        id: i32,
    ) -> Result<Option<Certificate>, RepositoryError> {
        use temps_entities::domains;

        let result = domains::Entity::find()
            .filter(domains::Column::Id.eq(id))
            .one(self.db.as_ref())
            .await?;

        match result {
            Some(entity) => {
                let cert: Certificate = entity.into();
                Ok(Some(self.decrypt_certificate(cert)?))
            }
            None => Ok(None),
        }
    }

    async fn find_certificate_for_sni(
        &self,
        sni: &str,
    ) -> Result<Option<Certificate>, RepositoryError> {
        // First try exact match
        if let Some(cert) = self.find_certificate(sni).await? {
            if !cert.certificate_pem.is_empty() && !cert.private_key_pem.is_empty() {
                return Ok(Some(cert));
            }
        }

        // Try wildcard match
        let parts: Vec<&str> = sni.split('.').collect();
        if parts.len() >= 2 {
            let wildcard = format!("*.{}", parts[1..].join("."));
            if let Some(cert) = self.find_certificate(&wildcard).await? {
                if !cert.certificate_pem.is_empty() && !cert.private_key_pem.is_empty() {
                    return Ok(Some(cert));
                }
            }
        }

        Ok(None)
    }

    async fn list_certificates(
        &self,
        filter: CertificateFilter,
    ) -> Result<Vec<Certificate>, RepositoryError> {
        use temps_entities::domains;

        let mut query = domains::Entity::find();

        // Apply status filter
        if let Some(status) = &filter.status {
            let status_str = match status {
                CertificateStatus::Active => "active",
                CertificateStatus::Pending => "pending",
                CertificateStatus::PendingDns => "pending_dns",
                CertificateStatus::PendingValidation => "pending_validation",
                CertificateStatus::Failed { .. } => "failed",
                CertificateStatus::Expired => "expired",
            };
            query = query.filter(domains::Column::Status.eq(status_str));
        }

        // Apply wildcard filter
        if let Some(is_wildcard) = filter.is_wildcard {
            query = query.filter(domains::Column::IsWildcard.eq(is_wildcard));
        }

        // Apply expiring filter
        if let Some(days) = filter.expiring_within_days {
            let expiry_date = Utc::now() + Duration::days(days as i64);
            query = query.filter(
                Condition::all()
                    .add(domains::Column::ExpirationTime.is_not_null())
                    .add(domains::Column::ExpirationTime.lte(expiry_date)),
            );
        }

        // Apply domain pattern filter
        if let Some(pattern) = &filter.domain_pattern {
            query = query.filter(domains::Column::Domain.contains(pattern));
        }

        let results = query.all(self.db.as_ref()).await?;

        // Decrypt all certificates
        let certs: Vec<Certificate> = results.into_iter().map(Into::into).collect();
        certs
            .into_iter()
            .map(|cert| self.decrypt_certificate(cert))
            .collect()
    }

    async fn update_certificate_status(
        &self,
        domain: &str,
        status: CertificateStatus,
    ) -> Result<(), RepositoryError> {
        use temps_entities::domains;

        let (status_str, last_error, last_error_type) = match &status {
            CertificateStatus::Active => ("active", None, None),
            CertificateStatus::Pending => ("pending", None, None),
            CertificateStatus::PendingDns => ("pending_dns", None, None),
            CertificateStatus::PendingValidation => ("pending_validation", None, None),
            CertificateStatus::Failed { error, error_type } => {
                ("failed", Some(error.as_str()), Some(error_type.as_str()))
            }
            CertificateStatus::Expired => ("expired", None, None),
        };

        let result = domains::Entity::update_many()
            .filter(domains::Column::Domain.eq(domain))
            .set(domains::ActiveModel {
                status: Set(status_str.to_string()),
                last_error: Set(last_error.map(String::from)),
                last_error_type: Set(last_error_type.map(String::from)),
                updated_at: Set(Utc::now()),
                ..Default::default()
            })
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(RepositoryError::NotFound(format!(
                "Domain '{}' not found",
                domain
            )));
        }

        Ok(())
    }

    async fn delete_certificate(&self, domain: &str) -> Result<(), RepositoryError> {
        use temps_entities::domains;

        let result = domains::Entity::delete_many()
            .filter(domains::Column::Domain.eq(domain))
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(RepositoryError::NotFound(format!(
                "Domain '{}' not found",
                domain
            )));
        }

        Ok(())
    }

    async fn find_expiring_certificates(
        &self,
        days: i32,
    ) -> Result<Vec<Certificate>, RepositoryError> {
        use temps_entities::domains;

        let expiry_date = Utc::now() + Duration::days(days as i64);
        let results = domains::Entity::find()
            .filter(
                Condition::all()
                    .add(domains::Column::ExpirationTime.is_not_null())
                    .add(domains::Column::ExpirationTime.lte(expiry_date))
                    .add(domains::Column::Status.eq("active")),
            )
            .all(self.db.as_ref())
            .await?;

        // Decrypt all certificates
        let certs: Vec<Certificate> = results.into_iter().map(Into::into).collect();
        certs
            .into_iter()
            .map(|cert| self.decrypt_certificate(cert))
            .collect()
    }

    async fn save_dns_challenge(&self, data: DnsChallengeData) -> Result<(), RepositoryError> {
        use temps_entities::domains;

        let result = domains::Entity::update_many()
            .filter(domains::Column::Domain.eq(&data.domain))
            .set(domains::ActiveModel {
                status: Set("pending_dns".to_string()),
                dns_challenge_token: Set(Some(data.txt_record_name.clone())),
                dns_challenge_value: Set(Some(data.txt_record_value.clone())),
                last_error: Set(data.order_url.clone()), // Store order URL in last_error temporarily
                updated_at: Set(Utc::now()),
                ..Default::default()
            })
            .exec(self.db.as_ref())
            .await?;

        // If domain doesn't exist, create it
        if result.rows_affected == 0 {
            let new_domain = domains::ActiveModel {
                domain: Set(data.domain.clone()),
                status: Set("pending_dns".to_string()),
                dns_challenge_token: Set(Some(data.txt_record_name)),
                dns_challenge_value: Set(Some(data.txt_record_value)),
                last_error: Set(data.order_url),
                created_at: Set(data.created_at),
                updated_at: Set(Utc::now()),
                verification_method: Set("dns-01".to_string()),
                is_wildcard: Set(data.domain.starts_with("*.")),
                ..Default::default()
            };
            domains::Entity::insert(new_domain)
                .exec(self.db.as_ref())
                .await?;
        }

        Ok(())
    }

    async fn find_dns_challenge(
        &self,
        domain: &str,
    ) -> Result<Option<DnsChallengeData>, RepositoryError> {
        use temps_entities::domains;

        let result = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain))
            .filter(domains::Column::Status.eq("pending_dns"))
            .one(self.db.as_ref())
            .await?;

        Ok(result.and_then(|r| {
            match (r.dns_challenge_token, r.dns_challenge_value) {
                (Some(token), Some(value)) => Some(DnsChallengeData {
                    domain: r.domain,
                    txt_record_name: token,
                    txt_record_value: value,
                    order_url: r.last_error, // Order URL stored in last_error
                    created_at: r.created_at,
                }),
                _ => None,
            }
        }))
    }

    async fn delete_dns_challenge(&self, domain: &str) -> Result<(), RepositoryError> {
        use temps_entities::domains;

        domains::Entity::update_many()
            .filter(domains::Column::Domain.eq(domain))
            .set(domains::ActiveModel {
                dns_challenge_token: Set(None),
                dns_challenge_value: Set(None),
                updated_at: Set(Utc::now()),
                ..Default::default()
            })
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    async fn save_http_challenge(&self, data: HttpChallengeData) -> Result<(), RepositoryError> {
        use temps_entities::domains;

        let result = domains::Entity::update_many()
            .filter(domains::Column::Domain.eq(&data.domain))
            .set(domains::ActiveModel {
                status: Set("pending_http".to_string()),
                http_challenge_token: Set(Some(data.token.clone())),
                http_challenge_key_authorization: Set(Some(data.key_authorization.clone())),
                last_error: Set(data.validation_url.clone()), // Store validation URL in last_error temporarily
                updated_at: Set(Utc::now()),
                ..Default::default()
            })
            .exec(self.db.as_ref())
            .await?;

        // If domain doesn't exist, create it
        if result.rows_affected == 0 {
            let new_domain = domains::ActiveModel {
                domain: Set(data.domain.clone()),
                status: Set("pending_http".to_string()),
                http_challenge_token: Set(Some(data.token)),
                http_challenge_key_authorization: Set(Some(data.key_authorization)),
                last_error: Set(data.validation_url),
                created_at: Set(data.created_at),
                updated_at: Set(Utc::now()),
                verification_method: Set("http-01".to_string()),
                is_wildcard: Set(data.domain.starts_with("*.")),
                ..Default::default()
            };
            domains::Entity::insert(new_domain)
                .exec(self.db.as_ref())
                .await?;
        }

        Ok(())
    }

    async fn find_http_challenge(
        &self,
        domain: &str,
    ) -> Result<Option<HttpChallengeData>, RepositoryError> {
        use temps_entities::domains;

        let result = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain))
            .filter(domains::Column::Status.eq("pending_http"))
            .one(self.db.as_ref())
            .await?;

        Ok(result.and_then(|r| {
            match (r.http_challenge_token, r.http_challenge_key_authorization) {
                (Some(token), Some(key_auth)) => Some(HttpChallengeData {
                    domain: r.domain,
                    token,
                    key_authorization: key_auth,
                    validation_url: r.last_error, // Validation URL stored in last_error
                    created_at: r.created_at,
                }),
                _ => None,
            }
        }))
    }

    async fn delete_http_challenge(&self, domain: &str) -> Result<(), RepositoryError> {
        use temps_entities::domains;

        domains::Entity::update_many()
            .filter(domains::Column::Domain.eq(domain))
            .set(domains::ActiveModel {
                http_challenge_token: Set(None),
                http_challenge_key_authorization: Set(None),
                updated_at: Set(Utc::now()),
                ..Default::default()
            })
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    async fn save_acme_account(&self, account: AcmeAccount) -> Result<(), RepositoryError> {
        use temps_entities::acme_accounts;

        let new_account = acme_accounts::ActiveModel {
            email: Set(account.email),
            environment: Set(account.environment),
            url: Set("https://acme-v02.api.letsencrypt.org/directory".to_string()), // Default URL
            account_data: Set(account.credentials),
            created_at: Set(account.created_at),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        acme_accounts::Entity::insert(new_account)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    async fn find_acme_account(
        &self,
        email: &str,
        environment: &str,
    ) -> Result<Option<AcmeAccount>, RepositoryError> {
        use temps_entities::acme_accounts;

        let result = acme_accounts::Entity::find()
            .filter(acme_accounts::Column::Email.eq(email))
            .filter(acme_accounts::Column::Environment.eq(environment))
            .one(self.db.as_ref())
            .await?;

        Ok(result.map(|r| AcmeAccount {
            email: r.email,
            environment: r.environment,
            credentials: r.account_data,
            created_at: r.created_at,
        }))
    }

    async fn save_acme_order(&self, order: AcmeOrder) -> Result<AcmeOrder, RepositoryError> {
        use temps_entities::acme_orders;

        let new_order = acme_orders::ActiveModel {
            order_url: Set(order.order_url),
            domain_id: Set(order.domain_id),
            email: Set(order.email),
            status: Set(order.status),
            identifiers: Set(order.identifiers),
            authorizations: Set(order.authorizations),
            finalize_url: Set(order.finalize_url),
            certificate_url: Set(order.certificate_url),
            error: Set(order.error),
            error_type: Set(order.error_type),
            token: Set(order.token),
            key_authorization: Set(order.key_authorization),
            expires_at: Set(order.expires_at),
            ..Default::default()
        };

        let result = acme_orders::Entity::insert(new_order)
            .on_conflict(
                OnConflict::column(acme_orders::Column::OrderUrl)
                    .update_columns([
                        acme_orders::Column::Status,
                        acme_orders::Column::Authorizations,
                        acme_orders::Column::FinalizeUrl,
                        acme_orders::Column::CertificateUrl,
                        acme_orders::Column::Error,
                        acme_orders::Column::ErrorType,
                        acme_orders::Column::Token,
                        acme_orders::Column::KeyAuthorization,
                        acme_orders::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_with_returning(self.db.as_ref())
            .await?;

        Ok(AcmeOrder {
            id: result.id,
            order_url: result.order_url,
            domain_id: result.domain_id,
            email: result.email,
            status: result.status,
            identifiers: result.identifiers,
            authorizations: result.authorizations,
            finalize_url: result.finalize_url,
            certificate_url: result.certificate_url,
            error: result.error,
            error_type: result.error_type,
            token: result.token,
            key_authorization: result.key_authorization,
            created_at: result.created_at,
            updated_at: result.updated_at,
            expires_at: result.expires_at,
        })
    }

    async fn find_acme_order_by_domain(
        &self,
        domain_id: i32,
    ) -> Result<Option<AcmeOrder>, RepositoryError> {
        use temps_entities::acme_orders;

        let result = acme_orders::Entity::find()
            .filter(acme_orders::Column::DomainId.eq(domain_id))
            .order_by_desc(acme_orders::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?;

        Ok(result.map(|r| AcmeOrder {
            id: r.id,
            order_url: r.order_url,
            domain_id: r.domain_id,
            email: r.email,
            status: r.status,
            identifiers: r.identifiers,
            authorizations: r.authorizations,
            finalize_url: r.finalize_url,
            certificate_url: r.certificate_url,
            error: r.error,
            error_type: r.error_type,
            token: r.token,
            key_authorization: r.key_authorization,
            created_at: r.created_at,
            updated_at: r.updated_at,
            expires_at: r.expires_at,
        }))
    }

    async fn find_acme_order_by_url(
        &self,
        order_url: &str,
    ) -> Result<Option<AcmeOrder>, RepositoryError> {
        use temps_entities::acme_orders;

        let result = acme_orders::Entity::find()
            .filter(acme_orders::Column::OrderUrl.eq(order_url))
            .one(self.db.as_ref())
            .await?;

        Ok(result.map(|r| AcmeOrder {
            id: r.id,
            order_url: r.order_url,
            domain_id: r.domain_id,
            email: r.email,
            status: r.status,
            identifiers: r.identifiers,
            authorizations: r.authorizations,
            finalize_url: r.finalize_url,
            certificate_url: r.certificate_url,
            error: r.error,
            error_type: r.error_type,
            token: r.token,
            key_authorization: r.key_authorization,
            created_at: r.created_at,
            updated_at: r.updated_at,
            expires_at: r.expires_at,
        }))
    }

    async fn list_all_orders(&self) -> Result<Vec<AcmeOrder>, RepositoryError> {
        use temps_entities::acme_orders;

        let results = acme_orders::Entity::find()
            .order_by_desc(acme_orders::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(results
            .into_iter()
            .map(|r| AcmeOrder {
                id: r.id,
                order_url: r.order_url,
                domain_id: r.domain_id,
                email: r.email,
                status: r.status,
                identifiers: r.identifiers,
                authorizations: r.authorizations,
                finalize_url: r.finalize_url,
                certificate_url: r.certificate_url,
                error: r.error,
                error_type: r.error_type,
                token: r.token,
                key_authorization: r.key_authorization,
                created_at: r.created_at,
                updated_at: r.updated_at,
                expires_at: r.expires_at,
            })
            .collect())
    }

    async fn update_acme_order_status(
        &self,
        order_url: &str,
        status: &str,
        error: Option<String>,
    ) -> Result<(), RepositoryError> {
        use temps_entities::acme_orders;

        acme_orders::Entity::update_many()
            .col_expr(acme_orders::Column::Status, Expr::value(status))
            .col_expr(acme_orders::Column::Error, Expr::value(error.clone()))
            .col_expr(
                acme_orders::Column::UpdatedAt,
                Expr::current_timestamp().into(),
            )
            .filter(acme_orders::Column::OrderUrl.eq(order_url))
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    async fn delete_acme_order(&self, order_url: &str) -> Result<(), RepositoryError> {
        use temps_entities::acme_orders;

        acme_orders::Entity::delete_many()
            .filter(acme_orders::Column::OrderUrl.eq(order_url))
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    async fn save_tls_acme_certificate(
        &self,
        domain: &str,
        cert_pem: &str,
        key_pem: &str,
        expires_at: UtcDateTime,
    ) -> Result<(), RepositoryError> {
        use temps_entities::tls_acme_certificates;

        let new_cert = tls_acme_certificates::ActiveModel {
            domain: Set(domain.to_string()),
            certificate: Set(cert_pem.to_string()),
            private_key: Set(key_pem.to_string()),
            expires_at: Set(expires_at),
            issued_at: Set(Utc::now()),
            ..Default::default()
        };

        tls_acme_certificates::Entity::insert(new_cert)
            .on_conflict(
                OnConflict::column(tls_acme_certificates::Column::Domain)
                    .update_columns([
                        tls_acme_certificates::Column::Certificate,
                        tls_acme_certificates::Column::PrivateKey,
                        tls_acme_certificates::Column::ExpiresAt,
                        tls_acme_certificates::Column::IssuedAt,
                    ])
                    .to_owned(),
            )
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    async fn find_tls_acme_certificate(
        &self,
        domain: &str,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        use temps_entities::tls_acme_certificates;

        let result = tls_acme_certificates::Entity::find()
            .filter(tls_acme_certificates::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?;

        Ok(result.map(|r| (r.certificate, r.private_key)))
    }

    async fn delete_tls_acme_certificate(&self, domain: &str) -> Result<(), RepositoryError> {
        use temps_entities::tls_acme_certificates;

        tls_acme_certificates::Entity::delete_many()
            .filter(tls_acme_certificates::Column::Domain.eq(domain))
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }
}

#[cfg(test)]
pub mod test_utils {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    pub struct MockCertificateRepository {
        certificates: Arc<RwLock<HashMap<String, Certificate>>>,
        challenges: Arc<RwLock<HashMap<String, DnsChallengeData>>>,
        http_challenges: Arc<RwLock<HashMap<String, HttpChallengeData>>>,
        accounts: Arc<RwLock<HashMap<String, AcmeAccount>>>,
    }

    impl MockCertificateRepository {
        pub fn new() -> Self {
            Self {
                certificates: Arc::new(RwLock::new(HashMap::new())),
                challenges: Arc::new(RwLock::new(HashMap::new())),
                http_challenges: Arc::new(RwLock::new(HashMap::new())),
                accounts: Arc::new(RwLock::new(HashMap::new())),
            }
        }
    }

    #[async_trait]
    impl CertificateRepository for MockCertificateRepository {
        async fn find_certificate_by_id(
            &self,
            id: i32,
        ) -> Result<Option<Certificate>, RepositoryError> {
            let certs = self.certificates.read().await;
            Ok(certs.values().find(|c| c.id == id).cloned())
        }

        async fn save_certificate(
            &self,
            cert: Certificate,
        ) -> Result<Certificate, RepositoryError> {
            let mut certs = self.certificates.write().await;
            certs.insert(cert.domain.clone(), cert.clone());
            Ok(cert)
        }

        async fn find_certificate(
            &self,
            domain: &str,
        ) -> Result<Option<Certificate>, RepositoryError> {
            let certs = self.certificates.read().await;
            Ok(certs.get(domain).cloned())
        }

        async fn find_certificate_for_sni(
            &self,
            sni: &str,
        ) -> Result<Option<Certificate>, RepositoryError> {
            self.find_certificate(sni).await
        }

        async fn list_certificates(
            &self,
            _filter: CertificateFilter,
        ) -> Result<Vec<Certificate>, RepositoryError> {
            let certs = self.certificates.read().await;
            Ok(certs.values().cloned().collect())
        }

        async fn update_certificate_status(
            &self,
            domain: &str,
            status: CertificateStatus,
        ) -> Result<(), RepositoryError> {
            let mut certs = self.certificates.write().await;
            if let Some(cert) = certs.get_mut(domain) {
                cert.status = status;
                Ok(())
            } else {
                Err(RepositoryError::NotFound(format!(
                    "Domain '{}' not found",
                    domain
                )))
            }
        }

        async fn delete_certificate(&self, domain: &str) -> Result<(), RepositoryError> {
            let mut certs = self.certificates.write().await;
            certs.remove(domain).ok_or_else(|| {
                RepositoryError::NotFound(format!("Domain '{}' not found", domain))
            })?;
            Ok(())
        }

        async fn find_expiring_certificates(
            &self,
            days: i32,
        ) -> Result<Vec<Certificate>, RepositoryError> {
            let certs = self.certificates.read().await;
            Ok(certs
                .values()
                .filter(|c| c.days_until_expiry() <= days as i64)
                .cloned()
                .collect())
        }

        async fn save_dns_challenge(&self, data: DnsChallengeData) -> Result<(), RepositoryError> {
            let mut challenges = self.challenges.write().await;
            challenges.insert(data.domain.clone(), data);
            Ok(())
        }

        async fn find_dns_challenge(
            &self,
            domain: &str,
        ) -> Result<Option<DnsChallengeData>, RepositoryError> {
            let challenges = self.challenges.read().await;
            Ok(challenges.get(domain).cloned())
        }

        async fn delete_dns_challenge(&self, domain: &str) -> Result<(), RepositoryError> {
            let mut challenges = self.challenges.write().await;
            challenges.remove(domain);
            Ok(())
        }

        async fn save_http_challenge(
            &self,
            data: HttpChallengeData,
        ) -> Result<(), RepositoryError> {
            let mut challenges = self.http_challenges.write().await;
            challenges.insert(data.domain.clone(), data);
            Ok(())
        }

        async fn find_http_challenge(
            &self,
            domain: &str,
        ) -> Result<Option<HttpChallengeData>, RepositoryError> {
            let challenges = self.http_challenges.read().await;
            Ok(challenges.get(domain).cloned())
        }

        async fn delete_http_challenge(&self, domain: &str) -> Result<(), RepositoryError> {
            let mut challenges = self.http_challenges.write().await;
            challenges.remove(domain);
            Ok(())
        }

        async fn save_acme_account(&self, account: AcmeAccount) -> Result<(), RepositoryError> {
            let mut accounts = self.accounts.write().await;
            let key = format!("{}:{}", account.email, account.environment);
            accounts.insert(key, account);
            Ok(())
        }

        async fn find_acme_account(
            &self,
            email: &str,
            environment: &str,
        ) -> Result<Option<AcmeAccount>, RepositoryError> {
            let accounts = self.accounts.read().await;
            let key = format!("{}:{}", email, environment);
            Ok(accounts.get(&key).cloned())
        }

        async fn save_acme_order(&self, order: AcmeOrder) -> Result<AcmeOrder, RepositoryError> {
            // For mock, just return the order with an ID if it doesn't have one
            Ok(order)
        }

        async fn find_acme_order_by_domain(
            &self,
            _domain_id: i32,
        ) -> Result<Option<AcmeOrder>, RepositoryError> {
            Ok(None)
        }

        async fn find_acme_order_by_url(
            &self,
            _order_url: &str,
        ) -> Result<Option<AcmeOrder>, RepositoryError> {
            Ok(None)
        }

        async fn list_all_orders(&self) -> Result<Vec<AcmeOrder>, RepositoryError> {
            Ok(vec![])
        }

        async fn update_acme_order_status(
            &self,
            _order_url: &str,
            _status: &str,
            _error: Option<String>,
        ) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn delete_acme_order(&self, _order_url: &str) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn save_tls_acme_certificate(
            &self,
            _domain: &str,
            _cert_pem: &str,
            _key_pem: &str,
            _expires_at: UtcDateTime,
        ) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn find_tls_acme_certificate(
            &self,
            _domain: &str,
        ) -> Result<Option<(String, String)>, RepositoryError> {
            Ok(None)
        }

        async fn delete_tls_acme_certificate(&self, _domain: &str) -> Result<(), RepositoryError> {
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn test_mock_repository_save_and_find() {
            let repo = MockCertificateRepository::new();

            let cert = Certificate {
                id: 1,
                domain: "test.example.com".to_string(),
                certificate_pem: "cert".to_string(),
                private_key_pem: "key".to_string(),
                expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
                last_renewed: None,
                is_wildcard: false,
                verification_method: "tls-alpn-01".to_string(),
                status: CertificateStatus::Active,
            };

            // Test save
            let saved = repo.save_certificate(cert.clone()).await.unwrap();
            assert_eq!(saved.domain, "test.example.com");

            // Test find
            let found = repo.find_certificate("test.example.com").await.unwrap();
            assert!(found.is_some());
            assert_eq!(found.unwrap().domain, "test.example.com");

            // Test not found
            let not_found = repo.find_certificate("nonexistent.com").await.unwrap();
            assert!(not_found.is_none());
        }

        #[tokio::test]
        async fn test_mock_repository_update_status() {
            let repo = MockCertificateRepository::new();

            let cert = Certificate {
                id: 1,
                domain: "status.example.com".to_string(),
                certificate_pem: "cert".to_string(),
                private_key_pem: "key".to_string(),
                expiration_time: chrono::Utc::now() + chrono::Duration::days(90),
                last_renewed: None,
                is_wildcard: false,
                verification_method: "tls-alpn-01".to_string(),
                status: CertificateStatus::Pending,
            };

            repo.save_certificate(cert).await.unwrap();

            // Update status
            repo.update_certificate_status("status.example.com", CertificateStatus::Active)
                .await
                .unwrap();

            let updated = repo
                .find_certificate("status.example.com")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(updated.status, CertificateStatus::Active);
        }

        #[tokio::test]
        async fn test_mock_repository_dns_challenge() {
            let repo = MockCertificateRepository::new();

            let challenge = DnsChallengeData {
                domain: "dns.example.com".to_string(),
                txt_record_name: "_acme-challenge.dns.example.com".to_string(),
                txt_record_value: "challenge-value".to_string(),
                order_url: Some("https://acme.example.com/order".to_string()),
                created_at: chrono::Utc::now(),
            };

            // Save challenge
            repo.save_dns_challenge(challenge.clone()).await.unwrap();

            // Find challenge
            let found = repo.find_dns_challenge("dns.example.com").await.unwrap();
            assert!(found.is_some());
            assert_eq!(found.unwrap().txt_record_value, "challenge-value");

            // Delete challenge
            repo.delete_dns_challenge("dns.example.com").await.unwrap();
            let deleted = repo.find_dns_challenge("dns.example.com").await.unwrap();
            assert!(deleted.is_none());
        }

        #[tokio::test]
        async fn test_list_all_orders_with_database() {
            use chrono::Utc;
            use sea_orm::{ActiveModelTrait, Set};
            use temps_database::test_utils::TestDatabase;
            use temps_entities::{acme_orders, domains};

            // Setup test database
            let test_db = TestDatabase::with_migrations().await.unwrap();
            let db = test_db.db.clone();

            // Create encryption service for repository
            let encryption_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
            let encryption_service =
                Arc::new(temps_core::EncryptionService::new(encryption_key).unwrap());

            let repo = DefaultCertificateRepository::new(db.clone(), encryption_service);

            // Create test domains first (required for foreign key)
            let now = Utc::now();
            let domain1 = domains::ActiveModel {
                domain: Set("test1.example.com".to_string()),
                status: Set("pending".to_string()),
                is_wildcard: Set(false),
                verification_method: Set("http-01".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            domain1.insert(db.as_ref()).await.unwrap();

            let domain2 = domains::ActiveModel {
                domain: Set("test2.example.com".to_string()),
                status: Set("valid".to_string()),
                is_wildcard: Set(false),
                verification_method: Set("http-01".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            domain2.insert(db.as_ref()).await.unwrap();

            // Insert test ACME orders
            let order1 = acme_orders::ActiveModel {
                order_url: Set("https://acme.example.com/order/1".to_string()),
                domain_id: Set(1),
                email: Set("test@example.com".to_string()),
                status: Set("pending".to_string()),
                identifiers: Set(serde_json::json!({"type": "dns", "value": "test1.example.com"})),
                authorizations: Set(None),
                finalize_url: Set(None),
                certificate_url: Set(None),
                error: Set(None),
                error_type: Set(None),
                token: Set(None),
                key_authorization: Set(None),
                expires_at: Set(None),
                ..Default::default()
            };

            let order2 = acme_orders::ActiveModel {
                order_url: Set("https://acme.example.com/order/2".to_string()),
                domain_id: Set(2),
                email: Set("test@example.com".to_string()),
                status: Set("valid".to_string()),
                identifiers: Set(serde_json::json!({"type": "dns", "value": "test2.example.com"})),
                authorizations: Set(None),
                finalize_url: Set(None),
                certificate_url: Set(None),
                error: Set(None),
                error_type: Set(None),
                token: Set(None),
                key_authorization: Set(None),
                expires_at: Set(None),
                ..Default::default()
            };

            order1.insert(db.as_ref()).await.unwrap();
            order2.insert(db.as_ref()).await.unwrap();

            // Test list_all_orders
            let orders = repo.list_all_orders().await.unwrap();

            assert_eq!(orders.len(), 2, "Should return 2 orders");
            assert_eq!(orders[0].order_url, "https://acme.example.com/order/2");
            assert_eq!(orders[0].status, "valid");
            assert_eq!(orders[1].order_url, "https://acme.example.com/order/1");
            assert_eq!(orders[1].status, "pending");
        }
    }
}
