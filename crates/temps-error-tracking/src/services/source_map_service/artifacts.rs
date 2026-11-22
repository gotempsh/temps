// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sentry CLI 3.8.0 (b602219e19ba29562996597a4d01b4a3bec4e7fc)
//! source bundles, as written by symbolic-debuginfo 13.8.0. Archives are
//! inspected in memory, never extracted or followed over the network.
use super::{normalize_file_path, SourceMapService, MAX_SOURCE_MAP_BYTES};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};
use tokio::sync::Semaphore;
use utoipa::ToSchema;

pub const CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub static ARTIFACT_WORKERS: Semaphore = Semaphore::const_new(1);
const MAX_FILES: usize = 256;
const MAX_MAPS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum ArtifactUploadError {
    #[error("Invalid source artifact for project {project_id}: {reason}")]
    Invalid {
        project_id: i32,
        reason: &'static str,
    },
    #[error(
        "Source artifact staging quota exhausted for project {0}; retry after staged chunks expire"
    )]
    Quota(i32),
    #[error("Source artifact database operation failed: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Source artifact decoding task failed for project {0}")]
    Worker(i32),
}
fn invalid(project_id: i32, reason: &'static str) -> ArtifactUploadError {
    ArtifactUploadError::Invalid { project_id, reason }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAssembly {
    pub state: String,
    pub missing_chunks: Vec<String>,
    pub detail: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    files: BTreeMap<String, FileInfo>,
}
#[derive(Deserialize)]
struct FileInfo {
    #[serde(rename = "type")]
    ty: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}
struct ArtifactMap {
    debug_id: String,
    path: String,
    data: Vec<u8>,
    checksum: String,
}

pub fn valid_checksum(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn decode_bundle(project_id: i32, data: &[u8]) -> Result<Vec<ArtifactMap>, ArtifactUploadError> {
    let bad = |reason| invalid(project_id, reason);
    // SYSB followed by the little-endian format version; ZIP offsets include
    // this prefix. Plain ZIPs are not an advertised artifact bundle format.
    if data.len() > MAX_SOURCE_MAP_BYTES || !data.starts_with(b"SYSB\x02\0\0\0") {
        return Err(bad(
            "expected a version 2 SYSB artifact bundle (maximum 50 MiB)",
        ));
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(data)).map_err(|_| bad("invalid ZIP directory"))?;
    if archive.len() > MAX_FILES + 1 {
        return Err(bad("at most 256 artifact files"));
    }
    let mut total = 0_u64;
    let mut names = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| bad("invalid ZIP entry"))?;
        let name = entry.name().map_err(|_| bad("invalid ZIP entry name"))?;
        if !names.insert(name.to_string())
            || entry.enclosed_name().is_none()
            || name.contains('\\')
            || entry.is_symlink()
            || entry.is_dir()
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
        {
            return Err(bad("unsafe, duplicate or unsupported archive entry"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| bad("archive size overflow"))?;
        if total > MAX_SOURCE_MAP_BYTES as u64 {
            return Err(bad("expanded archive exceeds 50 MiB"));
        }
    }
    let read = |archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str, limit: usize| {
        let mut entry = archive
            .by_name(name)
            .map_err(|_| bad("missing manifest file entry"))?;
        if entry.size() > limit as u64 {
            return Err(bad("artifact entry exceeds limit"));
        }
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| bad("invalid compressed artifact or CRC"))?;
        if bytes.len() > limit {
            return Err(bad("artifact entry exceeds limit"));
        }
        Ok(bytes)
    };
    let manifest: Manifest =
        serde_json::from_slice(&read(&mut archive, "manifest.json", 1024 * 1024)?)
            .map_err(|_| bad("invalid source manifest"))?;
    if manifest.files.is_empty()
        || manifest.files.len() > MAX_FILES
        || names.len() != manifest.files.len() + 1
    {
        return Err(bad("manifest must describe every artifact file"));
    }
    let mut sources = BTreeMap::new();
    for (entry, info) in &manifest.files {
        if info.ty == "source" {
            if info.url.len() > 4096 || info.url.contains('\0') {
                return Err(bad("invalid bundled source URL"));
            }
            let content = String::from_utf8(read(&mut archive, entry, MAX_SOURCE_MAP_BYTES)?)
                .map_err(|_| bad("bundled source is not UTF-8"))?;
            let key = artifact_url(&info.url)
                .ok_or_else(|| bad("invalid bundled source URL"))?
                .to_string();
            if sources.insert(key, content).is_some() {
                return Err(bad("duplicate bundled source URL"));
            }
        }
    }
    let mut maps = Vec::new();
    let mut stored_bytes = 0usize;
    let mut ids = BTreeSet::new();
    for (entry, info) in &manifest.files {
        if !names.contains(entry)
            || info.url.is_empty()
            || info.url.len() > 4096
            || info.url.contains('\0')
            || info.headers.len() > 32
            || info
                .headers
                .iter()
                .any(|(k, v)| k.len() > 128 || v.len() > 4096 || v.contains('\0'))
        {
            return Err(bad("invalid manifest reference or metadata"));
        }
        if info.ty == "indexed_ram_bundle" {
            return Err(bad("indexed RAM bundles are unsupported"));
        }
        if info.ty != "source_map" {
            continue;
        }
        if maps.len() >= MAX_MAPS {
            return Err(bad("at most 128 source maps per artifact bundle"));
        }
        let mut bytes = read(&mut archive, entry, MAX_SOURCE_MAP_BYTES)?;
        let mut sm: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| bad("invalid source map JSON"))?;
        let header_id = info
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("debug-id"))
            .map(|(_, v)| v.as_str());
        let embedded_id = sm
            .get("debug_id")
            .or_else(|| sm.get("debugId"))
            .and_then(|v| v.as_str());
        let debug_id = canonical_debug_id(header_id.or(embedded_id).ok_or_else(|| {
            bad("source map is missing its Debug ID; run sentry-cli sourcemaps inject first")
        })?)
        .ok_or_else(|| bad("invalid source map Debug ID"))?;
        if embedded_id.is_some_and(|v| canonical_debug_id(v).as_deref() != Some(&debug_id))
            || !ids.insert(debug_id.clone())
        {
            return Err(bad("conflicting or duplicate source map Debug ID"));
        }
        let remaining = MAX_SOURCE_MAP_BYTES.saturating_sub(stored_bytes);
        if hydrate_sources(
            &mut sm,
            &info.url,
            &sources,
            remaining.saturating_sub(bytes.len()),
        )
        .map_err(bad)?
        {
            bytes = serde_json::to_vec(&sm).map_err(|_| bad("source context encoding failed"))?;
        }
        stored_bytes = stored_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| bad("source map size overflow"))?;
        if stored_bytes > MAX_SOURCE_MAP_BYTES {
            return Err(bad("expanded source maps exceed 50 MiB"));
        }
        sourcemap::SourceMap::from_slice(&bytes)
            .map_err(|_| bad("unsupported or malformed source map"))?;
        maps.push(ArtifactMap {
            debug_id,
            path: normalize_file_path(info.url.strip_suffix(".map").unwrap_or(&info.url)),
            checksum: hex::encode(Sha256::digest(&bytes)),
            data: bytes,
        });
    }
    if maps.is_empty() {
        return Err(bad("artifact bundle contains no Debug ID source maps"));
    }
    Ok(maps)
}

fn artifact_url(value: &str) -> Option<url::Url> {
    url::Url::parse(value).ok().or_else(|| {
        url::Url::parse("http://bundle.invalid/")
            .ok()?
            .join(value.strip_prefix('~').unwrap_or(value))
            .ok()
    })
}

/// Inline only missing context from the same uploaded bundle. No network or
/// filesystem lookup; shared stored-byte budget prevents source amplification.
fn hydrate_sources(
    map: &mut serde_json::Value,
    map_url: &str,
    sources: &BTreeMap<String, String>,
    mut budget: usize,
) -> Result<bool, &'static str> {
    if sources.is_empty() {
        return Ok(false);
    }
    let Some(names) = map.get("sources").and_then(|v| v.as_array()) else {
        return Ok(false);
    };
    let count = names.len();
    let Some(mut base) = artifact_url(map_url) else {
        return Err("invalid source map URL");
    };
    if let Some(root) = map
        .get("sourceRoot")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
    {
        base = base
            .join(&if root.ends_with('/') {
                root.to_owned()
            } else {
                format!("{root}/")
            })
            .map_err(|_| "invalid source root")?;
    }
    let urls = names
        .iter()
        .map(|name| {
            name.as_str()
                .and_then(|name| base.join(name).ok())
                .map(|url| url.to_string())
        })
        .collect::<Vec<_>>();
    if map
        .get("sourcesContent")
        .is_none_or(|value| value.is_null())
    {
        map["sourcesContent"] = serde_json::json!(vec![serde_json::Value::Null; count]);
    }
    let Some(contents) = map.get_mut("sourcesContent").and_then(|v| v.as_array_mut()) else {
        return Err("invalid sourcesContent array");
    };
    contents.resize(count, serde_json::Value::Null);
    let mut changed = false;
    for (slot, url) in contents.iter_mut().zip(urls) {
        if slot.is_null() {
            if let Some(content) = url.as_ref().and_then(|url| sources.get(url)) {
                budget = budget
                    .checked_sub(content.len())
                    .ok_or("expanded source maps exceed 50 MiB")?;
                *slot = serde_json::Value::String(content.clone());
                changed = true;
            }
        }
    }
    Ok(changed)
}

pub(super) fn canonical_debug_id(value: &str) -> Option<String> {
    let id = uuid::Uuid::parse_str(value).ok()?;
    (!id.is_nil()).then(|| id.hyphenated().to_string())
}

impl SourceMapService {
    pub async fn cleanup_artifact_chunks(&self) -> Result<u64, sea_orm::DbErr> {
        Ok(self.db.execute_unprepared("DELETE FROM source_map_upload_chunks WHERE (project_id,checksum) IN (SELECT project_id,checksum FROM source_map_upload_chunks WHERE created_at<NOW()-INTERVAL '1 hour' ORDER BY created_at LIMIT 256)").await?.rows_affected())
    }

    /// Temporary staging only: 1 hour TTL, 64 MiB/128 chunks per project,
    /// 256 MiB/2048 chunks installation-wide. One batched transaction, with a
    /// control-plane advisory lock to make quotas exact across server processes.
    pub async fn stage_artifact_chunks(
        &self,
        project_id: i32,
        chunks: Vec<(String, Vec<u8>)>,
    ) -> Result<(), ArtifactUploadError> {
        if chunks.is_empty() || chunks.len() > 1 {
            return Err(invalid(project_id, "one chunk per request"));
        }
        for (checksum, data) in &chunks {
            if !valid_checksum(checksum)
                || data.is_empty()
                || data.len() > CHUNK_BYTES
                || sha1_smol::Sha1::from(data).digest().to_string() != *checksum
            {
                return Err(invalid(project_id, "chunk length or SHA1 mismatch"));
            }
        }
        let tx = self.db.begin().await?;
        tx.execute_unprepared(
            "SET LOCAL statement_timeout='10s'; SELECT pg_advisory_xact_lock(1897562201);
            DELETE FROM source_map_upload_chunks WHERE created_at < NOW()-INTERVAL '1 hour'",
        )
        .await?;
        for (checksum, data) in chunks {
            let row = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT count(*) AS chunks,COALESCE(sum(octet_length(data)),0)::bigint AS bytes,
                 count(*) FILTER(WHERE project_id=$1) AS project_chunks,
                 COALESCE(sum(octet_length(data)) FILTER(WHERE project_id=$1),0)::bigint AS project_bytes,
                 bool_or(project_id=$1 AND checksum=$2) AS present FROM source_map_upload_chunks",
                [project_id.into(), checksum.clone().into()])).await?.ok_or_else(|| invalid(project_id,"missing quota result"))?;
            let present = row.try_get::<Option<bool>>("", "present")?.unwrap_or(false);
            if !present
                && (row.try_get::<i64>("", "bytes")? + data.len() as i64 > 256 * 1024 * 1024
                    || row.try_get::<i64>("", "project_bytes")? + data.len() as i64
                        > 64 * 1024 * 1024
                    || row.try_get::<i64>("", "chunks")? >= 2048
                    || row.try_get::<i64>("", "project_chunks")? >= 128)
            {
                return Err(ArtifactUploadError::Quota(project_id));
            }
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "INSERT INTO source_map_upload_chunks(project_id,checksum,data) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
                [project_id.into(),checksum.into(),data.into()])).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn assemble_artifact(
        &self,
        project_id: i32,
        checksum: String,
        chunks: Vec<String>,
        release: Option<String>,
        dist: Option<String>,
    ) -> Result<ArtifactAssembly, ArtifactUploadError> {
        if !valid_checksum(&checksum)
            || chunks.is_empty()
            || chunks.len() > 7
            || chunks.iter().any(|s| !valid_checksum(s))
            || release
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 1024 || s.contains('\0'))
            || dist
                .as_ref()
                .is_some_and(|s| s.len() > 256 || s.contains('\0'))
        {
            return Err(invalid(
                project_id,
                "invalid assembly checksum, chunk list, release or distribution",
            ));
        }
        // A receipt is valid only while every map from that atomic import is
        // still present. Deletion makes the CLI upload again, rather than lying.
        let complete = self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT count(*)=max(artifact_bundle_count) AS complete FROM source_maps WHERE project_id=$1 AND artifact_bundle=$2",
            [project_id.into(),checksum.clone().into()])).await?.and_then(|r|r.try_get::<Option<bool>>("","complete").ok().flatten()).unwrap_or(false);
        if complete {
            return Ok(ArtifactAssembly {
                state: "ok".into(),
                missing_chunks: vec![],
                detail: None,
            });
        }
        let rows = self.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT checksum,data FROM source_map_upload_chunks WHERE project_id=$1 AND checksum=ANY($2) AND created_at>NOW()-INTERVAL '1 hour'",
            [project_id.into(), chunks.clone().into()])).await?;
        let mut available = BTreeMap::new();
        for row in rows {
            available.insert(
                row.try_get::<String>("", "checksum")?,
                row.try_get::<Vec<u8>>("", "data")?,
            );
        }
        let missing_chunks = chunks
            .iter()
            .filter(|c| !available.contains_key(*c))
            .cloned()
            .collect::<Vec<_>>();
        if !missing_chunks.is_empty() {
            return Ok(ArtifactAssembly {
                state: "not_found".into(),
                missing_chunks,
                detail: None,
            });
        }
        let total = chunks
            .iter()
            .map(|c| available.get(c).map_or(0, Vec::len))
            .sum::<usize>();
        if total > MAX_SOURCE_MAP_BYTES {
            return Err(invalid(project_id, "assembled bundle exceeds 50 MiB"));
        }
        let mut bytes = Vec::with_capacity(total);
        for chunk in &chunks {
            if let Some(data) = available.get(chunk) {
                bytes.extend_from_slice(data);
            }
        }
        drop(available);
        if sha1_smol::Sha1::from(&bytes).digest().to_string() != checksum {
            return Err(invalid(project_id, "bundle SHA1 mismatch"));
        }
        let maps = tokio::task::spawn_blocking(move || decode_bundle(project_id, &bytes))
            .await
            .map_err(|_| ArtifactUploadError::Worker(project_id))??;
        let tx = self.db.begin().await?;
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(1897562202,$1)",
            [project_id.into()],
        ))
        .await?;
        let count = maps.len() as i32;
        // At most 128 records per control-plane upload. One transaction keeps
        // conflict/error handling atomic; telemetry hot paths do not use this.
        for map in maps {
            let size = map.data.len() as i64;
            let label = release
                .clone()
                .unwrap_or_else(|| format!("debug-id:{}", map.debug_id));
            let result = tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "INSERT INTO source_maps(project_id,release,file_path,source_map_data,dist,size_bytes,checksum,debug_id,artifact_bundle,artifact_bundle_count,created_at)
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,NOW())
                 ON CONFLICT(project_id,debug_id) WHERE debug_id IS NOT NULL DO UPDATE SET
                 artifact_bundle=EXCLUDED.artifact_bundle,artifact_bundle_count=EXCLUDED.artifact_bundle_count
                 WHERE source_maps.checksum=EXCLUDED.checksum",
                [project_id.into(),label.into(),map.path.into(),map.data.into(),dist.clone().into(),size.into(),map.checksum.into(),map.debug_id.into(),checksum.clone().into(),count.into()])).await?;
            if result.rows_affected() != 1 {
                return Err(invalid(
                    project_id,
                    "Debug ID already belongs to different source content",
                ));
            }
        }
        tx.commit().await?;
        Ok(ArtifactAssembly {
            state: "ok".into(),
            missing_chunks: vec![],
            detail: None,
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    pub(in crate::services::source_map_service) fn bundle(maps: &[(&str, &str)]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        cursor.write_all(b"SYSB\x02\0\0\0").unwrap();
        let mut writer = zip::ZipWriter::new(cursor);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut files = BTreeMap::new();
        for (index, (id, source)) in maps.iter().enumerate() {
            let path = format!("files/_/_/bundle-{index}.js.map");
            writer.start_file(&path, options).unwrap();
            writer
                .write_all(
                    &serde_json::to_vec(&serde_json::json!({
                        "version":3,"sources":["synthetic-original.js"],"sourcesContent":[source],
                        "names":["syntheticFailure"],"mappings":"AAAAA","debug_id":id
                    }))
                    .unwrap(),
                )
                .unwrap();
            files.insert(path,serde_json::json!({"type":"source_map","url":format!("~/bundle-{index}.js.map"),"headers":{"debug-id":id}}));
        }
        writer.start_file("manifest.json", options).unwrap();
        writer
            .write_all(&serde_json::to_vec(&serde_json::json!({"files":files})).unwrap())
            .unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn artifact_sources_hydrate_only_missing_context_with_a_shared_byte_budget() {
        let source = BTreeMap::from([(
            "http://bundle.invalid/src/original.js".into(),
            "synthetic original".into(),
        )]);
        let mut map = serde_json::json!({"sources":["../src/original.js","unavailable.js"],"sourcesContent":[null,"existing"]});
        assert!(hydrate_sources(&mut map, "~/assets/app.js.map", &source, 1024).unwrap());
        assert_eq!(map["sourcesContent"][0], "synthetic original");
        assert_eq!(map["sourcesContent"][1], "existing");
        let mut rooted = serde_json::json!({"sources":["original.js"],"sourceRoot":"../src"});
        assert!(hydrate_sources(&mut rooted, "~/assets/app.js.map", &source, 1024).unwrap());
        let webpack = BTreeMap::from([(
            "webpack:///src/original.js".into(),
            "synthetic webpack source".into(),
        )]);
        let mut scheme =
            serde_json::json!({"sourceRoot":"webpack:///","sources":["src/original.js"]});
        assert!(hydrate_sources(&mut scheme, "~/assets/app.js.map", &webpack, 1024).unwrap());
        assert_eq!(scheme["sourcesContent"][0], "synthetic webpack source");
        let mut limited = serde_json::json!({"sources":["../src/original.js"]});
        assert!(hydrate_sources(&mut limited, "~/assets/app.js.map", &source, 1).is_err());
    }

    #[test]
    fn artifact_bundle_rejects_invalid_utf8_entry_names() {
        let mut bytes = bundle(&[("00112233-4455-4677-8899-aabbccddeeff", "synthetic source")]);
        // Mark the first local and central-directory name as UTF-8, then
        // replace its first byte with an invalid sequence. ZIP CRCs cover
        // content, so this isolates filename decoding from content validity.
        for (signature, flags, name) in [(b"PK\x03\x04", 6, 30), (b"PK\x01\x02", 8, 46)] {
            let offset = bytes.windows(4).position(|v| v == signature).unwrap();
            bytes[offset + flags + 1] |= 0x08;
            bytes[offset + name] = 0xff;
        }
        assert!(matches!(
            decode_bundle(7, &bytes),
            Err(ArtifactUploadError::Invalid { project_id: 7, .. })
        ));
    }

    #[test]
    fn artifact_bundle_validates_identity_and_bounds() {
        let id = "00112233-4455-4677-8899-aabbccddeeff";
        let bytes = bundle(&[(id, "synthetic source")]);
        let maps = decode_bundle(7, &bytes).unwrap();
        assert_eq!(maps[0].debug_id, id);
        assert_eq!(maps[0].path, "~/bundle-0.js");
        assert!(decode_bundle(7, b"PKinvalid").is_err());
        assert!(decode_bundle(7, &bundle(&[("invalid", "x")])).is_err());
        assert!(decode_bundle(7, &bundle(&[(id, "x"), (id, "y")])).is_err());
        assert!(
            decode_bundle(7, &bundle(&[("00000000-0000-0000-0000-000000000000", "x")])).is_err()
        );
        assert!(valid_checksum(&"a".repeat(40)));
        assert!(!valid_checksum(&"A".repeat(40)));
        assert!(!valid_checksum(&"f".repeat(39)));
    }

    #[test]
    fn artifact_bundle_rejects_traversal_and_expansion() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../outside", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"x").unwrap();
        let mut bytes = b"SYSB\x02\0\0\0".to_vec();
        bytes.extend(writer.finish().unwrap().into_inner());
        assert!(decode_bundle(7, &bytes).is_err());
        let bytes = bundle(&[(
            "00112233-4455-4677-8899-aabbccddeeff",
            &"x".repeat(MAX_SOURCE_MAP_BYTES),
        )]);
        assert!(
            bytes.len() < CHUNK_BYTES,
            "synthetic compressed bomb is small on the wire"
        );
        assert!(decode_bundle(7, &bytes).is_err());
    }
}
