//! Live tail service for real-time log streaming via SSE
//!
//! Reads from the in-memory ingest buffer broadcast channel.
//! Filters are applied server-side before streaming to the client.

use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::types::{LogLine, TailFilter};

/// Service for live-tailing logs via Server-Sent Events.
///
/// The tail endpoint does not store or re-index lines — it reads directly from
/// the in-memory ingest buffer's broadcast channel.
pub struct TailService {
    /// Broadcast receiver factory — each subscriber gets a new receiver
    tail_tx: broadcast::Sender<LogLine>,
}

impl TailService {
    /// Create a new TailService from the collector's broadcast sender.
    pub fn new(tail_tx: broadcast::Sender<LogLine>) -> Self {
        Self { tail_tx }
    }

    /// Create a filtered stream of log lines matching the given filter.
    ///
    /// The returned stream applies level and text filters server-side before
    /// yielding lines. The stream will complete when the underlying broadcast
    /// channel is dropped.
    pub fn subscribe(
        &self,
        filter: TailFilter,
    ) -> impl tokio_stream::Stream<Item = LogLine> + Send + 'static {
        let rx = self.tail_tx.subscribe();
        let stream = BroadcastStream::new(rx);

        stream.filter_map(move |result| {
            match result {
                Ok(line) => {
                    if matches_tail_filter(&line, &filter) {
                        Some(line)
                    } else {
                        None
                    }
                }
                Err(_) => None, // Lag or closed channel
            }
        })
    }

    /// Get the current number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tail_tx.receiver_count()
    }
}

/// Check if a log line matches the tail filter.
fn matches_tail_filter(line: &LogLine, filter: &TailFilter) -> bool {
    // Project ID must match
    if line.project_id != filter.project_id {
        return false;
    }

    // Service must match
    if line.service != filter.service {
        return false;
    }

    // Environment must match
    if line.env != filter.env {
        return false;
    }

    // Level filter
    if !filter.levels.is_empty() && !filter.levels.contains(&line.level) {
        return false;
    }

    // Text filter (substring match, case-insensitive)
    if let Some(ref text) = filter.text {
        if !line.msg.to_lowercase().contains(&text.to_lowercase()) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogLevel, LogStream};

    fn make_line(service: &str, level: LogLevel, msg: &str) -> LogLine {
        LogLine {
            ts: chrono::Utc::now(),
            stream: LogStream::Stdout,
            level,
            msg: msg.to_string(),
            fields: None,
            container_id: "cnt1".to_string(),
            service: service.to_string(),
            env: "1".to_string(),
            project_id: 42,
            deploy_id: None,
            node_id: None,
            node_name: None,
        }
    }

    fn make_filter(service: &str) -> TailFilter {
        TailFilter {
            project_id: 42,
            service: service.to_string(),
            env: "1".to_string(),
            levels: vec![],
            text: None,
        }
    }

    #[test]
    fn test_matches_tail_filter_basic() {
        let line = make_line("web", LogLevel::Info, "hello");
        let filter = make_filter("web");
        assert!(matches_tail_filter(&line, &filter));
    }

    #[test]
    fn test_matches_tail_filter_wrong_service() {
        let line = make_line("api", LogLevel::Info, "hello");
        let filter = make_filter("web");
        assert!(!matches_tail_filter(&line, &filter));
    }

    #[test]
    fn test_matches_tail_filter_level() {
        let line = make_line("web", LogLevel::Info, "hello");
        let mut filter = make_filter("web");
        filter.levels = vec![LogLevel::Error];
        assert!(!matches_tail_filter(&line, &filter));
    }

    #[test]
    fn test_matches_tail_filter_text() {
        let line = make_line("web", LogLevel::Error, "database timeout");
        let mut filter = make_filter("web");
        filter.text = Some("timeout".to_string());
        assert!(matches_tail_filter(&line, &filter));

        filter.text = Some("connection".to_string());
        assert!(!matches_tail_filter(&line, &filter));
    }

    #[test]
    fn test_matches_tail_filter_wrong_project() {
        let mut line = make_line("web", LogLevel::Info, "hello");
        line.project_id = 999; // Different project
        let filter = make_filter("web");
        assert!(!matches_tail_filter(&line, &filter));
    }

    #[tokio::test]
    async fn test_subscribe_receives_matching_lines() {
        let (tx, _) = broadcast::channel(100);
        let service = TailService::new(tx.clone());

        let filter = make_filter("web");
        let mut stream = Box::pin(service.subscribe(filter));

        // Send a matching line
        let line = make_line("web", LogLevel::Info, "test");
        tx.send(line.clone()).unwrap();

        // Send a non-matching line (different service)
        tx.send(make_line("api", LogLevel::Info, "other")).unwrap();

        // Drop sender to close the stream
        drop(tx);

        let received = stream.next().await;
        assert!(received.is_some());
        assert_eq!(received.unwrap().msg, "test");
    }
}
