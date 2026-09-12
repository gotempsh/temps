// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Local, workspace-owned process control. No host credentials or TCP listener.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use temps_agent_runtime::{
    journal::{EventJournalLimits, InMemoryEventJournal},
    lifecycle::{DeliveryState, RetryAdvice, RuntimeFailure, RuntimeFailureKind, RuntimeId},
    protocol_host::{RemoteRuntimeHost, RemoteRuntimeHostLimits},
    providers::Codex,
    retained::{
        DisposeOutcome, InProcessRuntimeClient, RetainedRuntimeLimits, RetainedRuntimeResult,
        RuntimeClient, RuntimeHandle, RuntimeSpec,
    },
    AgentRuntime,
};
use temps_agent_runtime::{
    ManagedProcessError, ManagedProcessId, ManagedProcessLogLine, ManagedProcessSnapshot,
    ManagedProcessSpec, ManagedProcessSupervisor, RestartPolicy,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::Mutex,
};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const AGENT_FRAME_READ_TIMEOUT: Duration = Duration::from_secs(60);
pub const AGENT_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const ISOLATED_MODEL_RELAY_ORIGIN: &str = "http://temps-sandbox-egress-proxy:3128";

fn isolated_codex_adapter() -> Result<Codex, Error> {
    Codex::default()
        .with_insecure_model_relay_origin(ISOLATED_MODEL_RELAY_ORIGIN)
        .map_err(|source| Error::RetainedConfiguration {
            detail: format!("configure isolated Codex model relay: {source}"),
        })
}

/// Read-only SDK protocol probe. No runtime is acquired or disposed.
pub async fn check_agent_runtime(socket: &Path) -> Result<(), Error> {
    use temps_agent_runtime::lifecycle::{RuntimeFailureKind, RuntimeId};
    use temps_agent_runtime::protocol::{
        decode_host_frame, encode_client_frame, ClientFrame, ClientRequest, HostFrame,
        MAX_PROTOCOL_FRAME_BYTES, PROTOCOL_VERSION_V4,
    };
    tokio::time::timeout(AGENT_CHECK_TIMEOUT, async {
        let mut stream = UnixStream::connect(socket)
            .await
            .map_err(|source| io("connect retained runtime socket", source))?;
        let probe_id = format!(
            "temps-runtime-compatibility-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|source| Error::RetainedProtocol {
                    detail: source.to_string()
                })?
                .as_nanos()
        );
        let runtime_id = RuntimeId::new(probe_id).map_err(|source| Error::RetainedProtocol {
            detail: source.to_string(),
        })?;
        let frame = ClientFrame {
            version: PROTOCOL_VERSION_V4,
            request_id: "temps-runtime-compatibility-probe".into(),
            request: ClientRequest::Health { runtime_id },
        };
        let bytes = encode_client_frame(&frame).map_err(|source| Error::RetainedProtocol {
            detail: source.to_string(),
        })?;
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|source| io("write retained runtime probe length", source))?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|source| io("write retained runtime probe", source))?;
        let mut prefix = [0u8; 4];
        stream
            .read_exact(&mut prefix)
            .await
            .map_err(|source| io("read retained runtime probe length", source))?;
        let size = u32::from_be_bytes(prefix) as usize;
        if size == 0 || size > MAX_PROTOCOL_FRAME_BYTES {
            return Err(Error::FrameLimit {
                direction: "retained runtime response",
                limit: MAX_PROTOCOL_FRAME_BYTES,
            });
        }
        let mut response = vec![0u8; size];
        stream
            .read_exact(&mut response)
            .await
            .map_err(|source| io("read retained runtime probe", source))?;
        match decode_host_frame(&response) {
            Ok(HostFrame::Failure {
                request_id,
                failure,
            }) if request_id == frame.request_id
                && failure.kind == RuntimeFailureKind::RuntimeNotFound =>
            {
                Ok(())
            }
            Ok(_) => Err(Error::RetainedProtocol {
                detail: "unexpected SDK health probe response".into(),
            }),
            Err(source) => Err(Error::RetainedProtocol {
                detail: source.to_string(),
            }),
        }
    })
    .await
    .map_err(|_| Error::Timeout {
        operation: "retained runtime compatibility probe",
    })?
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("runtime {operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid runtime request: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },
    #[error("runtime protocol {received} is unsupported; expected {PROTOCOL_VERSION}")]
    Version { received: u16 },
    #[error("runtime {direction} frame exceeds {limit} bytes")]
    FrameLimit {
        direction: &'static str,
        limit: usize,
    },
    #[error("runtime {operation} timed out")]
    Timeout { operation: &'static str },
    #[error("working directory must be an existing directory within workspace {root}")]
    Directory { root: PathBuf },
    #[error(transparent)]
    Process(#[from] ManagedProcessError),
    #[error("failed to configure retained agent runtime: {detail}")]
    RetainedConfiguration { detail: String },
    #[error("retained agent protocol connection failed: {detail}")]
    RetainedProtocol { detail: String },
    #[error("failed to dispose retained runtime {runtime_id} during daemon shutdown: {detail}")]
    RetainedShutdown {
        runtime_id: RuntimeId,
        detail: String,
    },
    #[error("retained harness recovery for backend epoch {epoch} failed: {detail}")]
    Recovery { epoch: u64, detail: String },
}

pub fn io(operation: &'static str, source: std::io::Error) -> Error {
    Error::Io { operation, source }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub operation: Operation,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    RecoverHarness {
        epoch: u64,
    },
    Execute {
        program: PathBuf,
        args: Vec<String>,
        directory: PathBuf,
        environment: std::collections::BTreeMap<String, String>,
    },
    ExecuteBounded {
        program: PathBuf,
        args: Vec<String>,
        directory: PathBuf,
        environment: std::collections::BTreeMap<String, String>,
        max_output_bytes: u64,
    },
    Health,
    List,
    Start {
        name: String,
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        directory: PathBuf,
        #[serde(default)]
        restart: bool,
    },
    StartUnique {
        idempotency_key: String,
        name: String,
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        directory: PathBuf,
        #[serde(default)]
        restart: bool,
    },
    Logs {
        id: ManagedProcessId,
    },
    Stop {
        id: ManagedProcessId,
    },
    Restart {
        id: ManagedProcessId,
    },
    Delete {
        id: ManagedProcessId,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Output {
        stream: String,
        data: String,
    },
    Exit {
        code: i32,
    },
    Health {
        version: u16,
        package_version: String,
        capabilities: Vec<String>,
    },
    Processes {
        processes: Vec<ManagedProcessSnapshot>,
    },
    Process {
        process: ManagedProcessSnapshot,
    },
    ProcessConflict {
        process: ManagedProcessSnapshot,
    },
    Logs {
        lines: Vec<ManagedProcessLogLine>,
    },
    Deleted,
    Error {
        code: String,
        detail: String,
    },
}

impl From<Error> for Response {
    fn from(error: Error) -> Self {
        let code = match &error {
            Error::Io { .. } => "io",
            Error::Json { .. } => "invalid_request",
            Error::Version { .. } => "incompatible_version",
            Error::FrameLimit { .. } => "frame_limit",
            Error::Timeout { .. } => "timeout",
            Error::Directory { .. } => "invalid_directory",
            Error::Process(_) => "process_error",
            Error::RetainedConfiguration { .. } => "retained_configuration",
            Error::RetainedProtocol { .. } => "retained_protocol",
            Error::RetainedShutdown { .. } => "retained_shutdown",
            Error::Recovery { .. } => "recovery",
        };
        Self::Error {
            code: code.into(),
            detail: error.to_string(),
        }
    }
}

pub struct Daemon {
    root: PathBuf,
    supervisor: ManagedProcessSupervisor,
    // Serialize mutations: two connections must not restart the same process concurrently.
    mutations: Mutex<()>,
    starts: Mutex<std::collections::HashMap<String, AtomicStartRecord>>,
    retained: Arc<WorkspaceRuntimeClient>,
    runtime_host: RemoteRuntimeHost,
    recovery_epoch: Mutex<Option<u64>>,
}

const MAX_RETAINED_RUNTIMES: usize = 32;

struct WorkspaceRuntimeClient {
    root: PathBuf,
    inner: InProcessRuntimeClient,
    runtime_ids: Mutex<std::collections::HashSet<RuntimeId>>,
}

impl WorkspaceRuntimeClient {
    async fn dispose_all(&self) -> Result<(), Error> {
        let ids = self
            .runtime_ids
            .lock()
            .await
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for runtime_id in ids {
            if let Err(source) = self.dispose(&runtime_id).await {
                first_error.get_or_insert_with(|| Error::RetainedShutdown {
                    runtime_id,
                    detail: source.to_string(),
                });
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn invalid_directory(&self, spec: &RuntimeSpec) -> RuntimeFailure {
        RuntimeFailure {
            runtime_id: Some(spec.runtime_id.clone()),
            invocation_id: None,
            kind: RuntimeFailureKind::InvalidRequest,
            retry: RetryAdvice::Never,
            delivery: DeliveryState::NotSent,
            message: format!(
                "runtime {} working directory must be an existing directory within workspace {}",
                spec.runtime_id,
                self.root.display()
            ),
            provider_code: None,
        }
    }
}

#[async_trait::async_trait]
impl RuntimeClient for WorkspaceRuntimeClient {
    async fn acquire(&self, mut spec: RuntimeSpec) -> RetainedRuntimeResult<RuntimeHandle> {
        let candidate = if spec.working_directory.is_absolute() {
            spec.working_directory.clone()
        } else {
            self.root.join(&spec.working_directory)
        };
        let directory = candidate
            .canonicalize()
            .map_err(|_| self.invalid_directory(&spec))?;
        if !directory.starts_with(&self.root) || !directory.is_dir() {
            return Err(self.invalid_directory(&spec));
        }
        spec.working_directory = directory;
        let runtime_id = spec.runtime_id.clone();
        let handle = self.inner.acquire(spec).await?;
        self.runtime_ids.lock().await.insert(runtime_id);
        Ok(handle)
    }

    async fn attach(&self, runtime_id: &RuntimeId) -> RetainedRuntimeResult<RuntimeHandle> {
        self.inner.attach(runtime_id).await
    }

    async fn dispose(&self, runtime_id: &RuntimeId) -> RetainedRuntimeResult<DisposeOutcome> {
        let outcome = self.inner.dispose(runtime_id).await?;
        self.runtime_ids.lock().await.remove(runtime_id);
        Ok(outcome)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AtomicStartSignature {
    name: String,
    program: PathBuf,
    args: Vec<String>,
    directory: PathBuf,
    restart: bool,
}

#[derive(Clone)]
struct AtomicStartRecord {
    signature: AtomicStartSignature,
    process_id: ManagedProcessId,
}

impl Daemon {
    pub fn new(root: &Path) -> Result<Self, Error> {
        let root = root
            .canonicalize()
            .map_err(|source| io("resolve workspace", source))?;
        if !root.is_dir() {
            return Err(Error::Directory { root });
        }
        let mut builder = AgentRuntime::builder();
        builder.register(isolated_codex_adapter()?);
        let runtime = builder
            .build()
            .map_err(|source| Error::RetainedConfiguration {
                detail: source.to_string(),
            })?;
        let inner = InProcessRuntimeClient::with_limits(
            runtime,
            RetainedRuntimeLimits {
                max_runtimes: MAX_RETAINED_RUNTIMES,
            },
        )
        .map_err(|source| Error::RetainedConfiguration {
            detail: source.to_string(),
        })?;
        let retained = Arc::new(WorkspaceRuntimeClient {
            root: root.clone(),
            inner,
            runtime_ids: Mutex::new(std::collections::HashSet::new()),
        });
        let journal = InMemoryEventJournal::new(EventJournalLimits {
            max_runtimes: MAX_RETAINED_RUNTIMES,
            max_invocations_per_runtime: 128,
            max_events_per_invocation: 4096,
            max_replay_events: 2048,
        })
        .map_err(|source| Error::RetainedConfiguration {
            detail: source.to_string(),
        })?;
        let client: Arc<dyn RuntimeClient> = retained.clone();
        let runtime_host = RemoteRuntimeHost::with_limits(
            client,
            Arc::new(journal),
            RemoteRuntimeHostLimits {
                completed_request_capacity: 1024,
                pending_request_capacity: 64,
            },
        )
        .map_err(|source| Error::RetainedConfiguration {
            detail: source.to_string(),
        })?;
        Ok(Self {
            root,
            supervisor: ManagedProcessSupervisor::builder()
                .max_processes(32)
                .max_log_lines(256)
                .max_log_line_chars(2048)
                .build()?,
            mutations: Mutex::new(()),
            starts: Mutex::new(std::collections::HashMap::new()),
            retained,
            runtime_host,
            recovery_epoch: Mutex::new(None),
        })
    }

    pub async fn execute(&self, request: Request) -> Result<Response, Error> {
        if request.version != PROTOCOL_VERSION {
            return Err(Error::Version {
                received: request.version,
            });
        }
        match request.operation {
            Operation::RecoverHarness { epoch } => {
                let mut completed = self.recovery_epoch.lock().await;
                if completed.is_some_and(|previous| epoch < previous) {
                    return Err(Error::Recovery {
                        epoch,
                        detail: "an older backend epoch cannot reclaim this sandbox".into(),
                    });
                }
                if *completed != Some(epoch) {
                    self.runtime_host
                        .dispose_all_runtimes()
                        .await
                        .map_err(|source| Error::Recovery {
                            epoch,
                            detail: format!("provider termination was not confirmed: {source}"),
                        })?;
                    *completed = Some(epoch);
                }
                Ok(Response::Deleted)
            }
            Operation::Execute { .. } | Operation::ExecuteBounded { .. } => Err(io(
                "execute requires streaming connection",
                std::io::Error::from(std::io::ErrorKind::InvalidInput),
            )),
            Operation::Health => Ok(Response::Health {
                version: PROTOCOL_VERSION,
                package_version: env!("CARGO_PKG_VERSION").into(),
                capabilities: vec![
                    "managed_processes".into(),
                    "bounded_logs".into(),
                    "bounded_execute".into(),
                    "atomic_process_start".into(),
                    "retained_runtime".into(),
                    "recover_harness".into(),
                ],
            }),
            Operation::List => Ok(Response::Processes {
                processes: self.supervisor.list().await,
            }),
            Operation::Logs { id } => Ok(Response::Logs {
                lines: self.supervisor.logs(&id).await?,
            }),
            Operation::Start {
                name,
                program,
                args,
                directory,
                restart,
            } => {
                let _guard = self.mutations.lock().await;
                let directory =
                    self.root
                        .join(directory)
                        .canonicalize()
                        .map_err(|_| Error::Directory {
                            root: self.root.clone(),
                        })?;
                if !directory.starts_with(&self.root) || !directory.is_dir() {
                    return Err(Error::Directory {
                        root: self.root.clone(),
                    });
                }
                let spec = ManagedProcessSpec::background(name, program, directory)
                    .args(args)
                    .restart_policy(if restart {
                        RestartPolicy::OnFailure
                    } else {
                        RestartPolicy::Never
                    });
                let handle = self.supervisor.start(spec).await?;
                Ok(Response::Process {
                    process: handle.snapshot().await?,
                })
            }
            Operation::StartUnique {
                idempotency_key,
                name,
                program,
                args,
                directory,
                restart,
            } => {
                let _guard = self.mutations.lock().await;
                if idempotency_key.is_empty()
                    || idempotency_key.len() > 200
                    || !idempotency_key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                {
                    return Err(Error::Process(ManagedProcessError::InvalidSpec {
                        field: "idempotency_key",
                        message:
                            "must contain 1-200 ASCII letters, digits, hyphens, or underscores"
                                .to_string(),
                    }));
                }
                let signature = AtomicStartSignature {
                    name: name.clone(),
                    program: program.clone(),
                    args: args.clone(),
                    directory: directory.clone(),
                    restart,
                };
                if let Some(record) = self.starts.lock().await.get(&idempotency_key).cloned() {
                    if record.signature != signature {
                        return Err(Error::Process(ManagedProcessError::InvalidSpec {
                            field: "idempotency_key",
                            message: "was already used with different process arguments"
                                .to_string(),
                        }));
                    }
                    return Ok(Response::Process {
                        process: self.supervisor.snapshot(&record.process_id).await?,
                    });
                }
                if let Some(existing) = self
                    .supervisor
                    .list()
                    .await
                    .into_iter()
                    .find(|process| process.name == name)
                {
                    return Ok(Response::ProcessConflict { process: existing });
                }
                let directory =
                    self.root
                        .join(directory)
                        .canonicalize()
                        .map_err(|_| Error::Directory {
                            root: self.root.clone(),
                        })?;
                if !directory.starts_with(&self.root) || !directory.is_dir() {
                    return Err(Error::Directory {
                        root: self.root.clone(),
                    });
                }
                let spec = ManagedProcessSpec::background(name, program, directory)
                    .args(args)
                    .restart_policy(if restart {
                        RestartPolicy::OnFailure
                    } else {
                        RestartPolicy::Never
                    });
                let handle = self.supervisor.start(spec).await?;
                let process = handle.snapshot().await?;
                self.starts.lock().await.insert(
                    idempotency_key,
                    AtomicStartRecord {
                        signature,
                        process_id: process.id.clone(),
                    },
                );
                Ok(Response::Process { process })
            }
            Operation::Stop { id } => {
                let _guard = self.mutations.lock().await;
                Ok(Response::Process {
                    process: self.supervisor.stop(&id).await?,
                })
            }
            Operation::Restart { id } => {
                let _guard = self.mutations.lock().await;
                Ok(Response::Process {
                    process: self.supervisor.restart(&id).await?,
                })
            }
            Operation::Delete { id } => {
                let _guard = self.mutations.lock().await;
                self.supervisor.delete(&id).await?;
                self.starts
                    .lock()
                    .await
                    .retain(|_, record| record.process_id != id);
                Ok(Response::Deleted)
            }
        }
    }

    pub async fn shutdown(&self) -> Result<(), Error> {
        self.retained.dispose_all().await?;
        let _guard = self.mutations.lock().await;
        for process in self.supervisor.list().await {
            self.supervisor.stop(&process.id).await?;
        }
        Ok(())
    }
}

/// Serves SDK retained-runtime frames on an already peer-authenticated private stream.
pub async fn serve_agent_connection(stream: UnixStream, daemon: Arc<Daemon>) -> Result<(), Error> {
    serve_agent_connection_with_read_timeout(stream, daemon, AGENT_FRAME_READ_TIMEOUT).await
}

async fn serve_agent_connection_with_read_timeout(
    stream: UnixStream,
    daemon: Arc<Daemon>,
    read_timeout: Duration,
) -> Result<(), Error> {
    let (reader, writer) = stream.into_split();
    temps_agent_runtime::protocol_stream::serve_authenticated_stream_with_read_timeout(
        &daemon.runtime_host,
        reader,
        writer,
        read_timeout,
    )
    .await
    .map_err(|source| Error::RetainedProtocol {
        detail: source.to_string(),
    })
}

/// Transparently forwards stdin/stdout to the private retained-runtime socket.
pub async fn connect_client(socket: &Path) -> Result<(), Error> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|source| io("connect retained runtime socket", source))?;
    let (mut reader, mut writer) = stream.into_split();
    let upload = tokio::spawn(async move {
        tokio::io::copy(&mut tokio::io::stdin(), &mut writer)
            .await
            .map_err(|source| io("forward retained runtime input", source))
    });
    let download = tokio::spawn(async move {
        tokio::io::copy(&mut reader, &mut tokio::io::stdout())
            .await
            .map_err(|source| io("forward retained runtime output", source))
    });
    upload.await.map_err(|source| {
        io(
            "join retained runtime input forwarder",
            std::io::Error::other(source),
        )
    })??;
    download.await.map_err(|source| {
        io(
            "join retained runtime output forwarder",
            std::io::Error::other(source),
        )
    })??;
    Ok(())
}

async fn frame<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    limit: usize,
    direction: &'static str,
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        BufReader::new(reader.take((limit + 1) as u64)).read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(|_| Error::Timeout {
        operation: "read frame",
    })?
    .map_err(|source| io("read frame", source))?;
    if bytes.len() > limit {
        return Err(Error::FrameLimit { direction, limit });
    }
    Ok(bytes)
}

/// One request per connection. Disconnecting never terminates a managed process.
pub async fn serve_connection(mut stream: UnixStream, daemon: Arc<Daemon>) -> Result<(), Error> {
    let peer_uid = stream
        .peer_cred()
        .map_err(|source| io("authenticate control request peer", source))?
        .uid();
    let response = match frame(&mut stream, MAX_REQUEST_BYTES, "request").await {
        Ok(bytes) => match serde_json::from_slice::<Request>(&bytes) {
            Ok(mut request) => {
                if let Operation::RecoverHarness { epoch } = request.operation {
                    if peer_uid != 0 {
                        let response: Response = Error::Recovery {
                            epoch,
                            detail: "recovery requires a host-authorized root connection".into(),
                        }
                        .into();
                        let bytes = serde_json::to_vec(&response)?;
                        stream
                            .write_all(&bytes)
                            .await
                            .map_err(|source| io("send unauthorized recovery response", source))?;
                        stream.write_all(b"\n").await.map_err(|source| {
                            io("finish unauthorized recovery response", source)
                        })?;
                        return Ok(());
                    }
                }
                if request.version == PROTOCOL_VERSION {
                    let operation = std::mem::replace(&mut request.operation, Operation::Health);
                    let execution = match operation {
                        Operation::Execute {
                            program,
                            args,
                            directory,
                            environment,
                        } => Some((program, args, directory, environment, None)),
                        Operation::ExecuteBounded {
                            program,
                            args,
                            directory,
                            environment,
                            max_output_bytes,
                        } => Some((
                            program,
                            args,
                            directory,
                            environment,
                            Some(max_output_bytes),
                        )),
                        operation => {
                            request.operation = operation;
                            None
                        }
                    };
                    if let Some((program, args, directory, environment, max_output_bytes)) =
                        execution
                    {
                        let result = execute_stream(
                            &mut stream,
                            &daemon,
                            program,
                            args,
                            directory,
                            environment,
                            max_output_bytes,
                        )
                        .await;
                        return match result {
                            Ok(()) => Ok(()),
                            Err(error) => write_response(&mut stream, &Response::from(error)).await,
                        };
                    }
                }
                daemon.execute(request).await.unwrap_or_else(Response::from)
            }
            Err(source) => Response::from(Error::Json { source }),
        },
        Err(error) => Response::from(error),
    };
    let mut bytes = serde_json::to_vec(&response)?;
    if bytes.len() >= MAX_RESPONSE_BYTES {
        return Err(Error::FrameLimit {
            direction: "response",
            limit: MAX_RESPONSE_BYTES,
        });
    }
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(10), stream.write_all(&bytes))
        .await
        .map_err(|_| Error::Timeout {
            operation: "write response",
        })?
        .map_err(|source| io("write response", source))
}

async fn execute_stream(
    stream: &mut UnixStream,
    daemon: &Daemon,
    program: PathBuf,
    args: Vec<String>,
    directory: PathBuf,
    environment: std::collections::BTreeMap<String, String>,
    max_output_bytes: Option<u64>,
) -> Result<(), Error> {
    use base64::Engine;
    use temps_agent_runtime::{
        CommandSpec, ExecutionTransport, LocalTransport, TransportSpawnRequest,
    };
    let directory = daemon
        .root
        .join(directory)
        .canonicalize()
        .map_err(|_| Error::Directory {
            root: daemon.root.clone(),
        })?;
    if !directory.starts_with(&daemon.root) {
        return Err(Error::Directory {
            root: daemon.root.clone(),
        });
    }
    let mut child = LocalTransport
        .spawn(TransportSpawnRequest {
            working_directory: directory,
            command: CommandSpec {
                program,
                args: args.into_iter().map(Into::into).collect(),
                environment: environment
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
                clear_environment: true,
                initial_stdin: None,
                interactive_stdin: false,
            },
        })
        .await
        .map_err(|source| io("spawn runtime command", std::io::Error::other(source)))?;
    // Execute has no stdin channel. Close the transport's piped writer so
    // harnesses which read stdin before processing their prompt receive EOF.
    drop(child.take_stdin());
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let mut readers = tokio::task::JoinSet::new();
    for (name, reader) in [
        ("stdout", child.take_stdout()),
        ("stderr", child.take_stderr()),
    ] {
        if let Some(mut reader) = reader {
            let tx = tx.clone();
            readers.spawn(async move {
                let mut buf = [0; 8192];
                loop {
                    let count = reader.read(&mut buf).await?;
                    if count == 0 {
                        break;
                    }
                    if tx.send((name, buf[..count].to_vec())).await.is_err() {
                        break;
                    }
                }
                Ok::<_, std::io::Error>(())
            });
        }
    }
    drop(tx);
    let (mut input, mut output) = stream.split();
    let mut disconnected = [0u8; 1];
    let mut output_bytes = 0u64;
    let exit = loop {
        tokio::select! {
            _ = input.read(&mut disconnected) => {
                child.terminate().await.map_err(|source| io("cancel runtime command", std::io::Error::other(source)))?;
                tokio::time::timeout(Duration::from_secs(5), child.wait()).await
                    .map_err(|_| Error::Timeout { operation: "reap cancelled command" })?
                    .map_err(|source| io("reap cancelled command", std::io::Error::other(source)))?;
                return Ok(());
            },
            status = child.wait() => {
                let status = status.map_err(|source| io("wait runtime command", std::io::Error::other(source)))?;
                child.disarm();
                break status.code.unwrap_or(1);
            },
            Some((stream_name, data)) = rx.recv() => {
                output_bytes = output_bytes.saturating_add(data.len() as u64);
                if let Some(limit) = max_output_bytes.filter(|limit| output_bytes > *limit) {
                    child.terminate().await.map_err(|source| io("limit runtime command output", std::io::Error::other(source)))?;
                    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                    return Err(Error::FrameLimit { direction: "command output", limit: limit as usize });
                }
                let response = Response::Output {
                    stream: stream_name.into(),
                    data: base64::engine::general_purpose::STANDARD.encode(data),
                };
                write_response(&mut output, &response).await?
            },
        }
    };
    // Detached descendants can retain pipes; don't let them hold the turn open.
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some((stream_name, data)) = rx.recv().await {
            output_bytes = output_bytes.saturating_add(data.len() as u64);
            if max_output_bytes.is_some_and(|limit| output_bytes > limit) {
                return Err(Error::FrameLimit {
                    direction: "command output",
                    limit: max_output_bytes.unwrap_or_default() as usize,
                });
            }
            let response = Response::Output {
                stream: stream_name.into(),
                data: base64::engine::general_purpose::STANDARD.encode(data),
            };
            write_response(&mut output, &response).await?;
        }
        Ok::<_, Error>(())
    })
    .await;
    readers.abort_all();
    write_response(&mut output, &Response::Exit { code: exit }).await
}

async fn write_response<W: tokio::io::AsyncWrite + Unpin>(
    output: &mut W,
    response: &Response,
) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(10), output.write_all(&bytes))
        .await
        .map_err(|_| Error::Timeout {
            operation: "stream output",
        })?
        .map_err(|source| io("stream output", source))
}

/// Transparent streaming client used by the Temps Docker provider.
pub async fn exec_client(socket: &Path, program: PathBuf, args: Vec<String>) -> Result<i32, Error> {
    exec_client_inner(socket, program, args, None).await
}

pub async fn exec_client_bounded(
    socket: &Path,
    program: PathBuf,
    args: Vec<String>,
    max_output_bytes: u64,
) -> Result<i32, Error> {
    if max_output_bytes == 0 {
        return Err(io(
            "validate bounded command output limit",
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
        ));
    }
    exec_client_inner(socket, program, args, Some(max_output_bytes)).await
}

async fn exec_client_inner(
    socket: &Path,
    program: PathBuf,
    args: Vec<String>,
    max_output_bytes: Option<u64>,
) -> Result<i32, Error> {
    use base64::Engine;
    let request = Request {
        version: PROTOCOL_VERSION,
        operation: match max_output_bytes {
            Some(max_output_bytes) => Operation::ExecuteBounded {
                program,
                args,
                directory: std::env::current_dir()
                    .map_err(|source| io("get command directory", source))?,
                environment: std::env::vars().collect(),
                max_output_bytes,
            },
            None => Operation::Execute {
                program,
                args,
                directory: std::env::current_dir()
                    .map_err(|source| io("get command directory", source))?,
                environment: std::env::vars().collect(),
            },
        },
    };
    let mut bytes = serde_json::to_vec(&request)?;
    if bytes.len() >= MAX_REQUEST_BYTES {
        return Err(Error::FrameLimit {
            direction: "request",
            limit: MAX_REQUEST_BYTES,
        });
    }
    bytes.push(b'\n');
    let mut socket = UnixStream::connect(socket)
        .await
        .map_err(|source| io("connect runtime", source))?;
    socket
        .write_all(&bytes)
        .await
        .map_err(|source| io("send command", source))?;
    let mut reader = BufReader::new(socket);
    loop {
        let mut bytes = Vec::new();
        (&mut reader)
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|source| io("receive command output", source))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(Error::FrameLimit {
                direction: "response",
                limit: MAX_RESPONSE_BYTES,
            });
        }
        if bytes.is_empty() {
            return Err(io(
                "runtime disconnected before command completed",
                std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
            ));
        }
        match serde_json::from_slice::<Response>(&bytes)? {
            Response::Output { stream, data } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|source| io("decode command output", std::io::Error::other(source)))?;
                use std::io::Write;
                if stream == "stdout" {
                    std::io::stdout().write_all(&data)
                } else {
                    std::io::stderr().write_all(&data)
                }
                .map_err(|source| io("forward command output", source))?;
            }
            Response::Exit { code } => return Ok(code),
            Response::Error { detail, .. } => {
                return Err(io("execute command", std::io::Error::other(detail)))
            }
            _ => {
                return Err(io(
                    "unexpected runtime frame",
                    std::io::Error::from(std::io::ErrorKind::InvalidData),
                ))
            }
        }
    }
}

pub async fn request(socket: &Path, bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if bytes.len() >= MAX_REQUEST_BYTES {
        return Err(Error::FrameLimit {
            direction: "request",
            limit: MAX_REQUEST_BYTES,
        });
    }
    let _: Request = serde_json::from_slice(bytes)?;
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|source| io("connect socket", source))?;
    stream
        .write_all(bytes)
        .await
        .map_err(|source| io("send request", source))?;
    stream
        .write_all(b"\n")
        .await
        .map_err(|source| io("finish request", source))?;
    frame(&mut stream, MAX_RESPONSE_BYTES, "response").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_agent_runtime::{AgentAdapter, Provider, SecretString, TurnRequest};

    #[test]
    fn daemon_codex_adapter_accepts_only_the_isolated_relay_origin() {
        let adapter = isolated_codex_adapter().unwrap();
        for (url, allowed) in [
            (
                "http://temps-sandbox-egress-proxy:3128/.temps/model-relay",
                true,
            ),
            (
                "http://temps-sandbox-egress-proxy:3129/.temps/model-relay",
                false,
            ),
            ("http://other-proxy:3128/.temps/model-relay", false),
        ] {
            let mut request = TurnRequest::new(Provider::Codex, ".", "inspect");
            request
                .environment
                .insert("RELAY_TOKEN".into(), SecretString::new("test-token"));
            request.harness_options.insert(
                "model_relay".into(),
                serde_json::json!({
                    "base_url": url, "token_env": "RELAY_TOKEN"
                })
                .to_string(),
            );
            assert_eq!(adapter.command(&request).is_ok(), allowed, "{url}");
        }
    }
    fn req(operation: Operation) -> Request {
        Request {
            version: 1,
            operation,
        }
    }

    #[tokio::test]
    async fn non_root_peer_cannot_recover_or_poison_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new(dir.path()).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        if client.peer_cred().unwrap().uid() == 0 {
            return;
        }
        let owner = Arc::clone(&daemon);
        let task = tokio::spawn(async move { serve_connection(server, owner).await });
        let bytes = serde_json::to_vec(&req(Operation::RecoverHarness { epoch: 42 })).unwrap();
        client.write_all(&bytes).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        let reply = frame(&mut client, MAX_RESPONSE_BYTES, "response")
            .await
            .unwrap();
        assert!(
            matches!(serde_json::from_slice::<Response>(&reply).unwrap(), Response::Error { code, .. } if code == "recovery"),
            "{}",
            String::from_utf8_lossy(&reply)
        );
        task.await.unwrap().unwrap();
        assert_eq!(*daemon.recovery_epoch.lock().await, None);
    }

    #[tokio::test]
    async fn disconnect_does_not_stop_service_and_controls_work() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new(dir.path()).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let owner = daemon.clone();
        let task = tokio::spawn(async move { serve_connection(server, owner).await });
        let bytes = serde_json::to_vec(&req(Operation::Start {
            name: "worker".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo ready; exec sleep 60".into()],
            directory: ".".into(),
            restart: false,
        }))
        .unwrap();
        client.write_all(&bytes).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        let result = frame(&mut client, MAX_RESPONSE_BYTES, "response")
            .await
            .unwrap();
        let Response::Process { process } = serde_json::from_slice(&result).unwrap() else {
            panic!("expected process")
        };
        drop(client);
        task.await.unwrap().unwrap();
        assert!(matches!(
            daemon
                .supervisor
                .snapshot(&process.id)
                .await
                .unwrap()
                .status,
            temps_agent_runtime::ManagedProcessStatus::Running
        ));
        let restarted = daemon
            .execute(req(Operation::Restart {
                id: process.id.clone(),
            }))
            .await
            .unwrap();
        assert!(matches!(
            restarted,
            Response::Process {
                process: ManagedProcessSnapshot {
                    restart_count: 1,
                    ..
                }
            }
        ));
        daemon
            .execute(req(Operation::Stop {
                id: process.id.clone(),
            }))
            .await
            .unwrap();
        assert!(matches!(
            daemon
                .supervisor
                .snapshot(&process.id)
                .await
                .unwrap()
                .status,
            temps_agent_runtime::ManagedProcessStatus::Cancelled
        ));
        daemon
            .execute(req(Operation::Delete { id: process.id }))
            .await
            .unwrap();
        assert!(daemon.supervisor.list().await.is_empty());
    }

    #[tokio::test]
    async fn start_unique_is_atomic_idempotent_and_conflicts_on_name_or_signature() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(dir.path()).unwrap();
        let start = || Operation::StartUnique {
            idempotency_key: "tmcp_start_1".into(),
            name: "web".into(),
            program: "/bin/sleep".into(),
            args: vec!["60".into()],
            directory: ".".into(),
            restart: false,
        };
        let Response::Process { process: first } = daemon.execute(req(start())).await.unwrap()
        else {
            panic!("expected initial process")
        };
        let Response::Process { process: replay } = daemon.execute(req(start())).await.unwrap()
        else {
            panic!("expected idempotent process replay")
        };
        assert_eq!(first.id, replay.id);
        assert_eq!(daemon.supervisor.list().await.len(), 1);

        let changed = daemon
            .execute(req(Operation::StartUnique {
                idempotency_key: "tmcp_start_1".into(),
                name: "web".into(),
                program: "/bin/false".into(),
                args: vec![],
                directory: ".".into(),
                restart: false,
            }))
            .await;
        assert!(matches!(
            changed,
            Err(Error::Process(ManagedProcessError::InvalidSpec {
                field: "idempotency_key",
                ..
            }))
        ));

        let conflict = daemon
            .execute(req(Operation::StartUnique {
                idempotency_key: "tmcp_start_2".into(),
                name: "web".into(),
                program: "/bin/sleep".into(),
                args: vec!["30".into()],
                directory: ".".into(),
                restart: false,
            }))
            .await
            .unwrap();
        assert!(
            matches!(conflict, Response::ProcessConflict { process } if process.id == first.id)
        );
        daemon
            .execute(req(Operation::Stop {
                id: first.id.clone(),
            }))
            .await
            .unwrap();
        daemon
            .execute(req(Operation::Delete { id: first.id }))
            .await
            .unwrap();
        assert!(daemon.starts.lock().await.is_empty());
    }

    #[tokio::test]
    async fn concurrent_start_unique_requests_cannot_duplicate_a_process() {
        fn start(key: &str) -> Request {
            req(Operation::StartUnique {
                idempotency_key: key.to_string(),
                name: "web".into(),
                program: "/bin/sleep".into(),
                args: vec!["60".into()],
                directory: ".".into(),
                restart: false,
            })
        }

        let dir = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new(dir.path()).unwrap());
        let (first, replay) = tokio::join!(
            daemon.execute(start("tmcp_same")),
            daemon.execute(start("tmcp_same"))
        );
        let Response::Process { process: first } = first.unwrap() else {
            panic!("expected first process")
        };
        let Response::Process { process: replay } = replay.unwrap() else {
            panic!("expected idempotent replay")
        };
        assert_eq!(first.id, replay.id);
        assert_eq!(daemon.supervisor.list().await.len(), 1);
        daemon
            .execute(req(Operation::Stop {
                id: first.id.clone(),
            }))
            .await
            .unwrap();

        let other_dir = tempfile::tempdir().unwrap();
        let other = Arc::new(Daemon::new(other_dir.path()).unwrap());
        let (left, right) = tokio::join!(
            other.execute(start("tmcp_left")),
            other.execute(start("tmcp_right"))
        );
        let responses = [left.unwrap(), right.unwrap()];
        assert_eq!(
            responses
                .iter()
                .filter(|response| matches!(response, Response::Process { .. }))
                .count(),
            1
        );
        assert_eq!(
            responses
                .iter()
                .filter(|response| matches!(response, Response::ProcessConflict { .. }))
                .count(),
            1
        );
        assert_eq!(other.supervisor.list().await.len(), 1);
        let process = other.supervisor.list().await.remove(0);
        other
            .execute(req(Operation::Stop { id: process.id }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejects_version_and_directory_escape() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(dir.path()).unwrap();
        assert!(matches!(
            daemon
                .execute(Request {
                    version: 99,
                    operation: Operation::Health
                })
                .await,
            Err(Error::Version { .. })
        ));
        for directory in [
            PathBuf::from(".."),
            PathBuf::from("/"),
            PathBuf::from("missing"),
        ] {
            assert!(matches!(
                daemon
                    .execute(req(Operation::Start {
                        name: "bad".into(),
                        program: "sleep".into(),
                        args: vec![],
                        directory,
                        restart: false
                    }))
                    .await,
                Err(Error::Directory { .. })
            ));
        }
    }

    #[tokio::test]
    async fn oversized_request_is_bounded() {
        let bytes = vec![b'x'; MAX_REQUEST_BYTES + 2];
        assert!(matches!(
            frame(&mut bytes.as_slice(), MAX_REQUEST_BYTES, "request").await,
            Err(Error::FrameLimit { .. })
        ));
    }

    #[tokio::test]
    async fn spawn_failure_is_contextual_and_does_not_retain_record() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(dir.path()).unwrap();
        let error = daemon
            .execute(req(Operation::Start {
                name: "missing executable".into(),
                program: dir.path().join("nonexistent"),
                args: vec![],
                directory: ".".into(),
                restart: false,
            }))
            .await
            .err()
            .unwrap();
        assert!(matches!(error, Error::Process(_)));
        assert!(error.to_string().contains("missing executable"));
        assert!(daemon.supervisor.list().await.is_empty());
    }

    #[tokio::test]
    async fn symlink_outside_workspace_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let daemon = Daemon::new(dir.path()).unwrap();
        assert!(matches!(
            daemon
                .execute(req(Operation::Start {
                    name: "escape".into(),
                    program: "pwd".into(),
                    args: vec![],
                    directory: "escape".into(),
                    restart: false,
                }))
                .await,
            Err(Error::Directory { .. })
        ));
    }

    #[tokio::test]
    async fn shutdown_stops_running_processes() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(dir.path()).unwrap();
        let Response::Process { process } = daemon
            .execute(req(Operation::Start {
                name: "owned".into(),
                program: "/bin/sleep".into(),
                args: vec!["60".into()],
                directory: ".".into(),
                restart: false,
            }))
            .await
            .unwrap()
        else {
            panic!("expected process")
        };
        daemon.shutdown().await.unwrap();
        assert!(matches!(
            daemon
                .supervisor
                .snapshot(&process.id)
                .await
                .unwrap()
                .status,
            temps_agent_runtime::ManagedProcessStatus::Cancelled
        ));
    }

    #[tokio::test]
    async fn harness_recovery_preserves_managed_process_and_rejects_stale_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(dir.path()).unwrap();
        let Response::Process { process } = daemon
            .execute(req(Operation::Start {
                name: "application-server".into(),
                program: "/bin/sleep".into(),
                args: vec!["60".into()],
                directory: ".".into(),
                restart: false,
            }))
            .await
            .unwrap()
        else {
            panic!("expected managed process")
        };
        for epoch in [42, 42, 43] {
            assert!(matches!(
                daemon
                    .execute(req(Operation::RecoverHarness { epoch }))
                    .await,
                Ok(Response::Deleted)
            ));
            assert_eq!(
                daemon
                    .supervisor
                    .snapshot(&process.id)
                    .await
                    .unwrap()
                    .status,
                temps_agent_runtime::ManagedProcessStatus::Running
            );
        }
        assert!(matches!(
            daemon
                .execute(req(Operation::RecoverHarness { epoch: 41 }))
                .await,
            Err(Error::Recovery { .. })
        ));
        assert_eq!(
            daemon
                .supervisor
                .snapshot(&process.id)
                .await
                .unwrap()
                .status,
            temps_agent_runtime::ManagedProcessStatus::Running
        );
        daemon.supervisor.stop(&process.id).await.unwrap();
    }

    #[tokio::test]
    async fn execute_failure_reaches_client() {
        let dir = tempfile::tempdir().unwrap();
        for (program, directory, expected) in [
            (
                dir.path().join("missing"),
                dir.path().to_path_buf(),
                "spawn runtime command",
            ),
            (
                PathBuf::from("/bin/sh"),
                PathBuf::from("/"),
                "working directory",
            ),
        ] {
            let daemon = Arc::new(Daemon::new(dir.path()).unwrap());
            let (mut client, server) = UnixStream::pair().unwrap();
            let task = tokio::spawn(serve_connection(server, daemon));
            let request = req(Operation::Execute {
                program,
                directory,
                args: vec![],
                environment: Default::default(),
            });
            client
                .write_all(&serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            client.write_all(b"\n").await.unwrap();
            let response = frame(&mut client, MAX_RESPONSE_BYTES, "response")
                .await
                .unwrap();
            let Response::Error { detail, .. } = serde_json::from_slice(&response).unwrap() else {
                panic!("expected contextual error");
            };
            assert!(detail.contains(expected), "{detail}");
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn execute_disconnect_terminates_foreground_process() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new(dir.path()).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_connection(server, daemon));
        let request = req(Operation::Execute {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "echo $$ > pid; echo ready; exec sleep 60".into(),
            ],
            directory: dir.path().into(),
            environment: Default::default(),
        });
        client
            .write_all(&serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        client.write_all(b"\n").await.unwrap();
        frame(&mut client, MAX_RESPONSE_BYTES, "response")
            .await
            .unwrap();
        let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap();
        drop(client);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .output()
            .unwrap();
        assert!(
            !alive.status.success(),
            "foreground process survived disconnect"
        );
    }

    #[tokio::test]
    async fn bounded_execute_terminates_output_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new(dir.path()).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_connection(server, daemon));
        let request = req(Operation::ExecuteBounded {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "head -c 700 /dev/zero; head -c 700 /dev/zero >&2; exec sleep 60".into(),
            ],
            directory: dir.path().into(),
            environment: Default::default(),
            max_output_bytes: 1024,
        });
        client
            .write_all(&serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        client.write_all(b"\n").await.unwrap();
        let mut saw_limit = false;
        for _ in 0..10 {
            let response = frame(&mut client, MAX_RESPONSE_BYTES, "response")
                .await
                .unwrap();
            if let Response::Error { detail, .. } = serde_json::from_slice(&response).unwrap() {
                saw_limit = detail.contains("command output") && detail.contains("1024");
                break;
            }
        }
        assert!(
            saw_limit,
            "bounded execution did not report its output limit"
        );
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn streaming_exec_preserves_output_and_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new(dir.path()).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_connection(server, daemon));
        let request = req(Operation::Execute {
            program: "/bin/sh".into(),
            // Reading to EOF must complete even while the client socket stays
            // connected; otherwise real Codex invocations wait indefinitely.
            args: vec![
                "-c".into(),
                "cat >/dev/null; printf out; printf err >&2; exit 7".into(),
            ],
            directory: dir.path().into(),
            environment: Default::default(),
        });
        client
            .write_all(&serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        client.write_all(b"\n").await.unwrap();
        let mut reader = BufReader::new(client);
        let mut output = Vec::new();
        let mut saw_exit = false;
        loop {
            let mut line = String::new();
            if tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
                .await
                .expect("non-interactive command did not receive stdin EOF")
                .unwrap()
                == 0
            {
                break;
            }
            match serde_json::from_str::<Response>(&line).unwrap() {
                Response::Output { stream, data } => output.push((stream, data)),
                Response::Exit { code } => {
                    assert_eq!(code, 7);
                    saw_exit = true;
                }
                _ => panic!("unexpected response"),
            }
        }
        assert!(saw_exit);
        assert!(output.contains(&("stdout".into(), "b3V0".into())));
        assert!(output.contains(&("stderr".into(), "ZXJy".into())));
        task.await.unwrap().unwrap();
    }

    struct SocketConnector(PathBuf);

    #[tokio::test]
    async fn compatibility_probe_requires_sdk_health_response() {
        let workspace = tempfile::tempdir().unwrap();
        let socket = workspace.path().join("agent.sock");
        let daemon = Arc::new(Daemon::new(workspace.path()).unwrap());
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server_daemon = daemon.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve_agent_connection(stream, server_daemon).await.unwrap();
        });
        check_agent_runtime(&socket).await.unwrap();
        server.await.unwrap();
        daemon.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn compatibility_probe_rejects_socket_without_sdk_response() {
        let workspace = tempfile::tempdir().unwrap();
        let socket = workspace.path().join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = stream.write_all(b"old runtime").await;
        });
        assert!(check_agent_runtime(&socket).await.is_err());
        server.await.unwrap();
    }

    #[async_trait::async_trait]
    impl temps_agent_runtime::protocol_client::RemoteRuntimeConnector for SocketConnector {
        async fn connect(
            &self,
        ) -> Result<
            Arc<dyn temps_agent_runtime::protocol_client::RemoteRuntimeConnection>,
            temps_agent_runtime::protocol_client::RemoteTransportError,
        > {
            let stream = UnixStream::connect(&self.0).await.map_err(|_| {
                temps_agent_runtime::protocol_client::RemoteTransportError::Disconnected {
                    message: "private retained runtime socket unavailable".into(),
                }
            })?;
            let (reader, writer) = stream.into_split();
            Ok(Arc::new(
                temps_agent_runtime::protocol_stream::AuthenticatedStreamConnection::new(
                    reader, writer,
                ),
            ))
        }
    }

    #[tokio::test]
    async fn private_socket_retains_runtime_across_client_connections() {
        use temps_agent_runtime::{
            lifecycle::RuntimeId,
            protocol_client::RemoteRuntimeClient,
            retained::{RuntimeClient, RuntimeSpec},
            Provider,
        };

        let workspace = tempfile::tempdir().unwrap();
        // Keep the path below Unix sockaddr's small fixed path limit.
        let socket_dir = tempfile::tempdir_in("/tmp").unwrap();
        let socket = socket_dir.path().join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let daemon = Arc::new(Daemon::new(workspace.path()).unwrap());
        let server_daemon = daemon.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let daemon = server_daemon.clone();
                tokio::spawn(async move { serve_agent_connection(stream, daemon).await });
            }
        });
        let connector = Arc::new(SocketConnector(socket));
        let runtime_id = RuntimeId::new("daemon-retained-runtime").unwrap();
        let first = RemoteRuntimeClient::new(connector.clone());
        let handle = first
            .acquire(RuntimeSpec::new(
                runtime_id.clone(),
                Provider::Codex,
                workspace.path(),
            ))
            .await
            .unwrap();
        assert_eq!(handle.runtime_id(), &runtime_id);
        drop(handle);
        drop(first);

        let second = RemoteRuntimeClient::new(connector);
        let attached = second.attach(&runtime_id).await.unwrap();
        assert_eq!(attached.runtime_id(), &runtime_id);
        second.dispose(&runtime_id).await.unwrap();
        server.await.unwrap();
        daemon.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retained_acquire_rejects_directory_outside_workspace() {
        use temps_agent_runtime::{lifecycle::RuntimeId, retained::RuntimeClient, Provider};

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(workspace.path()).unwrap();
        let spec = RuntimeSpec::new(
            RuntimeId::new("outside-workspace").unwrap(),
            Provider::OpenCode,
            outside.path(),
        );
        let error = daemon.retained.acquire(spec).await.unwrap_err();
        assert_eq!(error.kind, RuntimeFailureKind::InvalidRequest);
        assert!(error.message.contains("outside-workspace"));
        assert!(error
            .message
            .contains(&workspace.path().display().to_string()));
    }

    #[tokio::test]
    async fn timed_out_idle_peer_releases_connection_capacity() {
        let workspace = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new(workspace.path()).unwrap());
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let (mut stalled_client, stalled_server) = UnixStream::pair().unwrap();
        stalled_client.write_all(&[0]).await.unwrap();
        let permit = slots.clone().try_acquire_owned().unwrap();
        let stalled_daemon = daemon.clone();
        let stalled = tokio::spawn(async move {
            let _permit = permit;
            serve_agent_connection_with_read_timeout(
                stalled_server,
                stalled_daemon,
                Duration::from_millis(20),
            )
            .await
        });
        assert!(slots.clone().try_acquire_owned().is_err());
        assert!(matches!(
            stalled.await.unwrap(),
            Err(Error::RetainedProtocol { detail }) if detail.contains("timed out")
        ));
        assert!(slots.try_acquire_owned().is_ok());
    }
}
