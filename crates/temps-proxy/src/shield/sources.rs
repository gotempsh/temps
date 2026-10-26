// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control-plane jobs that keep the reputation lists current (ADR 066,
//! D2 and I9).
//!
//! Every [`TICK`], each control plane:
//!
//! 1. **Fetches** every enabled source last tried more than
//!    [`FETCH_INTERVAL`] ago. The `UPDATE` that stamps `last_attempt_at` is
//!    the claim, so one plane fetches each source per interval however many
//!    run. A fetch sends the stored `ETag`s; a list that parses badly, or is
//!    mostly not networks, is refused and the last good copy kept.
//! 2. **Compiles** a new `shield_snapshots` revision from every enabled
//!    source when a list changed since the newest revision, or once a day
//!    so undated lists stay fresh. It runs under a transaction-scoped
//!    advisory lock, so two planes never compile at once, and keeps the
//!    last [`KEEP_SNAPSHOTS`].
//!
//! Every ingress then loads the new revision (see [`super::lists`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, EntityTrait, QueryOrder,
    Statement, TransactionTrait,
};
use sha2::{Digest, Sha256};
use temps_entities::{shield_snapshots, shield_sources};
use temps_shield::feeds::{self, Feed, FeedError};
use temps_shield::{ShieldSnapshot, SnapshotError, SourceList};
use tracing::{debug, info, warn};

pub const TICK: Duration = Duration::from_secs(5 * 60);
pub const FETCH_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// A new revision at least this often, so lists dated by their fetch do not
/// age out of the snapshot while they are being fetched.
pub const RECOMPILE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const KEEP_SNAPSHOTS: i64 = 10;
/// The full IPv6 bogon list, the largest, is 2.6 MB.
const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const COMPILE_LOCK: &str = "temps_shield_compile";

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("database: {0}")]
    Database(#[from] DbErr),
    #[error("{url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url}: HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url}: larger than {MAX_DOCUMENT_BYTES} bytes")]
    TooLarge { url: String },
    #[error("{url}: not UTF-8")]
    NotText { url: String },
    #[error("{source_name}: {error}")]
    Feed {
        source_name: &'static str,
        error: FeedError,
    },
    #[error("{source_name}: stored list: {error}")]
    Stored {
        source_name: String,
        error: SnapshotError,
    },
    #[error("snapshot: {0}")]
    Snapshot(#[from] SnapshotError),
}

/// What one fetch found.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// Another plane holds the claim, the source is disabled, or it was
    /// fetched recently.
    NotDue,
    /// Every document answered 304, or its networks are the stored ones.
    Unchanged,
    Changed {
        entries: usize,
    },
}

pub struct ShieldSources {
    db: Arc<DatabaseConnection>,
    http: reqwest::Client,
    feeds: &'static [Feed],
}

impl ShieldSources {
    pub fn new(db: Arc<DatabaseConnection>) -> Result<Self, reqwest::Error> {
        Self::with_feeds(db, feeds::FEEDS)
    }

    pub fn with_feeds(
        db: Arc<DatabaseConnection>,
        feeds: &'static [Feed],
    ) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("temps/", env!("CARGO_PKG_VERSION"), " (shield)"))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { db, http, feeds })
    }

    /// Run the jobs every [`TICK`] on the current runtime, the first time a
    /// minute after start so startup is not slowed by downloads.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let mut ticker = tokio::time::interval(TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                self.run_once().await;
            }
        })
    }

    /// One pass: fetch what is due, then compile if needed. Failures are
    /// logged and left for the next pass.
    pub async fn run_once(&self) {
        if let Err(e) = self.ensure_sources().await {
            debug!(error = %e, "Shield sources are not available yet");
            return;
        }
        for feed in self.feeds {
            match self.fetch(feed).await {
                Ok(Fetched::Changed { entries }) => {
                    info!(source = feed.name, entries, "Shield list updated");
                }
                Ok(Fetched::Unchanged | Fetched::NotDue) => {}
                Err(e) => {
                    warn!(source = feed.name, error = %e, "Shield list fetch failed; keeping the last good copy");
                    if let Err(e) = self.record_error(feed, &e).await {
                        debug!(source = feed.name, error = %e, "could not record the fetch error");
                    }
                }
            }
        }
        match self.compile().await {
            Ok(Some(revision)) => info!(revision, "Shield snapshot compiled"),
            Ok(None) => {}
            Err(e) => warn!(error = %e, "Shield snapshot compile failed"),
        }
    }

    /// A row for every feed, enabled as the feed is by default. Existing
    /// rows, and an operator's choice in them, are left alone.
    async fn ensure_sources(&self) -> Result<(), DbErr> {
        for feed in self.feeds {
            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO shield_sources (name, enabled) VALUES ($1, $2) \
                     ON CONFLICT (name) DO NOTHING",
                    [feed.name.into(), feed.enabled_by_default.into()],
                ))
                .await?;
        }
        Ok(())
    }

    /// Fetch `feed` if it is due and this plane wins the claim.
    pub async fn fetch(&self, feed: &'static Feed) -> Result<Fetched, SourceError> {
        let Some(claim) = self.claim(feed).await? else {
            return Ok(Fetched::NotDue);
        };
        let now = Utc::now();
        let mut documents: BTreeMap<&str, Document> = BTreeMap::new();
        for url in feed.urls {
            // Without a stored copy a 304 would leave nothing to use.
            let etag = claim.list.as_ref().and(claim.etags.get(*url));
            documents.insert(url, self.get(url, etag.map(String::as_str)).await?);
        }
        if documents
            .values()
            .all(|d| matches!(d, Document::NotModified))
        {
            self.record_unchanged(feed, claim.list, now.timestamp())
                .await?;
            return Ok(Fetched::Unchanged);
        }
        // Some changed and some did not: the unchanged ones are needed in
        // full to rebuild the list.
        for url in feed.urls {
            if matches!(documents.get(*url), Some(Document::NotModified)) {
                documents.insert(url, self.get(url, None).await?);
            }
        }
        let mut etags = serde_json::Map::new();
        let mut texts = Vec::with_capacity(documents.len());
        for (url, document) in &documents {
            match document {
                Document::Body { text, etag } => {
                    if let Some(etag) = etag {
                        etags.insert((*url).to_string(), etag.clone().into());
                    }
                    texts.push(text.as_str());
                }
                // Refetched above without a condition, so never a 304.
                Document::NotModified => {}
            }
        }
        let list =
            feeds::assemble(feed, &texts, now.timestamp()).map_err(|error| SourceError::Feed {
                source_name: feed.name,
                error,
            })?;
        let digest = hex::encode(list.digest()?);
        // A stored copy the compiler could not read was left out of the
        // snapshot, so a readable one is a change even with the same networks.
        let stored_readable = claim
            .list
            .as_deref()
            .is_some_and(|bytes| SourceList::decode(bytes).is_ok());
        let changed = !stored_readable || claim.sha256.as_deref() != Some(digest.as_str());
        let entries = list.len();
        let published_at = chrono::DateTime::from_timestamp(list.published_at, 0).unwrap_or(now);
        let body = list.encode()?;
        // `updated_at` moves only when the networks did: it is what tells
        // the compiler a new revision is needed.
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE shield_sources SET etags = $2, last_fetched_at = now(), \
                 last_error = NULL, published_at = $3, entries = $4, sha256 = $5, list = $6, \
                 updated_at = CASE WHEN $7 THEN clock_timestamp() ELSE updated_at END \
                 WHERE name = $1",
                [
                    feed.name.into(),
                    serde_json::Value::Object(etags).into(),
                    published_at.into(),
                    i32::try_from(entries).unwrap_or(i32::MAX).into(),
                    digest.into(),
                    body.into(),
                    changed.into(),
                ],
            ))
            .await?;
        Ok(if changed {
            Fetched::Changed { entries }
        } else {
            Fetched::Unchanged
        })
    }

    /// Stamp the attempt if the source is enabled and due. The row update is
    /// atomic, so of several planes only one gets the row back.
    async fn claim(&self, feed: &Feed) -> Result<Option<Claim>, SourceError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE shield_sources SET last_attempt_at = now() \
                 WHERE name = $1 AND enabled \
                 AND (last_attempt_at IS NULL \
                      OR last_attempt_at < now() - make_interval(secs => $2)) \
                 RETURNING etags, sha256, list",
                [feed.name.into(), (FETCH_INTERVAL.as_secs() as f64).into()],
            ))
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let etags: serde_json::Value = row.try_get("", "etags")?;
        let etags = etags
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(url, etag)| Some((url.clone(), etag.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(Claim {
            etags,
            sha256: row.try_get("", "sha256")?,
            list: row.try_get("", "list")?,
        }))
    }

    async fn get(&self, url: &str, etag: Option<&str>) -> Result<Document, SourceError> {
        let http = |source| SourceError::Http {
            url: url.to_string(),
            source,
        };
        let mut request = self.http.get(url);
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let mut response = request.send().await.map_err(http)?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(Document::NotModified);
        }
        if !status.is_success() {
            return Err(SourceError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(http)? {
            if body.len() + chunk.len() > MAX_DOCUMENT_BYTES {
                return Err(SourceError::TooLarge {
                    url: url.to_string(),
                });
            }
            body.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(body).map_err(|_| SourceError::NotText {
            url: url.to_string(),
        })?;
        Ok(Document::Body { text, etag })
    }

    /// Every document answered 304. An undated list is as fresh as this
    /// fetch, so its stored copy is re-dated; a dated one keeps its date. A
    /// stored copy that no longer decodes is dropped with its ETags, so the
    /// next fetch downloads the list in full.
    async fn record_unchanged(
        &self,
        feed: &Feed,
        stored: Option<Vec<u8>>,
        now: i64,
    ) -> Result<(), SourceError> {
        let list = match stored.as_deref().map(SourceList::decode) {
            Some(Ok(list)) => Some(list),
            Some(Err(error)) => {
                self.forget_stored(feed).await?;
                return Err(SourceError::Stored {
                    source_name: feed.name.to_string(),
                    error,
                });
            }
            None => None,
        };
        let redated = match list {
            Some(mut list) if !feed.dated => {
                list.published_at = now;
                Some(list.encode()?)
            }
            Some(_) | None => None,
        };
        let published_at = chrono::DateTime::from_timestamp(now, 0);
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE shield_sources SET last_fetched_at = now(), last_error = NULL, \
                 list = COALESCE($2, list), \
                 published_at = CASE WHEN $2 IS NULL THEN published_at ELSE $3 END \
                 WHERE name = $1",
                [feed.name.into(), redated.into(), published_at.into()],
            ))
            .await?;
        Ok(())
    }

    async fn forget_stored(&self, feed: &Feed) -> Result<(), DbErr> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE shield_sources SET etags = '{}'::jsonb, sha256 = NULL, list = NULL, \
                 entries = 0, updated_at = clock_timestamp() WHERE name = $1",
                [feed.name.into()],
            ))
            .await?;
        Ok(())
    }

    async fn record_error(&self, feed: &Feed, error: &SourceError) -> Result<(), DbErr> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE shield_sources SET last_error = $2 WHERE name = $1",
                [feed.name.into(), error.to_string().into()],
            ))
            .await?;
        Ok(())
    }

    /// Store a new snapshot revision if one is needed and no other plane is
    /// compiling. Returns the new revision.
    pub async fn compile(&self) -> Result<Option<i64>, SourceError> {
        let txn = self.db.begin().await?;
        let locked = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_try_advisory_xact_lock(hashtext($1)) AS acquired",
                [COMPILE_LOCK.into()],
            ))
            .await?
            .map(|row| row.try_get::<bool>("", "acquired"))
            .transpose()?
            .unwrap_or(false);
        if !locked {
            return Ok(None);
        }
        // Read the sources and the newest snapshot in one statement each;
        // the snapshot records the newest source change it was built from,
        // so a change that commits while this runs is seen by the next pass.
        let sources = shield_sources::Entity::find().all(&txn).await?;
        let sources_updated_at = sources.iter().map(|source| source.updated_at).max();
        let latest = shield_snapshots::Entity::find()
            .order_by_desc(shield_snapshots::Column::Revision)
            .one(&txn)
            .await?;
        let recompile_after = Utc::now()
            - chrono::Duration::from_std(RECOMPILE_INTERVAL).unwrap_or(chrono::Duration::days(1));
        let due = match &latest {
            None => true,
            Some(latest) => {
                latest.created_at < recompile_after
                    || sources_updated_at > latest.sources_updated_at
            }
        };
        if !due {
            return Ok(None);
        }

        let mut lists = Vec::new();
        for source in sources {
            let (true, Some(bytes)) = (source.enabled, source.list.as_deref()) else {
                continue;
            };
            // One unreadable copy must not hold back every other list; the
            // next fetch of that source replaces it.
            let mut list = match SourceList::decode(bytes) {
                Ok(list) => list,
                Err(error) => {
                    warn!(source = %source.name, error = %error, "Leaving an unreadable Shield list out of the snapshot");
                    continue;
                }
            };
            // The verdict and age limit are the code's, not the row's, so a
            // release that changes them applies to stored copies too.
            if let Some(feed) = self.feeds.iter().find(|feed| feed.name == source.name) {
                list.verdict = feed.verdict;
                list.max_age_secs = Some(feed.max_age_secs);
            }
            lists.push(list);
        }
        lists.sort_by(|a, b| a.source.cmp(&b.source));

        let revision: i64 = txn
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT nextval(pg_get_serial_sequence('shield_snapshots', 'revision')) \
                 AS revision",
            ))
            .await?
            .map(|row| row.try_get::<i64>("", "revision"))
            .transpose()?
            .ok_or_else(|| DbErr::RecordNotFound("shield_snapshots revision".into()))?;
        let snapshot = ShieldSnapshot {
            revision: u64::try_from(revision).unwrap_or(0),
            generated_at: Utc::now().timestamp(),
            lists,
        };
        let body = snapshot.encode()?;
        let sha256 = hex::encode(Sha256::digest(&body));
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO shield_snapshots (revision, sha256, body, sources_updated_at) \
             VALUES ($1, $2, $3, $4)",
            [
                revision.into(),
                sha256.into(),
                body.into(),
                sources_updated_at.into(),
            ],
        ))
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM shield_snapshots WHERE revision NOT IN \
             (SELECT revision FROM shield_snapshots ORDER BY revision DESC LIMIT $1)",
            [KEEP_SNAPSHOTS.into()],
        ))
        .await?;
        txn.commit().await?;
        Ok(Some(revision))
    }
}

struct Claim {
    etags: BTreeMap<String, String>,
    sha256: Option<String>,
    list: Option<Vec<u8>>,
}

enum Document {
    NotModified,
    Body { text: String, etag: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
    use temps_shield::feeds::Format;
    use temps_shield::{Action, ListVerdict};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-route HTTP server: answers `body` with `ETag: "v1"`, or 304
    /// when the request carries that tag. Counts the requests it answers.
    async fn serve(body: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/list.txt", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut request = vec![0; 4096];
                let n = stream.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
                let response = if request.contains("if-none-match: \"v1\"") {
                    "HTTP/1.1 304 Not Modified\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\netag: \"v1\"\r\ncontent-length: {}\r\n\
                         connection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (url, hits)
    }

    fn feed(name: &'static str, url: String, verdict: ListVerdict) -> Feed {
        let urls: &'static [&'static str] = Box::leak(vec![&*url.leak()].into_boxed_slice());
        Feed {
            name,
            urls,
            format: Format::Plain,
            verdict,
            max_age_secs: 7 * 24 * 60 * 60,
            dated: false,
            enabled_by_default: true,
            terms: "test",
        }
    }

    /// A migrated database, or `None` without a container runtime.
    async fn database() -> anyhow::Result<Option<TestDatabase>> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Ok(Some(database)),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The whole path: fetch, claim, 304, compile, the daily recompile, and
    /// what an ingress then sees.
    #[tokio::test]
    async fn fetched_lists_become_a_snapshot_an_ingress_loads() -> anyhow::Result<()> {
        let Some(database) = database().await? else {
            eprintln!("Skipping shield sources integration: Docker runtime unavailable");
            return Ok(());
        };
        let db = database.connection_arc().clone();
        let (url, hits) = serve("# test list\n198.18.0.0/15\n").await;
        let (broken_url, _) = serve("<html>oops</html>\n").await;
        let feeds: &'static [Feed] = Box::leak(Box::new([
            feed("good", url, ListVerdict::Block),
            feed("broken", broken_url, ListVerdict::Block),
        ]));
        let sources = ShieldSources::with_feeds(db.clone(), feeds).unwrap();

        sources.run_once().await;
        let revision = sources_revision(&db).await;
        assert_eq!(revision, Some(1));
        let lists = crate::shield::ShieldLists::default();
        assert!(lists.refresh(&db, &[]).await.unwrap());
        let ip = "198.18.3.4".parse().unwrap();
        let verdict = lists.evaluate(ip, Utc::now().timestamp());
        assert_eq!(verdict.action, Action::Block);
        assert_eq!(verdict.reason_strings(), ["good (198.18.0.0/15)"]);
        let error: Option<String> = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT last_error FROM shield_sources WHERE name = 'broken'",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "last_error")
            .unwrap();
        assert!(error.unwrap().contains("did not parse"));

        // Claimed this hour: nothing is fetched or compiled again.
        sources.run_once().await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(sources_revision(&db).await, Some(1));

        // Due again: the server answers 304, the list is re-dated, and
        // nothing changed, so no revision.
        db.execute_unprepared(
            "UPDATE shield_sources SET last_attempt_at = now() - interval '2 hours'",
        )
        .await
        .unwrap();
        assert_eq!(sources.fetch(&feeds[0]).await.unwrap(), Fetched::Unchanged);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(sources.compile().await.unwrap(), None);

        // A day later the snapshot is recompiled anyway.
        db.execute_unprepared(
            "UPDATE shield_snapshots SET created_at = now() - interval '25 hours'",
        )
        .await
        .unwrap();
        assert_eq!(sources.compile().await.unwrap(), Some(2));

        // Disabling a source drops it from the next revision.
        db.execute_unprepared(
            "UPDATE shield_sources SET enabled = false, updated_at = now() WHERE name = 'good'",
        )
        .await
        .unwrap();
        assert_eq!(sources.compile().await.unwrap(), Some(3));
        assert!(lists.refresh(&db, &[]).await.unwrap());
        assert_eq!(
            lists.evaluate(ip, Utc::now().timestamp()),
            temps_shield::Verdict::PASS
        );

        // An unreadable stored copy is left out of the snapshot rather than
        // failing it, and a 304 then drops it so the next fetch is in full.
        db.execute_unprepared(
            "UPDATE shield_sources SET enabled = true, list = '\\x00'::bytea, \
             last_attempt_at = now() - interval '2 hours', updated_at = now() WHERE name = 'good'",
        )
        .await?;
        assert!(sources.compile().await?.is_some());
        assert!(matches!(
            sources.fetch(&feeds[0]).await,
            Err(SourceError::Stored { .. })
        ));
        db.execute_unprepared(
            "UPDATE shield_sources SET last_attempt_at = now() - interval '2 hours' WHERE name = 'good'",
        )
        .await?;
        assert_eq!(
            sources.fetch(&feeds[0]).await?,
            Fetched::Changed { entries: 1 }
        );

        // The same networks arriving in full (no ETag to send) over an
        // unreadable copy whose checksum still matches are a change too, so
        // the list is back in the next snapshot rather than the next day's.
        db.execute_unprepared(
            "UPDATE shield_sources SET list = '\\x00'::bytea, etags = '{}'::jsonb, \
             last_attempt_at = now() - interval '2 hours' WHERE name = 'good'",
        )
        .await?;
        let before = sources_revision(&db).await;
        assert_eq!(
            sources.fetch(&feeds[0]).await?,
            Fetched::Changed { entries: 1 }
        );
        let revision = sources.compile().await?;
        assert!(revision.is_some() && revision > before);
        assert!(lists.refresh(&db, &[]).await?);
        assert_eq!(
            lists.evaluate(ip, Utc::now().timestamp()).action,
            Action::Block
        );

        // Only the last ten revisions are kept.
        for _ in 0..12 {
            db.execute_unprepared("UPDATE shield_sources SET updated_at = now()")
                .await
                .unwrap();
            sources.compile().await.unwrap();
        }
        let kept: i64 = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS kept FROM shield_snapshots",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "kept")
            .unwrap();
        assert_eq!(kept, KEEP_SNAPSHOTS);
        Ok(())
    }

    async fn sources_revision(db: &DatabaseConnection) -> Option<i64> {
        db.query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT max(revision) AS revision FROM shield_snapshots",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "revision")
        .unwrap()
    }
}
