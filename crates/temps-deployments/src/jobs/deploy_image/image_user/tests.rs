// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use temps_deployer::{BuilderError, ImageBuilder, ImageConfig, ImageImportStream, ImageInfo};
use temps_docker_util::image_user::ImageUserError;

use super::*;
use crate::jobs::deploy_image::tests::TrackingMockContainerDeployer;
use crate::jobs::{DeployImageJobBuilder, DeploymentTarget};
use crate::workflow::{LogWriter, WorkflowError};

/// An image store holding one image: its `USER` and the files under it.
struct FakeImageStore {
    /// `None`: an agent that does not report the image configuration.
    config: Option<ImageConfig>,
    /// Images whose configuration differs from `config`, by tag.
    per_image: HashMap<&'static str, ImageConfig>,
    files: HashMap<&'static str, Vec<u8>>,
    exported: Mutex<Vec<String>>,
    /// The image reference each export named.
    exported_from: Mutex<Vec<String>>,
}

impl FakeImageStore {
    fn with_user(user: Option<&str>) -> Self {
        Self {
            config: Some(ImageConfig::from_raw(&[], user, &[], &[])),
            per_image: HashMap::new(),
            files: HashMap::new(),
            exported: Mutex::new(Vec::new()),
            exported_from: Mutex::new(Vec::new()),
        }
    }

    fn image_with_user(mut self, image: &'static str, user: Option<&str>) -> Self {
        self.per_image
            .insert(image, ImageConfig::from_raw(&[], user, &[], &[]));
        self
    }

    fn file(mut self, path: &'static str, contents: &str) -> Self {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        let name = path.rsplit('/').next().unwrap();
        builder
            .append_data(&mut header, name, contents.as_bytes())
            .unwrap();
        self.files.insert(path, builder.into_inner().unwrap());
        self
    }
}

#[async_trait]
impl ImageBuilder for FakeImageStore {
    async fn build_image(
        &self,
        _request: temps_deployer::BuildRequest,
    ) -> Result<temps_deployer::BuildResult, BuilderError> {
        unimplemented!("not used")
    }

    async fn build_image_with_callback(
        &self,
        _request: temps_deployer::BuildRequestWithCallback,
    ) -> Result<temps_deployer::BuildResult, BuilderError> {
        unimplemented!("not used")
    }

    async fn import_image(&self, _image_path: PathBuf, _tag: &str) -> Result<String, BuilderError> {
        unimplemented!("not used")
    }

    async fn save_image(&self, _image_name: &str, _output_path: &Path) -> Result<(), BuilderError> {
        unimplemented!("not used")
    }

    async fn export_path_stream(
        &self,
        image_name: &str,
        source_path: &str,
    ) -> Result<ImageImportStream, BuilderError> {
        self.exported.lock().unwrap().push(source_path.to_string());
        self.exported_from
            .lock()
            .unwrap()
            .push(image_name.to_string());
        let Some(archive) = self.files.get(source_path).cloned() else {
            return Err(BuilderError::Other(format!(
                "no {source_path} in {image_name}"
            )));
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            bytes::Bytes::from(archive),
        )])))
    }

    async fn extract_from_image(
        &self,
        _image_name: &str,
        _source_path: &str,
        _destination_path: &Path,
    ) -> Result<(), BuilderError> {
        unimplemented!("not used")
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        unimplemented!("not used")
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
        unimplemented!("not used")
    }

    async fn inspect_image(&self, image_name: &str) -> Result<ImageInfo, BuilderError> {
        if image_name == "missing:1" {
            return Err(BuilderError::ImageNotFound(image_name.to_string()));
        }
        Ok(ImageInfo {
            id: format!("sha256:{image_name}"),
            architecture: "amd64".to_string(),
            os: "linux".to_string(),
            platform: "linux/amd64".to_string(),
            size_bytes: 1,
            tags: vec![image_name.to_string()],
            created: None,
            working_dir: None,
            config: self
                .per_image
                .get(image_name)
                .cloned()
                .or_else(|| self.config.clone()),
        })
    }

    fn get_native_platform(&self) -> String {
        "linux/amd64".to_string()
    }
}

const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\n\
nodejs:x:1001:1001::/home/nodejs:/bin/sh\n\
toor:x:0:0::/root:/bin/sh\n";

#[tokio::test]
async fn an_image_without_a_user_is_root_and_reads_no_files() {
    let store = FakeImageStore::with_user(None);
    assert_eq!(
        check_image_user(&store, "my-app:1").await,
        ImageUserCheck::Checked {
            declared: None,
            verdict: ImageUserVerdict::Root
        }
    );
    assert!(store.exported.lock().unwrap().is_empty());
}

#[tokio::test]
async fn root_zero_and_uid_zero_with_any_group_are_root() {
    for user in ["root", "0", "0:0", "0:staff", "root:staff"] {
        // `root` is looked up in the image's passwd, like any name.
        let store = FakeImageStore::with_user(Some(user))
            .file("/etc/passwd", PASSWD)
            .file("/etc/group", "root:x:0:\n");
        let check = check_image_user(&store, "my-app:1").await;
        assert_eq!(
            check,
            ImageUserCheck::Checked {
                declared: Some(user.to_string()),
                verdict: ImageUserVerdict::Root
            },
            "USER {user}"
        );
    }
}

#[tokio::test]
async fn a_named_user_resolves_against_the_images_own_passwd() {
    let store = FakeImageStore::with_user(Some("nodejs")).file("/etc/passwd", PASSWD);
    assert_eq!(
        check_image_user(&store, "my-app:1").await,
        ImageUserCheck::Checked {
            declared: Some("nodejs".to_string()),
            verdict: ImageUserVerdict::NonRoot { uid: 1001 }
        }
    );
    assert_eq!(*store.exported.lock().unwrap(), vec!["/etc/passwd"]);

    // A name the image maps to uid 0 is root.
    let store = FakeImageStore::with_user(Some("toor")).file("/etc/passwd", PASSWD);
    assert!(matches!(
        check_image_user(&store, "my-app:1").await,
        ImageUserCheck::Checked {
            verdict: ImageUserVerdict::Root,
            ..
        }
    ));
}

#[tokio::test]
async fn a_numeric_uid_and_gid_needs_no_files() {
    let store = FakeImageStore::with_user(Some("10001:10001"));
    assert_eq!(
        check_image_user(&store, "my-app:1").await,
        ImageUserCheck::Checked {
            declared: Some("10001:10001".to_string()),
            verdict: ImageUserVerdict::NonRoot { uid: 10001 }
        }
    );
    assert!(store.exported.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unresolvable_named_user_is_unknown_not_root() {
    let store = FakeImageStore::with_user(Some("ghost")).file("/etc/passwd", PASSWD);
    let check = check_image_user(&store, "my-app:1").await;
    assert!(
        matches!(
            &check,
            ImageUserCheck::Checked {
                verdict: ImageUserVerdict::Unresolved(ImageUserError::UserNotFound { .. }),
                ..
            }
        ),
        "{check:?}"
    );
    let note = image_user_note(&check).expect("an unresolved user is a note, not the warning");
    assert!(note.contains("not reported as root"), "{note}");
    assert!(note.contains("ghost"), "{note}");

    // An image with no passwd to look the name up in cannot be checked.
    let store = FakeImageStore::with_user(Some("ghost"));
    let check = check_image_user(&store, "my-app:1").await;
    assert!(
        matches!(&check, ImageUserCheck::NotChecked { reason } if reason.contains("/etc/passwd")),
        "{check:?}"
    );
}

/// The image's files are read from the image that was inspected, by id,
/// never by re-resolving a tag that may have moved since.
#[tokio::test]
async fn identity_files_are_read_from_the_inspected_image_id() {
    let store = FakeImageStore::with_user(Some("nodejs")).file("/etc/passwd", PASSWD);
    check_image_user(&store, "my-app:1").await;
    assert_eq!(
        *store.exported_from.lock().unwrap(),
        vec!["sha256:my-app:1"]
    );
}

#[tokio::test]
async fn a_user_too_long_to_be_reported_is_unknown_not_root() {
    let long = format!(
        "app:{}",
        "g".repeat(temps_deployer::MAX_IMAGE_CONFIG_USER_BYTES)
    );
    let store = FakeImageStore::with_user(Some(&long));
    let check = check_image_user(&store, "my-app:1").await;
    assert!(
        matches!(&check, ImageUserCheck::NotChecked { reason } if reason.contains("longer than")),
        "{check:?}"
    );
    assert!(store.exported.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_image_that_cannot_be_inspected_or_described_is_not_checked() {
    let store = FakeImageStore::with_user(None);
    let check = check_image_user(&store, "missing:1").await;
    assert!(
        matches!(&check, ImageUserCheck::NotChecked { reason } if reason.contains("missing:1")),
        "{check:?}"
    );

    let older_agent = FakeImageStore {
        config: None,
        ..FakeImageStore::with_user(None)
    };
    let check = check_image_user(&older_agent, "my-app:1").await;
    assert!(
        matches!(&check, ImageUserCheck::NotChecked { reason } if reason.contains("did not report")),
        "{check:?}"
    );
}

#[test]
fn the_root_warning_names_the_fix_for_each_origin() {
    let preset = root_user_warning(&ImageOrigin::Preset("python".to_string()), None);
    assert!(
        preset.starts_with("⚠️ This image runs as root (uid 0)."),
        "{preset}"
    );
    assert!(preset.contains("'python' preset"), "{preset}");
    assert!(!preset.contains("your Dockerfile"), "{preset}");

    let dockerfile = root_user_warning(&ImageOrigin::RepositoryDockerfile, None);
    assert!(
        dockerfile.contains(
            "Add a non-root USER to your Dockerfile, e.g. `RUN useradd -r -u 10001 app` and \
             `USER app`"
        ),
        "{dockerfile}"
    );

    let prebuilt = root_user_warning(&ImageOrigin::PrebuiltImage, None);
    assert!(prebuilt.contains("Rebuild the image"), "{prebuilt}");

    for message in [preset, dockerfile, prebuilt] {
        assert!(message.contains(NON_ROOT_USER_DOCS_URL), "{message}");
        assert!(message.contains("The deployment continues"), "{message}");
    }
}

#[test]
fn a_non_root_user_is_noted_with_its_uid() {
    let note = image_user_note(&ImageUserCheck::Checked {
        declared: Some("nodejs".to_string()),
        verdict: ImageUserVerdict::NonRoot { uid: 1001 },
    })
    .unwrap();
    assert_eq!(note, "Image runs as user 'nodejs' (uid 1001)");
    assert_eq!(
        image_user_note(&ImageUserCheck::Checked {
            declared: None,
            verdict: ImageUserVerdict::Root
        }),
        None,
        "root gets the warning instead"
    );
}

#[test]
fn the_recorded_user_is_bounded() {
    assert_eq!(recorded_image_user(None), None);
    assert_eq!(recorded_image_user(Some("app")).as_deref(), Some("app"));
    let long = "a".repeat(MAX_RECORDED_IMAGE_USER_CHARS + 10);
    assert_eq!(
        recorded_image_user(Some(&long)).map(|user| user.chars().count()),
        Some(MAX_RECORDED_IMAGE_USER_CHARS)
    );
}

/// Every line the job writes, with nothing else.
#[derive(Default)]
struct CapturedLog(Mutex<Vec<String>>);

#[async_trait]
impl LogWriter for CapturedLog {
    async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
        self.0.lock().unwrap().push(message);
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        1
    }
}

fn job(
    store: FakeImageStore,
    db: Option<Arc<temps_database::DbConnection>>,
    external_image: bool,
) -> DeployImageJob {
    let mut builder = DeployImageJobBuilder::new(
        "my-app",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("my-app".to_string())
    .image_builder(Arc::new(store));
    if let Some(db) = db {
        builder = builder.failed_container_retention(db, 42);
    }
    if external_image {
        builder = builder.external_image_tag("example.com/my-app:1".to_string());
    }
    builder
        .build(Arc::new(TrackingMockContainerDeployer::new()))
        .expect("valid deploy job")
}

fn image_output() -> BuildImageOutput {
    BuildImageOutput {
        image_tag: "my-app:1".to_string(),
        image_id: "sha256:my-app".to_string(),
        size_bytes: 1,
        build_context: PathBuf::from("."),
        dockerfile_path: PathBuf::from("Dockerfile"),
        image_tags_by_platform: HashMap::new(),
        builder_node_id: None,
        builder_node_ids_by_platform: HashMap::new(),
        image_ids_by_platform: HashMap::new(),
    }
}

fn mock_db() -> Arc<temps_database::DbConnection> {
    Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection(),
    )
}

fn deployment_updates(db: Arc<temps_database::DbConnection>) -> Vec<String> {
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still has owners"));
    db.into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .filter(|statement| statement.sql.contains("UPDATE \"deployments\""))
        .map(|statement| format!("{statement:?}"))
        .collect()
}

#[tokio::test]
async fn a_root_image_is_warned_about_once_and_recorded_on_the_row() {
    let db = mock_db();
    let job = job(FakeImageStore::with_user(None), Some(db.clone()), false);
    let log = Arc::new(CapturedLog::default());
    let mut context = WorkflowContext::new("run-42".to_string(), 42, 3, 4, log.clone());
    context
        .set_output("build", DOCKERFILE_PRESET_OUTPUT, "python")
        .unwrap();

    job.check_and_record_image_user(&image_output(), &context)
        .await;

    let lines = log.0.lock().unwrap().clone();
    let warnings: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("runs as root"))
        .collect();
    assert_eq!(warnings.len(), 1, "{lines:?}");
    assert!(warnings[0].contains("'python' preset"), "{}", warnings[0]);

    drop(job);
    let updates = deployment_updates(db);
    assert_eq!(updates.len(), 1, "{updates:?}");
    let update = &updates[0];
    assert!(update.contains("runs_as_root"), "{update}");
    assert!(update.contains("image_user"), "{update}");
    assert!(update.contains("Bool(Some(true))"), "{update}");
    assert!(update.contains("42"), "{update}");
}

#[tokio::test]
async fn a_non_root_image_is_recorded_with_its_user_and_not_warned_about() {
    let db = mock_db();
    let store = FakeImageStore::with_user(Some("nodejs")).file("/etc/passwd", PASSWD);
    let job = job(store, Some(db.clone()), true);
    let log = Arc::new(CapturedLog::default());
    let context = WorkflowContext::new("run-42".to_string(), 42, 3, 4, log.clone());

    job.check_and_record_image_user(&image_output(), &context)
        .await;

    let lines = log.0.lock().unwrap().clone();
    assert!(
        lines.iter().all(|line| !line.contains("runs as root")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "Image runs as user 'nodejs' (uid 1001)"),
        "{lines:?}"
    );
    drop(job);
    let updates = deployment_updates(db);
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert!(updates[0].contains("Bool(Some(false))"), "{}", updates[0]);
    assert!(updates[0].contains("\"nodejs\""), "{}", updates[0]);
}

#[tokio::test]
async fn an_image_that_was_not_checked_leaves_the_row_alone() {
    let db = mock_db();
    let older_agent = FakeImageStore {
        config: None,
        ..FakeImageStore::with_user(None)
    };
    let job = job(older_agent, Some(db.clone()), true);
    let log = Arc::new(CapturedLog::default());
    let context = WorkflowContext::new("run-42".to_string(), 42, 3, 4, log.clone());

    job.check_and_record_image_user(&image_output(), &context)
        .await;

    assert!(log
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.starts_with("Could not check which user the image runs as")));
    drop(job);
    assert!(deployment_updates(db).is_empty());
}

/// A multi-architecture build is checked per architecture: one image that
/// runs as root is warned about, by name, even when the primary is not.
#[tokio::test]
async fn a_root_image_for_another_architecture_is_warned_about() {
    let db = mock_db();
    let store = FakeImageStore::with_user(Some("nodejs"))
        .file("/etc/passwd", PASSWD)
        .image_with_user("my-app:1-arm64", None);
    let job = job(store, Some(db.clone()), false);
    let log = Arc::new(CapturedLog::default());
    let context = WorkflowContext::new("run-42".to_string(), 42, 3, 4, log.clone());
    let mut output = image_output();
    output.image_tags_by_platform = HashMap::from([
        ("linux/amd64".to_string(), "my-app:1".to_string()),
        ("linux/arm64".to_string(), "my-app:1-arm64".to_string()),
    ]);

    job.check_and_record_image_user(&output, &context).await;

    let lines = log.0.lock().unwrap().clone();
    let warnings: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("runs as root"))
        .collect();
    assert_eq!(warnings.len(), 1, "{lines:?}");
    assert!(warnings[0].contains("my-app:1-arm64"), "{}", warnings[0]);
    drop(job);
    let updates = deployment_updates(db);
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert!(updates[0].contains("Bool(Some(true))"), "{}", updates[0]);
}

/// A log writer that never returns, like a stalled log store.
struct StalledLog;

#[async_trait]
impl LogWriter for StalledLog {
    async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
        std::future::pending().await
    }

    fn stage_id(&self) -> i32 {
        1
    }
}

/// One deadline covers the whole step, the log line and the row update
/// included, so a stalled write cannot keep the replicas from starting.
#[tokio::test]
async fn the_whole_advisory_step_is_bounded_by_one_deadline() {
    let job = job(FakeImageStore::with_user(None), Some(mock_db()), false);
    let context = WorkflowContext::new("run-42".to_string(), 42, 3, 4, Arc::new(StalledLog));
    let finished = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        job.check_and_record_image_user_within(
            &image_output(),
            &context,
            std::time::Duration::from_millis(100),
        ),
    )
    .await;
    assert!(finished.is_ok(), "the step outlived its deadline");
}

#[test]
fn the_origin_follows_what_built_the_image() {
    let context = WorkflowContext::new(
        "run-1".to_string(),
        1,
        1,
        1,
        Arc::new(CapturedLog::default()),
    );
    let built = job(FakeImageStore::with_user(None), None, false);
    assert_eq!(
        built.image_origin(&context),
        ImageOrigin::RepositoryDockerfile
    );
    let prebuilt = job(FakeImageStore::with_user(None), None, true);
    assert_eq!(prebuilt.image_origin(&context), ImageOrigin::PrebuiltImage);

    let mut context = context;
    context
        .set_output("build", DOCKERFILE_PRESET_OUTPUT, "go")
        .unwrap();
    assert_eq!(
        built.image_origin(&context),
        ImageOrigin::Preset("go".to_string())
    );
}
