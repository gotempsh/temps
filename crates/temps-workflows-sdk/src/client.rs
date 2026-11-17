// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{Error, Event, Run, StartedRun, Target};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

#[async_trait]
pub trait Transport: Send + Sync {
    async fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        retry: bool,
    ) -> Result<Value, Error>;
}
pub struct HttpTransport {
    url: String,
    http: reqwest::Client,
}
impl HttpTransport {
    pub fn new(api_url: &str) -> Result<Self, Error> {
        let url = reqwest::Url::parse(api_url).map_err(|_| Error::Configuration {
            reason: "API URL must be an absolute HTTP(S) URL".into(),
        })?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Configuration {
                reason: "API URL must use HTTP(S) without credentials, query or fragment".into(),
            });
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Configuration {
                reason: "could not create workflow HTTP client".into(),
            })?;
        Ok(Self {
            url: api_url.trim_end_matches('/').into(),
            http,
        })
    }
}
#[async_trait]
impl Transport for HttpTransport {
    async fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        retry: bool,
    ) -> Result<Value, Error> {
        let method =
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| Error::Configuration {
                reason: "invalid HTTP method".into(),
            })?;
        let operation = format!("{} {}", method, path);
        let attempts = if retry { 4 } else { 1 };
        let mut status = 0;
        for attempt in 0..attempts {
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", self.url))
                .bearer_auth(token)
                .header("temps-workflows-protocol", "1");
            if let Some(body) = &body {
                request = request.json(body);
            }
            status = match request.send().await {
                Err(_) => 0,
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let mut bytes = Vec::new();
                    let mut interrupted = false;
                    let max = if (200..300).contains(&status) {
                        128 * 1024 * 1024
                    } else {
                        65536
                    };
                    loop {
                        match response.chunk().await {
                            Ok(Some(chunk)) => {
                                if bytes.len() + chunk.len() > max {
                                    interrupted = true;
                                    break;
                                }
                                bytes.extend_from_slice(&chunk);
                            }
                            Ok(None) => break,
                            Err(_) => {
                                interrupted = true;
                                break;
                            }
                        }
                    }
                    if interrupted {
                        0
                    } else if (200..300).contains(&status) {
                        return serde_json::from_slice(&bytes).map_err(|_| Error::Encoding {
                            operation: operation.clone(),
                        });
                    } else {
                        status
                    }
                }
            };
            if !matches!(status, 0 | 502 | 503 | 504) {
                break;
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
            }
        }
        Err(Error::Api { operation, status })
    }
}
pub struct Client {
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) token: String,
}
impl Client {
    pub fn new(api_url: &str, token: String) -> Result<Self, Error> {
        Self::with_transport(Arc::new(HttpTransport::new(api_url)?), token)
    }
    pub fn with_transport(transport: Arc<dyn Transport>, token: String) -> Result<Self, Error> {
        if token.is_empty() {
            return Err(Error::Configuration {
                reason: "workflow credential is required".into(),
            });
        }
        Ok(Self { transport, token })
    }
    pub async fn start(
        &self,
        workflow: &str,
        input: Option<Value>,
        idempotency_key: Option<&str>,
        target: Option<Target>,
    ) -> Result<StartedRun, Error> {
        let path = target_path(target, "runs")?;
        let reply = self
            .transport
            .call(
                "POST",
                &path,
                &self.token,
                Some(json!({"workflow":workflow,"input":input,"idempotency_key":idempotency_key})),
                false,
            )
            .await?;
        serde_json::from_value(reply).map_err(|_| Error::Encoding {
            operation: "start run".into(),
        })
    }
    pub async fn get_run(&self, run_id: &str) -> Result<Run, Error> {
        if !run_id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
            return Err(Error::Configuration {
                reason: "run ID must be a UUID".into(),
            });
        }
        let reply = self
            .transport
            .call(
                "GET",
                &format!("/workflows/v1/runs/{run_id}"),
                &self.token,
                None,
                false,
            )
            .await?;
        serde_json::from_value(reply).map_err(|_| Error::Encoding {
            operation: "read run".into(),
        })
    }
    pub async fn send_event(&self, event: Event, target: Option<Target>) -> Result<(), Error> {
        let body = serde_json::to_value(event).map_err(|_| Error::Encoding {
            operation: "send event".into(),
        })?;
        self.transport
            .call(
                "POST",
                &target_path(target, "events")?,
                &self.token,
                Some(body),
                false,
            )
            .await?;
        Ok(())
    }
}
fn target_path(target: Option<Target>, operation: &str) -> Result<String, Error> {
    match target {
        None => Ok(format!("/workflows/v1/{operation}")),
        Some(t) if t.project_id > 0 && t.environment_id > 0 => Ok(format!(
            "/workflows/v1/projects/{}/environments/{}/{operation}",
            t.project_id, t.environment_id
        )),
        Some(_) => Err(Error::Configuration {
            reason: "workflow target IDs must be positive".into(),
        }),
    }
}
