// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Same-conversation watch management. The model can supply a filter, never
//! the destination conversation, caller, internal route or HTTP method.

use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use temps_ai::streaming::ChatTool;
use temps_auth::context::AuthContext;
use temps_core::{
    ai_tool_call::{AiToolCall, ChatWatchScope},
    RequestMetadata,
};
use tower::ServiceExt;

/// Own the interval from claiming a turn until its model task is registered.
/// A watch uses one absolute budget across provider, sandbox, seed and tool
/// discovery. The router reserves a separate interval for claim cleanup.
pub(crate) async fn prepare_user_turn(
    service: &crate::ConversationService,
    conversation: &temps_entities::ai_conversations::Model,
    turn_id: &str,
    watch: Option<&temps_core::ai_tool_call::PluginWatchDelivery>,
    prepare: impl std::future::Future<Output = Result<(), crate::ChatError>>,
) -> Result<chrono::DateTime<chrono::Utc>, crate::ChatError> {
    let timed_out = || crate::ChatError::WatchPreparationTimeout {
        conversation_id: conversation.public_id.clone(),
        turn_id: turn_id.to_owned(),
    };
    if watch.is_some_and(|w| w.prepare_by <= tokio::time::Instant::now()) {
        return Err(timed_out());
    }
    let mut claimed = false;
    let start = async {
        let started = service.claim_turn(conversation, turn_id).await?;
        claimed = true;
        prepare.await?;
        Ok(started)
    };
    let result = match watch {
        Some(wake) => tokio::time::timeout_at(wake.prepare_by, start)
            .await
            .unwrap_or_else(|_| Err(timed_out())),
        None => start.await,
    };
    if let Err(error) = &result {
        // A claim's commit acknowledgement can race its deadline. Cleanup is
        // conditional on the opaque turn ID and cannot clear a newer owner.
        if claimed || matches!(error, crate::ChatError::WatchPreparationTimeout { .. }) {
            if let Err(cause) = service
                .finish_failed_turn(conversation.id, turn_id, error)
                .await
            {
                tracing::error!(conversation_id=conversation.id, turn_id, %cause, "Failed to release chat turn after preparation");
            }
        }
    }
    result
}

/// Recover the original transcript message when a prior process stopped during preparation.
pub(crate) async fn prepared_message(
    db: &sea_orm::DatabaseConnection,
    conversation_id: i64,
    delivery: &str,
) -> Result<Option<temps_entities::ai_messages::Model>, sea_orm::DbErr> {
    use sea_orm::{DatabaseBackend, EntityTrait, Statement};
    temps_entities::ai_messages::Entity::find()
        .from_raw_sql(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
        SELECT m.* FROM plugin_watch_deliveries d JOIN ai_messages m ON m.id=d.wake_message_id
        JOIN plugin_chat_watches w ON w.id=d.watch_id
        WHERE d.id=$1::text::uuid AND d.status='pending' AND w.conversation_id=$2
          AND m.conversation_id=$2 AND m.role='user'
    "#,
            [delivery.into(), conversation_id.into()],
        ))
        .one(db)
        .await
}

/// Called inside the registered model task, after the startup and cancellation barrier.
/// Exactly one task can start this delivery. A failed preparation leaves it pending.
pub(crate) async fn start_delivery(
    db: &sea_orm::DatabaseConnection,
    conversation_id: i64,
    delivery: &str,
) -> Result<bool, sea_orm::DbErr> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let result = db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres, r#"
        UPDATE plugin_watch_deliveries d SET status='delivered', delivered_at=statement_timestamp()
        FROM plugin_chat_watches w, external_plugin_actors a, ai_conversations c, sessions s
        WHERE d.id=$1::text::uuid AND w.conversation_id=$2 AND d.watch_id=w.id
          AND d.wake_message_id IS NOT NULL AND d.status='pending' AND NOT w.paused AND w.mode='wake'
          AND a.id=w.actor_id AND a.binary_sha256=w.digest AND a.active AND a.ai_tools_enabled
          AND c.id=w.conversation_id AND s.id=w.session_id AND s.user_id=c.created_by
          AND NOT s.mfa_pending AND s.expires_at>statement_timestamp()
    "#, [delivery.into(), conversation_id.into()])).await?;
    Ok(result.rows_affected() == 1)
}

pub(crate) const TOOL: &str = "temps_watch";

#[derive(Clone, Default)]
pub struct WatchTools(Arc<OnceLock<Router>>);

impl WatchTools {
    pub fn connect(&self, router: Router) {
        let _ = self.0.set(router);
    }
    pub(crate) fn ready(&self) -> bool {
        self.0.get().is_some()
    }
}

pub(crate) struct ConversationWatches {
    pub tools: WatchTools,
    pub chat: String,
    pub metadata: RequestMetadata,
}

impl ConversationWatches {
    pub async fn dispatch(&self, auth: &AuthContext, arguments: &str) -> String {
        let Ok(mut input) = serde_json::from_str::<serde_json::Map<String, Value>>(arguments)
        else {
            return "Invalid watch arguments. Supply a JSON object.".into();
        };
        let Some(router) = self.tools.0.get() else {
            return "Watch service unavailable".into();
        };
        let read = match input.get("action").and_then(Value::as_str) {
            Some("discover" | "list") => true,
            Some("create") => {
                input.entry("mode").or_insert_with(|| json!("wake"));
                false
            }
            Some("pause" | "resume" | "remove") => false,
            _ => return "Unknown watch action".into(),
        };
        let input = Value::Object(input);
        let path = format!("/api/ai/conversations/{}/plugin-watches", self.chat);
        let Ok(mut request) = Request::builder()
            .uri(path)
            .method(if read { "GET" } else { "POST" })
            .header("content-type", "application/json")
            .body(if read {
                Body::empty()
            } else {
                Body::from(input.to_string())
            })
        else {
            return "Invalid conversation reference".into();
        };
        request.extensions_mut().insert(auth.clone());
        request.extensions_mut().insert(self.metadata.clone());
        request.extensions_mut().insert(AiToolCall);
        request
            .extensions_mut()
            .insert(ChatWatchScope(self.chat.clone()));

        let response = match router.clone().oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let status = response.status();
        match to_bytes(response.into_body(), 256 * 1024).await {
            Ok(body) if status.is_success() && body.is_empty() => {
                json!({"ok":true,"action":input["action"]}).to_string()
            }
            Ok(body) if status.is_success() => String::from_utf8_lossy(&body).into_owned(),
            Ok(body) => format!(
                "Watch request returned {}. {}",
                status,
                String::from_utf8_lossy(&body)
            ),
            Err(_) => "Watch response exceeded the size limit".into(),
        }
    }
}

pub(crate) fn tool() -> ChatTool {
    ChatTool {
        name: TOOL.into(),
        description: "You manage persistent watches for the user in this conversation. When asked to watch a named resource, discover the plugin event source, digest and filter fields, then use authorized plugin read tools to resolve its name to the required IDs. Ask for clarification if the resource is ambiguous. Create the watch yourself using plugin, source, digest and where. By default a matching event wakes this chat to follow the user’s watch request. Choose notify only for a notification without an agent turn. List watches to find IDs for pause, resume or remove.".into(),
        parameters: json!({"type":"object","additionalProperties":false,"required":["action"],
            "properties":{
                "action":{"type":"string","enum":["discover","create","list","pause","resume","remove"]},
                "plugin":{"type":"string"}, "source":{"type":"string"}, "digest":{"type":"string"},
                "where":{"type":"object","description":"Exact matches on the source's declared filterable fields using their declared JSON types."},
                "mode":{"type":"string","enum":["notify","wake"],"default":"wake"}, "id":{"type":"string"}
            }}),
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::{prepared_message, start_delivery};
    use sea_orm::ConnectionTrait;

    #[tokio::test]
    async fn interrupted_watch_preparation_reuses_one_message_and_starts_only_once() {
        let fixture = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) if temps_test_utils::is_container_runtime_unavailable(&e.to_string()) => {
                eprintln!("Skipping watch recovery test: Docker unavailable");
                return;
            }
            Err(e) => panic!("watch recovery fixture: {e}"),
        };
        let db = fixture.connection_arc();
        let delivery = "a5d0ad5c-7f29-4757-aa6a-f32d6b35d663";
        db.execute_unprepared(r#"
            INSERT INTO users(id,name,email) VALUES(9901,'Owner','owner@example.test');
            INSERT INTO sessions(id,user_id,expires_at,session_token,mfa_pending) VALUES(9901,9901,now()+interval '1 day','fixture',false);
            INSERT INTO ai_conversations(id,public_id,created_by,context_type,context_id) VALUES(9910,'fixture',9901,'assistant','assistant');
            INSERT INTO external_plugin_actors(id,plugin_name,binary_sha256,source_identity,active,ai_tools_enabled)
            VALUES('db8821b7-04c6-4ea0-9d88-72ed7152b06c','reports','digest','file',true,true);
            INSERT INTO plugin_chat_watches(conversation_id,actor_id,digest,source,subject,predicate,mode,session_id)
            SELECT 9910,id,'digest','changed','1','{}','wake',9901 FROM external_plugin_actors WHERE plugin_name='reports';
            INSERT INTO plugin_watch_deliveries(id,watch_id,event_id)
            SELECT 'a5d0ad5c-7f29-4757-aa6a-f32d6b35d663',id,'fixture-event' FROM plugin_chat_watches WHERE conversation_id=9910;
        "#).await.unwrap();
        assert!(prepared_message(&db, 9910, delivery)
            .await
            .unwrap()
            .is_none());
        let insert = r#"INSERT INTO ai_messages(conversation_id,role,content,metadata) VALUES(9910,'user','Resource changed','{"plugin_watch_delivery":"a5d0ad5c-7f29-4757-aa6a-f32d6b35d663"}')"#;
        db.execute_unprepared(insert).await.unwrap();
        let recovered = prepared_message(&db, 9910, delivery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.content, "Resource changed");
        assert!(prepared_message(&db, 9911, delivery)
            .await
            .unwrap()
            .is_none());
        assert!(db.execute_unprepared(insert).await.is_err());
        db.execute_unprepared("UPDATE sessions SET mfa_pending=true WHERE id=9901")
            .await
            .unwrap();
        assert!(!start_delivery(&db, 9910, delivery).await.unwrap());
        db.execute_unprepared("UPDATE sessions SET mfa_pending=false WHERE id=9901")
            .await
            .unwrap();
        assert!(start_delivery(&db, 9910, delivery).await.unwrap());
        assert!(!start_delivery(&db, 9910, delivery).await.unwrap());
        assert!(prepared_message(&db, 9910, delivery)
            .await
            .unwrap()
            .is_none());
    }
}
