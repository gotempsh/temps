use super::*;
use samael::attribute::{Attribute, AttributeValue};
use samael::idp::response_builder::ResponseAttribute;
use samael::idp::sp_extractor::RequiredAttribute;
use samael::idp::{CertificateParams, IdentityProvider, KeyType, Rsa as IdpRsa};
use samael::schema::{Subject, SubjectNameID};
use samael::traits::ToXml;
use temps_database::test_utils::TestDatabase;

// ------------------------------------------------------------------
// Pure functions: no DB, no signing
// ------------------------------------------------------------------

#[test]
fn random_token_is_64_hex_chars_and_varies() {
    let a = random_token();
    let b = random_token();
    assert_eq!(a.len(), 64);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b, "two calls must not collide");
}

fn mapping(idp_group: &str, role: &str, priority: i32) -> saml_role_mappings::Model {
    saml_role_mappings::Model {
        id: 0,
        provider_id: 1,
        priority,
        idp_group: idp_group.to_string(),
        role: role.to_string(),
        created_at: Utc::now(),
    }
}

fn test_provider(default_role: &str) -> saml_providers::Model {
    saml_providers::Model {
        id: 1,
        name: "test".to_string(),
        template: "generic".to_string(),
        sp_entity_id: "https://temps.example.com/api/auth/saml/metadata/test-aabbccdd".to_string(),
        idp_entity_id: "https://idp.example.com/metadata".to_string(),
        idp_sso_url: "https://idp.example.com/sso".to_string(),
        idp_x509_cert: "-----BEGIN CERTIFICATE-----\nMII=\n-----END CERTIFICATE-----\n".to_string(),
        idp_metadata_url: None,
        group_attribute: "groups".to_string(),
        role_attribute: "roles".to_string(),
        default_role: default_role.to_string(),
        email_attribute: None,
        jit_provisioning: true,
        enabled: true,
        trust_idp_email: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn evaluate_role_wildcard_matches_any_group() {
    let provider = test_provider("user");
    let mappings = vec![mapping("*", "admin", 100)];
    let role = evaluate_role(&provider, &mappings, &["anything".to_string()]);
    assert_eq!(role, RoleType::Admin);
}

#[test]
fn evaluate_role_group_match_returns_mapped_role() {
    let provider = test_provider("user");
    let mappings = vec![mapping("engineering", "admin", 100)];
    let role = evaluate_role(&provider, &mappings, &["engineering".to_string()]);
    assert_eq!(role, RoleType::Admin);
}

#[test]
fn evaluate_role_priority_order_respected() {
    let provider = test_provider("user");
    // Lower priority value wins -- first match in iteration order.
    let mappings = vec![
        mapping("engineering", "admin", 10),
        mapping("engineering", "user", 20),
    ];
    let role = evaluate_role(&provider, &mappings, &["engineering".to_string()]);
    assert_eq!(role, RoleType::Admin);
}

#[test]
fn evaluate_role_no_match_returns_default_role() {
    let provider = test_provider("user");
    let mappings = vec![mapping("engineering", "admin", 100)];
    let role = evaluate_role(&provider, &mappings, &["sales".to_string()]);
    assert_eq!(role, RoleType::User);
}

#[test]
fn validate_return_to_accepts_relative_paths() {
    assert!(validate_return_to("/dashboard").is_ok());
    assert!(validate_return_to("/projects/1").is_ok());
}

#[test]
fn validate_return_to_rejects_absolute_and_scheme_relative() {
    assert!(validate_return_to("https://evil.com").is_err());
    assert!(validate_return_to("//evil.com").is_err());
}

#[test]
fn validate_return_to_rejects_backslash_and_control_chars() {
    assert!(validate_return_to("/\\evil.com").is_err());
    assert!(validate_return_to("/foo\r\nSet-Cookie: x").is_err());
}

#[test]
fn pem_to_base64_der_body_strips_armor_and_whitespace() {
    let pem = "-----BEGIN CERTIFICATE-----\nAAAA\nBBBB\n-----END CERTIFICATE-----\n";
    assert_eq!(pem_to_base64_der_body(pem), "AAAABBBB");
}

#[test]
fn base64_der_to_pem_wraps_with_armor() {
    let pem = base64_der_to_pem("AAAABBBB");
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(pem.trim_end().ends_with("-----END CERTIFICATE-----"));
    assert_eq!(pem_to_base64_der_body(&pem), "AAAABBBB");
}

fn assertion_with(name_id: Option<(&str, Option<&str>)>, attrs: Vec<(&str, &str)>) -> Assertion {
    let subject = name_id.map(|(value, format)| Subject {
        name_id: Some(SubjectNameID {
            format: format.map(|f| f.to_string()),
            value: value.to_string(),
        }),
        subject_confirmations: None,
    });
    let attribute_statements = if attrs.is_empty() {
        None
    } else {
        Some(vec![samael::schema::AttributeStatement {
            attributes: attrs
                .into_iter()
                .map(|(name, value)| Attribute {
                    friendly_name: None,
                    name: Some(name.to_string()),
                    name_format: None,
                    values: vec![AttributeValue {
                        attribute_type: None,
                        value: Some(value.to_string()),
                    }],
                })
                .collect(),
        }])
    };
    Assertion {
        id: "test-assertion".to_string(),
        issue_instant: Utc::now(),
        version: "2.0".to_string(),
        issuer: samael::schema::Issuer::default(),
        signature: None,
        subject,
        conditions: None,
        authn_statements: None,
        attribute_statements,
    }
}

#[test]
fn extract_identity_uses_email_attribute_when_configured() {
    let mut provider = test_provider("user");
    provider.email_attribute = Some("email".to_string());
    let assertion = assertion_with(
        Some(("subj-123", None)),
        vec![("email", "USER@Example.com")],
    );
    let identity = extract_identity(&assertion, &provider).unwrap();
    assert_eq!(identity.subject, "subj-123");
    // lowercased + trimmed, matching resolve_user's OIDC counterpart.
    assert_eq!(identity.email, "user@example.com");
}

#[test]
fn extract_identity_falls_back_to_email_format_name_id() {
    let provider = test_provider("user");
    let assertion = assertion_with(
        Some((
            "user@example.com",
            Some("urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress"),
        )),
        vec![],
    );
    let identity = extract_identity(&assertion, &provider).unwrap();
    assert_eq!(identity.email, "user@example.com");
}

#[test]
fn extract_identity_fails_when_no_email_source() {
    let provider = test_provider("user");
    // No email_attribute configured, and NameID format is not emailAddress.
    let assertion = assertion_with(
        Some((
            "subj-123",
            Some("urn:oasis:names:tc:SAML:2.0:nameid-format:persistent"),
        )),
        vec![],
    );
    let err = extract_identity(&assertion, &provider).unwrap_err();
    assert!(matches!(err, SamlError::EmailMissing));
}

#[test]
fn extract_identity_fails_when_name_id_missing() {
    let provider = test_provider("user");
    let assertion = assertion_with(None, vec![]);
    let err = extract_identity(&assertion, &provider).unwrap_err();
    assert!(matches!(err, SamlError::NameIdMissing));
}

#[test]
fn extract_identity_reads_group_attribute_for_role_mapping() {
    let provider = test_provider("user");
    let assertion = assertion_with(
        Some((
            "user@example.com",
            Some("urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress"),
        )),
        vec![("groups", "engineering")],
    );
    let identity = extract_identity(&assertion, &provider).unwrap();
    assert_eq!(identity.groups, vec!["engineering".to_string()]);
}

#[test]
fn validate_cert_pem_rejects_garbage() {
    assert!(validate_cert_pem("not a cert").is_err());
}

#[test]
fn validate_cert_pem_accepts_generated_cert() {
    let idp = IdentityProvider::generate_new(KeyType::Rsa(IdpRsa::Rsa2048)).unwrap();
    let cert_der = idp
        .create_certificate(&CertificateParams {
            common_name: "Test IdP",
            issuer_name: "Test IdP",
            days_until_expiration: 365,
        })
        .unwrap();
    let pem =
        base64_der_to_pem(&samael::crypto::mime_encode_x509_cert(&cert_der).replace('\n', ""));
    assert!(validate_cert_pem(&pem).is_ok());
}

// ------------------------------------------------------------------
// DB-backed: login-state atomicity + resolve_user
// ------------------------------------------------------------------

async fn insert_test_provider(
    db: &DatabaseConnection,
    trust_idp_email: bool,
    jit_provisioning: bool,
) -> saml_providers::Model {
    saml_providers::ActiveModel {
        name: Set(format!("test-provider-{}", random_token())),
        template: Set("generic".to_string()),
        sp_entity_id: Set("https://temps.example.com/api/auth/saml/metadata/test".to_string()),
        idp_entity_id: Set("https://idp.example.com/metadata".to_string()),
        idp_sso_url: Set("https://idp.example.com/sso".to_string()),
        idp_x509_cert: Set(
            "-----BEGIN CERTIFICATE-----\nMII=\n-----END CERTIFICATE-----\n".to_string(),
        ),
        idp_metadata_url: Set(None),
        group_attribute: Set("groups".to_string()),
        role_attribute: Set("roles".to_string()),
        default_role: Set("user".to_string()),
        email_attribute: Set(None),
        jit_provisioning: Set(jit_provisioning),
        enabled: Set(true),
        trust_idp_email: Set(trust_idp_email),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

fn identity(subject: &str, email: &str) -> ExtractedIdentity {
    ExtractedIdentity {
        subject: subject.to_string(),
        email: email.to_string(),
        groups: vec![],
    }
}

#[tokio::test]
async fn start_login_stores_relay_state_and_authn_request_id() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service);

    let provider = insert_test_provider(db.as_ref(), true, true).await;
    let start = service
        .start_login(
            provider.id,
            "https://temps.example.com/api/auth/saml/acs",
            None,
        )
        .await
        .unwrap();
    assert!(start.redirect_url.contains("SAMLRequest="));

    let row = saml_login_states::Entity::find()
        .filter(saml_login_states::Column::ProviderId.eq(provider.id))
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("login state row must exist");
    assert!(!row.authn_request_id.is_empty());
    assert!(!row.relay_state.is_empty());
}

#[tokio::test]
async fn consume_login_state_is_atomic_second_call_fails() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service);
    let provider = insert_test_provider(db.as_ref(), true, true).await;

    let start_result = service
        .start_login(
            provider.id,
            "https://temps.example.com/api/auth/saml/acs",
            None,
        )
        .await
        .unwrap();
    let relay_state = start_result
        .redirect_url
        .split("RelayState=")
        .nth(1)
        .unwrap()
        .to_string();

    let first = service.consume_login_state(&relay_state).await;
    assert!(first.is_ok(), "first consume must succeed");

    let second = service.consume_login_state(&relay_state).await;
    assert!(
        matches!(second, Err(SamlError::StateNotFound { .. })),
        "second consume of the same relay_state must fail -- replay must not be possible"
    );
}

#[tokio::test]
async fn consume_login_state_rejects_expired() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let provider = insert_test_provider(db.as_ref(), true, true).await;

    let relay_state = random_token();
    saml_login_states::ActiveModel {
        relay_state: Set(relay_state.clone()),
        authn_request_id: Set("req-1".to_string()),
        provider_id: Set(provider.id),
        return_to: Set(None),
        expires_at: Set(Utc::now() - ChronoDuration::minutes(1)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service);
    let result = service.consume_login_state(&relay_state).await;
    assert!(matches!(result, Err(SamlError::StateExpired { .. })));
}

#[tokio::test]
async fn resolve_user_finds_by_provider_and_subject() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service.clone());
    let provider = insert_test_provider(db.as_ref(), true, true).await;

    let created = user_service
        .create_user(
            "Existing User".to_string(),
            "existing@example.com".to_string(),
            None,
            vec![RoleType::User],
        )
        .await
        .unwrap();
    let mut active: users::ActiveModel = users::Entity::find_by_id(created.user.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .into();
    active.saml_provider_id = Set(Some(provider.id));
    active.saml_subject = Set(Some("subj-existing".to_string()));
    active.update(db.as_ref()).await.unwrap();

    let resolved = service
        .resolve_user(
            &provider,
            &identity("subj-existing", "existing@example.com"),
            RoleType::User,
        )
        .await
        .unwrap();
    assert_eq!(resolved.user.id, created.user.id);
}

#[tokio::test]
async fn resolve_user_links_by_email_when_trust_true() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service.clone());
    let provider = insert_test_provider(db.as_ref(), true, true).await;

    let created = user_service
        .create_user(
            "Password User".to_string(),
            "linkme@example.com".to_string(),
            None,
            vec![RoleType::User],
        )
        .await
        .unwrap();

    let resolved = service
        .resolve_user(
            &provider,
            &identity("subj-new", "linkme@example.com"),
            RoleType::User,
        )
        .await
        .unwrap();
    assert_eq!(resolved.user.id, created.user.id);
    assert_eq!(resolved.user.saml_subject.as_deref(), Some("subj-new"));
    assert_eq!(resolved.user.saml_provider_id, Some(provider.id));
}

#[tokio::test]
async fn resolve_user_rejects_email_link_when_trust_false() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service.clone());
    let provider = insert_test_provider(db.as_ref(), false, true).await;

    user_service
        .create_user(
            "Password User".to_string(),
            "notrust@example.com".to_string(),
            None,
            vec![RoleType::User],
        )
        .await
        .unwrap();

    let result = service
        .resolve_user(
            &provider,
            &identity("subj-new", "notrust@example.com"),
            RoleType::User,
        )
        .await;
    assert!(matches!(result, Err(SamlError::EmailNotTrusted { .. })));
}

#[tokio::test]
async fn resolve_user_jit_provisions_when_enabled_and_trust_true() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service);
    let provider = insert_test_provider(db.as_ref(), true, true).await;

    let resolved = service
        .resolve_user(
            &provider,
            &identity("subj-jit", "jitnew@example.com"),
            RoleType::User,
        )
        .await
        .unwrap();
    assert_eq!(resolved.user.email, "jitnew@example.com");
    assert_eq!(resolved.user.saml_subject.as_deref(), Some("subj-jit"));
}

#[tokio::test]
async fn resolve_user_rejects_jit_when_trust_false() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service);
    let provider = insert_test_provider(db.as_ref(), false, true).await;

    let result = service
        .resolve_user(
            &provider,
            &identity("subj-jit", "jitnotrust@example.com"),
            RoleType::User,
        )
        .await;
    assert!(matches!(result, Err(SamlError::EmailNotTrusted { .. })));
}

#[tokio::test]
async fn resolve_user_rejects_jit_when_disabled() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.connection_arc();
    let user_service = Arc::new(UserService::new(db.clone()));
    user_service.initialize_roles().await.unwrap();
    let service = SamlService::new(db.clone(), user_service);
    let provider = insert_test_provider(db.as_ref(), true, false).await;

    let result = service
        .resolve_user(
            &provider,
            &identity("subj-jit", "jitdisabled@example.com"),
            RoleType::User,
        )
        .await;
    assert!(matches!(result, Err(SamlError::UserNotProvisioned { .. })));
}

// ------------------------------------------------------------------
// End-to-end: genuinely signed assertions via samael's own IdP
// signing capability. These exercise the real XML-DSig verification
// path (libxmlsec1) in the same binary that ships to production.
// ------------------------------------------------------------------

struct TestFixture {
    idp: IdentityProvider,
    provider: saml_providers::Model,
    sp_entity_id: String,
    acs_url: String,
}

fn build_fixture() -> TestFixture {
    let idp = IdentityProvider::generate_new(KeyType::Rsa(IdpRsa::Rsa2048)).unwrap();
    let cert_der = idp
        .create_certificate(&CertificateParams {
            common_name: "Test IdP",
            issuer_name: "Test IdP",
            days_until_expiration: 365,
        })
        .unwrap();
    let cert_body = samael::crypto::mime_encode_x509_cert(&cert_der).replace('\n', "");
    let cert_pem = base64_der_to_pem(&cert_body);

    let sp_entity_id = "https://temps.example.com/api/auth/saml/metadata/test".to_string();
    let acs_url = "https://temps.example.com/api/auth/saml/acs".to_string();

    let provider = saml_providers::Model {
        id: 1,
        name: "e2e-test".to_string(),
        template: "generic".to_string(),
        sp_entity_id: sp_entity_id.clone(),
        idp_entity_id: "https://idp.example.com/metadata".to_string(),
        idp_sso_url: "https://idp.example.com/sso".to_string(),
        idp_x509_cert: cert_pem,
        idp_metadata_url: None,
        group_attribute: "groups".to_string(),
        role_attribute: "roles".to_string(),
        default_role: "user".to_string(),
        email_attribute: Some("email".to_string()),
        jit_provisioning: true,
        enabled: true,
        trust_idp_email: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    TestFixture {
        idp,
        provider,
        sp_entity_id,
        acs_url,
    }
}

fn sign_test_response(
    fixture: &TestFixture,
    request_id: &str,
    name_id: &str,
    email: &str,
) -> String {
    use base64::Engine;
    let cert_der = fixture
        .idp
        .create_certificate(&CertificateParams {
            common_name: "Test IdP",
            issuer_name: "Test IdP",
            days_until_expiration: 365,
        })
        .unwrap();
    let signed = fixture
        .idp
        .sign_authn_response(
            &cert_der,
            name_id,
            &fixture.sp_entity_id,
            &fixture.acs_url,
            &fixture.provider.idp_entity_id,
            request_id,
            &[ResponseAttribute {
                required_attribute: RequiredAttribute {
                    name: "email".to_string(),
                    format: None,
                },
                value: email,
            }],
        )
        .unwrap();
    let xml = signed.to_string().unwrap();
    base64::engine::general_purpose::STANDARD.encode(xml.as_bytes())
}

#[test]
fn process_acs_response_accepts_genuinely_signed_assertion() {
    let fixture = build_fixture();
    let request_id = "req-happy-path";
    let response_b64 = sign_test_response(&fixture, request_id, "subj-e2e", "e2e@example.com");

    let sp = build_service_provider(&fixture.provider, &fixture.acs_url).unwrap();
    let assertion = sp
        .parse_base64_response(&response_b64, Some(&[request_id]))
        .expect("a genuinely signed, well-formed response must be accepted");

    let identity = extract_identity(&assertion, &fixture.provider).unwrap();
    assert_eq!(identity.subject, "subj-e2e");
    assert_eq!(identity.email, "e2e@example.com");
}

#[test]
fn process_acs_response_rejects_wrong_audience() {
    let fixture = build_fixture();
    let request_id = "req-wrong-audience";
    // Sign a response for a DIFFERENT SP entity ID than what our
    // ServiceProvider is configured with.
    use base64::Engine;
    let cert_der = fixture
        .idp
        .create_certificate(&CertificateParams {
            common_name: "Test IdP",
            issuer_name: "Test IdP",
            days_until_expiration: 365,
        })
        .unwrap();
    let signed = fixture
        .idp
        .sign_authn_response(
            &cert_der,
            "subj-e2e",
            "https://attacker-controlled-sp.example.com",
            &fixture.acs_url,
            &fixture.provider.idp_entity_id,
            request_id,
            &[],
        )
        .unwrap();
    let xml = signed.to_string().unwrap();
    let response_b64 = base64::engine::general_purpose::STANDARD.encode(xml.as_bytes());

    let sp = build_service_provider(&fixture.provider, &fixture.acs_url).unwrap();
    let result = sp.parse_base64_response(&response_b64, Some(&[request_id]));
    assert!(
        result.is_err(),
        "a response whose Audience doesn't match our sp_entity_id must be rejected"
    );
}

#[test]
fn process_acs_response_rejects_wrong_in_response_to() {
    let fixture = build_fixture();
    let response_b64 = sign_test_response(&fixture, "req-actual", "subj-e2e", "e2e@example.com");

    let sp = build_service_provider(&fixture.provider, &fixture.acs_url).unwrap();
    // The caller expects a DIFFERENT request id than what was signed --
    // simulates a login_state row for a different, unrelated login attempt.
    let result = sp.parse_base64_response(&response_b64, Some(&["req-different"]));
    assert!(
        result.is_err(),
        "InResponseTo mismatch must be rejected -- this is the check that stops cross-session assertion injection"
    );
}

#[test]
fn process_acs_response_rejects_tampered_name_id_after_signing() {
    // The XSW-relevant property under test: once signed, the response
    // cannot be modified without invalidating the signature over the
    // modified content. We tamper with the NameID text directly in the
    // signed XML string (the simplest possible tamper) and confirm the
    // tampered value can never reach a validated Assertion.
    let fixture = build_fixture();
    let request_id = "req-tamper";
    let cert_der = fixture
        .idp
        .create_certificate(&CertificateParams {
            common_name: "Test IdP",
            issuer_name: "Test IdP",
            days_until_expiration: 365,
        })
        .unwrap();
    let signed = fixture
        .idp
        .sign_authn_response(
            &cert_der,
            "subj-original",
            &fixture.sp_entity_id,
            &fixture.acs_url,
            &fixture.provider.idp_entity_id,
            request_id,
            &[],
        )
        .unwrap();
    let xml = signed.to_string().unwrap();
    assert!(
        xml.contains("subj-original"),
        "sanity check: unsigned NameID must be present in the serialized XML"
    );

    // Tamper: replace the legitimate subject with an attacker-chosen one.
    // This breaks the digest the signature covers.
    let tampered_xml = xml.replace("subj-original", "subj-attacker-injected");

    use base64::Engine;
    let tampered_b64 = base64::engine::general_purpose::STANDARD.encode(tampered_xml.as_bytes());

    let sp = build_service_provider(&fixture.provider, &fixture.acs_url).unwrap();
    let result = sp.parse_base64_response(&tampered_b64, Some(&[request_id]));
    assert!(
        result.is_err(),
        "a response tampered after signing must be rejected -- the tampered NameID must never reach a caller as a validated Assertion"
    );
}

#[test]
fn process_acs_response_rejects_unsigned_response() {
    let fixture = build_fixture();
    // A plausible-looking but entirely unsigned Response -- simulates an
    // attacker who skips the IdP entirely and POSTs directly to the ACS
    // endpoint with a hand-crafted, well-formed-but-unsigned document.
    let unsigned_xml = format!(
        r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol"
             xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion"
             ID="_unsigned" Version="2.0" IssueInstant="2026-01-01T00:00:00Z"
             Destination="{acs}" InResponseTo="req-unsigned">
          <saml:Issuer>{issuer}</saml:Issuer>
          <samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Success"/></samlp:Status>
          <saml:Assertion ID="_a1" IssueInstant="2026-01-01T00:00:00Z" Version="2.0">
            <saml:Issuer>{issuer}</saml:Issuer>
            <saml:Subject>
              <saml:NameID>attacker@example.com</saml:NameID>
              <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                <saml:SubjectConfirmationData Recipient="{acs}" InResponseTo="req-unsigned"/>
              </saml:SubjectConfirmation>
            </saml:Subject>
            <saml:Conditions>
              <saml:AudienceRestriction><saml:Audience>{audience}</saml:Audience></saml:AudienceRestriction>
            </saml:Conditions>
          </saml:Assertion>
        </samlp:Response>"#,
        acs = fixture.acs_url,
        issuer = fixture.provider.idp_entity_id,
        audience = fixture.sp_entity_id,
    );

    use base64::Engine;
    let response_b64 = base64::engine::general_purpose::STANDARD.encode(unsigned_xml.as_bytes());
    let sp = build_service_provider(&fixture.provider, &fixture.acs_url).unwrap();
    let result = sp.parse_base64_response(&response_b64, Some(&["req-unsigned"]));
    assert!(
        result.is_err(),
        "a completely unsigned response must never be accepted"
    );
}
