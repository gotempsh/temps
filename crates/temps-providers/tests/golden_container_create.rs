// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Golden captures of the container-create requests the managed-service
//! engines send on the control plane (ADR-055 phase 0).
//!
//! Each engine's real create path runs against a fake Engine API
//! (`temps-docker-capture`) that records every `POST /containers/create`
//! and fails it. Phase 4 moves databases onto the Docker driver; these
//! fixtures say what that driver must send, and where the worker path
//! (`temps-node-agent`'s `create_service`) differs today (defect 1).
//! `TEMPS_UPDATE_GOLDEN=1` rewrites them.

// Integration-test crate: clippy's `allow-*-in-tests` only covers `#[test]`
// fns and `cfg(test)` modules, not the fixture helpers here, where a panic is
// the intended failure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test fixtures; a panic fails the test"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use temps_docker_capture::{assert_golden, golden_value, FakeDocker, Method};
use temps_providers::externalsvc::mariadb::MariaDbService;
use temps_providers::externalsvc::mongodb::MongodbService;
use temps_providers::externalsvc::postgres::PostgresService;
use temps_providers::externalsvc::redis::RedisService;
use temps_providers::externalsvc::{ExternalService, ServiceConfig, ServiceType};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(format!("{name}.json"))
}

/// A fake where images are present and the workload network exists.
async fn fake() -> FakeDocker {
    let fake = FakeDocker::start()
        .await
        .expect("start the fake Engine API");
    fake.respond(Method::GET, "/images/", 200, json!({}));
    fake.respond(
        Method::GET,
        "/networks",
        200,
        json!([{ "Name": temps_core::NETWORK_NAME.as_str() }]),
    );
    fake
}

fn config(service_type: ServiceType, parameters: Value) -> ServiceConfig {
    ServiceConfig {
        name: "golden".to_string(),
        service_type,
        version: None,
        parameters,
    }
}

fn recorded(fake: &FakeDocker) -> Value {
    Value::Array(fake.creates().iter().map(golden_value).collect())
}

#[tokio::test]
async fn golden_postgres() {
    let fake = fake().await;
    let service = PostgresService::new("golden".to_string(), Arc::new(fake.docker()));
    let result = service
        .init(config(
            ServiceType::Postgres,
            json!({ "port": "15432", "password": "testpass123" }),
        ))
        .await;
    assert!(result.is_err(), "the fake fails every create");
    // The WAL-G probe is created first; its failure is tolerated.
    assert_eq!(fake.creates().len(), 2, "{:?}", fake.calls());
    assert_golden(&fixture("postgres"), &recorded(&fake));
}

#[tokio::test]
async fn golden_redis() {
    let fake = fake().await;
    let service = RedisService::new("golden".to_string(), Arc::new(fake.docker()));
    let result = service
        .init(config(
            ServiceType::Redis,
            json!({ "port": "16379", "password": "testpass123" }),
        ))
        .await;
    assert!(result.is_err(), "the fake fails every create");
    assert_eq!(fake.creates().len(), 1, "{:?}", fake.calls());
    assert_golden(&fixture("redis"), &recorded(&fake));
}

#[tokio::test]
async fn golden_mariadb() {
    let fake = fake().await;
    let service = MariaDbService::new("golden".to_string(), Arc::new(fake.docker()));
    let result = service
        .init(config(
            ServiceType::Mariadb,
            json!({ "port": "13306", "password": "testpass123", "root_password": "rootpass123" }),
        ))
        .await;
    assert!(result.is_err(), "the fake fails every create");
    assert_eq!(fake.creates().len(), 1, "{:?}", fake.calls());
    assert_golden(&fixture("mariadb"), &recorded(&fake));
}

#[tokio::test]
async fn golden_mongodb() {
    let fake = fake().await;
    let service = MongodbService::new("golden".to_string(), Arc::new(fake.docker()));
    service
        .init(config(
            ServiceType::Mongodb,
            json!({ "port": "17017", "password": "testpass123" }),
        ))
        .await
        .expect("init makes no Docker call");
    assert!(
        service.start().await.is_err(),
        "the fake fails every create"
    );
    assert_eq!(fake.creates().len(), 1, "{:?}", fake.calls());
    assert_golden(&fixture("mongodb"), &recorded(&fake));
}
