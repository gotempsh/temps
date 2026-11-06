// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The labels this worker reports, and what the control plane accepted
//! (ADR-091 D3, D4).
//!
//! Labels saved in `agent.json` are re-read every [`RELOAD_INTERVAL`], so
//! `temps agent labels set` takes effect without restarting the agent. Labels
//! given with `temps agent --labels` / `TEMPS_NODE_LABELS` are pinned and never
//! reloaded. After a heartbeat carrying a new label set succeeds, the agent
//! records it in [`STATUS_FILE`], which is how the CLI confirms a change
//! locally.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// How often `agent.json` is re-read for changed labels.
pub const RELOAD_INTERVAL: Duration = Duration::from_secs(5);
/// Next to `agent.json`: the labels the control plane last accepted.
pub const STATUS_FILE: &str = "labels-status.json";

/// Where the labels the agent reports come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelsOrigin {
    /// The `labels` object of `agent.json`, reloaded while the agent runs.
    AgentJson,
    /// `temps agent --labels` or `TEMPS_NODE_LABELS`, which override
    /// `agent.json` for the life of the process.
    Override,
}

/// How the agent treats its labels. `data_dir` is the directory holding
/// `agent.json`; `None` turns off both the reload and the record (tests, and
/// embedders without an agent directory).
#[derive(Debug, Clone)]
pub struct LabelsSettings {
    pub origin: LabelsOrigin,
    pub data_dir: Option<PathBuf>,
}

impl Default for LabelsSettings {
    fn default() -> Self {
        Self {
            origin: LabelsOrigin::AgentJson,
            data_dir: None,
        }
    }
}

/// Largest `agent.json` read: it holds a token, a few paths and the labels.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
/// Largest [`STATUS_FILE`] read.
pub const MAX_STATUS_BYTES: u64 = 64 * 1024;

/// Read `path` only if it is a regular file of at most `max` bytes, without
/// following a symlink at `path` and without blocking on a FIFO or device:
/// a root `temps agent labels` run may read files in a directory another
/// user can change. Returns the text and the metadata of the file read, so
/// a caller can check it was not swapped before writing over it.
pub fn read_regular_file(path: &Path, max: u64) -> std::io::Result<(String, std::fs::Metadata)> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let too_large = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is larger than {max} bytes", path.display()),
        )
    };
    if meta.len() > max {
        return Err(too_large());
    }
    let mut text = String::new();
    file.take(max + 1).read_to_string(&mut text)?;
    if text.len() as u64 > max {
        return Err(too_large());
    }
    Ok((text, meta))
}

/// What [`STATUS_FILE`] holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelsStatus {
    pub node_id: i32,
    pub labels: serde_json::Value,
    pub origin: LabelsOrigin,
    /// When the heartbeat carrying `labels` succeeded.
    pub accepted_at: DateTime<Utc>,
}

/// The `labels` object in the `agent.json` at `path`: `None` when the file
/// cannot be read or parsed, or `labels` is not an object, so the caller
/// keeps the labels it has. A file without `labels` has none.
pub fn read_saved_labels(path: &Path) -> Option<serde_json::Value> {
    let (text, _) = read_regular_file(path, MAX_CONFIG_BYTES).ok()?;
    let config: serde_json::Value = match serde_json::from_str(&text) {
        Ok(config) => config,
        Err(error) => {
            debug!(%error, path = %path.display(), "agent config unreadable; keeping the current labels");
            return None;
        }
    };
    match config.get("labels") {
        None | Some(serde_json::Value::Null) => Some(serde_json::json!({})),
        Some(labels @ serde_json::Value::Object(_)) => Some(labels.clone()),
        Some(_) => {
            warn!(path = %path.display(), "agent.json 'labels' is not an object; keeping the current labels");
            None
        }
    }
}

/// The labels the heartbeat sends, starting from `initial`. With labels
/// from `agent.json` and a data directory, a background task re-reads the
/// file and publishes a changed set, which wakes the heartbeat.
pub fn watch_labels(
    settings: &LabelsSettings,
    initial: serde_json::Value,
) -> (
    watch::Receiver<serde_json::Value>,
    Option<watch::Sender<serde_json::Value>>,
) {
    let (sender, receiver) = watch::channel(initial);
    match (settings.origin, &settings.data_dir) {
        (LabelsOrigin::AgentJson, Some(dir)) => {
            tokio::spawn(reload_loop(dir.join("agent.json"), sender));
            (receiver, None)
        }
        // Nothing reloads: the caller keeps the sender so the channel stays
        // open and `changed()` simply never fires.
        _ => (receiver, Some(sender)),
    }
}

async fn reload_loop(path: PathBuf, sender: watch::Sender<serde_json::Value>) {
    let mut interval = tokio::time::interval(RELOAD_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        // The heartbeat loop is gone: nothing reads the labels any more.
        if sender.is_closed() {
            return;
        }
        let read_path = path.clone();
        let Ok(Some(saved)) =
            tokio::task::spawn_blocking(move || read_saved_labels(&read_path)).await
        else {
            continue;
        };
        let changed = sender.send_if_modified(|current| {
            if *current == saved {
                return false;
            }
            *current = saved.clone();
            true
        });
        if changed {
            info!(labels = %saved, "labels changed in agent.json; reporting them now");
        }
    }
}

/// Write [`STATUS_FILE`] in `data_dir`: a temporary file, owner-only, renamed
/// over the old record, so a reader never sees half of one.
pub fn record_accepted(data_dir: &Path, status: &LabelsStatus) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(status).map_err(std::io::Error::other)?;
    let path = data_dir.join(STATUS_FILE);
    let tmp = data_dir.join(format!("{STATUS_FILE}.tmp"));
    {
        use std::io::Write;
        // Created exclusively, after any leftover is removed: `O_EXCL` never
        // follows a symlink planted in its place.
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&text)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

/// [`STATUS_FILE`] in `data_dir`, if there is a readable one.
pub fn read_status(data_dir: &Path) -> Option<LabelsStatus> {
    let (text, _) = read_regular_file(&data_dir.join(STATUS_FILE), MAX_STATUS_BYTES).ok()?;
    serde_json::from_str(&text).ok()
}

/// Records each newly accepted label set, once.
pub(crate) struct AcceptedLabels {
    settings: LabelsSettings,
    node_id: i32,
    recorded: Option<serde_json::Value>,
    /// The labels the last heartbeat attempt carried, accepted or not.
    sending: Option<serde_json::Value>,
}

impl AcceptedLabels {
    pub(crate) fn new(settings: LabelsSettings, node_id: i32) -> Self {
        Self {
            settings,
            node_id,
            recorded: None,
            sending: None,
        }
    }

    /// Before each heartbeat: labels different from the last attempt's make
    /// the next acceptance a new one, even if they equal what was recorded
    /// earlier (A recorded, B never accepted, back to A: A is recorded
    /// again with a new time, so `temps agent labels` can confirm it).
    pub(crate) fn sending(&mut self, labels: &serde_json::Value) {
        if self.sending.as_ref() != Some(labels) {
            if self.sending.is_some() {
                self.recorded = None;
            }
            self.sending = Some(labels.clone());
        }
    }

    /// After a heartbeat carrying `labels` succeeded: write the record if
    /// these labels have not been recorded by this process yet. A failed
    /// write is logged and retried after the next heartbeat.
    pub(crate) async fn accepted(&mut self, labels: &serde_json::Value) {
        let Some(dir) = self.settings.data_dir.clone() else {
            return;
        };
        if self.recorded.as_ref() == Some(labels) {
            return;
        }
        let status = LabelsStatus {
            node_id: self.node_id,
            labels: labels.clone(),
            origin: self.settings.origin,
            accepted_at: Utc::now(),
        };
        match tokio::task::spawn_blocking(move || record_accepted(&dir, &status)).await {
            Ok(Ok(())) => {
                debug!(node_id = self.node_id, labels = %labels, "recorded the labels the control plane accepted");
                self.recorded = Some(labels.clone());
            }
            Ok(Err(error)) => warn!(
                node_id = self.node_id,
                %error,
                "could not record the labels the control plane accepted; `temps agent labels` \
                 cannot confirm them"
            ),
            Err(error) => {
                warn!(node_id = self.node_id, %error, "recording accepted labels panicked")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("agent.json");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn saved_labels_are_read_from_agent_json_or_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), r#"{"node_id":1,"labels":{"gpu":"true"}}"#);
        assert_eq!(
            read_saved_labels(&path),
            Some(serde_json::json!({"gpu":"true"}))
        );
        write(dir.path(), r#"{"node_id":1}"#);
        assert_eq!(read_saved_labels(&path), Some(serde_json::json!({})));
        // Half-written or hand-broken: keep what is in force.
        write(dir.path(), r#"{"node_id":1,"labels":{"gp"#);
        assert_eq!(read_saved_labels(&path), None);
        write(dir.path(), r#"{"labels":"gpu=true"}"#);
        assert_eq!(read_saved_labels(&path), None);
        assert_eq!(read_saved_labels(&dir.path().join("missing.json")), None);
    }

    #[tokio::test]
    async fn a_label_change_in_agent_json_is_published() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), r#"{"labels":{"zone":"a"}}"#);
        let settings = LabelsSettings {
            origin: LabelsOrigin::AgentJson,
            data_dir: Some(dir.path().to_path_buf()),
        };
        let (mut labels, kept) = watch_labels(&settings, serde_json::json!({"zone":"a"}));
        assert!(kept.is_none());
        write(
            dir.path(),
            r#"{"labels":{"zone":"a","temps.sh/role":"dedicated"}}"#,
        );
        tokio::time::timeout(RELOAD_INTERVAL * 3, labels.changed())
            .await
            .expect("the change is published within a few reloads")
            .unwrap();
        assert_eq!(
            *labels.borrow(),
            serde_json::json!({"zone":"a","temps.sh/role":"dedicated"})
        );
    }

    #[tokio::test]
    async fn pinned_labels_are_never_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), r#"{"labels":{"zone":"b"}}"#);
        let settings = LabelsSettings {
            origin: LabelsOrigin::Override,
            data_dir: Some(dir.path().to_path_buf()),
        };
        let (mut labels, kept) = watch_labels(&settings, serde_json::json!({"zone":"a"}));
        assert!(
            kept.is_some(),
            "the sender is kept so the channel stays open"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), labels.changed())
                .await
                .is_err()
        );
        assert_eq!(*labels.borrow(), serde_json::json!({"zone":"a"}));
    }

    #[tokio::test]
    async fn labels_changed_back_are_recorded_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut accepted = AcceptedLabels::new(
            LabelsSettings {
                origin: LabelsOrigin::AgentJson,
                data_dir: Some(dir.path().to_path_buf()),
            },
            7,
        );
        let a = serde_json::json!({"zone": "a"});
        accepted.sending(&a);
        accepted.accepted(&a).await;
        let first = read_status(dir.path()).unwrap().accepted_at;
        // B is sent but never accepted (the control plane is down) ...
        accepted.sending(&serde_json::json!({"zone": "b"}));
        // ... and the operator goes back to A, which is then accepted.
        accepted.sending(&a);
        tokio::time::sleep(Duration::from_millis(5)).await;
        accepted.accepted(&a).await;
        let second = read_status(dir.path()).unwrap();
        assert_eq!(second.labels, a);
        assert!(
            second.accepted_at > first,
            "a new acceptance gets a new record"
        );
        // The same labels beat after beat: no rewrite.
        accepted.sending(&a);
        std::fs::remove_file(dir.path().join(STATUS_FILE)).unwrap();
        accepted.accepted(&a).await;
        assert!(read_status(dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn only_small_regular_files_are_read_and_symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent.json");
        std::fs::write(&file, r#"{"labels":{}}"#).unwrap();
        assert!(read_regular_file(&file, 1024).is_ok());
        assert!(read_regular_file(&file, 4).is_err(), "over the limit");
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(
            read_regular_file(&link, 1024).is_err(),
            "a symlink is not followed"
        );
        assert_eq!(read_saved_labels(&link), None);
        assert!(read_regular_file(dir.path(), 1024).is_err(), "a directory");
        // A FIFO would block a plain read forever.
        let fifo = dir.path().join(STATUS_FILE);
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo has no other preconditions.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert!(read_status(dir.path()).is_none());
    }

    #[tokio::test]
    async fn accepted_labels_are_recorded_once_per_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut accepted = AcceptedLabels::new(
            LabelsSettings {
                origin: LabelsOrigin::Override,
                data_dir: Some(dir.path().to_path_buf()),
            },
            7,
        );
        accepted.accepted(&serde_json::json!({"gpu":"true"})).await;
        let first = read_status(dir.path()).unwrap();
        assert_eq!(first.node_id, 7);
        assert_eq!(first.origin, LabelsOrigin::Override);
        assert_eq!(first.labels, serde_json::json!({"gpu":"true"}));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(STATUS_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // The same labels again: no rewrite.
        std::fs::remove_file(dir.path().join(STATUS_FILE)).unwrap();
        accepted.accepted(&serde_json::json!({"gpu":"true"})).await;
        assert!(read_status(dir.path()).is_none());
        accepted.accepted(&serde_json::json!({})).await;
        assert_eq!(
            read_status(dir.path()).unwrap().labels,
            serde_json::json!({})
        );
        assert!(!dir.path().join(format!("{STATUS_FILE}.tmp")).exists());
    }
}
