use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_entities::source_maps;
use thiserror::Error;
use tracing::{debug, warn};

use super::types::CreateErrorEventData;

#[derive(Error, Debug)]
pub enum SourceMapError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Source map not found for release '{release}' and file '{file_path}'")]
    NotFound { release: String, file_path: String },

    #[error("Source map parsing error: {0}")]
    ParseError(String),

    #[error("Validation error: {0}")]
    Validation(String),
}

/// Response type for listing source maps (without the binary data)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceMapInfo {
    pub id: i32,
    pub project_id: i32,
    pub release: String,
    pub file_path: String,
    pub dist: Option<String>,
    pub size_bytes: i64,
    pub checksum: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

impl From<source_maps::Model> for SourceMapInfo {
    fn from(model: source_maps::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            release: model.release,
            file_path: model.file_path,
            dist: model.dist,
            size_bytes: model.size_bytes,
            checksum: model.checksum,
            created_at: model.created_at,
        }
    }
}

/// Service for managing source maps and symbolicating stack traces
pub struct SourceMapService {
    db: Arc<DatabaseConnection>,
}

impl SourceMapService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Upload a source map for a specific release and file path.
    /// If a source map already exists for the same (project, release, file_path), it is replaced.
    pub async fn upload(
        &self,
        project_id: i32,
        release: &str,
        file_path: &str,
        source_map_data: Vec<u8>,
        dist: Option<String>,
    ) -> Result<SourceMapInfo, SourceMapError> {
        if release.is_empty() {
            return Err(SourceMapError::Validation(
                "Release version cannot be empty".to_string(),
            ));
        }
        if file_path.is_empty() {
            return Err(SourceMapError::Validation(
                "File path cannot be empty".to_string(),
            ));
        }
        if source_map_data.is_empty() {
            return Err(SourceMapError::Validation(
                "Source map data cannot be empty".to_string(),
            ));
        }

        // Validate that the data is a valid source map
        sourcemap::SourceMap::from_slice(&source_map_data)
            .map_err(|e| SourceMapError::ParseError(format!("Invalid source map: {}", e)))?;

        let size_bytes = source_map_data.len() as i64;
        let checksum = {
            let mut hasher = Sha256::new();
            hasher.update(&source_map_data);
            hex::encode(hasher.finalize())
        };

        let normalized_path = normalize_file_path(file_path);

        // Upsert: delete existing if present, then insert
        let existing = source_maps::Entity::find()
            .filter(source_maps::Column::ProjectId.eq(project_id))
            .filter(source_maps::Column::Release.eq(release))
            .filter(source_maps::Column::FilePath.eq(&normalized_path))
            .one(self.db.as_ref())
            .await?;

        if let Some(existing) = existing {
            source_maps::Entity::delete_by_id(existing.id)
                .exec(self.db.as_ref())
                .await?;
            debug!(
                "Replaced existing source map for release='{}' file='{}'",
                release, normalized_path
            );
        }

        let new_map = source_maps::ActiveModel {
            project_id: Set(project_id),
            release: Set(release.to_string()),
            file_path: Set(normalized_path),
            source_map_data: Set(source_map_data),
            dist: Set(dist),
            size_bytes: Set(size_bytes),
            checksum: Set(Some(checksum)),
            created_at: Set(Utc::now()),
            ..Default::default()
        };

        let model = new_map.insert(self.db.as_ref()).await?;
        Ok(SourceMapInfo::from(model))
    }

    /// List all source maps for a project release.
    /// Only fetches metadata columns — excludes the large `source_map_data` blob.
    pub async fn list_for_release(
        &self,
        project_id: i32,
        release: &str,
    ) -> Result<Vec<SourceMapInfo>, SourceMapError> {
        #[derive(FromQueryResult)]
        struct SourceMapMetadata {
            id: i32,
            project_id: i32,
            release: String,
            file_path: String,
            dist: Option<String>,
            size_bytes: i64,
            checksum: Option<String>,
            created_at: chrono::DateTime<Utc>,
        }

        let maps = source_maps::Entity::find()
            .filter(source_maps::Column::ProjectId.eq(project_id))
            .filter(source_maps::Column::Release.eq(release))
            .order_by_asc(source_maps::Column::FilePath)
            .select_only()
            .column(source_maps::Column::Id)
            .column(source_maps::Column::ProjectId)
            .column(source_maps::Column::Release)
            .column(source_maps::Column::FilePath)
            .column(source_maps::Column::Dist)
            .column(source_maps::Column::SizeBytes)
            .column(source_maps::Column::Checksum)
            .column(source_maps::Column::CreatedAt)
            .into_model::<SourceMapMetadata>()
            .all(self.db.as_ref())
            .await?;

        Ok(maps
            .into_iter()
            .map(|m| SourceMapInfo {
                id: m.id,
                project_id: m.project_id,
                release: m.release,
                file_path: m.file_path,
                dist: m.dist,
                size_bytes: m.size_bytes,
                checksum: m.checksum,
                created_at: m.created_at,
            })
            .collect())
    }

    /// List all releases that have source maps for a project
    pub async fn list_releases(&self, project_id: i32) -> Result<Vec<String>, SourceMapError> {
        use sea_orm::{sea_query::Expr, FromQueryResult, QuerySelect};

        #[derive(Debug, FromQueryResult)]
        struct ReleaseRow {
            release: String,
        }

        let releases = source_maps::Entity::find()
            .filter(source_maps::Column::ProjectId.eq(project_id))
            .select_only()
            .column(source_maps::Column::Release)
            .group_by(source_maps::Column::Release)
            .order_by_desc(Expr::col(source_maps::Column::CreatedAt).max())
            .into_model::<ReleaseRow>()
            .all(self.db.as_ref())
            .await?;

        Ok(releases.into_iter().map(|r| r.release).collect())
    }

    /// Delete all source maps for a specific release
    pub async fn delete_release(
        &self,
        project_id: i32,
        release: &str,
    ) -> Result<u64, SourceMapError> {
        let result = source_maps::Entity::delete_many()
            .filter(source_maps::Column::ProjectId.eq(project_id))
            .filter(source_maps::Column::Release.eq(release))
            .exec(self.db.as_ref())
            .await?;

        debug!(
            "Deleted {} source maps for project_id={} release='{}'",
            result.rows_affected, project_id, release
        );

        Ok(result.rows_affected)
    }

    /// Delete source maps for releases that are no longer tied to any active deployment.
    ///
    /// An "active" release is one whose commit SHA matches the `commit_sha` of any
    /// deployment currently pointed to by an environment's `current_deployment_id`.
    /// All other source maps for the given project are deleted.
    ///
    /// Returns the number of source map rows deleted.
    pub async fn delete_stale_source_maps(&self, project_id: i32) -> Result<u64, SourceMapError> {
        use temps_entities::{deployments, environments};

        // Step 1: Find all environment current_deployment_ids for this project
        let active_environments = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .all(self.db.as_ref())
            .await?;

        let active_deployment_ids: Vec<i32> = active_environments
            .iter()
            .filter_map(|env| env.current_deployment_id)
            .collect();

        if active_deployment_ids.is_empty() {
            debug!(
                "No active deployments found for project {} — skipping source map cleanup",
                project_id
            );
            return Ok(0);
        }

        // Step 2: Get the releases (commit SHAs) for those active deployments
        let active_deployments = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(active_deployment_ids))
            .all(self.db.as_ref())
            .await?;

        let keep_releases: Vec<String> = active_deployments
            .into_iter()
            .map(|d| d.commit_sha.unwrap_or_else(|| format!("deploy-{}", d.id)))
            .collect();

        if keep_releases.is_empty() {
            return Ok(0);
        }

        // Step 3: Delete source maps whose release is NOT in the active set
        let result = source_maps::Entity::delete_many()
            .filter(source_maps::Column::ProjectId.eq(project_id))
            .filter(source_maps::Column::Release.is_not_in(keep_releases.clone()))
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected > 0 {
            debug!(
                "Cleaned up {} stale source map(s) for project {} (keeping releases: {:?})",
                result.rows_affected, project_id, keep_releases
            );
        }

        Ok(result.rows_affected)
    }

    /// Delete a specific source map by ID
    pub async fn delete_by_id(
        &self,
        project_id: i32,
        source_map_id: i32,
    ) -> Result<(), SourceMapError> {
        let map = source_maps::Entity::find_by_id(source_map_id)
            .filter(source_maps::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SourceMapError::NotFound {
                release: "unknown".to_string(),
                file_path: format!("id={}", source_map_id),
            })?;

        source_maps::Entity::delete_by_id(map.id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    /// Symbolicate stack frames in an error event using stored source maps.
    ///
    /// Looks up source maps by the event's release version, then resolves each
    /// stack frame from minified to original coordinates.
    ///
    /// Modifies the error data in-place, adding original file/line/col/function
    /// information to each frame.
    pub async fn symbolicate_error_event(
        &self,
        error_data: &mut CreateErrorEventData,
    ) -> Result<(), SourceMapError> {
        let release = match &error_data.release_version {
            Some(r) if !r.is_empty() => r.clone(),
            _ => {
                debug!(
                    "Skipping symbolication for project {}: no release version",
                    error_data.project_id
                );
                return Ok(());
            }
        };

        debug!(
            "Attempting symbolication for project {} release '{}'",
            error_data.project_id, release
        );

        // Process each exception's stack trace
        let num_exceptions = error_data.exceptions.len();
        for (i, exception) in error_data.exceptions.iter_mut().enumerate() {
            if let Some(stack_trace) = &mut exception.stack_trace {
                debug!(
                    "Symbolicating exception {}/{} stack trace (type: {})",
                    i + 1,
                    num_exceptions,
                    exception.exception_type
                );
                self.symbolicate_stack_trace(error_data.project_id, &release, stack_trace)
                    .await;
            }
        }

        // Process legacy stack_trace field
        if let Some(stack_trace) = &mut error_data.stack_trace {
            debug!("Symbolicating legacy stack_trace field");
            self.symbolicate_stack_trace(error_data.project_id, &release, stack_trace)
                .await;
        }

        Ok(())
    }

    /// Symbolicate stack traces in a raw stored event (JSONB `data` column) on the fly.
    ///
    /// This is used at read time to symbolicate events that were stored before
    /// symbolication was implemented, or when ingestion-time symbolication missed them.
    ///
    /// The data structure is: `{ "sentry": { "release": "...", "exception": { "values": [{ "stacktrace": { "frames": [...] } }] } } }`
    ///
    /// Frames already marked with `"symbolicated": true` are skipped.
    pub async fn symbolicate_stored_event(&self, project_id: i32, data: &mut serde_json::Value) {
        // Extract the release from the sentry data
        let release = match data
            .get("sentry")
            .and_then(|s| s.get("release"))
            .and_then(|r| r.as_str())
        {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => return, // No release, can't symbolicate
        };

        // Get mutable reference to the exception values array
        let exceptions = match data
            .get_mut("sentry")
            .and_then(|s| s.get_mut("exception"))
            .and_then(|e| e.get_mut("values"))
            .and_then(|v| v.as_array_mut())
        {
            Some(excs) => excs,
            None => return,
        };

        let mut any_symbolicated = false;
        for exception in exceptions.iter_mut() {
            let stacktrace = match exception.get_mut("stacktrace") {
                Some(st) => st,
                None => continue,
            };

            let frames = match stacktrace.get_mut("frames").and_then(|f| f.as_array_mut()) {
                Some(f) => f,
                None => continue,
            };

            // Check if any frame needs symbolication
            let needs_symbolication = frames.iter().any(|f| {
                !f.get("symbolicated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    && f.get("lineno").is_some()
            });

            if !needs_symbolication {
                continue;
            }

            for frame in frames.iter_mut() {
                let obj = match frame.as_object_mut() {
                    Some(o) => o,
                    None => continue,
                };

                // Skip already-symbolicated frames
                if obj
                    .get("symbolicated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    continue;
                }

                // Prefer abs_path over filename for source map lookup
                let abs_path = obj.get("abs_path").and_then(|v| v.as_str());
                let raw_filename = obj.get("filename").and_then(|v| v.as_str());
                let filename = match abs_path.or(raw_filename) {
                    Some(f) => f.to_string(),
                    None => continue,
                };

                let lineno = match obj.get("lineno").and_then(|v| v.as_u64()) {
                    Some(l) => l as u32,
                    None => continue,
                };

                let colno = obj.get("colno").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                if let Some(resolved) = self
                    .resolve_frame(project_id, &release, &filename, lineno, colno)
                    .await
                {
                    apply_resolved_to_json(obj, resolved, &filename);
                    any_symbolicated = true;
                }
            }
        }

        if any_symbolicated {
            debug!(
                "On-the-fly symbolication resolved frames for project {} release '{}'",
                project_id, release
            );
        }
    }

    /// Symbolicate a single stack trace JSON value in-place.
    ///
    /// The stack trace format is either:
    /// - `{ "frames": [...] }` (Sentry format)
    /// - `[...]` (plain array of frames)
    async fn symbolicate_stack_trace(
        &self,
        project_id: i32,
        release: &str,
        stack_trace: &mut serde_json::Value,
    ) {
        let frames = match stack_trace {
            serde_json::Value::Object(obj) => obj.get_mut("frames"),
            serde_json::Value::Array(_) => Some(stack_trace),
            _ => None,
        };

        let frames = match frames {
            Some(serde_json::Value::Array(f)) => f,
            _ => return,
        };

        for frame in frames.iter_mut() {
            let obj = match frame.as_object_mut() {
                Some(o) => o,
                None => continue,
            };

            // Prefer abs_path (full URL/path) over filename (basename) for source map lookup.
            // Server-side Sentry sends filename="route.js" but abs_path="app:///path/to/.next/server/app/api/route.js"
            // The abs_path is essential for matching against stored source map paths.
            let abs_path = obj.get("abs_path").and_then(|v| v.as_str());
            let raw_filename = obj.get("filename").and_then(|v| v.as_str());
            let filename = match abs_path.or(raw_filename) {
                Some(f) => f.to_string(),
                None => continue,
            };

            debug!(
                "Frame lookup: filename={:?} abs_path={:?} → using '{}'",
                raw_filename, abs_path, filename
            );

            let lineno = match obj.get("lineno").and_then(|v| v.as_u64()) {
                Some(l) => l as u32,
                None => continue,
            };

            let colno = obj.get("colno").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            // Try to find and apply source map for this frame
            if let Some(resolved) = self
                .resolve_frame(project_id, release, &filename, lineno, colno)
                .await
            {
                apply_resolved_to_json(obj, resolved, &filename);
            }
        }
    }

    /// Try to resolve a single frame using stored source maps.
    /// Returns None if no source map is found or resolution fails.
    async fn resolve_frame(
        &self,
        project_id: i32,
        release: &str,
        filename: &str,
        lineno: u32,
        colno: u32,
    ) -> Option<ResolvedFrame> {
        // Try multiple path normalizations to find a matching source map
        let candidates = generate_lookup_paths(filename);

        debug!(
            "resolve_frame: project={} release='{}' filename='{}' candidates={:?}",
            project_id, release, filename, candidates
        );

        // Phase 1: Exact match against candidate paths
        for candidate in &candidates {
            if let Some(resolved) = self
                .try_source_map(project_id, release, filename, candidate, lineno, colno)
                .await
            {
                debug!(
                    "resolve_frame: MATCHED candidate '{}' → {}:{}:{}",
                    candidate, resolved.file, resolved.line, resolved.column
                );
                return Some(resolved);
            }
        }

        // Phase 2: Suffix-based match as fallback (handles bare filenames like "route.js")
        // Only attempt if the filename looks like a bare basename (no path separators, no scheme)
        if !filename.contains('/') && !filename.contains("://") {
            let suffix_pattern = format!("%/{}", filename);
            debug!(
                "resolve_frame: trying suffix match with pattern '{}'",
                suffix_pattern
            );
            let source_map = source_maps::Entity::find()
                .filter(source_maps::Column::ProjectId.eq(project_id))
                .filter(source_maps::Column::Release.eq(release))
                .filter(source_maps::Column::FilePath.like(&suffix_pattern))
                .one(self.db.as_ref())
                .await
                .ok()
                .flatten();

            if let Some(map) = source_map {
                debug!("resolve_frame: suffix match found: '{}'", map.file_path);
                return self.apply_source_map(&map, filename, lineno, colno);
            }
        }

        debug!(
            "resolve_frame: NO match found for '{}' in release '{}'",
            filename, release
        );
        None
    }

    /// Try to find and apply a source map for a specific candidate path.
    async fn try_source_map(
        &self,
        project_id: i32,
        release: &str,
        original_filename: &str,
        candidate: &str,
        lineno: u32,
        colno: u32,
    ) -> Option<ResolvedFrame> {
        let source_map = source_maps::Entity::find()
            .filter(source_maps::Column::ProjectId.eq(project_id))
            .filter(source_maps::Column::Release.eq(release))
            .filter(source_maps::Column::FilePath.eq(candidate))
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()?;

        self.apply_source_map(&source_map, original_filename, lineno, colno)
    }

    /// Parse a source map and resolve coordinates, extracting source context if available.
    fn apply_source_map(
        &self,
        map: &source_maps::Model,
        original_filename: &str,
        lineno: u32,
        colno: u32,
    ) -> Option<ResolvedFrame> {
        match sourcemap::SourceMap::from_slice(&map.source_map_data) {
            Ok(sm) => {
                // Source maps use 0-indexed lines and columns,
                // but Sentry uses 1-indexed lines and 0-indexed columns
                let line = if lineno > 0 { lineno - 1 } else { 0 };

                if let Some(token) = sm.lookup_token(line, colno) {
                    let file = token.get_source().unwrap_or(original_filename).to_string();
                    let resolved_line = token.get_src_line() + 1; // Back to 1-indexed
                    let resolved_col = token.get_src_col();
                    let function = token.get_name().map(|s| s.to_string());

                    // Extract source context from sourcesContent if available
                    let (context_line, pre_context, post_context) =
                        if let Some(source_content) = sm.get_source_contents(token.get_src_id()) {
                            extract_source_context(source_content, resolved_line)
                        } else {
                            (None, vec![], vec![])
                        };

                    return Some(ResolvedFrame {
                        file,
                        line: resolved_line,
                        column: resolved_col,
                        function,
                        context_line,
                        pre_context,
                        post_context,
                    });
                }
                None
            }
            Err(e) => {
                warn!(
                    "Failed to parse source map for file '{}' release '{}': {}",
                    map.file_path, map.release, e
                );
                None
            }
        }
    }
}

/// Number of context lines to extract above and below the error line.
const CONTEXT_LINES: u32 = 5;

/// A resolved stack frame with original source coordinates and source context.
struct ResolvedFrame {
    file: String,
    line: u32,
    column: u32,
    function: Option<String>,
    /// The source code line at the error position.
    context_line: Option<String>,
    /// Source lines before the error line (up to CONTEXT_LINES).
    pre_context: Vec<String>,
    /// Source lines after the error line (up to CONTEXT_LINES).
    post_context: Vec<String>,
}

/// Extract source context (pre_context, context_line, post_context) from source content.
/// `line` is 1-indexed.
fn extract_source_context(
    source_content: &str,
    line: u32,
) -> (Option<String>, Vec<String>, Vec<String>) {
    let lines: Vec<&str> = source_content.lines().collect();
    let line_idx = line.saturating_sub(1) as usize;

    if line_idx >= lines.len() {
        return (None, vec![], vec![]);
    }

    let context_line = Some(lines[line_idx].to_string());

    let start = line_idx.saturating_sub(CONTEXT_LINES as usize);
    let pre_context: Vec<String> = lines[start..line_idx]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let end = std::cmp::min(line_idx + 1 + CONTEXT_LINES as usize, lines.len());
    let post_context: Vec<String> = lines[line_idx + 1..end]
        .iter()
        .map(|s| s.to_string())
        .collect();

    (context_line, pre_context, post_context)
}

/// Apply a resolved frame's data to a JSON frame object.
/// Stores original values and replaces with resolved source info including context.
fn apply_resolved_to_json(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    resolved: ResolvedFrame,
    original_filename: &str,
) {
    // Store original minified info
    obj.insert(
        "original_filename".to_string(),
        serde_json::Value::String(original_filename.to_string()),
    );
    if let Some(orig_lineno) = obj.get("lineno").cloned() {
        obj.insert("original_lineno".to_string(), orig_lineno);
    }
    if let Some(orig_colno) = obj.get("colno").cloned() {
        obj.insert("original_colno".to_string(), orig_colno);
    }

    // Replace with resolved info
    obj.insert(
        "filename".to_string(),
        serde_json::Value::String(resolved.file),
    );
    obj.insert(
        "lineno".to_string(),
        serde_json::Value::Number(resolved.line.into()),
    );
    obj.insert(
        "colno".to_string(),
        serde_json::Value::Number(resolved.column.into()),
    );
    if let Some(name) = resolved.function {
        obj.insert("function".to_string(), serde_json::Value::String(name));
    }

    // Source context
    if let Some(context_line) = resolved.context_line {
        obj.insert(
            "context_line".to_string(),
            serde_json::Value::String(context_line),
        );
    }
    if !resolved.pre_context.is_empty() {
        obj.insert(
            "pre_context".to_string(),
            serde_json::Value::Array(
                resolved
                    .pre_context
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    if !resolved.post_context.is_empty() {
        obj.insert(
            "post_context".to_string(),
            serde_json::Value::Array(
                resolved
                    .post_context
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }

    obj.insert("symbolicated".to_string(), serde_json::Value::Bool(true));
}

/// Normalize a file path for storage.
/// Strips the origin, adds the ~ prefix convention.
///
/// Examples:
/// - "https://example.com/assets/main.js" → "~/assets/main.js"
/// - "http://localhost:3000/static/js/bundle.js" → "~/static/js/bundle.js"
/// - "~/assets/main.js" → "~/assets/main.js" (already normalized)
/// - "app:///src/main.js" → "~/src/main.js"
/// - "/assets/main.js" → "~/assets/main.js"
fn normalize_file_path(path: &str) -> String {
    // Already normalized
    if path.starts_with("~/") {
        return path.to_string();
    }

    // Strip common URL schemes and origins
    let stripped = if let Some(idx) = path.find("://") {
        let after_scheme = &path[idx + 3..];
        // Find the path portion after the host
        if let Some(path_idx) = after_scheme.find('/') {
            &after_scheme[path_idx..]
        } else {
            after_scheme
        }
    } else {
        path
    };

    // Strip leading slashes and add ~ prefix
    let clean = stripped.trim_start_matches('/');
    format!("~/{}", clean)
}

/// Generate candidate lookup paths from a stack trace filename.
/// Returns paths to try in order of specificity.
///
/// This handles the mismatch between how filenames appear in stack traces
/// (full URLs) and how they're stored (normalized with ~ prefix).
///
/// Handles multiple scenarios:
/// - Browser: `https://example.com/_next/static/chunks/main.js` → `~/_next/static/chunks/main.js`
/// - Server `abs_path`: `app:///path/to/.next/server/app/api/route.js` → `~/_next/server/app/api/route.js`
/// - Server `filename`: `route.js` (basename only) → suffix match against stored paths
/// - Already normalized: `~/assets/main.js`
fn generate_lookup_paths(filename: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut add = |p: String| {
        if seen.insert(p.clone()) {
            paths.push(p);
        }
    };

    // Exact match first
    add(filename.to_string());

    // Normalized form (strips scheme+host, adds ~/ prefix)
    let normalized = normalize_file_path(filename);
    if normalized != filename {
        add(normalized.clone());
    }

    // Also try without the ~ prefix if present
    if let Some(stripped) = filename.strip_prefix("~/") {
        add(format!("/{}", stripped));
    }

    // Handle app:/// scheme (Sentry server-side abs_path)
    // e.g. "app:///path/to/.next/server/app/api/route.js"
    if let Some(stripped) = filename.strip_prefix("app://") {
        let path = stripped; // stripped already has leading /

        // Try with ~ prefix
        let clean = path.trim_start_matches('/');
        add(format!("~/{}", clean));

        // For Next.js server-side, the path often contains .next/server/...
        // Stored source maps use _next/server/... (dot replaced with underscore during capture)
        if let Some(next_idx) = path.find(".next/") {
            let next_path = &path[next_idx..];
            // .next → _next rewrite
            let rewritten = format!("~/{}", next_path.replacen(".next/", "_next/", 1));
            add(rewritten);
            // Also try without rewrite (in case source maps were stored with .next)
            add(format!("~/{}", next_path));
        }
    }

    // For HTTP/HTTPS URLs, also try .next → _next rewrite
    if (filename.starts_with("http://") || filename.starts_with("https://"))
        && filename.contains(".next/")
    {
        let rewritten = normalize_file_path(&filename.replace(".next/", "_next/"));
        add(rewritten);
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, DatabaseConnection, Set};
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::projects;

    /// A minimal valid source map for testing.
    /// Maps line 1, col 0 of "out.js" to line 2, col 4 of "input.js", function "greet"
    fn test_source_map() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 3,
            "file": "out.js",
            "sources": ["input.js"],
            "names": ["greet"],
            "mappings": "AACA,IAAAA"
        }))
        .unwrap()
    }

    async fn setup_test_db() -> TestDatabase {
        TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database")
    }

    async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
        use temps_entities::preset::Preset;
        use uuid::Uuid;

        let unique_slug = format!("test-sm-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(unique_slug),
            preset: Set(Preset::NextJs),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        project
            .insert(db.as_ref())
            .await
            .expect("Failed to create project")
            .id
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_upload_and_list_source_maps() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload a source map
        let info = service
            .upload(
                project_id,
                "1.0.0",
                "~/assets/main.js",
                test_source_map(),
                None,
            )
            .await
            .expect("Failed to upload source map");

        assert_eq!(info.project_id, project_id);
        assert_eq!(info.release, "1.0.0");
        assert_eq!(info.file_path, "~/assets/main.js");
        assert!(info.checksum.is_some());
        assert!(info.size_bytes > 0);

        // List source maps for the release
        let maps = service
            .list_for_release(project_id, "1.0.0")
            .await
            .expect("Failed to list source maps");

        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].file_path, "~/assets/main.js");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_upload_replaces_existing() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload initial
        service
            .upload(
                project_id,
                "1.0.0",
                "~/assets/main.js",
                test_source_map(),
                None,
            )
            .await
            .expect("Failed to upload");

        // Upload replacement
        service
            .upload(
                project_id,
                "1.0.0",
                "~/assets/main.js",
                test_source_map(),
                None,
            )
            .await
            .expect("Failed to upload replacement");

        // Should still be only one
        let maps = service
            .list_for_release(project_id, "1.0.0")
            .await
            .expect("Failed to list");

        assert_eq!(maps.len(), 1);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_upload_normalizes_url_paths() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload with full URL - should normalize to ~/
        service
            .upload(
                project_id,
                "1.0.0",
                "https://example.com/assets/main.js",
                test_source_map(),
                None,
            )
            .await
            .expect("Failed to upload");

        let maps = service
            .list_for_release(project_id, "1.0.0")
            .await
            .expect("Failed to list");

        assert_eq!(maps[0].file_path, "~/assets/main.js");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_list_releases() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload maps for two releases
        service
            .upload(project_id, "1.0.0", "~/a.js", test_source_map(), None)
            .await
            .unwrap();
        service
            .upload(project_id, "2.0.0", "~/b.js", test_source_map(), None)
            .await
            .unwrap();

        let releases = service
            .list_releases(project_id)
            .await
            .expect("Failed to list releases");

        assert_eq!(releases.len(), 2);
        assert!(releases.contains(&"1.0.0".to_string()));
        assert!(releases.contains(&"2.0.0".to_string()));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_delete_release() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        service
            .upload(project_id, "1.0.0", "~/a.js", test_source_map(), None)
            .await
            .unwrap();
        service
            .upload(project_id, "1.0.0", "~/b.js", test_source_map(), None)
            .await
            .unwrap();

        let deleted = service
            .delete_release(project_id, "1.0.0")
            .await
            .expect("Failed to delete release");

        assert_eq!(deleted, 2);

        let maps = service.list_for_release(project_id, "1.0.0").await.unwrap();
        assert!(maps.is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_upload_rejects_invalid_source_map() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        let result = service
            .upload(
                project_id,
                "1.0.0",
                "~/main.js",
                b"not a valid source map".to_vec(),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SourceMapError::ParseError(_) => {} // Expected
            other => panic!("Expected ParseError, got: {:?}", other),
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_upload_rejects_empty_release() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        let result = service
            .upload(project_id, "", "~/main.js", test_source_map(), None)
            .await;

        assert!(matches!(result, Err(SourceMapError::Validation(_))));
    }

    #[test]
    fn test_normalize_file_path_https_url() {
        assert_eq!(
            normalize_file_path("https://example.com/assets/main.js"),
            "~/assets/main.js"
        );
    }

    #[test]
    fn test_normalize_file_path_http_url() {
        assert_eq!(
            normalize_file_path("http://localhost:3000/static/js/bundle.js"),
            "~/static/js/bundle.js"
        );
    }

    #[test]
    fn test_normalize_file_path_already_normalized() {
        assert_eq!(normalize_file_path("~/assets/main.js"), "~/assets/main.js");
    }

    #[test]
    fn test_normalize_file_path_app_scheme() {
        assert_eq!(normalize_file_path("app:///src/main.js"), "~/src/main.js");
    }

    #[test]
    fn test_normalize_file_path_absolute() {
        assert_eq!(normalize_file_path("/assets/main.js"), "~/assets/main.js");
    }

    #[test]
    fn test_normalize_file_path_relative() {
        assert_eq!(normalize_file_path("assets/main.js"), "~/assets/main.js");
    }

    #[test]
    fn test_generate_lookup_paths_full_url() {
        let paths = generate_lookup_paths("https://example.com/assets/main.js");
        assert!(paths.contains(&"https://example.com/assets/main.js".to_string()));
        assert!(paths.contains(&"~/assets/main.js".to_string()));
    }

    #[test]
    fn test_generate_lookup_paths_normalized() {
        let paths = generate_lookup_paths("~/assets/main.js");
        assert!(paths.contains(&"~/assets/main.js".to_string()));
        assert!(paths.contains(&"/assets/main.js".to_string()));
    }

    #[test]
    fn test_generate_lookup_paths_app_scheme() {
        // Sentry server-side abs_path
        let paths = generate_lookup_paths("app:///path/to/.next/server/app/api/route.js");
        assert!(paths.contains(&"app:///path/to/.next/server/app/api/route.js".to_string()));
        assert!(paths.contains(&"~/path/to/.next/server/app/api/route.js".to_string()));
        // .next → _next rewrite for Next.js
        assert!(paths.contains(&"~/_next/server/app/api/route.js".to_string()));
        assert!(paths.contains(&"~/.next/server/app/api/route.js".to_string()));
    }

    #[test]
    fn test_generate_lookup_paths_app_scheme_no_next() {
        // Non-Next.js app:/// path
        let paths = generate_lookup_paths("app:///src/index.js");
        assert!(paths.contains(&"app:///src/index.js".to_string()));
        assert!(paths.contains(&"~/src/index.js".to_string()));
    }

    #[test]
    fn test_generate_lookup_paths_bare_filename() {
        // Bare basename (no path separators) — used by Sentry server-side filename field
        let paths = generate_lookup_paths("route.js");
        assert!(paths.contains(&"route.js".to_string()));
        assert!(paths.contains(&"~/route.js".to_string()));
    }

    #[test]
    fn test_generate_lookup_paths_no_duplicates() {
        let paths = generate_lookup_paths("~/assets/main.js");
        // Should not have duplicates
        let unique: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(paths.len(), unique.len());
    }

    #[test]
    fn test_normalize_file_path_app_scheme_next() {
        assert_eq!(normalize_file_path("app:///src/index.js"), "~/src/index.js");
        assert_eq!(
            normalize_file_path("app:///.next/server/app/api/route.js"),
            "~/.next/server/app/api/route.js"
        );
    }

    /// End-to-end test: source map stored as ~/_next/server/... (captured from Docker image),
    /// Sentry Node SDK sends abs_path as app:///workdir/.next/server/...,
    /// symbolication should resolve the frame.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_symbolicate_nextjs_server_frame_via_abs_path() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload source map as stored by CaptureSourceMapsJob (with _next prefix)
        service
            .upload(
                project_id,
                "abc123",
                "~/_next/server/app/api/debug/sentry/route.js",
                test_source_map(),
                None,
            )
            .await
            .expect("Failed to upload source map");

        // Simulate a stack trace as the Sentry Node SDK would send it.
        // The mapper extracts abs_path (full path) and filename (basename).
        let mut stack_trace = serde_json::json!({
            "frames": [
                {
                    "filename": "route.js",
                    "abs_path": "app:///some/workdir/.next/server/app/api/debug/sentry/route.js",
                    "function": "a",
                    "lineno": 1,
                    "colno": 0,
                    "in_app": true
                }
            ]
        });

        service
            .symbolicate_stack_trace(project_id, "abc123", &mut stack_trace)
            .await;

        let frame = &stack_trace["frames"][0];
        assert_eq!(
            frame["symbolicated"].as_bool(),
            Some(true),
            "Frame should be symbolicated. Frame: {}",
            serde_json::to_string_pretty(&frame).unwrap()
        );
        // Original minified info should be preserved
        assert_eq!(
            frame["original_filename"].as_str(),
            Some("app:///some/workdir/.next/server/app/api/debug/sentry/route.js")
        );
    }

    /// Test that bare filename (no abs_path) falls back to suffix matching
    #[tokio::test]
    #[serial_test::serial]
    async fn test_symbolicate_bare_filename_suffix_match() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload source map with full path
        service
            .upload(
                project_id,
                "abc123",
                "~/_next/server/app/api/debug/sentry/route.js",
                test_source_map(),
                None,
            )
            .await
            .expect("Failed to upload source map");

        // Simulate a frame with only filename (no abs_path) — suffix match should find it
        let mut stack_trace = serde_json::json!({
            "frames": [
                {
                    "filename": "route.js",
                    "function": "a",
                    "lineno": 1,
                    "colno": 0
                }
            ]
        });

        service
            .symbolicate_stack_trace(project_id, "abc123", &mut stack_trace)
            .await;

        let frame = &stack_trace["frames"][0];
        assert_eq!(
            frame["symbolicated"].as_bool(),
            Some(true),
            "Frame should be symbolicated via suffix match. Frame: {}",
            serde_json::to_string_pretty(&frame).unwrap()
        );
    }

    /// Test on-the-fly symbolication of stored raw Sentry event data.
    /// Simulates reading an event from the DB that was stored without symbolication,
    /// then symbolicating it at read time.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_symbolicate_stored_event_on_the_fly() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload source map
        service
            .upload(
                project_id,
                "abc123sha",
                "~/_next/server/app/api/debug/sentry/route.js",
                test_source_map(),
                None,
            )
            .await
            .expect("Failed to upload source map");

        // Simulate stored event data as it would appear in the JSONB `data` column
        let mut stored_data = serde_json::json!({
            "source": "sentry",
            "sentry": {
                "release": "abc123sha",
                "exception": {
                    "values": [
                        {
                            "type": "Error",
                            "value": "test error",
                            "stacktrace": {
                                "frames": [
                                    {
                                        "filename": "route.js",
                                        "abs_path": "app:///workdir/.next/server/app/api/debug/sentry/route.js",
                                        "function": "a",
                                        "lineno": 1,
                                        "colno": 0,
                                        "in_app": true
                                    },
                                    {
                                        "filename": "node:internal/process/task_queues",
                                        "function": "processTicksAndRejections",
                                        "lineno": 95,
                                        "colno": 5,
                                        "in_app": false
                                    }
                                ]
                            }
                        }
                    ]
                }
            }
        });

        // Run on-the-fly symbolication
        service
            .symbolicate_stored_event(project_id, &mut stored_data)
            .await;

        // Check that the first frame was symbolicated (it has a matching source map)
        let frame0 = &stored_data["sentry"]["exception"]["values"][0]["stacktrace"]["frames"][0];
        assert_eq!(
            frame0["symbolicated"].as_bool(),
            Some(true),
            "First frame should be symbolicated on-the-fly. Frame: {}",
            serde_json::to_string_pretty(&frame0).unwrap()
        );
        assert_eq!(
            frame0["original_filename"].as_str(),
            Some("app:///workdir/.next/server/app/api/debug/sentry/route.js"),
            "Original filename should be preserved"
        );

        // Second frame should NOT be symbolicated (no source map for node internals)
        let frame1 = &stored_data["sentry"]["exception"]["values"][0]["stacktrace"]["frames"][1];
        assert!(
            frame1.get("symbolicated").is_none() || frame1["symbolicated"].as_bool() == Some(false),
            "Node internal frame should not be symbolicated"
        );
    }

    /// Test that already-symbolicated frames are skipped during on-the-fly symbolication
    #[tokio::test]
    #[serial_test::serial]
    async fn test_symbolicate_stored_event_skips_already_symbolicated() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Upload source map
        service
            .upload(
                project_id,
                "abc123sha",
                "~/_next/server/app/api/route.js",
                test_source_map(),
                None,
            )
            .await
            .unwrap();

        // Stored event with an already-symbolicated frame
        let mut stored_data = serde_json::json!({
            "source": "sentry",
            "sentry": {
                "release": "abc123sha",
                "exception": {
                    "values": [
                        {
                            "type": "Error",
                            "value": "test",
                            "stacktrace": {
                                "frames": [
                                    {
                                        "filename": "src/original.ts",
                                        "function": "handleRequest",
                                        "lineno": 42,
                                        "colno": 10,
                                        "symbolicated": true,
                                        "original_filename": "route.js"
                                    }
                                ]
                            }
                        }
                    ]
                }
            }
        });

        service
            .symbolicate_stored_event(project_id, &mut stored_data)
            .await;

        // Frame should remain unchanged — filename stays as the resolved original
        let frame = &stored_data["sentry"]["exception"]["values"][0]["stacktrace"]["frames"][0];
        assert_eq!(frame["filename"].as_str(), Some("src/original.ts"));
        assert_eq!(frame["lineno"].as_u64(), Some(42));
    }

    #[test]
    fn test_extract_source_context() {
        let source = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\nline 11\nline 12";
        let (ctx_line, pre, post) = extract_source_context(source, 7); // 1-indexed

        assert_eq!(ctx_line.as_deref(), Some("line 7"));
        assert_eq!(pre, vec!["line 2", "line 3", "line 4", "line 5", "line 6"]);
        assert_eq!(
            post,
            vec!["line 8", "line 9", "line 10", "line 11", "line 12"]
        );
    }

    #[test]
    fn test_extract_source_context_near_start() {
        let source = "line 1\nline 2\nline 3\nline 4\nline 5";
        let (ctx_line, pre, post) = extract_source_context(source, 2);

        assert_eq!(ctx_line.as_deref(), Some("line 2"));
        assert_eq!(pre, vec!["line 1"]);
        assert_eq!(post, vec!["line 3", "line 4", "line 5"]);
    }

    #[test]
    fn test_extract_source_context_near_end() {
        let source = "line 1\nline 2\nline 3";
        let (ctx_line, pre, post) = extract_source_context(source, 3);

        assert_eq!(ctx_line.as_deref(), Some("line 3"));
        assert_eq!(pre, vec!["line 1", "line 2"]);
        assert!(post.is_empty());
    }

    #[test]
    fn test_extract_source_context_out_of_bounds() {
        let source = "line 1\nline 2";
        let (ctx_line, pre, post) = extract_source_context(source, 99);

        assert!(ctx_line.is_none());
        assert!(pre.is_empty());
        assert!(post.is_empty());
    }

    /// Test that symbolication includes source context when sourcesContent is present
    #[tokio::test]
    #[serial_test::serial]
    async fn test_symbolicate_includes_source_context() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = SourceMapService::new(db.clone());
        let project_id = create_test_project(&db).await;

        // Create a source map with sourcesContent embedded
        let source_map_with_content = serde_json::to_vec(&serde_json::json!({
            "version": 3,
            "file": "out.js",
            "sources": ["input.ts"],
            "sourcesContent": [
                "import { foo } from 'bar'\n\nfunction greet(name: string) {\n  console.log(`Hello, ${name}!`)\n  throw new Error('test error')\n}\n\nexport default greet\n"
            ],
            "names": ["greet"],
            "mappings": "AACA,IAAAA"
        }))
        .unwrap();

        service
            .upload(
                project_id,
                "v1.0",
                "~/out.js",
                source_map_with_content,
                None,
            )
            .await
            .expect("Failed to upload");

        // Symbolicate a frame that resolves to line 2 of input.ts
        let mut stack_trace = serde_json::json!({
            "frames": [{
                "filename": "out.js",
                "abs_path": "https://example.com/out.js",
                "function": "a",
                "lineno": 1,
                "colno": 0
            }]
        });

        service
            .symbolicate_stack_trace(project_id, "v1.0", &mut stack_trace)
            .await;

        let frame = &stack_trace["frames"][0];
        assert_eq!(frame["symbolicated"].as_bool(), Some(true));

        // Should have context_line from the resolved source
        assert!(
            frame.get("context_line").is_some(),
            "Symbolicated frame should include context_line when sourcesContent is available. Frame: {}",
            serde_json::to_string_pretty(&frame).unwrap()
        );

        // Should have pre_context and/or post_context
        let has_pre = frame
            .get("pre_context")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        let has_post = frame
            .get("post_context")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        assert!(
            has_pre || has_post,
            "Should have at least some context lines. Frame: {}",
            serde_json::to_string_pretty(&frame).unwrap()
        );
    }
}
