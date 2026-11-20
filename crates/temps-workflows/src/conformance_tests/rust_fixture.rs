// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use temps_workflows_sdk::{Client, Context, Error, Executor, StepOptions, Workflow};

struct Fixture {
    effects: HashMap<String, u32>,
    variant: String,
    gate: tokio::sync::watch::Sender<bool>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            effects: HashMap::new(),
            variant: "a".into(),
            gate: tokio::sync::watch::channel(false).0,
        }
    }
}
type Shared = Arc<Mutex<Fixture>>;
struct Definition {
    name: &'static str,
    state: Shared,
}
fn effect(state: &Shared, key: &str) -> Value {
    let mut state = state.lock().unwrap();
    let value = state.effects.entry(key.into()).or_default();
    *value += 1;
    json!(*value)
}
async fn counted(
    context: &mut Context,
    state: &Shared,
    name: &str,
    key: &str,
) -> Result<Value, Error> {
    context
        .step(name, StepOptions::default(), |_| async {
            Ok(effect(state, key))
        })
        .await
}
#[async_trait]
impl Workflow for Definition {
    fn name(&self) -> &str {
        self.name
    }
    async fn run(&self, c: &mut Context) -> Result<Value, Error> {
        match self.name {
            "conf-replay" => {
                let a = counted(c, &self.state, "a", "conf-replay/a").await?;
                c.sleep("pause", Duration::from_millis(1500)).await?;
                let b = counted(c, &self.state, "b", "conf-replay/b").await?;
                let now = c.now("now").await?;
                let now = chrono::DateTime::parse_from_rfc3339(now.as_str().unwrap())
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                Ok(json!({"a":a,"b":b,"now":now,"random":c.random("random").await?}))
            }
            "conf-retry" => {
                c.step("flaky", StepOptions::default(), |attempt| async move {
                    effect(&self.state, "conf-retry/flaky");
                    if attempt < 3 {
                        Err(Error::Step {
                            name: "flaky".into(),
                            message: "upstream failed".into(),
                        })
                    } else {
                        Ok(json!({"attempt":attempt}))
                    }
                })
                .await
            }
            "conf-retry-exhausted" => {
                c.step(
                    "always-fails",
                    StepOptions {
                        retries: 1,
                        ..Default::default()
                    },
                    |_| async {
                        effect(&self.state, "conf-retry-exhausted/always-fails");
                        Err(Error::Step {
                            name: "always-fails".into(),
                            message: "the upstream is down".into(),
                        })
                    },
                )
                .await
            }
            "conf-suspend" => {
                let input = c.input.clone().unwrap();
                let approval = c
                    .wait_for_event(
                        "approval",
                        "conf.approved",
                        Some(input["key"].as_str().unwrap().into()),
                        Some(Duration::from_millis(
                            input["timeoutMs"].as_u64().unwrap_or(3600000),
                        )),
                    )
                    .await?;
                Ok(
                    json!({"approval":approval,"after":counted(c,&self.state,"after","conf-suspend/after").await?}),
                )
            }
            "conf-incompatible" => {
                let name = if self.state.lock().unwrap().variant == "a" {
                    "one"
                } else {
                    "uno"
                };
                counted(c, &self.state, name, &format!("conf-incompatible/{name}")).await?;
                c.sleep("gap", Duration::from_millis(1500)).await?;
                Ok(json!("done"))
            }
            "conf-gated" => {
                c.step("gated", StepOptions::default(), |_| async {
                    let entered = effect(&self.state, "conf-gated/entered");
                    let mut gate = self.state.lock().unwrap().gate.subscribe();
                    while !*gate.borrow_and_update() {
                        gate.changed().await.unwrap();
                    }
                    Ok(json!({"entered":entered}))
                })
                .await
            }
            "conf-idempotent" => counted(c, &self.state, "once", "conf-idempotent/once").await,
            "conf-parent" => {
                let before = counted(c, &self.state, "before", "conf-parent/before").await?;
                let child = c.child("square", "conf-child", c.input.clone()).await?;
                Ok(json!({"before":before,"child":child}))
            }
            "conf-child" => {
                let input = c.input.clone().unwrap();
                c.step(
                    "square",
                    StepOptions {
                        retries: 0,
                        ..Default::default()
                    },
                    |_| async {
                        effect(&self.state, "conf-child/square");
                        if input["fail"].as_bool().unwrap_or(false) {
                            return Err(Error::Step {
                                name: "square".into(),
                                message: "the child cannot square that".into(),
                            });
                        }
                        Ok(json!(input["n"].as_i64().unwrap().pow(2)))
                    },
                )
                .await
            }
            _ => unreachable!(),
        }
    }
}
async fn state(State(state): State<Shared>) -> Json<HashMap<String, u32>> {
    Json(state.lock().unwrap().effects.clone())
}
async fn change(State(state): State<Shared>, Path(action): Path<String>) -> Json<bool> {
    let mut state = state.lock().unwrap();
    match action.as_str() {
        "reset" => *state = Fixture::default(),
        "gate" => {
            state.gate.send_replace(true);
        }
        "variant/a" => state.variant = "a".into(),
        "variant/b" => state.variant = "b".into(),
        _ => {}
    }
    Json(true)
}
pub(super) async fn start(api: &str, token: &str, port: u16) -> tokio::task::JoinHandle<()> {
    let state = Arc::new(Mutex::new(Fixture::default()));
    let definitions = [
        "conf-replay",
        "conf-retry",
        "conf-retry-exhausted",
        "conf-suspend",
        "conf-incompatible",
        "conf-gated",
        "conf-idempotent",
        "conf-parent",
        "conf-child",
    ]
    .into_iter()
    .map(|name| {
        Arc::new(Definition {
            name,
            state: state.clone(),
        }) as Arc<dyn Workflow>
    })
    .collect();
    let client = Client::new(api, token.into()).unwrap();
    let router = Executor::new(&client, definitions)
        .await
        .unwrap()
        .router()
        .merge(
            Router::new()
                .route("/__fixture/state", get(self::state))
                .route("/__fixture/{*action}", post(change))
                .with_state(state),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    })
}
