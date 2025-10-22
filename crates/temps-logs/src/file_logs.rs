//! File-based logging service for pipeline operations
//!
//! This module provides utilities for:
//! - Creating structured log files with date-based organization
//! - Appending to logs asynchronously
//! - Tailing logs in real-time
//! - Reading log content

use chrono::Utc;
use futures::Stream;
use std::path::PathBuf;
use tokio::fs::{create_dir_all, File};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, SeekFrom};
use tokio::time::Duration;
use tracing::{debug, trace};

pub struct LogService {
    log_base_path: PathBuf,
}

impl LogService {
    pub fn new(log_base_path: PathBuf) -> Self {
        LogService { log_base_path }
    }

    pub fn get_log_path(&self, log_id: &str) -> PathBuf {
        // If log_id already contains .log extension or path separators, treat it as a full path
        if log_id.contains('/') || log_id.ends_with(".log") {
            self.log_base_path.join(log_id)
        } else {
            // Legacy behavior: add .log extension
            self.log_base_path.join(format!("{}.log", log_id))
        }
    }

    pub async fn create_log_path(&self, log_id: &str) -> Result<PathBuf, std::io::Error> {
        // If log_id contains path separators, it's already a full path with directory structure
        let log_path = if log_id.contains('/') {
            PathBuf::from(log_id)
        } else {
            // Legacy behavior: create date-based path
            let now = Utc::now();
            let date_path = now.format("%Y/%m/%d/%H").to_string();
            PathBuf::from(date_path).join(format!("{}.log", log_id))
        };

        let full_path = self.log_base_path.join(&log_path);

        // Ensure the directory exists
        if let Some(parent) = full_path.parent() {
            create_dir_all(parent).await?;
        }

        Ok(log_path)
    }

    pub async fn append_to_log(
        &self,
        log_path: &str,
        log_entry: &str,
    ) -> Result<(), std::io::Error> {
        let log_path = self.get_log_path(log_path);
        tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(log_path)
            .await?
            .write(log_entry.as_bytes())
            .await?;
        Ok(())
    }

    pub async fn get_log_content(&self, log_id: &str) -> Result<String, std::io::Error> {
        let log_path = self.get_log_path(log_id);
        tokio::fs::read_to_string(log_path).await
    }

    pub async fn tail_log(
        &self,
        log_id: &str,
    ) -> Result<impl Stream<Item = Result<String, std::io::Error>>, std::io::Error> {
        let log_path = self.get_log_path(log_id);
        debug!("Attempting to tail log at path: {:?}", log_path);

        // Create file if it doesn't exist
        if !log_path.exists() {
            trace!("Log file doesn't exist, creating new file");
            File::create(&log_path).await?;
        }

        // Open file in read mode
        let file = File::open(&log_path).await?;
        let file_size = file.metadata().await?.len();
        let mut reader = BufReader::new(file);

        // If file has content, seek to position to get last 1000 lines
        if file_size > 0 {
            let mut buffer = Vec::new();
            reader.read_to_end(&mut buffer).await?;

            let lines = buffer.split(|&b| b == b'\n').collect::<Vec<_>>();
            let start_pos = if lines.len() > 1000 {
                // Get position to start reading from for last 1000 lines
                let skip_lines = lines.len() - 1000;
                lines
                    .iter()
                    .take(skip_lines)
                    .map(|line| line.len() + 1) // +1 for newline
                    .sum::<usize>() as u64
            } else {
                0
            };

            reader.seek(SeekFrom::Start(start_pos)).await?;
        }

        Ok(async_stream::stream! {
            let mut buffer = String::new();

            loop {
                match reader.read_line(&mut buffer).await {
                    Ok(0) => {
                        // Reached EOF, wait a bit before trying again
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Ok(_) => {
                        let line = buffer.trim_end().to_string();
                        if !line.is_empty() {
                            yield Ok(line);
                        }
                        buffer.clear();
                    }
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_log_service_creation() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-log";
        let log_path = log_service.get_log_path(log_id);

        assert!(log_path.to_string_lossy().contains("test-log.log"));
    }

    #[tokio::test]
    async fn test_create_log_path() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-create";
        let log_path = log_service.create_log_path(log_id).await.unwrap();

        // Should create a date-based path
        assert!(log_path.to_string_lossy().contains("test-create.log"));

        // Full path should exist after creation
        let full_path = temp_dir.path().join(&log_path);
        assert!(full_path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn test_append_and_read_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-append";
        let log_path = log_service.create_log_path(log_id).await.unwrap();
        let log_path_str = log_path.to_str().unwrap();

        // Append some content
        log_service
            .append_to_log(log_path_str, "First line\n")
            .await
            .unwrap();
        log_service
            .append_to_log(log_path_str, "Second line\n")
            .await
            .unwrap();

        // Read back the content
        let content = log_service.get_log_content(log_path_str).await.unwrap();
        assert_eq!(content, "First line\nSecond line\n");
    }

    #[tokio::test]
    async fn test_tail_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-tail";
        let log_path = log_service.create_log_path(log_id).await.unwrap();
        let log_path_str = log_path.to_str().unwrap();

        // Write initial content
        log_service
            .append_to_log(log_path_str, "Initial line\n")
            .await
            .unwrap();

        // Start tailing
        let _stream = log_service.tail_log(log_path_str).await.unwrap();

        // This is a basic test - in practice, tailing would be used with continuous writes
        // For testing purposes, we just verify the stream can be created
        // We can't easily test the streaming behavior in a unit test
    }

    #[tokio::test]
    async fn test_get_log_content_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let result = log_service.get_log_content("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_append_to_log_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-create-on-append";

        // File doesn't exist yet
        let log_path = log_service.get_log_path(log_id);
        assert!(!log_path.exists());

        // Append should create the file
        log_service
            .append_to_log(log_id, "First line\n")
            .await
            .unwrap();

        // File should now exist
        assert!(log_path.exists());

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, "First line\n");
    }

    #[tokio::test]
    async fn test_empty_log_content() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-empty";

        // Create empty file
        log_service.append_to_log(log_id, "").await.unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, "");
    }

    #[tokio::test]
    async fn test_append_multiple_entries_same_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-multiple";

        // Append multiple entries
        for i in 1..=5 {
            log_service
                .append_to_log(log_id, &format!("Line {}\n", i))
                .await
                .unwrap();
        }

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n");
    }

    #[tokio::test]
    async fn test_log_path_with_special_characters() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-with-dashes_and_underscores";
        let log_path = log_service.get_log_path(log_id);

        assert!(log_path
            .to_string_lossy()
            .contains("test-with-dashes_and_underscores.log"));

        // Should be able to write to it
        log_service
            .append_to_log(log_id, "Content with special chars\n")
            .await
            .unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, "Content with special chars\n");
    }

    #[tokio::test]
    async fn test_create_log_path_directory_structure() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-dir-structure";
        let log_path = log_service.create_log_path(log_id).await.unwrap();

        // Should create a date-based path structure
        let path_str = log_path.to_string_lossy();
        assert!(path_str.contains("/")); // Should have directory separators
        assert!(path_str.ends_with("test-dir-structure.log"));

        // Directory should exist
        let full_path = temp_dir.path().join(&log_path);
        assert!(full_path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn test_tail_log_nonexistent_file_creates_it() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-tail-create";
        let log_path = log_service.get_log_path(log_id);

        // File doesn't exist
        assert!(!log_path.exists());

        // Tail should create the file
        let _stream = log_service.tail_log(log_id).await.unwrap();

        // File should now exist
        assert!(log_path.exists());
    }

    #[tokio::test]
    async fn test_unicode_content() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-unicode";
        let unicode_content = "测试 🚀 émojis and ñoñó\n";

        log_service
            .append_to_log(log_id, unicode_content)
            .await
            .unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, unicode_content);
    }

    #[tokio::test]
    async fn test_large_log_entry() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-large";
        let large_content = "x".repeat(10_000) + "\n";

        log_service
            .append_to_log(log_id, &large_content)
            .await
            .unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, large_content);
        assert_eq!(content.len(), 10_001); // 10k chars + newline
    }

    #[tokio::test]
    async fn test_concurrent_append_operations() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = Arc::new(LogService::new(temp_dir.path().to_path_buf()));

        let log_id = "test-concurrent";
        let num_tasks = 10;
        let entries_per_task = 5;

        let mut handles = Vec::new();

        // Spawn multiple tasks that append to the same log concurrently
        for task_id in 0..num_tasks {
            let service = Arc::clone(&log_service);
            let log_id = log_id.to_string();

            let handle = tokio::spawn(async move {
                for entry_id in 0..entries_per_task {
                    let content = format!("Task {} Entry {}\n", task_id, entry_id);
                    service.append_to_log(&log_id, &content).await.unwrap();
                }
            });

            handles.push(handle);
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Verify all entries were written
        let content = log_service.get_log_content(log_id).await.unwrap();
        let line_count = content.lines().count();
        assert_eq!(line_count, num_tasks * entries_per_task);

        // Verify no data corruption (each line should contain "Task" and "Entry")
        for line in content.lines() {
            assert!(line.contains("Task"));
            assert!(line.contains("Entry"));
        }
    }

    #[tokio::test]
    async fn test_concurrent_read_write_operations() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = Arc::new(LogService::new(temp_dir.path().to_path_buf()));

        let log_id = "test-read-write";

        // Pre-populate with some content
        for i in 0..5 {
            log_service
                .append_to_log(log_id, &format!("Initial line {}\n", i))
                .await
                .unwrap();
        }

        let service_read = Arc::clone(&log_service);
        let service_write = Arc::clone(&log_service);

        // Start concurrent read and write operations
        let read_handle = tokio::spawn(async move {
            for _ in 0..10 {
                let _content = service_read.get_log_content(log_id).await.unwrap();
                sleep(Duration::from_millis(10)).await;
            }
        });

        let write_handle = tokio::spawn(async move {
            for i in 0..10 {
                service_write
                    .append_to_log(log_id, &format!("Concurrent write {}\n", i))
                    .await
                    .unwrap();
                sleep(Duration::from_millis(10)).await;
            }
        });

        // Wait for both operations to complete
        read_handle.await.unwrap();
        write_handle.await.unwrap();

        // Verify final state
        let final_content = log_service.get_log_content(log_id).await.unwrap();
        let lines: Vec<&str> = final_content.lines().collect();

        // Should have initial 5 lines + 10 concurrent writes
        assert_eq!(lines.len(), 15);

        // Verify initial lines are present
        for i in 0..5 {
            assert!(lines
                .iter()
                .any(|line| line.contains(&format!("Initial line {}", i))));
        }

        // Verify concurrent writes are present
        for i in 0..10 {
            assert!(lines
                .iter()
                .any(|line| line.contains(&format!("Concurrent write {}", i))));
        }
    }

    #[tokio::test]
    async fn test_very_long_log_lines() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-long-lines";

        // Create lines of varying lengths
        let short_line = "Short line\n";
        let medium_line = "A".repeat(1000) + "\n";
        let long_line = "B".repeat(50_000) + "\n";

        log_service.append_to_log(log_id, short_line).await.unwrap();
        log_service
            .append_to_log(log_id, &medium_line)
            .await
            .unwrap();
        log_service.append_to_log(log_id, &long_line).await.unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "Short line");
        assert_eq!(lines[1].len(), 1000);
        assert_eq!(lines[2].len(), 50_000);

        // Verify content integrity
        assert!(lines[1].chars().all(|c| c == 'A'));
        assert!(lines[2].chars().all(|c| c == 'B'));
    }

    #[tokio::test]
    async fn test_log_with_only_newlines() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-newlines";

        // Append only newlines
        log_service.append_to_log(log_id, "\n\n\n").await.unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, "\n\n\n");
        assert_eq!(content.lines().count(), 3); // Three empty lines
    }

    #[tokio::test]
    async fn test_log_without_newlines() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-no-newlines";

        // Append content without newlines
        log_service.append_to_log(log_id, "Line 1").await.unwrap();
        log_service.append_to_log(log_id, "Line 2").await.unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, "Line 1Line 2");
    }

    #[tokio::test]
    async fn test_mixed_line_endings() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-line-endings";

        // Mix different line endings
        log_service
            .append_to_log(log_id, "Unix line\n")
            .await
            .unwrap();
        log_service
            .append_to_log(log_id, "Windows line\r\n")
            .await
            .unwrap();
        log_service
            .append_to_log(log_id, "Mac line\r")
            .await
            .unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert_eq!(content, "Unix line\nWindows line\r\nMac line\r");
    }

    #[tokio::test]
    async fn test_binary_content() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-binary";

        // Create binary content (non-UTF8)
        let binary_data = vec![0x00, 0xFF, 0xFE, 0xFD, 0x01, 0x02, 0x03];
        let binary_string = String::from_utf8_lossy(&binary_data);

        log_service
            .append_to_log(log_id, &binary_string)
            .await
            .unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert!(!content.is_empty());
        // Binary content should be handled gracefully (replacement characters for invalid UTF-8)
    }

    #[tokio::test]
    async fn test_rapid_sequential_appends() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-rapid";
        let num_entries = 1000;

        // Rapidly append many entries
        for i in 0..num_entries {
            log_service
                .append_to_log(log_id, &format!("{}\n", i))
                .await
                .unwrap();
        }

        let content = log_service.get_log_content(log_id).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), num_entries);

        // Verify all numbers are present and in order
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(*line, index.to_string());
        }
    }
}
