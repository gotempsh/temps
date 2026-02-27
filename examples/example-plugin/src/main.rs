//! Example external plugin for Temps.
//!
//! A simple "Cron Jobs" plugin that demonstrates the plugin SDK.
//! Does NOT require Postgres — works standalone for testing.
//!
//! ## Test it manually
//!
//! ```bash
//! # Terminal 1: build and run the plugin
//! cargo build -p temps-example-plugin
//! ./target/debug/temps-example-plugin \
//!     --socket-path /tmp/temps-cron.sock \
//!     --database-url sqlite::memory: \
//!     --auth-secret test-secret \
//!     --data-dir /tmp/temps-cron-data
//!
//! # Terminal 2: talk to it over the unix socket
//! curl --unix-socket /tmp/temps-cron.sock http://localhost/health
//! curl --unix-socket /tmp/temps-cron.sock http://localhost/cron-jobs
//! curl --unix-socket /tmp/temps-cron.sock http://localhost/cron-jobs/42
//! curl --unix-socket /tmp/temps-cron.sock -X POST http://localhost/cron-jobs \
//!     -H 'Content-Type: application/json' \
//!     -d '{"name":"nightly build","schedule":"0 3 * * *","command":"make build"}'
//! curl --unix-socket /tmp/temps-cron.sock http://localhost/stats
//! ```

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use temps_plugin_sdk::prelude::*;
use temps_plugin_sdk::protocol::TempsAuth;

// ============================================================================
// Plugin Definition
// ============================================================================

#[derive(Default)]
struct CronPlugin;

impl ExternalPlugin for CronPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::builder("cron-jobs", "0.1.0")
            .display_name("Cron Jobs")
            .description("Schedule and manage recurring tasks for your projects")
            .requires_db(false) // No Postgres needed for this demo
            .nav(NavEntry {
                label: "Cron Jobs".into(),
                icon: "clock".into(),
                section: NavSection::Platform,
                path: "/cron-jobs".into(),
                order: 45,
            })
            .nav(NavEntry {
                label: "Cron Settings".into(),
                icon: "settings".into(),
                section: NavSection::Settings,
                path: "/cron-settings".into(),
                order: 55,
            })
            .build()
    }

    fn router(&self, ctx: PluginContext) -> axum::Router {
        // In-memory store for demo purposes
        let store = Arc::new(Mutex::new(vec![
            CronJob {
                id: 1,
                name: "Daily backup".into(),
                schedule: "0 0 * * *".into(),
                command: "backup --full".into(),
                enabled: true,
                created_at: "2026-01-15T10:00:00Z".into(),
            },
            CronJob {
                id: 2,
                name: "Cache cleanup".into(),
                schedule: "*/15 * * * *".into(),
                command: "cache:clear".into(),
                enabled: false,
                created_at: "2026-01-20T14:30:00Z".into(),
            },
        ]));

        axum::Router::new()
            .route("/cron-jobs", get(list_cron_jobs).post(create_cron_job))
            .route("/cron-jobs/{id}", get(get_cron_job).delete(delete_cron_job))
            .route("/stats", get(get_stats))
            .with_state(AppState { ctx, store })
    }

    fn on_start(&self, ctx: &PluginContext) -> Result<(), PluginSdkError> {
        tracing::info!(
            plugin = ctx.plugin_name(),
            data_dir = %ctx.data_dir().display(),
            "Cron Jobs plugin starting up"
        );
        Ok(())
    }
}

// Generate the main() function
temps_plugin_sdk::main!(CronPlugin);

// ============================================================================
// State & Types
// ============================================================================

#[derive(Clone)]
struct AppState {
    ctx: PluginContext,
    store: Arc<Mutex<Vec<CronJob>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronJob {
    id: i32,
    name: String,
    schedule: String,
    command: String,
    enabled: bool,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct CreateCronJobRequest {
    name: String,
    schedule: String,
    command: String,
}

#[derive(Debug, Serialize)]
struct StatsResponse {
    total_jobs: usize,
    plugin_name: String,
    plugin_data_dir: String,
}

// ============================================================================
// Handlers
// ============================================================================

async fn list_cron_jobs(
    TempsAuth(user): TempsAuth,
    State(state): State<AppState>,
) -> impl IntoResponse {
    tracing::info!(user_id = ?user.user_id, role = %user.role, "Listing cron jobs");
    let jobs = state.store.lock().unwrap().clone();
    Json(jobs)
}

async fn create_cron_job(
    TempsAuth(user): TempsAuth,
    State(state): State<AppState>,
    Json(req): Json<CreateCronJobRequest>,
) -> impl IntoResponse {
    tracing::info!(user_id = ?user.user_id, name = %req.name, "Creating cron job");

    let mut store = state.store.lock().unwrap();
    let next_id = store.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = CronJob {
        id: next_id,
        name: req.name,
        schedule: req.schedule,
        command: req.command,
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    store.push(job.clone());

    (StatusCode::CREATED, Json(job))
}

async fn get_cron_job(State(state): State<AppState>, Path(id): Path<i32>) -> impl IntoResponse {
    let store = state.store.lock().unwrap();
    match store.iter().find(|j| j.id == id) {
        Some(job) => Json(job.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, format!("Cron job {} not found", id)).into_response(),
    }
}

async fn delete_cron_job(
    TempsAuth(user): TempsAuth,
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> impl IntoResponse {
    tracing::info!(user_id = ?user.user_id, cron_job_id = id, "Deleting cron job");
    let mut store = state.store.lock().unwrap();
    let before = store.len();
    store.retain(|j| j.id != id);
    if store.len() < before {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, format!("Cron job {} not found", id)).into_response()
    }
}

async fn get_stats(State(state): State<AppState>) -> impl IntoResponse {
    let count = state.store.lock().unwrap().len();
    Json(StatsResponse {
        total_jobs: count,
        plugin_name: state.ctx.plugin_name().to_string(),
        plugin_data_dir: state.ctx.data_dir().display().to_string(),
    })
}
