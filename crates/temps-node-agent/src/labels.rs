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
    read_bounded(path, max, false)
}

/// [`read_regular_file`], but following a symlink at `path`: for the agent
/// reading its own config, which it already follows at start.
fn read_bounded(
    path: &Path,
    max: u64,
    follow_symlink: bool,
) -> std::io::Result<(String, std::fs::Metadata)> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let no_follow = if follow_symlink { 0 } else { libc::O_NOFOLLOW };
        options.custom_flags(no_follow | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    let _ = follow_symlink;
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
    /// The control plane that accepted them: node IDs are only unique
    /// within one. `None` in a record written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane_url: Option<String>,
    pub labels: serde_json::Value,
    pub origin: LabelsOrigin,
    /// When the heartbeat carrying `labels` succeeded.
    pub accepted_at: DateTime<Utc>,
}

/// The node and control plane the running agent reports as. `agent.json`
/// labels are applied only while the file still names the same ones: a
/// `temps join` that re-enrolled this directory while the old agent still
/// runs must not have its labels reported to the old control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrollment {
    pub node_id: i32,
    pub control_plane_url: String,
}

impl Enrollment {
    /// Whether `config` (an `agent.json` object) is this enrollment's.
    pub fn owns(&self, config: &serde_json::Value) -> bool {
        let url = |url: &str| url.trim_end_matches('/').to_string();
        config.get("node_id").and_then(serde_json::Value::as_i64) == Some(i64::from(self.node_id))
            && config
                .get("control_plane_url")
                .and_then(serde_json::Value::as_str)
                .map(url)
                == Some(url(&self.control_plane_url))
    }
}

/// Which version of a file was read: replaced (a new inode) or rewritten
/// in place (a new size or modification time) shows as a different one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileVersion {
    dev: u64,
    ino: u64,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl FileVersion {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let (dev, ino) = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let (dev, ino) = (0, 0);
        Self {
            dev,
            ino,
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

/// The `labels` object in the `agent.json` at `path`: `None` when the file
/// cannot be read or parsed, `labels` is not an object, or the file belongs
/// to another enrollment than `enrollment`, so the caller keeps the labels
/// it has. A file without `labels` has none.
pub fn read_saved_labels(path: &Path, enrollment: &Enrollment) -> Option<serde_json::Value> {
    read_saved(path, enrollment).map(|(labels, _)| labels)
}

fn read_saved(path: &Path, enrollment: &Enrollment) -> Option<(serde_json::Value, FileVersion)> {
    // The agent's own config: a symlinked agent.json is followed, as at
    // start, so its labels still reload.
    let (text, meta) = read_bounded(path, MAX_CONFIG_BYTES, true).ok()?;
    let config: serde_json::Value = match serde_json::from_str(&text) {
        Ok(config) => config,
        Err(error) => {
            debug!(%error, path = %path.display(), "agent config unreadable; keeping the current labels");
            return None;
        }
    };
    // A document that is not an object (`null`, an array: a broken hand
    // edit) says nothing about labels; it must not clear them.
    if !config.is_object() {
        warn!(path = %path.display(), "agent.json is not an object; keeping the current labels");
        return None;
    }
    if !enrollment.owns(&config) {
        debug!(
            path = %path.display(),
            node_id = enrollment.node_id,
            "agent.json now names another node or control plane (re-joined); keeping the \
             labels this agent reports until it is restarted"
        );
        return None;
    }
    let labels = match config.get("labels") {
        None | Some(serde_json::Value::Null) => serde_json::json!({}),
        Some(labels @ serde_json::Value::Object(_)) => labels.clone(),
        Some(_) => {
            warn!(path = %path.display(), "agent.json 'labels' is not an object; keeping the current labels");
            return None;
        }
    };
    Some((labels, FileVersion::of(&meta)))
}

/// The labels the heartbeat sends. `revision` grows whenever `agent.json`
/// is written, even with the same labels, so a heartbeat can tell a change
/// it must confirm again (A, then B, then back to A before any heartbeat
/// carried B) from the same labels beat after beat.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentLabels {
    pub labels: serde_json::Value,
    pub revision: u64,
}

/// The labels the heartbeat sends, starting from `initial`. With labels
/// from `agent.json` and a data directory, a background task re-reads the
/// file and publishes a changed set, which wakes the heartbeat.
pub fn watch_labels(
    settings: &LabelsSettings,
    initial: serde_json::Value,
    enrollment: Enrollment,
) -> (
    watch::Receiver<CurrentLabels>,
    Option<watch::Sender<CurrentLabels>>,
) {
    let (sender, receiver) = watch::channel(CurrentLabels {
        labels: initial,
        revision: 0,
    });
    match (settings.origin, &settings.data_dir) {
        (LabelsOrigin::AgentJson, Some(dir)) => {
            tokio::spawn(reload_loop(dir.join("agent.json"), enrollment, sender));
            (receiver, None)
        }
        // Nothing reloads: the caller keeps the sender so the channel stays
        // open and `changed()` simply never fires.
        _ => (receiver, Some(sender)),
    }
}

async fn reload_loop(path: PathBuf, enrollment: Enrollment, sender: watch::Sender<CurrentLabels>) {
    let mut interval = tokio::time::interval(RELOAD_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seen: Option<FileVersion> = None;
    loop {
        interval.tick().await;
        // The heartbeat loop is gone: nothing reads the labels any more.
        if sender.is_closed() {
            return;
        }
        let read_path = path.clone();
        let read_enrollment = enrollment.clone();
        let Ok(Some((saved, version))) =
            tokio::task::spawn_blocking(move || read_saved(&read_path, &read_enrollment)).await
        else {
            continue;
        };
        // Rewritten since the last read, possibly with labels that changed
        // and changed back in between.
        let rewritten = seen.is_some_and(|seen| seen != version);
        seen = Some(version);
        let changed = sender.send_if_modified(|current| {
            if current.labels == saved && !rewritten {
                return false;
            }
            current.labels = saved.clone();
            current.revision += 1;
            true
        });
        if changed {
            info!(labels = %saved, "agent.json labels written; reporting them now");
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
    read_status_file(data_dir).map(|(status, _)| status)
}

/// [`read_status`] with the metadata of the file read, so a root caller can
/// check who wrote it.
pub fn read_status_file(data_dir: &Path) -> Option<(LabelsStatus, std::fs::Metadata)> {
    let (text, meta) = read_regular_file(&data_dir.join(STATUS_FILE), MAX_STATUS_BYTES).ok()?;
    Some((serde_json::from_str(&text).ok()?, meta))
}

/// Records each newly accepted label set, once.
pub(crate) struct AcceptedLabels {
    settings: LabelsSettings,
    enrollment: Enrollment,
    recorded: Option<serde_json::Value>,
    /// The revision the last heartbeat attempt carried, accepted or not.
    sending: Option<u64>,
}

impl AcceptedLabels {
    pub(crate) fn new(settings: LabelsSettings, enrollment: Enrollment) -> Self {
        Self {
            settings,
            enrollment,
            recorded: None,
            sending: None,
        }
    }

    /// Before each heartbeat: a new revision (the file was written since
    /// the last attempt) makes the next acceptance a new one, even with
    /// labels equal to what was recorded earlier (A recorded, then B, then
    /// back to A: A is recorded again with a new time, so `temps agent
    /// labels` can confirm it).
    pub(crate) fn sending(&mut self, labels: &CurrentLabels) {
        if self.sending != Some(labels.revision) {
            if self.sending.is_some() {
                self.recorded = None;
            }
            self.sending = Some(labels.revision);
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
        let node_id = self.enrollment.node_id;
        let status = LabelsStatus {
            node_id,
            control_plane_url: Some(self.enrollment.control_plane_url.clone()),
            labels: labels.clone(),
            origin: self.settings.origin,
            accepted_at: Utc::now(),
        };
        match tokio::task::spawn_blocking(move || record_accepted(&dir, &status)).await {
            Ok(Ok(())) => {
                debug!(node_id, labels = %labels, "recorded the labels the control plane accepted");
                self.recorded = Some(labels.clone());
            }
            Ok(Err(error)) => warn!(
                node_id,
                %error,
                "could not record the labels the control plane accepted; `temps agent labels` \
                 cannot confirm them"
            ),
            Err(error) => {
                warn!(node_id, %error, "recording accepted labels panicked")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLANE: &str = "https://cp.example.test";

    fn enrollment() -> Enrollment {
        Enrollment {
            node_id: 1,
            control_plane_url: PLANE.to_string(),
        }
    }

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("agent.json");
        std::fs::write(&path, text).unwrap();
        path
    }

    /// An `agent.json` of [`enrollment`] with `labels` (raw JSON).
    fn config(labels: &str) -> String {
        format!(r#"{{"node_id":1,"control_plane_url":"{PLANE}","labels":{labels}}}"#)
    }

    #[test]
    fn saved_labels_are_read_from_agent_json_or_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ours = enrollment();
        let path = write(dir.path(), &config(r#"{"gpu":"true"}"#));
        assert_eq!(
            read_saved_labels(&path, &ours),
            Some(serde_json::json!({"gpu":"true"}))
        );
        write(
            dir.path(),
            &format!(r#"{{"node_id":1,"control_plane_url":"{PLANE}/"}}"#),
        );
        assert_eq!(read_saved_labels(&path, &ours), Some(serde_json::json!({})));
        // Half-written or hand-broken: keep what is in force.
        write(dir.path(), r#"{"node_id":1,"labels":{"gp"#);
        assert_eq!(read_saved_labels(&path, &ours), None);
        write(dir.path(), &config(r#""gpu=true""#));
        assert_eq!(read_saved_labels(&path, &ours), None);
        // Valid JSON but not a config object: keep, never clear, the labels
        // (a dedicated node must not turn ordinary on a broken edit).
        for broken in ["null", "[]", "\"x\"", "3"] {
            write(dir.path(), broken);
            assert_eq!(read_saved_labels(&path, &ours), None, "{broken}");
        }
        assert_eq!(
            read_saved_labels(&dir.path().join("missing.json"), &ours),
            None
        );
    }

    #[test]
    fn a_config_from_another_enrollment_is_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let ours = enrollment();
        // Re-joined as another node, or to another control plane, while
        // this agent still runs: its labels are not this agent's to report.
        let path = write(
            dir.path(),
            &format!(r#"{{"node_id":2,"control_plane_url":"{PLANE}","labels":{{"gpu":"true"}}}}"#),
        );
        assert_eq!(read_saved_labels(&path, &ours), None);
        write(
            dir.path(),
            r#"{"node_id":1,"control_plane_url":"https://other.example.test","labels":{}}"#,
        );
        assert_eq!(read_saved_labels(&path, &ours), None);
        write(dir.path(), r#"{"node_id":1,"labels":{}}"#);
        assert_eq!(read_saved_labels(&path, &ours), None);
    }

    #[tokio::test]
    async fn a_label_change_in_agent_json_is_published() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &config(r#"{"zone":"a"}"#));
        let settings = LabelsSettings {
            origin: LabelsOrigin::AgentJson,
            data_dir: Some(dir.path().to_path_buf()),
        };
        let (mut labels, kept) =
            watch_labels(&settings, serde_json::json!({"zone":"a"}), enrollment());
        assert!(kept.is_none());
        // Let the first read see the file as the agent started with it.
        tokio::time::sleep(Duration::from_millis(200)).await;
        write(
            dir.path(),
            &config(r#"{"zone":"a","temps.sh/role":"dedicated"}"#),
        );
        tokio::time::timeout(RELOAD_INTERVAL * 3, labels.changed())
            .await
            .expect("the change is published within a few reloads")
            .unwrap();
        let first = labels.borrow_and_update().clone();
        assert_eq!(
            first.labels,
            serde_json::json!({"zone":"a","temps.sh/role":"dedicated"})
        );
        // Written again with the same labels (changed and changed back
        // between two reads): a new revision, so it is confirmed again.
        write(
            dir.path(),
            // A different length, so the rewrite shows even where
            // modification times are coarse.
            &config(r#"{"temps.sh/role":"dedicated", "zone":"a"}"#),
        );
        tokio::time::timeout(RELOAD_INTERVAL * 3, labels.changed())
            .await
            .expect("a rewrite is published")
            .unwrap();
        let second = labels.borrow_and_update().clone();
        assert_eq!(second.labels, first.labels);
        assert!(second.revision > first.revision);
    }

    #[tokio::test]
    async fn pinned_labels_are_never_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &config(r#"{"zone":"b"}"#));
        let settings = LabelsSettings {
            origin: LabelsOrigin::Override,
            data_dir: Some(dir.path().to_path_buf()),
        };
        let (mut labels, kept) =
            watch_labels(&settings, serde_json::json!({"zone":"a"}), enrollment());
        assert!(
            kept.is_some(),
            "the sender is kept so the channel stays open"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), labels.changed())
                .await
                .is_err()
        );
        assert_eq!(labels.borrow().labels, serde_json::json!({"zone":"a"}));
    }

    #[tokio::test]
    async fn labels_changed_back_are_recorded_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut accepted = AcceptedLabels::new(
            LabelsSettings {
                origin: LabelsOrigin::AgentJson,
                data_dir: Some(dir.path().to_path_buf()),
            },
            enrollment(),
        );
        let a = serde_json::json!({"zone": "a"});
        let at = |revision| CurrentLabels {
            labels: a.clone(),
            revision,
        };
        accepted.sending(&at(0));
        accepted.accepted(&a).await;
        let first = read_status(dir.path()).unwrap().accepted_at;
        // agent.json is written (B, then back to A, or B never accepted
        // because the control plane is down) and A is accepted again.
        accepted.sending(&at(2));
        tokio::time::sleep(Duration::from_millis(5)).await;
        accepted.accepted(&a).await;
        let second = read_status(dir.path()).unwrap();
        assert_eq!(second.labels, a);
        assert_eq!(second.control_plane_url.as_deref(), Some(PLANE));
        // A record from before the field existed still reads.
        std::fs::write(
            dir.path().join(STATUS_FILE),
            r#"{"node_id":1,"labels":{},"origin":"agent_json","accepted_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(read_status(dir.path()).unwrap().control_plane_url, None);
        assert!(
            second.accepted_at > first,
            "a new acceptance gets a new record"
        );
        // The same revision beat after beat: no rewrite.
        accepted.sending(&at(2));
        std::fs::remove_file(dir.path().join(STATUS_FILE)).unwrap();
        accepted.accepted(&a).await;
        assert!(read_status(dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn only_small_regular_files_are_read_and_symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent.json");
        std::fs::write(&file, config("{}")).unwrap();
        assert!(read_regular_file(&file, 1024).is_ok());
        assert!(read_regular_file(&file, 4).is_err(), "over the limit");
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(
            read_regular_file(&link, 1024).is_err(),
            "a symlink is not followed"
        );
        // The agent's own config is followed, as it is at start.
        assert_eq!(
            read_saved_labels(&link, &enrollment()),
            Some(serde_json::json!({}))
        );
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
            Enrollment {
                node_id: 7,
                control_plane_url: PLANE.to_string(),
            },
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
