// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bunny DNS API adapter. Account credentials never leave the fixed API origin.
use super::{
    BunnyCredentials, DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord,
    DnsRecordContent, DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;
use async_trait::async_trait;
use reqwest::{header::HeaderValue, Method};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

pub struct BunnyProvider {
    client: reqwest::Client,
    key: HeaderValue,
    base: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Page {
    items: Vec<Zone>,
    has_more_items: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Zone {
    id: u64,
    domain: String,
    #[serde(default)]
    records: Vec<Record>,
    #[serde(default)]
    nameservers_detected: bool,
    #[serde(default)]
    nameserver1: String,
    #[serde(default)]
    nameserver2: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Record {
    #[serde(default)]
    id: u64,
    #[serde(rename = "Type")]
    kind: u8,
    ttl: u32,
    name: String,
    value: String,
    #[serde(default)]
    priority: u16,
    #[serde(default)]
    weight: u16,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    flags: u8,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    accelerated: bool,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}
impl BunnyProvider {
    pub fn new(credentials: BunnyCredentials) -> Result<Self, DnsError> {
        if credentials.api_key.trim().is_empty() {
            return Err(DnsError::InvalidCredentials(
                "Bunny DNS API key is empty".into(),
            ));
        }
        let mut key = HeaderValue::from_str(&credentials.api_key).map_err(|_| {
            DnsError::InvalidCredentials(
                "Bunny DNS API key contains invalid header characters".into(),
            )
        })?;
        key.set_sensitive(true);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| DnsError::ApiError("Failed to initialize Bunny DNS API client".into()))?;
        Ok(Self {
            client,
            key,
            base: "https://api.bunny.net".into(),
        })
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Record>,
    ) -> Result<reqwest::Response, DnsError> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header("AccessKey", self.key.clone());
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| DnsError::ApiError(format!("Bunny DNS request failed for {path}")))?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 | 403 => Err(DnsError::PermissionDenied(format!(
                "Bunny DNS key lacks access to {path}"
            ))),
            404 => Err(DnsError::RecordNotFound(path.into())),
            429 => Err(DnsError::RateLimited(format!(
                "Bunny DNS rate limited request for {path}"
            ))),
            status => Err(DnsError::ApiError(format!(
                "Bunny DNS request for {path} failed (HTTP {status})"
            ))),
        }
    }
    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Record>,
    ) -> Result<T, DnsError> {
        self.request(method, path, body)
            .await?
            .json()
            .await
            .map_err(|_| {
                DnsError::ApiError(format!("Bunny DNS returned an invalid response for {path}"))
            })
    }
    async fn zones(&self) -> Result<Vec<Zone>, DnsError> {
        let mut zones = Vec::new();
        for page in 1..=10000 {
            let result: Page = self
                .json(
                    Method::GET,
                    &format!("/dnszone?page={page}&perPage=100"),
                    None,
                )
                .await?;
            if result.has_more_items && result.items.is_empty() {
                return Err(DnsError::ApiError(format!(
                    "Bunny DNS pagination returned empty page {page} with more items"
                )));
            }
            zones.extend(result.items);
            if !result.has_more_items {
                return Ok(zones);
            }
        }
        Err(DnsError::ApiError(
            "Bunny DNS zone pagination exceeded 10000 pages".into(),
        ))
    }
    async fn zone(&self, domain: &str) -> Result<Zone, DnsError> {
        let domain = domain.trim_end_matches('.').to_lowercase();
        let zone = self
            .zones()
            .await?
            .into_iter()
            .filter(|z| {
                domain == z.domain.to_lowercase()
                    || domain.ends_with(&format!(".{}", z.domain.to_lowercase()))
            })
            .max_by_key(|z| z.domain.len())
            .ok_or_else(|| DnsError::ZoneNotFound(domain.clone()))?;
        let fetched: Zone = self
            .json(Method::GET, &format!("/dnszone/{}", zone.id), None)
            .await?;
        if fetched.id != zone.id || !fetched.domain.eq_ignore_ascii_case(&zone.domain) {
            return Err(DnsError::ApiError(format!(
                "Bunny DNS zone {} returned mismatched identity for {domain}",
                zone.id
            )));
        }
        Ok(fetched)
    }
    fn public_zone(zone: Zone) -> DnsZone {
        DnsZone {
            id: zone.id.to_string(),
            name: zone.domain,
            status: if zone.nameservers_detected {
                "active"
            } else {
                "pending"
            }
            .into(),
            nameservers: vec![zone.nameserver1, zone.nameserver2]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect(),
            metadata: HashMap::new(),
        }
    }
    fn convert(record: Record, domain: &str) -> Result<DnsRecord, DnsError> {
        let content = match record.kind {
            0 => DnsRecordContent::A {
                address: record.value,
            },
            1 => DnsRecordContent::AAAA {
                address: record.value,
            },
            2 => DnsRecordContent::CNAME {
                target: record.value,
            },
            3 => DnsRecordContent::TXT {
                content: record.value,
            },
            4 => DnsRecordContent::MX {
                priority: record.priority,
                target: record.value,
            },
            8 => DnsRecordContent::SRV {
                priority: record.priority,
                weight: record.weight,
                port: record.port,
                target: record.value,
            },
            9 => DnsRecordContent::CAA {
                flags: record.flags,
                tag: record.tag,
                value: record.value,
            },
            10 => DnsRecordContent::PTR {
                target: record.value,
            },
            12 => DnsRecordContent::NS {
                nameserver: record.value,
            },
            kind => {
                return Err(DnsError::Validation(format!(
                    "Bunny DNS record {} in {domain} has unsupported type {kind}",
                    record.id
                )))
            }
        };
        let name = if record.name.is_empty() {
            "@".into()
        } else {
            record.name
        };
        let fqdn = if name == "@" {
            domain.into()
        } else {
            format!("{name}.{domain}")
        };
        let mut metadata = HashMap::new();
        if let Some(comment) = record.comment {
            metadata.insert("comment".into(), comment);
        }
        metadata.insert("disabled".into(), record.disabled.to_string());
        metadata.insert("accelerated".into(), record.accelerated.to_string());
        Ok(DnsRecord {
            id: Some(record.id.to_string()),
            zone: domain.into(),
            name,
            fqdn,
            content,
            ttl: record.ttl,
            proxied: record.accelerated,
            metadata,
        })
    }
    fn payload(request: DnsRecordRequest, domain: &str) -> Result<Record, DnsError> {
        if request.proxied {
            return Err(DnsError::Validation(format!("Bunny DNS record {} in {domain}: CDN acceleration must be configured through delivery settings",request.name)));
        }
        let ttl = request.ttl.unwrap_or(300);
        let ttl = if ttl == 1 { 300 } else { ttl };
        if !(30..=86400).contains(&ttl) {
            return Err(DnsError::Validation(format!(
                "Bunny DNS record {} in {domain} requires TTL between 30 and 86400 seconds",
                request.name
            )));
        }
        let mut r = Record {
            id: 0,
            kind: 0,
            ttl,
            name: if request.name == "@" {
                String::new()
            } else {
                request.name
            },
            value: String::new(),
            priority: 0,
            weight: 0,
            port: 0,
            flags: 0,
            tag: String::new(),
            comment: Some("Managed by Temps".into()),
            disabled: false,
            accelerated: false,
            extra: HashMap::new(),
        };
        match request.content {
            DnsRecordContent::A { address } => {
                address.parse::<std::net::Ipv4Addr>().map_err(|_| {
                    DnsError::Validation(format!("Invalid Bunny DNS IPv4 record in {domain}"))
                })?;
                r.value = address;
            }
            DnsRecordContent::AAAA { address } => {
                address.parse::<std::net::Ipv6Addr>().map_err(|_| {
                    DnsError::Validation(format!("Invalid Bunny DNS IPv6 record in {domain}"))
                })?;
                r.kind = 1;
                r.value = address;
            }
            DnsRecordContent::CNAME { target } => {
                r.kind = 2;
                r.value = target;
            }
            DnsRecordContent::TXT { content } => {
                r.kind = 3;
                r.value = content;
            }
            DnsRecordContent::MX { priority, target } => {
                r.kind = 4;
                r.priority = priority;
                r.value = target;
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => {
                r.kind = 8;
                r.priority = priority;
                r.weight = weight;
                r.port = port;
                r.value = target;
            }
            DnsRecordContent::CAA { flags, tag, value } => {
                r.kind = 9;
                r.flags = flags;
                r.tag = tag;
                r.value = value;
            }
            DnsRecordContent::PTR { target } => {
                r.kind = 10;
                r.value = target;
            }
            DnsRecordContent::NS { nameserver } => {
                r.kind = 12;
                r.value = nameserver;
            }
        }
        Ok(r)
    }
    fn record_id(id: &str) -> Result<u64, DnsError> {
        id.parse()
            .map_err(|_| DnsError::Validation(format!("Invalid Bunny DNS record ID {id}")))
    }
}
#[async_trait]
impl DnsProvider for BunnyProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Bunny
    }
    fn capabilities(&self) -> DnsProviderCapabilities {
        DnsProviderCapabilities {
            a_record: true,
            aaaa_record: true,
            cname_record: true,
            txt_record: true,
            mx_record: true,
            ns_record: true,
            srv_record: true,
            caa_record: true,
            wildcard: true,
            ..Default::default()
        }
    }
    async fn test_connection(&self) -> Result<bool, DnsError> {
        self.json::<Page>(Method::GET, "/dnszone?page=1&perPage=5", None)
            .await?;
        Ok(true)
    }
    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        Ok(self
            .zones()
            .await?
            .into_iter()
            .map(Self::public_zone)
            .collect())
    }
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        match self.zone(domain).await {
            Ok(z) => Ok(Some(Self::public_zone(z))),
            Err(DnsError::ZoneNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }
    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let z = self.zone(domain).await?;
        z.records
            .into_iter()
            .map(|r| Self::convert(r, &z.domain))
            .collect()
    }
    async fn get_record(
        &self,
        domain: &str,
        name: &str,
        kind: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError> {
        Ok(self
            .get_records(domain, name, kind)
            .await?
            .into_iter()
            .next())
    }
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let body = Self::payload(request, domain)?;
        let z = self.zone(domain).await?;
        let record = self
            .json(
                Method::PUT,
                &format!("/dnszone/{}/records", z.id),
                Some(&body),
            )
            .await?;
        Self::convert(record, &z.domain)
    }
    async fn update_record(
        &self,
        domain: &str,
        id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let id = Self::record_id(id)?;
        let mut body = Self::payload(request, domain)?;
        let z = self.zone(domain).await?;
        let existing = z
            .records
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| DnsError::RecordNotFound(id.to_string()))?;
        if existing.accelerated || existing.disabled {
            return Err(DnsError::Validation(format!("Bunny DNS record {id} in {domain} is accelerated or disabled; change these settings in Bunny before updating through Temps")));
        }
        body.id = id;
        body.comment = existing.comment;
        body.disabled = existing.disabled;
        body.accelerated = existing.accelerated;
        body.extra = existing.extra;
        let record = self
            .json(
                Method::POST,
                &format!("/dnszone/{}/records/{id}", z.id),
                Some(&body),
            )
            .await?;
        Self::convert(record, &z.domain)
    }
    async fn delete_record(&self, domain: &str, id: &str) -> Result<(), DnsError> {
        let id = Self::record_id(id)?;
        let z = self.zone(domain).await?;
        if !z.records.iter().any(|r| r.id == id) {
            return Err(DnsError::RecordNotFound(id.to_string()));
        }
        self.request(
            Method::DELETE,
            &format!("/dnszone/{}/records/{id}", z.id),
            None,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderCredentials;
    use serde_json::json;
    use wiremock::{
        matchers::{body_partial_json, header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };
    fn fixture_record(id: u64, kind: u8) -> serde_json::Value {
        json!({"Id":id,"Type":kind,"Ttl":300,"Name":"www","Value":"192.0.2.1","Comment":"foreign owner","Priority":0,"Weight":0,"Port":0,"Flags":0,"Tag":""})
    }
    fn fixture_zone(id: u64, domain: &str, records: Vec<serde_json::Value>) -> serde_json::Value {
        json!({"Id":id,"Domain":domain,"Records":records,"NameserversDetected":true,"Nameserver1":"ns1.example.net","Nameserver2":"ns2.example.net"})
    }
    fn request() -> DnsRecordRequest {
        DnsRecordRequest {
            name: "www".into(),
            content: DnsRecordContent::A {
                address: "192.0.2.2".into(),
            },
            ttl: Some(300),
            proxied: false,
        }
    }
    fn provider(server: &MockServer) -> BunnyProvider {
        let mut p = BunnyProvider::new(BunnyCredentials {
            api_key: "test-secret-key".into(),
        })
        .unwrap();
        p.base = server.uri();
        p
    }
    async fn zone_mocks(server: &MockServer, records: Vec<serde_json::Value>) {
        let z = fixture_zone(10, "example.com", records);
        Mock::given(method("GET"))
            .and(path("/dnszone"))
            .and(header("AccessKey", "test-secret-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Items":[z.clone()],"HasMoreItems":false})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(z))
            .mount(server)
            .await;
    }
    #[test]
    fn credentials_and_validation() {
        let creds = BunnyCredentials {
            api_key: "test-secret-key".into(),
        };
        assert!(!format!("{creds:?}").contains("test-secret-key"));
        assert_eq!(
            ProviderCredentials::Bunny(creds.clone()).masked()["api_key"],
            "***"
        );
        assert!(BunnyProvider::new(BunnyCredentials {
            api_key: "\n".into()
        })
        .is_err());
        assert_eq!(
            DnsProviderType::from_str("bunny.net").unwrap(),
            DnsProviderType::Bunny
        );
        assert_eq!(
            DnsProviderType::Bunny.required_credentials(),
            vec!["api_key"]
        );
        let mut r = request();
        r.proxied = true;
        assert!(BunnyProvider::payload(r, "example.com").is_err());
        let mut r = request();
        r.ttl = Some(2);
        assert!(BunnyProvider::payload(r, "example.com").is_err());
        assert!(BunnyProvider::record_id("../../foreign").is_err());
    }
    #[tokio::test]
    async fn pagination_and_longest_zone() {
        let server = MockServer::start().await;
        for (page, z, more) in [
            (1, fixture_zone(1, "example.com", vec![]), true),
            (2, fixture_zone(2, "sub.example.com", vec![]), false),
        ] {
            Mock::given(method("GET"))
                .and(path("/dnszone"))
                .and(query_param("page", page.to_string()))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"Items":[z],"HasMoreItems":more})),
                )
                .expect(2)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/dnszone/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_zone(
                2,
                "sub.example.com",
                vec![],
            )))
            .expect(1)
            .mount(&server)
            .await;
        let p = provider(&server);
        assert_eq!(p.list_zones().await.unwrap().len(), 2);
        assert_eq!(
            p.get_zone("app.sub.example.com.")
                .await
                .unwrap()
                .unwrap()
                .id,
            "2"
        );
    }
    #[tokio::test]
    async fn create_update_delete_preserves_provider_fields() {
        let server = MockServer::start().await;
        let mut existing = fixture_record(20, 0);
        existing["MonitorType"] = json!(2);
        zone_mocks(&server, vec![existing.clone()]).await;
        Mock::given(method("PUT")).and(path("/dnszone/10/records")).and(body_partial_json(json!({"Type":0,"Name":"www","Value":"192.0.2.2","Ttl":300,"Comment":"Managed by Temps"}))).respond_with(ResponseTemplate::new(201).set_body_json(fixture_record(21,0))).expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/dnszone/10/records/20"))
            .and(body_partial_json(
                json!({"Value":"192.0.2.2","Comment":"foreign owner","MonitorType":2}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(existing))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/dnszone/10/records/20"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let p = provider(&server);
        assert_eq!(
            p.create_record("example.com", request())
                .await
                .unwrap()
                .id
                .as_deref(),
            Some("21")
        );
        assert_eq!(
            p.update_record("example.com", "20", request())
                .await
                .unwrap()
                .metadata["comment"],
            "foreign owner"
        );
        p.delete_record("example.com", "20").await.unwrap();
        assert!(matches!(
            p.delete_record("example.com", "999").await,
            Err(DnsError::RecordNotFound(_))
        ));
    }
    #[tokio::test]
    async fn permission_transport_and_upstream_errors_are_sanitized() {
        for status in [401, 403, 429, 500, 302] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(status).set_body_string("test-secret-key upstream debug"),
                )
                .mount(&server)
                .await;
            let error = provider(&server).test_connection().await.unwrap_err();
            assert!(!error.to_string().contains("test-secret-key"));
            if status == 401 || status == 403 {
                assert!(matches!(error, DnsError::PermissionDenied(_)));
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("invalid test-secret-key"))
            .mount(&server)
            .await;
        assert!(!provider(&server)
            .test_connection()
            .await
            .unwrap_err()
            .to_string()
            .contains("test-secret-key"));
    }
    #[tokio::test]
    async fn unknown_types_fail_closed_and_disabled_foreign_records_remain_visible() {
        let server = MockServer::start().await;
        let mut disabled = fixture_record(20, 0);
        disabled["Disabled"] = json!(true);
        zone_mocks(&server, vec![disabled]).await;
        let records = provider(&server)
            .get_records("example.com", "www", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].metadata["disabled"], "true");
        let server = MockServer::start().await;
        zone_mocks(&server, vec![fixture_record(21, 7)]).await;
        assert!(matches!(
            provider(&server).list_records("example.com").await,
            Err(DnsError::Validation(_))
        ));
    }
    #[tokio::test]
    async fn missing_zone_and_broken_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"Items":[],"HasMoreItems":false})),
            )
            .mount(&server)
            .await;
        assert!(provider(&server)
            .get_zone("missing.example")
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            provider(&server).check_zone_access("missing.example").await,
            Err(DnsError::ZoneNotFound(_))
        ));
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"Items":[],"HasMoreItems":true})),
            )
            .mount(&server)
            .await;
        assert!(provider(&server).list_zones().await.is_err());
    }
    #[tokio::test]
    async fn unsafe_updates_and_mismatched_zones_never_mutate() {
        for flag in ["Accelerated", "Disabled"] {
            let server = MockServer::start().await;
            let mut record = fixture_record(20, 0);
            record[flag] = json!(true);
            zone_mocks(&server, vec![record]).await;
            let result = provider(&server)
                .update_record("example.com", "20", request())
                .await;
            assert!(matches!(result, Err(DnsError::Validation(_))));
            assert!(server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == "GET"));
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dnszone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"Items":[fixture_zone(10,"example.com",vec![])],"HasMoreItems":false}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_zone(
                11,
                "foreign.example",
                vec![],
            )))
            .mount(&server)
            .await;
        assert!(matches!(
            provider(&server)
                .create_record("example.com", request())
                .await,
            Err(DnsError::ApiError(_))
        ));
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET"));
    }

    #[test]
    fn structured_records_roundtrip() {
        for content in [
            DnsRecordContent::MX {
                priority: 10,
                target: "mail.example.com".into(),
            },
            DnsRecordContent::SRV {
                priority: 20,
                weight: 30,
                port: 443,
                target: "service.example.com".into(),
            },
            DnsRecordContent::CAA {
                flags: 0,
                tag: "issue".into(),
                value: "ca.example".into(),
            },
            DnsRecordContent::NS {
                nameserver: "ns.example.com".into(),
            },
            DnsRecordContent::PTR {
                target: "host.example.com".into(),
            },
            DnsRecordContent::TXT {
                content: "opaque challenge".into(),
            },
            DnsRecordContent::AAAA {
                address: "2001:db8::1".into(),
            },
            DnsRecordContent::CNAME {
                target: "target.example.com".into(),
            },
        ] {
            let expected = content.to_value_string();
            let mut r = request();
            r.content = content;
            let r = BunnyProvider::payload(r, "example.com").unwrap();
            assert_eq!(
                BunnyProvider::convert(r, "example.com")
                    .unwrap()
                    .content
                    .to_value_string(),
                expected
            );
        }
    }
}
