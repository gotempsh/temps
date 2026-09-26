// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Small conformance client for `bunx @temps-sdk/cli plugin dev`.
use std::sync::{Arc, Mutex};
use temps_plugin_sdk::prelude::*;

#[derive(Default)]
struct Probe {
    events: Arc<Mutex<Vec<PluginEvent>>>,
}

impl ExternalPlugin for Probe {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::builder("rust-dev-probe", "0.1.0")
            .event("deployment.*")
            .host_permissions(vec![
                temps_core::external_plugin::channel::PluginHostPermission::EventsRead,
                temps_core::external_plugin::channel::PluginHostPermission::ProjectsRead,
            ])
            .build()
    }

    fn router(&self, ctx: PluginContext) -> axum::Router {
        let events = self.events.clone();
        let project_ctx = ctx.clone();
        axum::Router::new()
            .route(
                "/ui/",
                axum::routing::get(|| async {
                    axum::response::Html("<h1>Rust plugin development probe</h1>")
                }),
            )
            .route(
                "/capabilities",
                axum::routing::get(move || {
                    let ctx = ctx.clone();
                    async move {
                        ctx.permissions().await.map(axum::Json).map_err(|error| {
                            (
                                axum::http::StatusCode::BAD_GATEWAY,
                                format!("Rust probe cannot query host capabilities: {error}"),
                            )
                        })
                    }
                }),
            )
            .route(
                "/project",
                axum::routing::get(move || {
                    let ctx = project_ctx.clone();
                    async move {
                        ctx.temps()
                            .get_project(1)
                            .await
                            .map(axum::Json)
                            .map_err(|error| {
                                (
                                    axum::http::StatusCode::BAD_GATEWAY,
                                    format!("Rust probe cannot query project fixture 1: {error}"),
                                )
                            })
                    }
                }),
            )
            .route(
                "/events",
                axum::routing::get(move || {
                    let events = events.clone();
                    async move {
                        events
                            .lock()
                            .map(|events| axum::Json(events.clone()))
                            .map_err(|_| {
                                (
                                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                    "Rust probe event log lock was poisoned",
                                )
                            })
                    }
                }),
            )
    }

    fn on_event(&self, _ctx: &PluginContext, event: PluginEvent) {
        self.remember(event);
    }
}

impl Probe {
    fn remember(&self, event: PluginEvent) {
        match self.events.lock() {
            Ok(mut events) => {
                if !events.iter().any(|saved| saved.id == event.id) {
                    if events.len() >= 100 {
                        events.remove(0);
                    }
                    events.push(event);
                }
            }
            Err(_) => eprintln!(
                "Rust probe cannot store event {}: event log lock was poisoned",
                event.id
            ),
        }
    }
}

temps_plugin_sdk::main!(Probe);

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: usize) -> PluginEvent {
        serde_json::from_value(serde_json::json!({
            "id": id.to_string(), "event_type": "deployment.succeeded",
            "timestamp": "2026-01-01T00:00:00Z", "project_id": 1,
            "data": { "deployment_id": id }
        }))
        .expect("valid example event")
    }

    #[test]
    fn duplicate_delivery_is_not_recorded_twice() {
        let probe = Probe::default();
        probe.remember(event(1));
        probe.remember(event(1));
        assert_eq!(probe.events.lock().expect("event log").len(), 1);
    }

    #[test]
    fn event_log_is_bounded() {
        let probe = Probe::default();
        for id in 0..101 {
            probe.remember(event(id));
        }
        let events = probe.events.lock().expect("event log");
        assert_eq!(events.len(), 100);
        assert_eq!(events[0].id, "1");
    }
}
