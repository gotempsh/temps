// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An outbound-only development tunnel. Requests are encrypted in the central
//! database, so polling and invocation may land on different control planes.
//! Only undeployed, unprotected preview environments may attach a local executor.
//! At most 16 sessions, four pending requests/session and 8 MiB/request.
use crate::{
    error::{WorkflowError, WorkflowResult},
    protocol::{Manifest, OutcomeRequest},
    token::{InvocationClaims, InvocationSigner, TokenPurpose},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement, TransactionTrait,
    Value,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use temps_core::{
    EncryptionService, ExecutorEndpoint, ExecutorEndpointSet, ExecutorEndpoints,
    ExecutorEndpointsError,
};
use utoipa::ToSchema;
use uuid::Uuid;

pub const DEV_BODY_LIMIT: usize = 8 * 1024 * 1024;
fn sql(query: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}
fn unavailable(environment_id: i32, reason: &'static str) -> WorkflowError {
    WorkflowError::DevTunnel {
        environment_id,
        reason,
    }
}
#[derive(Serialize, ToSchema)]
#[schema(as=WorkflowDevSessionResponse)]
pub struct DevSessionResponse {
    pub session_id: Uuid,
    pub manifest_token: String,
    pub expires_at: DateTime<Utc>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[schema(as=WorkflowDevRequest)]
pub struct DevRequest {
    pub id: Uuid,
    pub token: String,
    pub body: String,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[schema(as=WorkflowDevReply)]
pub struct DevReply {
    pub status: u16,
    pub outcome: Option<OutcomeRequest>,
}
#[derive(Serialize, ToSchema)]
#[schema(as=WorkflowDevPollResponse)]
pub struct DevPollResponse {
    pub request: Option<DevRequest>,
}
#[derive(FromQueryResult)]
struct Session {
    id: Uuid,
    manifest: Option<serde_json::Value>,
}
#[derive(FromQueryResult)]
struct Encrypted {
    id: Uuid,
    request_encrypted: String,
}
pub struct DevTunnel {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    signer: Arc<InvocationSigner>,
}
impl DevTunnel {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        signer: Arc<InvocationSigner>,
    ) -> Self {
        Self {
            db,
            encryption,
            signer,
        }
    }
    async fn valid(&self, project: i32, environment: i32, session: Uuid) -> WorkflowResult<()> {
        let found=self.db.query_one(sql("SELECT s.id FROM workflow_dev_sessions s JOIN environments e ON e.id=s.environment_id WHERE s.id=$1 AND e.id=$2 AND e.project_id=$3 AND e.is_preview AND NOT e.protected AND e.current_deployment_id IS NULL AND e.deleted_at IS NULL AND s.expires_at>now()",vec![session.into(),environment.into(),project.into()])).await.map_err(WorkflowError::db("checking workflow development session"))?;
        if found.is_none() {
            return Err(unavailable(
                environment,
                "session expired, changed, or environment is no longer an undeployed preview",
            ));
        }
        Ok(())
    }
    pub async fn open(&self, project: i32, environment: i32) -> WorkflowResult<DevSessionResponse> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("opening workflow development tunnel"))?;
        tx.execute(sql("SELECT pg_advisory_xact_lock(7343019010)", vec![]))
            .await
            .map_err(WorkflowError::db("locking workflow development admission"))?;
        tx.execute(sql(
            "DELETE FROM workflow_dev_sessions WHERE expires_at<=now()",
            vec![],
        ))
        .await
        .map_err(WorkflowError::db("expiring workflow development sessions"))?;
        let id = Uuid::new_v4();
        let result=tx.query_one(sql("INSERT INTO workflow_dev_sessions(id,environment_id,expires_at) SELECT $1,e.id,now()+interval '60 seconds' FROM environments e WHERE e.id=$2 AND e.project_id=$3 AND e.is_preview AND NOT e.protected AND e.current_deployment_id IS NULL AND e.deleted_at IS NULL AND (SELECT count(*) FROM workflow_dev_sessions)<16 ON CONFLICT(environment_id) DO NOTHING RETURNING expires_at",vec![id.into(),environment.into(),project.into()])).await.map_err(WorkflowError::db("registering workflow development session"))?;
        let Some(row) = result else {
            return Err(unavailable(environment,"requires an undeployed, unprotected preview environment with no attached developer; at most 16 sessions are allowed"));
        };
        let expires_at = row
            .try_get("", "expires_at")
            .map_err(WorkflowError::db("reading development session expiry"))?;
        tx.commit()
            .await
            .map_err(WorkflowError::db("committing workflow development session"))?;
        let manifest_token = self.signer.sign(&InvocationClaims {
            v: 1,
            typ: TokenPurpose::Manifest,
            pid: project,
            eid: environment,
            run: None,
            lease: None,
            exp: Utc::now().timestamp() + 60,
            body: None,
        })?;
        Ok(DevSessionResponse {
            session_id: id,
            manifest_token,
            expires_at,
        })
    }
    pub async fn configure(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
        manifest: Manifest,
    ) -> WorkflowResult<()> {
        self.valid(project, environment, session).await?;
        if manifest.protocol_version != 1
            || manifest.workflows.len() > 1000
            || manifest.workflows.iter().any(|w| {
                crate::types::validate_name("workflow", &w.name, 128).is_err()
                    || w.concurrency
                        .is_some_and(|v| v == 0 || v > crate::executor::MAX_WORKFLOW_CONCURRENCY)
            })
        {
            return Err(unavailable(environment,"invalid executor manifest; protocol version 1 and at most 1000 valid workflows are required"));
        }
        let value = serde_json::to_value(manifest)
            .map_err(|_| unavailable(environment, "could not encode executor manifest"))?;
        self.db.execute(sql("UPDATE workflow_dev_sessions SET manifest=$1 WHERE id=$2 AND environment_id=$3 AND expires_at>now()",vec![value.into(),session.into(),environment.into()])).await.map_err(WorkflowError::db("configuring local workflow executor"))?;
        Ok(())
    }
    pub async fn close(&self, project: i32, environment: i32, session: Uuid) -> WorkflowResult<()> {
        self.db.execute(sql("DELETE FROM workflow_dev_sessions s USING environments e WHERE s.id=$1 AND s.environment_id=e.id AND e.id=$2 AND e.project_id=$3",vec![session.into(),environment.into(),project.into()])).await.map_err(WorkflowError::db("closing workflow development tunnel"))?;
        Ok(())
    }
    pub async fn poll(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
    ) -> WorkflowResult<DevPollResponse> {
        self.valid(project, environment, session).await?;
        self.db.execute(sql("UPDATE workflow_dev_sessions SET expires_at=now()+interval '60 seconds' WHERE id=$1 AND expires_at>now()",vec![session.into()])).await.map_err(WorkflowError::db("renewing workflow development tunnel"))?;
        self.db
            .execute(sql(
                "DELETE FROM workflow_dev_requests WHERE session_id=$1 AND expires_at<=now()",
                vec![session.into()],
            ))
            .await
            .map_err(WorkflowError::db("expiring development requests"))?;
        let row=Encrypted::find_by_statement(sql("UPDATE workflow_dev_requests SET delivered=true WHERE id=(SELECT id FROM workflow_dev_requests WHERE session_id=$1 AND NOT delivered AND expires_at>now() ORDER BY created_at,id LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING id,request_encrypted",vec![session.into()])).one(self.db.as_ref()).await.map_err(WorkflowError::db("claiming development request"))?;
        let request = if let Some(row) = row {
            Some(self.decode(environment, row.id, &row.request_encrypted, "request")?)
        } else {
            None
        };
        Ok(DevPollResponse { request })
    }
    pub async fn respond(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
        id: Uuid,
        reply: DevReply,
    ) -> WorkflowResult<()> {
        self.valid(project, environment, session).await?;
        if !matches!(reply.status, 200 | 202 | 409 | 429 | 500 | 503)
            || (reply.status == 200 && reply.outcome.is_none())
        {
            return Err(unavailable(environment, "invalid local executor response"));
        }
        let body = self.encode(environment, id, &reply, "response")?;
        let affected=self.db.execute(sql("UPDATE workflow_dev_requests SET response_encrypted=$1 WHERE id=$2 AND session_id=$3 AND delivered AND response_encrypted IS NULL AND expires_at>now()",vec![body.into(),id.into(),session.into()])).await.map_err(WorkflowError::db("returning local executor outcome"))?;
        if affected.rows_affected() == 0 {
            return Err(unavailable(
                environment,
                "request expired or already answered",
            ));
        }
        Ok(())
    }
    fn encode<T: Serialize>(
        &self,
        environment: i32,
        id: Uuid,
        value: &T,
        part: &str,
    ) -> WorkflowResult<String> {
        let body = serde_json::to_vec(value)
            .map_err(|_| unavailable(environment, "could not encode tunnel message"))?;
        if body.len() > DEV_BODY_LIMIT {
            return Err(unavailable(
                environment,
                "tunnel message exceeds 8 MiB; reduce development payloads or deploy the executor",
            ));
        }
        self.encryption
            .encrypt_with_aad(
                &body,
                format!("workflow-dev/{environment}/{id}/{part}").as_bytes(),
            )
            .map_err(|_| unavailable(environment, "could not encrypt tunnel message"))
    }
    fn decode<T: serde::de::DeserializeOwned>(
        &self,
        environment: i32,
        id: Uuid,
        value: &str,
        part: &str,
    ) -> WorkflowResult<T> {
        let bytes = self
            .encryption
            .decrypt_with_aad(
                value,
                format!("workflow-dev/{environment}/{id}/{part}").as_bytes(),
            )
            .map_err(|_| unavailable(environment, "could not decrypt tunnel message"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| unavailable(environment, "could not decode tunnel message"))
    }
    pub async fn manifest(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
    ) -> WorkflowResult<Manifest> {
        self.valid(project, environment, session).await?;
        let row = Session::find_by_statement(sql(
            "SELECT id,manifest FROM workflow_dev_sessions WHERE id=$1",
            vec![session.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("reading local executor manifest"))?
        .ok_or_else(|| unavailable(environment, "session closed"))?;
        serde_json::from_value(
            row.manifest.ok_or_else(|| {
                unavailable(environment, "local executor manifest is not registered")
            })?,
        )
        .map_err(|_| unavailable(environment, "stored manifest is invalid"))
    }
    pub async fn invoke(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
        token: String,
        body: Vec<u8>,
        deadline: DateTime<Utc>,
    ) -> WorkflowResult<DevReply> {
        self.valid(project, environment, session).await?;
        let id = Uuid::new_v4();
        let request = DevRequest {
            id,
            token,
            body: String::from_utf8(body)
                .map_err(|_| unavailable(environment, "invocation body is not UTF-8"))?,
        };
        let encrypted = self.encode(environment, id, &request, "request")?;
        let tx = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("queuing development invocation"))?;
        tx.query_one(sql(
            "SELECT id FROM workflow_dev_sessions WHERE id=$1 FOR UPDATE",
            vec![session.into()],
        ))
        .await
        .map_err(WorkflowError::db("locking development session"))?;
        let written=tx.execute(sql("INSERT INTO workflow_dev_requests(id,session_id,request_encrypted,expires_at) SELECT $1,$2,$3,$4 WHERE (SELECT count(*) FROM workflow_dev_requests WHERE session_id=$2 AND expires_at>now())<4",vec![id.into(),session.into(),encrypted.into(),deadline.into()])).await.map_err(WorkflowError::db("queuing development invocation"))?;
        tx.commit()
            .await
            .map_err(WorkflowError::db("committing development invocation"))?;
        if written.rows_affected() == 0 {
            return Err(unavailable(
                environment,
                "all four local invocation slots are occupied",
            ));
        }
        let result = self
            .await_reply(project, environment, session, id, deadline)
            .await;
        // Cancellation leaves a bounded row that the next poll expires.
        if let Err(error) = self
            .db
            .execute(sql(
                "DELETE FROM workflow_dev_requests WHERE id=$1",
                vec![id.into()],
            ))
            .await
        {
            tracing::warn!(environment_id=environment,request_id=%id,%error,"Could not remove completed development request; expiry will remove it");
        }
        result
    }
    async fn await_reply(
        &self,
        project: i32,
        environment: i32,
        session: Uuid,
        id: Uuid,
        deadline: DateTime<Utc>,
    ) -> WorkflowResult<DevReply> {
        while Utc::now() < deadline {
            self.valid(project, environment, session).await?;
            let row = self
                .db
                .query_one(sql(
                    "SELECT response_encrypted FROM workflow_dev_requests WHERE id=$1",
                    vec![id.into()],
                ))
                .await
                .map_err(WorkflowError::db("waiting for local workflow executor"))?
                .ok_or_else(|| unavailable(environment, "request expired"))?;
            let response: Option<String> = row
                .try_get("", "response_encrypted")
                .map_err(WorkflowError::db("reading local workflow response"))?;
            if let Some(response) = response {
                return self.decode(environment, id, &response, "response");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(unavailable(
            environment,
            "local invocation exceeded its deadline",
        ))
    }
}
pub struct DevEndpoints {
    pub tunnel: Arc<DevTunnel>,
    pub deployed: Arc<dyn ExecutorEndpoints>,
}
#[async_trait]
impl ExecutorEndpoints for DevEndpoints {
    async fn endpoints(
        &self,
        project: i32,
        environment: i32,
        service: Option<&str>,
    ) -> Result<ExecutorEndpointSet, ExecutorEndpointsError> {
        let row=Session::find_by_statement(sql("SELECT s.id,s.manifest FROM workflow_dev_sessions s JOIN environments e ON e.id=s.environment_id WHERE e.id=$1 AND e.project_id=$2 AND e.is_preview AND NOT e.protected AND e.current_deployment_id IS NULL AND e.deleted_at IS NULL AND s.expires_at>now() AND s.manifest IS NOT NULL",vec![environment.into(),project.into()])).one(self.tunnel.db.as_ref()).await.map_err(|error|ExecutorEndpointsError::Lookup{project_id:project,environment_id:environment,reason:error.to_string()})?;
        if let Some(session) = row {
            return Ok(ExecutorEndpointSet {
                deployment_id: None,
                endpoints: vec![ExecutorEndpoint {
                    base_url: format!("dev://{}", session.id),
                    container_id: 0,
                    node_id: None,
                }],
                unavailable_reason: None,
            });
        }
        self.deployed.endpoints(project, environment, service).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_environment;
    #[tokio::test]
    async fn tunnel_is_scoped_encrypted_bounded_and_recovers_after_disconnect() {
        let Some(env) = test_environment("workflow development tunnel").await else {
            return;
        };
        let encryption =
            Arc::new(EncryptionService::new("12345678901234567890123456789012").unwrap());
        let signer = Arc::new(InvocationSigner::new(&encryption));
        let tunnel = Arc::new(DevTunnel::new(
            env.conn.clone(),
            encryption.clone(),
            signer.clone(),
        ));
        let (p, e) = (env.project_id, env.environment_id);
        assert!(tunnel
            .open(p, e)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("preview"));
        env.conn
            .execute(sql(
                "UPDATE environments SET is_preview=true WHERE id=$1",
                vec![e.into()],
            ))
            .await
            .unwrap();
        let session = tunnel.open(p, e).await.unwrap();
        assert!(tunnel.open(p, e).await.is_err());
        assert!(tunnel.poll(p + 1, e, session.session_id).await.is_err());
        let manifest = Manifest {
            protocol_version: 1,
            sdk: Some("test/1".into()),
            workflows: vec![],
        };
        tunnel
            .configure(p, e, session.session_id, manifest.clone())
            .await
            .unwrap();
        assert_eq!(
            tunnel.manifest(p, e, session.session_id).await.unwrap(),
            manifest
        );
        let endpoint = ExecutorEndpoint {
            base_url: format!("dev://{}", session.session_id),
            container_id: 0,
            node_id: None,
        };
        let set = ExecutorEndpointSet {
            deployment_id: None,
            endpoints: vec![endpoint],
            unavailable_reason: None,
        };
        let cache = crate::manifest::ManifestCache::new();
        let client = crate::executor::ExecutorClient::new()
            .unwrap()
            .with_dev(tunnel.clone());
        assert_eq!(
            cache.get(&client, &signer, p, e, &set).await.unwrap().sdk,
            Some("test/1".into())
        );
        let mut changed = manifest;
        changed.sdk = Some("test/2".into());
        tunnel
            .configure(p, e, session.session_id, changed)
            .await
            .unwrap();
        assert_eq!(
            cache.get(&client, &signer, p, e, &set).await.unwrap().sdk,
            Some("test/2".into())
        );
        let worker = tunnel.clone();
        let sid = session.session_id;
        let invocation = tokio::spawn(async move {
            worker
                .invoke(
                    p,
                    e,
                    sid,
                    "private-token".into(),
                    b"private-input".to_vec(),
                    Utc::now() + chrono::Duration::seconds(10),
                )
                .await
        });
        // A different control-plane object can deliver the persisted request.
        let second = DevTunnel::new(env.conn.clone(), encryption, signer);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let request = loop {
            if let Some(request) = second.poll(p, e, sid).await.unwrap().request {
                break request;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(request.token, "private-token");
        assert_eq!(request.body, "private-input");
        assert!(second.poll(p, e, sid).await.unwrap().request.is_none());
        let stored = Encrypted::find_by_statement(sql(
            "SELECT id,request_encrypted FROM workflow_dev_requests WHERE id=$1",
            vec![request.id.into()],
        ))
        .one(env.conn.as_ref())
        .await
        .unwrap()
        .unwrap();
        assert!(!stored.request_encrypted.contains("private"));
        assert!(second
            .respond(
                p + 1,
                e,
                sid,
                request.id,
                DevReply {
                    status: 202,
                    outcome: None
                }
            )
            .await
            .is_err());
        second
            .respond(
                p,
                e,
                sid,
                request.id,
                DevReply {
                    status: 202,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(invocation.await.unwrap().unwrap().status, 202);
        assert!(tunnel
            .invoke(
                p,
                e,
                sid,
                "x".into(),
                vec![b'a'; DEV_BODY_LIMIT],
                Utc::now() + chrono::Duration::seconds(10)
            )
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("8 MiB"));
        env.conn
            .execute(sql(
                "UPDATE workflow_dev_sessions SET expires_at=now()-interval '1 second' WHERE id=$1",
                vec![sid.into()],
            ))
            .await
            .unwrap();
        assert!(tunnel.poll(p, e, sid).await.is_err());
        let replacement = tunnel.open(p, e).await.unwrap();
        tunnel.close(p, e, sid).await.unwrap();
        assert!(tunnel.poll(p, e, replacement.session_id).await.is_ok());
        env.conn
            .execute(sql(
                "UPDATE environments SET protected=true WHERE id=$1",
                vec![e.into()],
            ))
            .await
            .unwrap();
        assert!(tunnel.poll(p, e, replacement.session_id).await.is_err());
        tunnel.close(p, e, replacement.session_id).await.unwrap();
    }
}
