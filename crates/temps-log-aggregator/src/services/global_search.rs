// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cross-project archive search. The database selects authorized chunks once;
//! bounded scans produce a single globally ordered page, never per-project pages.
use super::LogSearchService;
use crate::{
    error::LogAggregatorError,
    types::{LogLevel, LogLine, LogSearchLine},
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read};
use utoipa::ToSchema;
use uuid::Uuid;

const CHUNK_BATCH: i64 = 32;
const MAX_CHUNKS: usize = 512;
const MAX_COMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GlobalLogSource {
    #[default]
    Collected,
    Application,
    Service,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GlobalLogSearchRequest {
    #[schema(value_type = String)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String)]
    pub end_time: DateTime<Utc>,
    #[serde(default)]
    pub source: GlobalLogSource,
    /// Match project ID, slug or name. Empty selects all authorized projects.
    #[serde(default)]
    pub projects: Vec<String>,
    /// Database/service IDs or names, not container service labels.
    #[serde(default)]
    pub external_services: Vec<String>,
    /// Explicit resource identities, e.g. application:12 or service:34.
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub levels: Vec<LogLevel>,
    #[serde(default)]
    pub envs: Vec<String>,
    #[serde(default)]
    pub node_ids: Vec<i32>,
    pub deploy_id: Option<i32>,
    pub text: Option<String>,
    pub cursor: Option<String>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GlobalLogLine {
    #[serde(flatten)]
    pub line: LogSearchLine,
    pub project_id: Option<i32>,
    pub external_service_id: Option<i32>,
    pub owner: String,
    pub env: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalLogSearchResponse {
    /// Newest first, ordered by timestamp, chunk ID and line offset.
    pub lines: Vec<GlobalLogLine>,
    pub next_cursor: Option<String>,
    /// True means the scan budget was exhausted. Lines contain the newest
    /// matches found so far, but unread chunks may contain newer lines.
    /// No cursor is returned because the partial results cannot be paginated safely.
    pub scan_limit_reached: bool,
    pub scanned_chunks: usize,
    pub scanned_bytes: usize,
}

pub(crate) struct GlobalLogAccess {
    pub hidden_projects: Vec<i32>,
    pub bound_project: Option<i32>,
    pub unrestricted_services: bool,
    pub user_id: Option<i32>,
}

#[derive(Debug, FromQueryResult)]
struct Candidate {
    id: Uuid,
    project_id: i32,
    external_service_id: Option<i32>,
    owner: String,
    env: String,
    ended_at: DateTime<Utc>,
    storage_key: String,
    compressed_size_bytes: i32,
}

type LineKey = (DateTime<Utc>, Uuid, i32);
#[derive(Serialize, Deserialize)]
struct Cursor {
    version: u8,
    scope: String,
    before: LineKey,
}
fn invalid(message: &str) -> LogAggregatorError {
    LogAggregatorError::Validation {
        message: message.into(),
    }
}

impl GlobalLogSearchRequest {
    fn scope_hash(&self) -> Result<String, LogAggregatorError> {
        let mut q = self.clone();
        q.cursor = None;
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&q)?)))
    }
    fn validate(&self) -> Result<(), LogAggregatorError> {
        if self.end_time <= self.start_time
            || self.end_time - self.start_time > chrono::Duration::hours(24)
        {
            return Err(invalid("Choose a log window between zero and 24 hours"));
        }
        if !(1..=500).contains(&self.page_size.unwrap_or(100)) {
            return Err(invalid("Page size must be between 1 and 500"));
        }
        for values in [
            &self.projects,
            &self.external_services,
            &self.scopes,
            &self.envs,
        ] {
            if values.len() > 100 || values.iter().any(|s| s.len() > 256) {
                return Err(invalid("Too many or oversized log filters"));
            }
        }
        if self.node_ids.len() > 100 || self.text.as_ref().is_some_and(|s| s.len() > 1024) {
            return Err(invalid("Oversized log filter"));
        }
        for scope in &self.scopes {
            if !scope.split_once(':').is_some_and(|(kind, id)| {
                matches!(kind, "application" | "service")
                    && id.parse::<i32>().is_ok_and(|id| id > 0)
            }) {
                return Err(invalid("Invalid log resource scope"));
            }
        }
        Ok(())
    }
    fn cursor_key(&self) -> Result<Option<LineKey>, LogAggregatorError> {
        let Some(raw) = &self.cursor else {
            return Ok(None);
        };
        if raw.len() > 2048 {
            return Err(invalid("Invalid log cursor"));
        }
        let cursor: Cursor =
            serde_json::from_str(raw).map_err(|_| invalid("Invalid log cursor"))?;
        if cursor.version != 1
            || cursor.scope != self.scope_hash()?
            || cursor.before.0 < self.start_time
            || cursor.before.0 > self.end_time
            || cursor.before.2 < 0
        {
            return Err(invalid("Cursor does not match the log search"));
        }
        Ok(Some(cursor.before))
    }
}

impl LogSearchService {
    pub(crate) async fn search_global(
        &self,
        q: &GlobalLogSearchRequest,
        access: &GlobalLogAccess,
    ) -> Result<GlobalLogSearchResponse, LogAggregatorError> {
        q.validate()?;
        let before = q.cursor_key()?;
        let text = q.text.as_ref().map(|text| text.to_lowercase());
        let mut frontier: Option<(DateTime<Utc>, Uuid)> = None;
        let mut matches = BTreeMap::<LineKey, GlobalLogLine>::new();
        let cap = q.page_size.unwrap_or(100) as usize;
        let mut scanned_chunks = 0;
        let mut scanned_bytes: usize = 0;
        let mut complete = false;
        'scan: loop {
            let chunks = self
                .metadata_service
                .global_chunks(q, access, before.as_ref(), frontier)
                .await?;
            if chunks.is_empty() {
                complete = true;
                break;
            }
            let last_batch = chunks.len() < CHUNK_BATCH as usize;
            for chunk in chunks {
                // Scan all candidate chunks within the budget. Older chunk
                // writers recorded arrival bounds, so an ended_at value alone
                // cannot prove that an unread chunk contains no newer event.
                let declared = usize::try_from(chunk.compressed_size_bytes).unwrap_or(usize::MAX);
                if scanned_chunks >= MAX_CHUNKS
                    || declared > MAX_CHUNK_BYTES
                    || scanned_bytes.saturating_add(declared) > MAX_COMPRESSED_BYTES
                {
                    break 'scan;
                }
                let compressed = self
                    .storage
                    .read_chunk_range(&chunk.storage_key, 0, Some(MAX_CHUNK_BYTES as u64 + 1))
                    .await?;
                scanned_bytes += compressed.len();
                if compressed.len() > MAX_CHUNK_BYTES || scanned_bytes > MAX_COMPRESSED_BYTES {
                    break 'scan;
                }
                // At most one chunk is decoded per search; decoding never blocks
                // the async executor and decompression expansion is capped.
                let data =
                    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, LogAggregatorError> {
                        let decoder = zstd::stream::read::Decoder::new(compressed.as_slice())?;
                        let mut data = Vec::new();
                        decoder
                            .take(MAX_DECOMPRESSED_BYTES + 1)
                            .read_to_end(&mut data)?;
                        Ok(data)
                    })
                    .await
                    .map_err(|_| invalid("Log decompression task failed"))??;
                if data.len() as u64 > MAX_DECOMPRESSED_BYTES {
                    break 'scan;
                }
                scanned_chunks += 1;
                for (offset, raw) in data.split(|b| *b == b'\n').enumerate() {
                    let Ok(line) = serde_json::from_slice::<LogLine>(raw) else {
                        continue;
                    };
                    let key = (line.ts, chunk.id, offset as i32);
                    if before.as_ref().is_some_and(|before| key >= *before)
                        || line.ts < q.start_time
                        || line.ts > q.end_time
                        || (!q.levels.is_empty() && !q.levels.contains(&line.level))
                        || (!q.envs.is_empty() && !q.envs.contains(&line.env))
                        || (!q.node_ids.is_empty()
                            && !line.node_id.is_some_and(|id| q.node_ids.contains(&id)))
                        || q.deploy_id.is_some_and(|id| line.deploy_id != Some(id))
                        || text
                            .as_ref()
                            .is_some_and(|text| !line.msg.to_lowercase().contains(text))
                    {
                        continue;
                    }
                    if matches.len() > cap
                        && matches
                            .first_key_value()
                            .is_some_and(|(oldest, _)| key <= *oldest)
                    {
                        continue;
                    }
                    if raw.len() > 64 * 1024 {
                        break 'scan;
                    }
                    matches.insert(
                        key,
                        GlobalLogLine {
                            project_id: chunk
                                .external_service_id
                                .is_none()
                                .then_some(chunk.project_id),
                            external_service_id: chunk.external_service_id,
                            owner: chunk.owner.clone(),
                            env: chunk.env.clone(),
                            line: LogSearchLine {
                                timestamp: line.ts,
                                level: line.level,
                                service: line.service,
                                message: line.msg,
                                fields: line.fields,
                                chunk_id: chunk.id,
                                line_offset: offset as i32,
                                deploy_id: line.deploy_id,
                                container_id: line.container_id,
                                node_id: line.node_id,
                                node_name: line.node_name,
                                context: None,
                            },
                        },
                    );
                    if matches.len() > cap + 1 {
                        matches.pop_first();
                    }
                }
                frontier = Some((chunk.ended_at, chunk.id));
            }
            if last_batch {
                complete = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        if !complete {
            return Ok(GlobalLogSearchResponse {
                lines: matches
                    .into_iter()
                    .rev()
                    .take(cap)
                    .map(|(_, line)| line)
                    .collect(),
                next_cursor: None,
                scan_limit_reached: true,
                scanned_chunks,
                scanned_bytes,
            });
        }
        let has_more = matches.len() > cap;
        let page: Vec<_> = matches.into_iter().rev().take(cap).collect();
        let next_cursor = if has_more {
            page.last()
                .map(|(key, _)| {
                    serde_json::to_string(&Cursor {
                        version: 1,
                        scope: q.scope_hash()?,
                        before: *key,
                    })
                    .map_err(LogAggregatorError::from)
                })
                .transpose()?
        } else {
            None
        };
        Ok(GlobalLogSearchResponse {
            lines: page.into_iter().map(|(_, line)| line).collect(),
            next_cursor,
            scan_limit_reached: false,
            scanned_chunks,
            scanned_bytes,
        })
    }
}

impl super::LogMetadataService {
    async fn global_chunks(
        &self,
        q: &GlobalLogSearchRequest,
        access: &GlobalLogAccess,
        before: Option<&LineKey>,
        frontier: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<Candidate>, LogAggregatorError> {
        let mut values = Vec::<Value>::new();
        let mut bind = |value: Value| {
            values.push(value);
            format!("${}", values.len())
        };
        let hidden = bind(access.hidden_projects.clone().into());
        let bound = bind(access.bound_project.into());
        let unrestricted = bind(access.unrestricted_services.into());
        let user = bind(access.user_id.into());
        let from = bind(q.start_time.into());
        let to = bind(before.map_or(q.end_time, |key| key.0).into());
        let mut conditions = vec![
            format!("c.started_at <= {to} AND c.ended_at >= {from}"),
            format!(
                r#"(
            (c.external_service_id IS NULL AND p.id IS NOT NULL AND NOT (p.id = ANY({hidden}::int[])) AND ({bound}::int IS NULL OR p.id = {bound})) OR
            (c.external_service_id IS NOT NULL AND s.id IS NOT NULL AND (
                ({unrestricted} AND {bound}::int IS NULL) OR
                EXISTS(SELECT 1 FROM project_services ps JOIN projects ap ON ap.id=ps.project_id WHERE ps.service_id=s.id AND NOT(ap.id=ANY({hidden}::int[])) AND ({bound}::int IS NULL OR ap.id={bound})) OR
                ({bound}::int IS NULL AND s.created_by_user_id={user}::int AND NOT EXISTS(SELECT 1 FROM project_services ps WHERE ps.service_id=s.id))
            ))
        )"#
            ),
        ];
        match q.source {
            GlobalLogSource::Application => conditions.push("c.external_service_id IS NULL".into()),
            GlobalLogSource::Service => conditions.push("c.external_service_id IS NOT NULL".into()),
            _ => {}
        }
        if !q.projects.is_empty() {
            let v = bind(q.projects.clone().into());
            conditions.push(format!("c.external_service_id IS NULL AND (p.id::text=ANY({v}::text[]) OR p.name=ANY({v}::text[]) OR p.slug=ANY({v}::text[]))"));
        }
        if !q.external_services.is_empty() {
            let v = bind(q.external_services.clone().into());
            conditions.push(format!(
                "(s.id::text=ANY({v}::text[]) OR s.name=ANY({v}::text[]))"
            ));
        }
        if !q.scopes.is_empty() {
            let v = bind(q.scopes.clone().into());
            conditions.push(format!("(CASE WHEN c.external_service_id IS NULL THEN 'application:' || c.project_id::text ELSE 'service:' || c.external_service_id::text END)=ANY({v}::text[])"));
        }
        if !q.envs.is_empty() {
            let v = bind(q.envs.clone().into());
            conditions.push(format!("c.env=ANY({v}::text[])"));
        }
        if !q.node_ids.is_empty() {
            let v = bind(q.node_ids.clone().into());
            conditions.push(format!("c.node_id=ANY({v}::int[])"));
        }
        if let Some(id) = q.deploy_id {
            let v = bind(id.into());
            conditions.push(format!("c.deploy_id={v}"));
        }
        if let Some((time, id)) = frontier {
            let t = bind(time.into());
            let i = bind(id.into());
            conditions.push(format!("(c.ended_at,c.id)<({t},{i})"));
        }
        let sql = format!("SELECT c.id,c.project_id,c.external_service_id,COALESCE(s.name,p.name) AS owner,c.env,c.ended_at,c.storage_key,c.compressed_size_bytes FROM log_chunks c LEFT JOIN projects p ON p.id=c.project_id AND c.external_service_id IS NULL LEFT JOIN external_services s ON s.id=c.external_service_id WHERE {} ORDER BY c.ended_at DESC,c.id DESC LIMIT {CHUNK_BATCH}", conditions.join(" AND "));
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?;
        rows.iter()
            .map(|row| Candidate::from_query_result(row, "").map_err(Into::into))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        services::LogMetadataService,
        storage::{FilesystemStorage, LogStorage},
    };
    use sea_orm::{ConnectOptions, Database, DatabaseBackend, MockDatabase};
    use std::{
        collections::{BTreeMap, HashSet},
        sync::Arc,
    };

    fn request() -> GlobalLogSearchRequest {
        serde_json::from_value(serde_json::json!({"start_time":"2026-01-01T00:00:00Z", "end_time":"2026-01-02T00:00:00Z", "page_size":17})).unwrap()
    }

    fn mock_candidate(id: u128, key: &str, size: i32) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::new();
        row.insert("id".into(), Uuid::from_u128(id).into());
        row.insert("project_id".into(), 1.into());
        row.insert("external_service_id".into(), Option::<i32>::None.into());
        row.insert("owner".into(), "Project".into());
        row.insert("env".into(), "production".into());
        row.insert(
            "ended_at".into(),
            "2026-01-01T12:00:01Z"
                .parse::<DateTime<Utc>>()
                .unwrap()
                .into(),
        );
        row.insert("storage_key".into(), key.into());
        row.insert("compressed_size_bytes".into(), size.into());
        row
    }

    async fn mock_search(lines: &[(&str, &str)], text: Option<&str>) -> GlobalLogSearchResponse {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(dir.path().into()).unwrap());
        let mut content = Vec::new();
        for (timestamp, message) in lines {
            content.extend(
                serde_json::to_vec(&serde_json::json!({
                    "ts": timestamp, "level": "ERROR", "msg": message,
                    "stream": "stderr", "container_id": "container-1", "service": "app",
                    "env": "production", "project_id": 1
                }))
                .unwrap(),
            );
            content.push(b'\n');
        }
        let compressed = zstd::encode_all(content.as_slice(), 1).unwrap();
        storage.write_chunk("valid.zst", &compressed).await.unwrap();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    mock_candidate(2, "valid.zst", compressed.len() as i32),
                    mock_candidate(1, "oversized.zst", MAX_CHUNK_BYTES as i32 + 1),
                ]])
                .into_connection(),
        );
        let service = LogSearchService::new(storage, Arc::new(LogMetadataService::new(db)));
        let mut q = request();
        q.page_size = Some(2);
        q.text = text.map(str::to_owned);
        service
            .search_global(
                &q,
                &GlobalLogAccess {
                    hidden_projects: vec![],
                    bound_project: None,
                    unrestricted_services: true,
                    user_id: None,
                },
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn budget_exhaustion_keeps_newest_collected_lines_without_cursor() {
        let result = mock_search(
            &[
                ("2026-01-01T12:00:00Z", "oldest"),
                ("2026-01-01T12:00:03Z", "newest"),
                ("2026-01-01T12:00:02Z", "middle"),
            ],
            None,
        )
        .await;
        assert!(result.scan_limit_reached);
        assert_eq!(result.scanned_chunks, 1);
        assert!(result.scanned_bytes > 0);
        assert!(result.next_cursor.is_none());
        assert_eq!(
            result
                .lines
                .iter()
                .map(|line| line.line.message.as_str())
                .collect::<Vec<_>>(),
            vec!["newest", "middle"]
        );
    }

    #[tokio::test]
    async fn budget_exhaustion_without_matches_returns_empty_lines() {
        let result = mock_search(&[("2026-01-01T12:00:00Z", "unrelated")], Some("absent")).await;
        assert!(result.scan_limit_reached);
        assert!(result.lines.is_empty());
        assert!(result.next_cursor.is_none());
    }

    #[test]
    fn global_cursor_is_exact_and_bound_to_filters() {
        let mut q = request();
        let key = (
            "2026-01-01T12:00:00.123456789Z".parse().unwrap(),
            Uuid::new_v4(),
            23,
        );
        q.cursor = Some(
            serde_json::to_string(&Cursor {
                version: 1,
                scope: q.scope_hash().unwrap(),
                before: key,
            })
            .unwrap(),
        );
        assert_eq!(q.cursor_key().unwrap(), Some(key));
        q.text = Some("changed".into());
        assert!(q.cursor_key().is_err());
        q.cursor = Some("bad".into());
        assert!(q.cursor_key().is_err());
        q.page_size = Some(501);
        assert!(q.validate().is_err());
    }

    /// Uses session-local TEMP tables; never changes operator data or resources.
    #[tokio::test]
    async fn global_logs_postgres_pagination_access_and_budget() {
        let Ok(url) = std::env::var("GLOBAL_LOGS_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("TEST_DATABASE_URL"))
        else {
            eprintln!("Skipping PostgreSQL integration: TEST_DATABASE_URL is not configured");
            return;
        };
        let mut opts = ConnectOptions::new(url);
        opts.max_connections(1).min_connections(1);
        let db = Arc::new(Database::connect(opts).await.unwrap());
        db.execute_unprepared(r#"
            CREATE TEMP TABLE projects(id int PRIMARY KEY,name text,slug text);
            CREATE TEMP TABLE external_services(id int PRIMARY KEY,name text,created_by_user_id int);
            CREATE TEMP TABLE project_services(project_id int,service_id int);
            CREATE TEMP TABLE log_chunks(id uuid PRIMARY KEY,project_id int,external_service_id int,env text,ended_at timestamptz,started_at timestamptz,storage_key text,compressed_size_bytes int,node_id int,deploy_id int);
            INSERT INTO projects SELECT i,'Project '||i,'project-'||i FROM generate_series(1,110) i;
            INSERT INTO external_services VALUES(201,'Shared database',42),(202,'Private database',42),(203,'Standalone database',42);
            INSERT INTO project_services VALUES(1,201),(110,201),(110,202);
        "#).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(dir.path().into()).unwrap());
        let service = LogSearchService::new(
            storage.clone(),
            Arc::new(LogMetadataService::new(db.clone())),
        );
        for id in (1..=110).chain(201..=203) {
            let key = format!("{id}.zst");
            // Identical timestamps and multiple offsets exercise all cursor keys.
            let mut content = Vec::new();
            for offset in 0..2 {
                let value = serde_json::json!({"ts":"2026-01-01T12:00:00.123456789Z","level":"ERROR","msg":format!("message {id}:{offset}"),"stream":"stderr","container_id":format!("container-{id}"),"service":"app","env":"production","project_id":if id<200 {id} else {0}});
                content.extend(serde_json::to_vec(&value).unwrap());
                content.push(b'\n');
            }
            let compressed = zstd::encode_all(content.as_slice(), 1).unwrap();
            storage.write_chunk(&key, &compressed).await.unwrap();
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "INSERT INTO log_chunks VALUES($1,$2,$3,'production','2026-01-01T12:00:01Z','2026-01-01T12:00:00Z',$4,$5,NULL,NULL)",
                [Uuid::from_u128(id as u128).into(),if id<200 {id} else {0}.into(),if id>=200 {Some(id)} else {None}.into(),key.into(),(compressed.len() as i32).into()])).await.unwrap();
        }
        let unrestricted = GlobalLogAccess {
            hidden_projects: vec![],
            bound_project: None,
            unrestricted_services: true,
            user_id: Some(42),
        };
        let mut q = request();
        let mut seen = HashSet::new();
        let mut previous = None;
        loop {
            let response = service.search_global(&q, &unrestricted).await.unwrap();
            assert!(!response.scan_limit_reached);
            for line in response.lines {
                let key = (
                    line.line.timestamp,
                    line.line.chunk_id,
                    line.line.line_offset,
                );
                if let Some(previous) = previous {
                    assert!(key < previous);
                }
                previous = Some(key);
                assert!(seen.insert(key));
            }
            q.cursor = response.next_cursor;
            if q.cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            seen.len(),
            226,
            "all projects and services, no timestamp-tie gaps"
        );
        q = request();
        q.page_size = Some(500);
        let restricted = GlobalLogAccess {
            hidden_projects: vec![110],
            bound_project: None,
            unrestricted_services: false,
            user_id: Some(7),
        };
        let result = service.search_global(&q, &restricted).await.unwrap();
        assert_eq!(result.lines.len(), 220); // 109 projects + shared service
        assert!(result.lines.iter().all(|line| line.project_id != Some(110)
            && !matches!(line.external_service_id, Some(202 | 203))));
        let token = GlobalLogAccess {
            hidden_projects: vec![],
            bound_project: Some(1),
            unrestricted_services: false,
            user_id: None,
        };
        let result = service.search_global(&q, &token).await.unwrap();
        assert_eq!(result.lines.len(), 4);
        assert!(result
            .lines
            .iter()
            .all(|line| line.project_id == Some(1) || line.external_service_id == Some(201)));
        q.projects = vec!["project-105".into()];
        assert_eq!(
            service
                .search_global(&q, &unrestricted)
                .await
                .unwrap()
                .lines
                .len(),
            2
        );
        q.text = Some("absent".into());
        assert!(service
            .search_global(&q, &unrestricted)
            .await
            .unwrap()
            .lines
            .is_empty());
        q.text = None;
        q.projects.clear();
        db.execute_unprepared(
            "UPDATE log_chunks SET compressed_size_bytes=99999999 WHERE project_id=105",
        )
        .await
        .unwrap();
        let limited = service.search_global(&q, &unrestricted).await.unwrap();
        assert!(limited.scan_limit_reached);
        assert_eq!(limited.lines.len(), 16); // 8 newer chunks before project 105
        assert!(limited.next_cursor.is_none());
    }
}
