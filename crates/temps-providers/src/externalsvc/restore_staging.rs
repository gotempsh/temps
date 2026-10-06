// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stage restore inputs on the host in constant memory.
//!
//! A restore reads a backup object from S3 and hands it to a container. The
//! object can be as large as the database, so it must never be collected
//! into memory: it is streamed to a file in an attempt-owned temp directory,
//! decompressed file-to-file when needed, and then streamed into the
//! container with [`super::container_upload`].

use std::path::Path;

/// Why a restore input could not be staged on the host.
#[derive(Debug, thiserror::Error)]
pub enum StagingError {
    /// The S3 `GetObject` request failed.
    #[error("S3 GetObject failed for s3://{bucket}/{key}: {reason}")]
    Download {
        bucket: String,
        key: String,
        reason: String,
    },

    /// The object body broke off while streaming.
    #[error("Reading s3://{bucket}/{key} failed after {bytes_read} bytes: {reason}")]
    Read {
        bucket: String,
        key: String,
        bytes_read: u64,
        reason: String,
    },

    /// The host-side staging file could not be created, written or read.
    #[error("Staging file '{path}' for {what}: {reason}")]
    Io {
        path: String,
        what: String,
        reason: String,
    },

    /// The staged file is not valid gzip, or is truncated.
    #[error("Decompressing {what} from '{path}' failed: {reason}")]
    Decompress {
        path: String,
        what: String,
        reason: String,
    },
}

/// Stream the S3 object `bucket/key` into a new file at `dest`, chunk by
/// chunk, returning the number of bytes written. Refuses to overwrite an
/// existing file so a staging path is never shared between attempts.
pub(crate) async fn download_s3_object_to_file(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    dest: &Path,
) -> Result<u64, StagingError> {
    use tokio::io::AsyncWriteExt;

    let io_error = |e: std::io::Error| StagingError::Io {
        path: dest.display().to_string(),
        what: format!("s3://{}/{}", bucket, key),
        reason: e.to_string(),
    };

    let object = s3_client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| StagingError::Download {
            bucket: bucket.to_string(),
            key: key.to_string(),
            reason: aws_sdk_s3::error::DisplayErrorContext(&e).to_string(),
        })?;
    let mut body = object.body;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .await
        .map_err(io_error)?;
    let mut written = 0_u64;
    while let Some(chunk) = body.try_next().await.map_err(|e| StagingError::Read {
        bucket: bucket.to_string(),
        key: key.to_string(),
        bytes_read: written,
        reason: e.to_string(),
    })? {
        file.write_all(&chunk).await.map_err(io_error)?;
        written += chunk.len() as u64;
    }
    file.flush().await.map_err(io_error)?;
    Ok(written)
}

/// Gunzip `src` into a new file at `dest` on a blocking thread, streaming,
/// returning the decompressed size. `what` names the backup in errors.
pub(crate) async fn gunzip_file(src: &Path, dest: &Path, what: &str) -> Result<u64, StagingError> {
    let src = src.to_path_buf();
    let dest = dest.to_path_buf();
    let what = what.to_string();
    let task_what = what.clone();
    let task_src = src.display().to_string();
    tokio::task::spawn_blocking(move || -> Result<u64, StagingError> {
        let input = std::fs::File::open(&src).map_err(|e| StagingError::Io {
            path: src.display().to_string(),
            what: what.clone(),
            reason: e.to_string(),
        })?;
        let mut decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(input));
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| StagingError::Io {
                path: dest.display().to_string(),
                what: what.clone(),
                reason: e.to_string(),
            })?;
        std::io::copy(&mut decoder, &mut output).map_err(|e| StagingError::Decompress {
            path: src.display().to_string(),
            what,
            reason: e.to_string(),
        })
    })
    .await
    .map_err(|e| StagingError::Decompress {
        path: task_src,
        what: task_what,
        reason: format!("decompression task did not finish: {}", e),
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn gunzip_round_trips_through_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let src = dir.path().join("dump.sql.gz");
        let dest = dir.path().join("dump.sql");
        let payload: Vec<u8> = (0..200_000).map(|i| (i % 97) as u8).collect();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&payload).expect("compress");
        std::fs::write(&src, encoder.finish().expect("finish")).expect("write gz");

        let size = gunzip_file(&src, &dest, "test dump").await.expect("gunzip");

        assert_eq!(size, payload.len() as u64);
        assert_eq!(std::fs::read(&dest).expect("read"), payload);
    }

    #[tokio::test]
    async fn corrupt_gzip_is_a_decompress_error_naming_the_backup() {
        let dir = tempfile::tempdir().expect("temp dir");
        let src = dir.path().join("dump.sql.gz");
        std::fs::write(&src, b"not gzip at all").expect("write");

        let err = gunzip_file(&src, &dir.path().join("dump.sql"), "nightly dump")
            .await
            .expect_err("garbage is not gzip");

        assert!(
            matches!(&err, StagingError::Decompress { what, .. } if what == "nightly dump"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn staging_never_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let src = dir.path().join("a.gz");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"fresh").expect("compress");
        std::fs::write(&src, encoder.finish().expect("finish")).expect("write gz");
        let dest = dir.path().join("taken.sql");
        std::fs::write(&dest, b"another attempt's file").expect("write existing");

        let err = gunzip_file(&src, &dest, "dump")
            .await
            .expect_err("an existing destination must not be truncated");

        assert!(matches!(err, StagingError::Io { .. }), "{err:?}");
        assert_eq!(
            std::fs::read(&dest).expect("read"),
            b"another attempt's file"
        );
    }
}
