// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::ConnectionTrait;

struct StalledProvider(std::sync::atomic::AtomicBool);

#[async_trait]
impl AiService for StalledProvider {
    async fn is_available(&self) -> bool {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        std::future::pending().await
    }
    async fn complete(&self, _: AiRequest) -> Result<AiResponse, AiError> {
        panic!("a stalled provider cannot start a completion")
    }
    async fn chat_stream(&self, _: ChatTurnRequest) -> Result<TokenStream, AiError> {
        panic!("a stalled provider cannot start a model turn")
    }
}

#[tokio::test]
async fn stalled_watch_readiness_releases_the_durable_claim_for_the_next_turn() {
    let fixture = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(fixture) => fixture,
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Skipping watch preparation test: Docker unavailable");
            return;
        }
        Err(error) => panic!("watch preparation database: {error}"),
    };
    let db = fixture.connection_arc();
    db.execute_unprepared(
        "
        INSERT INTO users(id,name,email) VALUES(9911,'Owner','owner@example.test');
        INSERT INTO ai_conversations(id,public_id,created_by,context_type,context_id)
          VALUES(9911,'fixture-chat',9911,'assistant','assistant');
    ",
    )
    .await
    .unwrap();
    let conversation = ai_conversations::Entity::find_by_id(9911)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let mut service = db_service_from_arc(db.clone());
    let provider = Arc::new(StalledProvider(std::sync::atomic::AtomicBool::new(false)));
    service.ai = provider.clone();
    let auth = test_auth();
    let metadata = test_request_metadata();
    let watch = temps_core::ai_tool_call::PluginWatchDelivery {
        id: "fixture-delivery".into(),
        prepare_by: tokio::time::Instant::now() + std::time::Duration::from_secs(2),
    };
    let turn_id = "stalled-turn";
    let preparation = service.send_message(
        &conversation,
        turn_id,
        "Read the updated resource.",
        Some(serde_json::json!({"plugin_watch_delivery": watch.id})),
        None,
        Vec::new(),
        Vec::new(),
        None,
        &auth,
        &metadata,
        None,
    );
    let error = crate::watches::prepare_user_turn(
        &service,
        &conversation,
        turn_id,
        Some(&watch),
        preparation,
    )
    .await
    .expect_err("a stalled provider must time out");
    assert!(provider.0.load(std::sync::atomic::Ordering::SeqCst));
    assert!(matches!(error, ChatError::WatchPreparationTimeout { .. }));
    let failed = ai_conversations::Entity::find_by_id(9911)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.turn_status, "failed");
    assert!(failed.active_turn_id.is_none());
    assert_eq!(
        failed.metadata.as_ref().unwrap()["last_failure"]["code"],
        "watch_preparation_timeout"
    );
    service.claim_turn(&failed, "next-turn").await.unwrap();
}
