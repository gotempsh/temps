// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persisting assistant messages, tool calls and conversation metadata
//! (failures, context usage) for a turn.

use super::*;

#[cfg(test)]
pub(super) fn assistant_message_metadata(
    tools: &[serde_json::Value],
    parts: &[serde_json::Value],
    draft: bool,
) -> Option<serde_json::Value> {
    assistant_message_metadata_with(tools, parts, draft, None)
}

/// Like [`assistant_message_metadata`], additionally flagging a message whose
/// text is partial because the provider stream was cut short. The flag is what
/// lets a reload (and any API consumer) tell this message from a finished
/// answer.
pub(super) fn assistant_message_metadata_with(
    tools: &[serde_json::Value],
    parts: &[serde_json::Value],
    draft: bool,
    interruption: Option<&crate::PublicChatFailure>,
) -> Option<serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    if let Some(failure) = interruption {
        metadata.insert("interrupted".to_string(), serde_json::Value::Bool(true));
        metadata.insert(
            "interruption".to_string(),
            serde_json::json!({
                "code": failure.code,
                "title": failure.title,
                "detail": failure.detail,
            }),
        );
    }
    if !tools.is_empty() {
        metadata.insert(
            "tools".to_string(),
            serde_json::Value::Array(tools.to_vec()),
        );
    }
    if !parts.is_empty() {
        metadata.insert(
            "parts".to_string(),
            serde_json::Value::Array(parts.to_vec()),
        );
    }
    if draft {
        metadata.insert("draft".to_string(), serde_json::Value::Bool(true));
    }
    (!metadata.is_empty()).then_some(serde_json::Value::Object(metadata))
}

pub(super) fn conversation_metadata_with_failure(
    metadata: Option<&serde_json::Value>,
    failure: Option<&crate::PublicChatFailure>,
) -> Option<serde_json::Value> {
    let mut object = metadata
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    match failure {
        Some(failure) => {
            object.insert(
                "last_failure".to_string(),
                serde_json::json!({
                    "code": failure.code,
                    "title": failure.title,
                    "detail": failure.detail,
                    "retryable": failure.retryable,
                }),
            );
        }
        None => {
            object.remove("last_failure");
        }
    }
    (!object.is_empty()).then_some(serde_json::Value::Object(object))
}

pub(super) fn conversation_metadata_without_context_usage(
    metadata: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    let mut object = metadata
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    object.remove("context_usage");
    (!object.is_empty()).then_some(serde_json::Value::Object(object))
}

pub(super) async fn persist_conversation_failure(
    db: &DatabaseConnection,
    conversation_id: i64,
    owning_turn_id: Option<&str>,
    failure: &crate::PublicChatFailure,
) -> Result<(), sea_orm::DbErr> {
    let conversation = ai_conversations::Entity::find_by_id(conversation_id)
        .one(db)
        .await?;
    let Some(conversation) = conversation else {
        return Ok(());
    };
    if conversation
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("last_failure"))
        .is_some()
    {
        return Ok(());
    }
    let mut update = ai_conversations::Entity::update_many()
        .filter(ai_conversations::Column::Id.eq(conversation_id));
    if let Some(owning_turn_id) = owning_turn_id {
        update = update.filter(ai_conversations::Column::ActiveTurnId.eq(owning_turn_id));
    }
    update
        .col_expr(
            ai_conversations::Column::Metadata,
            Expr::value(conversation_metadata_with_failure(
                conversation.metadata.as_ref(),
                Some(failure),
            )),
        )
        .exec(db)
        .await?;
    Ok(())
}

pub(super) async fn persist_context_usage(
    db: &DatabaseConnection,
    conversation_id: i64,
    owning_turn_id: Option<&str>,
    usage: &PersistedContextWindowUsage,
) -> Result<(), sea_orm::DbErr> {
    let Some(conversation) = ai_conversations::Entity::find_by_id(conversation_id)
        .one(db)
        .await?
    else {
        return Ok(());
    };
    let metadata = conversation_metadata_with_context_usage(conversation.metadata.as_ref(), usage);
    let mut update = ai_conversations::Entity::update_many()
        .filter(ai_conversations::Column::Id.eq(conversation_id));
    if let Some(owning_turn_id) = owning_turn_id {
        update = update.filter(ai_conversations::Column::ActiveTurnId.eq(owning_turn_id));
    }
    update
        .col_expr(ai_conversations::Column::Metadata, Expr::value(metadata))
        .exec(db)
        .await?;
    Ok(())
}

pub(super) fn conversation_metadata_with_context_usage(
    existing: Option<&serde_json::Value>,
    usage: &PersistedContextWindowUsage,
) -> Option<serde_json::Value> {
    let mut metadata = existing
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    metadata.insert("context_usage".to_string(), serde_json::json!(usage));
    Some(serde_json::Value::Object(metadata))
}

pub(super) async fn persist_assistant_message(
    db: &DatabaseConnection,
    message_id: i64,
    content: &str,
    tools: &[serde_json::Value],
    completed_parts: &[serde_json::Value],
    open_text: &str,
    draft: bool,
) -> Result<(), sea_orm::DbErr> {
    persist_assistant_message_with(
        db,
        message_id,
        content,
        tools,
        completed_parts,
        open_text,
        draft,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_assistant_message_with(
    db: &DatabaseConnection,
    message_id: i64,
    content: &str,
    tools: &[serde_json::Value],
    completed_parts: &[serde_json::Value],
    open_text: &str,
    draft: bool,
    interruption: Option<&crate::PublicChatFailure>,
) -> Result<(), sea_orm::DbErr> {
    let mut parts = completed_parts.to_vec();
    if !open_text.is_empty() {
        parts.push(serde_json::json!({ "type": "text", "text": open_text }));
    }
    ai_messages::Entity::update_many()
        .filter(ai_messages::Column::Id.eq(message_id))
        .col_expr(
            ai_messages::Column::Content,
            Expr::value(content.to_string()),
        )
        .col_expr(
            ai_messages::Column::Metadata,
            Expr::value(assistant_message_metadata_with(
                tools,
                &parts,
                draft,
                interruption,
            )),
        )
        .exec(db)
        .await?;
    Ok(())
}

pub(super) fn record_tool_call(
    tools: &mut Vec<serde_json::Value>,
    parts: &mut Vec<serde_json::Value>,
    id: &str,
    name: &str,
    arguments: &str,
) {
    if tools
        .iter()
        .any(|tool| tool.get("id").and_then(serde_json::Value::as_str) == Some(id))
    {
        return;
    }
    let tool = serde_json::json!({
        "id": id,
        "name": name,
        "arguments": arguments,
        "result": serde_json::Value::Null,
    });
    tools.push(tool.clone());
    parts.push(serde_json::json!({ "type": "tool", "tool": tool }));
}

pub(super) fn record_tool_result(
    tools: &mut Vec<serde_json::Value>,
    parts: &mut Vec<serde_json::Value>,
    id: &str,
    name: &str,
    arguments: &str,
    result: &str,
) {
    record_tool_call(tools, parts, id, name, arguments);
    for tool in tools.iter_mut() {
        if tool.get("id").and_then(serde_json::Value::as_str) == Some(id) {
            tool["result"] = serde_json::Value::String(result.to_string());
        }
    }
    for part in parts.iter_mut() {
        let Some(tool) = part.get_mut("tool") else {
            continue;
        };
        if tool.get("id").and_then(serde_json::Value::as_str) == Some(id) {
            tool["result"] = serde_json::Value::String(result.to_string());
        }
    }
}

/// A provider can disconnect after announcing a native tool but before
/// sending its result. Close those calls in both replay shapes so a failed
/// turn does not leave an indefinitely spinning tool card after reload.
pub(super) const INTERRUPTED_TOOL_RESULT: &str = r#"{"is_error":true,"status":"interrupted","error":"Tool result unavailable because the AI turn stopped."}"#;

pub(super) fn finish_unresolved_tool_calls(
    tools: &mut [serde_json::Value],
    parts: &mut [serde_json::Value],
) -> Vec<(String, String)> {
    let unresolved = tools
        .iter()
        .filter(|tool| tool.get("result").is_none_or(serde_json::Value::is_null))
        .filter_map(|tool| {
            Some((
                tool.get("id")?.as_str()?.to_string(),
                tool.get("name")?.as_str()?.to_string(),
            ))
        })
        .collect::<Vec<_>>();
    for (id, _) in &unresolved {
        for tool in tools.iter_mut() {
            if tool.get("id").and_then(serde_json::Value::as_str) == Some(id) {
                tool["result"] = serde_json::Value::String(INTERRUPTED_TOOL_RESULT.to_string());
            }
        }
        for part in parts.iter_mut() {
            let Some(tool) = part.get_mut("tool") else {
                continue;
            };
            if tool.get("id").and_then(serde_json::Value::as_str) == Some(id) {
                tool["result"] = serde_json::Value::String(INTERRUPTED_TOOL_RESULT.to_string());
            }
        }
    }
    unresolved
}
