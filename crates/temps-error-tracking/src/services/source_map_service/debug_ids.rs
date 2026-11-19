// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{
    apply_resolved_to_json, artifacts::canonical_debug_id, extract_source_context, ResolvedFrame,
    SourceMapService,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::Semaphore;

static SYMBOLICATION_WORKERS: Semaphore = Semaphore::const_new(2);

impl SourceMapService {
    /// A single bounded batch lookup and one parse per map, rather than one
    /// database lookup/parse per frame. Missing/busy maps leave the event intact
    /// and can be resolved on a later native detail read after upload completes.
    pub(super) async fn symbolicate_debug_ids(&self, project_id: i32, event: &mut Value) {
        let Some(images) = event
            .pointer("/debug_meta/images")
            .and_then(Value::as_array)
        else {
            return;
        };
        let mut associations = BTreeMap::new();
        for image in images {
            if image.get("type").and_then(Value::as_str) != Some("sourcemap") {
                continue;
            }
            if let (Some(path), Some(id)) = (
                image.get("code_file").and_then(Value::as_str),
                image
                    .get("debug_id")
                    .and_then(Value::as_str)
                    .and_then(canonical_debug_id),
            ) {
                if path.len() <= 4096 && associations.len() < 1000 {
                    associations.insert(path.to_owned(), id);
                }
            }
        }
        if associations.is_empty() {
            return;
        }
        // Annotate identities even if artifacts are missing. Release-based
        // fallback must never apply an unrelated build to an exact Debug ID.
        visit_frames(event, &mut |frame| {
            let path = frame
                .get("abs_path")
                .or_else(|| frame.get("filename"))
                .and_then(Value::as_str);
            if let Some(id) = path.and_then(|p| associations.get(p)) {
                frame.insert("debug_id".into(), Value::String(id.clone()));
            }
        });
        let Ok(_permit) = SYMBOLICATION_WORKERS.try_acquire() else {
            return;
        };
        let ids = associations.values().take(32).cloned().collect::<Vec<_>>();
        let rows = match self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT debug_id,source_map_data FROM (
               SELECT debug_id,source_map_data,sum(size_bytes) OVER(ORDER BY debug_id) AS total
               FROM source_maps WHERE project_id=$1 AND debug_id=ANY($2)
             ) maps WHERE total<=52428800",
                [project_id.into(), ids.into()],
            ))
            .await
        {
            Ok(rows) => rows,
            Err(_) => {
                tracing::warn!(project_id, "Debug ID source map lookup failed");
                return;
            }
        };
        let mut bytes = Vec::new();
        for row in rows {
            if let (Ok(id), Ok(data)) = (
                row.try_get::<String>("", "debug_id"),
                row.try_get::<Vec<u8>>("", "source_map_data"),
            ) {
                bytes.push((id, data));
            }
        }
        let Ok(maps) = tokio::task::spawn_blocking(move || {
            bytes
                .into_iter()
                .filter_map(|(id, data)| {
                    sourcemap::SourceMap::from_slice(&data)
                        .ok()
                        .map(|map| (id, map))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .await
        else {
            return;
        };
        visit_frames(event, &mut |frame| {
            if frame.get("symbolicated").and_then(Value::as_bool) == Some(true) {
                return;
            }
            let Some(sm) = frame
                .get("debug_id")
                .and_then(Value::as_str)
                .and_then(|id| maps.get(id))
            else {
                return;
            };
            let Some(line) = frame
                .get("lineno")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0)
            else {
                return;
            };
            let column = frame
                .get("colno")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0);
            let Some(token) = sm.lookup_token(line - 1, column.saturating_sub(1)) else {
                return;
            };
            let filename = frame
                .get("abs_path")
                .or_else(|| frame.get("filename"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let source_line = token.get_src_line() + 1;
            let (context_line, pre_context, post_context) = sm
                .get_source_contents(token.get_src_id())
                .map(|s| extract_source_context(s, source_line))
                .unwrap_or_default();
            apply_resolved_to_json(
                frame,
                ResolvedFrame {
                    file: token.get_source().unwrap_or(&filename).to_owned(),
                    line: source_line,
                    column: token.get_src_col() + 1,
                    function: token.get_name().map(str::to_owned),
                    context_line,
                    pre_context,
                    post_context,
                },
                &filename,
            );
        });
    }
}

fn visit_frames(event: &mut Value, visitor: &mut impl FnMut(&mut serde_json::Map<String, Value>)) {
    // Explicit protocol paths keep this independent of arbitrary deeply nested
    // contexts/extra values. The envelope already bounds frame JSON bytes.
    for collection in ["exception", "threads"] {
        if let Some(values) = event
            .get_mut(collection)
            .and_then(|v| v.get_mut("values"))
            .and_then(Value::as_array_mut)
        {
            for value in values.iter_mut().take(1000) {
                visit_stack(value.get_mut("stacktrace"), visitor);
            }
        }
    }
    visit_stack(event.get_mut("stacktrace"), visitor);
}
fn visit_stack(
    stack: Option<&mut Value>,
    visitor: &mut impl FnMut(&mut serde_json::Map<String, Value>),
) {
    if let Some(frames) = stack
        .and_then(|s| s.get_mut("frames"))
        .and_then(Value::as_array_mut)
    {
        for frame in frames
            .iter_mut()
            .take(1000)
            .filter_map(Value::as_object_mut)
        {
            visitor(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{
        error_tracking_service::ErrorTrackingService,
        types::{CreateErrorEventData, ExceptionData},
    };
    use sea_orm::{ActiveModelTrait, Set};
    use std::sync::Arc;

    #[tokio::test]
    async fn grouping_survives_debug_id_upload_and_busy_symbolication() {
        let test_db = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test_db.connection_arc();
        let project = temps_entities::projects::ActiveModel {
            name: Set("Synthetic grouping".into()),
            slug: Set(format!("synthetic-grouping-{}", uuid::Uuid::new_v4())),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let maps = Arc::new(SourceMapService::new(db.clone()));
        let service = ErrorTrackingService::new(db.clone());
        service.set_source_map_service(maps.clone());
        let debug_id = uuid::Uuid::new_v4().to_string();
        let event = |custom: bool| {
            let stack = serde_json::json!({"frames":[{"filename":"app.min.js","abs_path":"app.min.js","function":"a","lineno":1,"colno":1,"in_app":true}]});
            let mut raw = serde_json::json!({"event_id":uuid::Uuid::new_v4().simple().to_string(),"exception":{"values":[{"type":"TypeError","value":"Synthetic failure","stacktrace":stack}]},"debug_meta":{"images":[{"type":"sourcemap","code_file":"app.min.js","debug_id":debug_id}]}});
            if custom {
                raw["fingerprint"] = serde_json::json!(["synthetic-bucket", "{{default}}"]);
            }
            CreateErrorEventData {
                project_id: project.id,
                source: Some("sentry".into()),
                raw_sentry_event: Some(raw),
                exception_type: Some("TypeError".into()),
                exception_value: Some("Synthetic failure".into()),
                stack_trace: Some(stack.clone()),
                exceptions: vec![ExceptionData {
                    exception_type: "TypeError".into(),
                    exception_value: Some("Synthetic failure".into()),
                    stack_trace: Some(stack),
                    mechanism: None,
                    module: None,
                    thread_id: None,
                }],
                ..Default::default()
            }
        };
        let mut groups = Vec::new();
        for custom in [false, true] {
            groups.push(service.process_error_event(event(custom)).await.unwrap());
        }
        assert_ne!(groups[0], groups[1]);
        let map = serde_json::to_vec(&serde_json::json!({"version":3,"sources":["synthetic-original.ts"],"sourcesContent":["throw new TypeError('synthetic');"],"names":["syntheticOriginal"],"mappings":"AAAAA"})).unwrap();
        let uploaded = maps
            .upload(project.id, "synthetic-release", "app.min.js.map", map, None)
            .await
            .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE source_maps SET debug_id=$1 WHERE id=$2",
            [debug_id.clone().into(), uploaded.id.into()],
        ))
        .await
        .unwrap();
        let mut resolved = event(false);
        maps.symbolicate_error_event(&mut resolved).await.unwrap();
        assert_eq!(
            resolved.stack_trace.as_ref().unwrap()["frames"][0]["function"],
            "syntheticOriginal"
        );
        for (index, custom) in [false, true].into_iter().enumerate() {
            assert_eq!(
                service.process_error_event(event(custom)).await.unwrap(),
                groups[index]
            );
        }
        let permits = SYMBOLICATION_WORKERS.acquire_many(2).await.unwrap();
        for (index, custom) in [false, true].into_iter().enumerate() {
            assert_eq!(
                service.process_error_event(event(custom)).await.unwrap(),
                groups[index]
            );
        }
        drop(permits);
        for group in groups {
            let (events, count) = service
                .crud
                .list_error_events(group, project.id, None, None)
                .await
                .unwrap();
            assert_eq!(count, 3);
            assert_eq!(events.len(), 3);
        }
        let count = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT count(*) AS n FROM error_events WHERE project_id=$1 AND data #>> '{sentry,exception,values,0,stacktrace,frames,0,function}'='syntheticOriginal'", [project.id.into()])).await.unwrap().unwrap();
        assert_eq!(
            count.try_get::<i64>("", "n").unwrap(),
            2,
            "available maps change stored display frames, not grouping"
        );
    }
}
