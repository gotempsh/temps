// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The WASM runtime driver (ADR-055 §6, phase 10).
//!
//! Runs WASI preview 1 command modules in this process under wasmtime. The
//! isolation is [`Isolation::InHost`]: the guest runs inside the node
//! runtime, so an escape from wasmtime lands in it, and admission only lets
//! an `Untrusted` workload use it with the operator's say-so (§3).
//!
//! An instance runs in one of two ways, chosen by its spec:
//!
//! - **HTTP trigger** (the instance publishes a port): `start` binds a
//!   listener on the plane's host interface and every request runs the
//!   module once, WAGI/CGI style. The request body is the guest's stdin,
//!   the request line and headers are CGI variables (`REQUEST_METHOD`,
//!   `PATH_INFO`, `QUERY_STRING`, `HTTP_<NAME>`), and stdout is the
//!   response: optional CGI headers (`Status: 404`, `Content-Type: ...`), a
//!   blank line, then the body. Output without a header block is a `200`
//!   `text/plain` body.
//! - **Command** (no port): `start` runs `_start` once; the instance exits
//!   with the guest's exit code, like a container.
//!
//! Limits are enforced per run, never ignored:
//!
//! - **memory**: the instance's memory limit (or the driver default) caps
//!   the guest's linear memory through a store limiter;
//! - **compute**: every run gets a fuel budget, and a run that exhausts it
//!   traps; fuel yields to the async runtime at an interval, so the run
//!   timeout can interrupt a guest that never returns;
//! - **concurrency**: runs per instance are bounded; a request over the
//!   bound gets an immediate `503`, never a queue;
//! - **I/O**: the request body, the response and the log ring are bounded.
//!
//! Network: the attachment kind is [`AttachmentKind::InHost`], an in-process
//! socket allow-list. Preview 1 has no socket API, so a module can open no
//! connection at all; the allow-list ([`SocketAllowList`]) is applied to the
//! WASI context anyway, so the policy is in place for preview 2 guests.

mod http;
mod logs;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use temps_workload::{
    Artifact, ArtifactKinds, AttachmentKind, Attachments, DriverCapabilities, DriverError,
    InstanceHandle, InstanceSpec, InstanceState, InstanceStats, InstanceStatus, Isolation,
    LimitSupport, ListedInstance, LogLine, LogQuery, LogStream, PortBinding, PreparedArtifact,
    RuntimeDriver, VolumeKinds,
};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinHandle;
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::WasiCtxBuilder;

pub use logs::LogRing;

/// The driver's name, in `InstanceHandle::driver` and the capabilities.
pub const DRIVER_NAME: &str = "wasm";

/// Fuel the guest burns between yields to the async runtime. Small enough
/// that a timeout interrupts a spinning guest within milliseconds, large
/// enough that yielding costs nothing measurable.
const FUEL_YIELD_INTERVAL: u64 = 100_000;

/// One allowed destination for a guest socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowedSocket {
    pub ip: IpAddr,
    /// `None` allows every port of `ip`.
    pub port: Option<u16>,
}

/// The in-host socket allow-list (§6 `InHost`). Empty denies every socket,
/// which is the default: an `Untrusted` workload's egress is at most
/// `restricted` and a function links to nothing yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SocketAllowList {
    pub entries: Vec<AllowedSocket>,
}

impl SocketAllowList {
    pub fn allows(&self, addr: &SocketAddr) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.ip == addr.ip() && entry.port.is_none_or(|port| port == addr.port()))
    }
}

/// How the driver is configured. Built once at startup; every bound has a
/// default sized for the 1 vCPU / 2 GB target.
#[derive(Debug, Clone)]
pub struct WasmDriverConfig {
    /// Largest module `prepare` accepts.
    pub max_module_bytes: usize,
    /// Compiled modules kept in memory, by digest. The oldest is dropped
    /// first; a dropped module is compiled again by the next `prepare`.
    pub max_cached_modules: usize,
    /// Guest memory when the instance sets no memory limit.
    pub default_memory_bytes: u64,
    /// Ceiling on any instance's guest memory, whatever its limit says.
    pub max_memory_bytes: u64,
    /// Fuel (roughly, wasm instructions) one run may burn.
    pub fuel_per_run: u64,
    /// Wall-clock limit of one run.
    pub run_timeout: Duration,
    /// Largest request body an HTTP trigger reads.
    pub max_request_body_bytes: usize,
    /// Largest stdout a run may write (the response, for an HTTP trigger).
    pub max_output_bytes: usize,
    /// Runs of one instance at a time; more get a `503`.
    pub max_concurrent_runs: usize,
    /// Open connections of one instance; accepting pauses at the bound.
    pub max_connections: usize,
    /// Bytes of log lines kept per instance.
    pub log_ring_bytes: usize,
    pub sockets: SocketAllowList,
}

impl Default for WasmDriverConfig {
    fn default() -> Self {
        Self {
            max_module_bytes: 32 * 1024 * 1024,
            max_cached_modules: 32,
            default_memory_bytes: 64 * 1024 * 1024,
            max_memory_bytes: 512 * 1024 * 1024,
            fuel_per_run: 2_000_000_000,
            run_timeout: Duration::from_secs(30),
            max_request_body_bytes: 1024 * 1024,
            max_output_bytes: 1024 * 1024,
            max_concurrent_runs: 8,
            max_connections: 256,
            log_ring_bytes: 64 * 1024,
            sockets: SocketAllowList::default(),
        }
    }
}

/// The store's data: the WASI context and the limiter.
struct RunState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

/// What one run of a module produced.
#[derive(Debug)]
pub(crate) struct RunOutcome {
    pub(crate) result: RunResult,
    pub(crate) stdout: Bytes,
    pub(crate) stderr: Bytes,
    pub(crate) fuel_used: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunResult {
    /// `_start` returned (0) or the guest called `proc_exit`.
    Exited(i32),
    OutOfFuel,
    TimedOut,
    /// A trap, or a module the runtime could not instantiate.
    Failed(String),
}

impl RunResult {
    pub(crate) fn exit_code(&self) -> i64 {
        match self {
            RunResult::Exited(code) => i64::from(*code),
            // The shell conventions for a killed process.
            RunResult::OutOfFuel | RunResult::TimedOut => 137,
            RunResult::Failed(_) => 134,
        }
    }

    pub(crate) fn describe(&self) -> String {
        match self {
            RunResult::Exited(code) => format!("exited {code}"),
            RunResult::OutOfFuel => "ran out of fuel (its compute budget per run)".to_string(),
            RunResult::TimedOut => "exceeded its run timeout".to_string(),
            RunResult::Failed(reason) => format!("failed: {reason}"),
        }
    }
}

/// Engine, linker and limits: everything a run needs, shared by every
/// instance of the driver.
pub(crate) struct Executor {
    engine: Engine,
    linker: Linker<RunState>,
    config: WasmDriverConfig,
}

impl Executor {
    /// Run `module` once with `stdin`, `env` and `args`.
    pub(crate) async fn run(
        &self,
        module: &Module,
        memory_bytes: u64,
        stdin: Bytes,
        env: &[(String, String)],
        args: &[String],
    ) -> RunOutcome {
        let stdout = MemoryOutputPipe::new(self.config.max_output_bytes);
        let stderr = MemoryOutputPipe::new(self.config.log_ring_bytes);
        let mut builder = WasiCtxBuilder::new();
        builder
            .stdin(MemoryInputPipe::new(stdin))
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .envs(env)
            .args(args)
            .allow_ip_name_lookup(false)
            .allow_udp(false)
            .allow_tcp(!self.config.sockets.entries.is_empty());
        let allow = self.config.sockets.clone();
        builder.socket_addr_check(move |addr, _use| {
            let allowed = allow.allows(&addr);
            Box::pin(async move { allowed })
        });
        let limits = StoreLimitsBuilder::new()
            .memory_size(usize::try_from(memory_bytes).unwrap_or(usize::MAX))
            .instances(1)
            .memories(1)
            .tables(4)
            .table_elements(100_000)
            .build();
        let mut store = Store::new(
            &self.engine,
            RunState {
                wasi: builder.build_p1(),
                limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        let fuel = self.config.fuel_per_run;
        let result = match self.call_start(&mut store, module, fuel).await {
            Ok(result) => result,
            Err(reason) => RunResult::Failed(reason),
        };
        let fuel_used = fuel.saturating_sub(store.get_fuel().unwrap_or(0));
        RunOutcome {
            result,
            stdout: stdout.contents(),
            stderr: stderr.contents(),
            fuel_used,
        }
    }

    async fn call_start(
        &self,
        store: &mut Store<RunState>,
        module: &Module,
        fuel: u64,
    ) -> Result<RunResult, String> {
        store
            .set_fuel(fuel)
            .map_err(|e| format!("setting the fuel budget: {e}"))?;
        store
            .fuel_async_yield_interval(Some(FUEL_YIELD_INTERVAL))
            .map_err(|e| format!("setting the fuel yield interval: {e}"))?;
        let run = async {
            let instance = self
                .linker
                .instantiate_async(&mut *store, module)
                .await
                .map_err(|e| CallError::Setup(format!("instantiating the module: {e}")))?;
            let start = instance
                .get_typed_func::<(), ()>(&mut *store, "_start")
                .map_err(|e| {
                    CallError::Setup(format!(
                        "the module exports no `_start` command entry point: {e}"
                    ))
                })?;
            start
                .call_async(&mut *store, ())
                .await
                .map_err(CallError::Guest)
        };
        match tokio::time::timeout(self.config.run_timeout, run).await {
            Err(_) => Ok(RunResult::TimedOut),
            Ok(Ok(())) => Ok(RunResult::Exited(0)),
            Ok(Err(CallError::Setup(reason))) => Err(reason),
            Ok(Err(CallError::Guest(error))) => {
                if let Some(exit) = error.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    Ok(RunResult::Exited(exit.0))
                } else if error.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
                    Ok(RunResult::OutOfFuel)
                } else {
                    Ok(RunResult::Failed(format!("trapped: {error}")))
                }
            }
        }
    }
}

/// Why `_start` did not return normally.
enum CallError {
    /// Before the guest ran: instantiation or a missing entry point.
    Setup(String),
    /// The guest trapped or exited.
    Guest(wasmtime::Error),
}

/// What an instance is doing.
#[derive(Debug)]
struct Runtime {
    state: InstanceState,
    exit_code: Option<i64>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    /// The listener's address, for an HTTP trigger.
    bound: Option<SocketAddr>,
    task: Option<JoinHandle<()>>,
    shutdown: Option<watch::Sender<bool>>,
}

/// One instance: a module with its environment and limits.
pub(crate) struct WasmInstance {
    handle: InstanceHandle,
    labels: BTreeMap<String, String>,
    module: Module,
    env: Vec<(String, String)>,
    args: Vec<String>,
    port: Option<PortBinding>,
    memory_bytes: u64,
    runtime: Mutex<Runtime>,
    exited: watch::Sender<bool>,
    pub(crate) runs: Arc<Semaphore>,
    pub(crate) logs: Mutex<LogRing>,
    pub(crate) requests: AtomicU64,
    pub(crate) rejected: AtomicU64,
    pub(crate) fuel_used: AtomicU64,
}

impl WasmInstance {
    fn runtime(&self) -> std::sync::MutexGuard<'_, Runtime> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn log(&self, stderr: bool, line: impl Into<Bytes>) {
        self.logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(stderr, line.into());
    }

    pub(crate) fn log_output(&self, stderr: bool, output: &[u8]) {
        let mut ring = self
            .logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for line in output
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
        {
            ring.push(stderr, Bytes::copy_from_slice(line));
        }
    }

    /// One run with this instance's module, environment and limits.
    pub(crate) async fn run(
        &self,
        executor: &Executor,
        stdin: Bytes,
        extra_env: Vec<(String, String)>,
    ) -> RunOutcome {
        let env: Vec<(String, String)> = if extra_env.is_empty() {
            self.env.clone()
        } else {
            self.env.iter().cloned().chain(extra_env).collect()
        };
        let outcome = executor
            .run(&self.module, self.memory_bytes, stdin, &env, &self.args)
            .await;
        self.fuel_used
            .fetch_add(outcome.fuel_used, Ordering::Relaxed);
        self.log_output(true, &outcome.stderr);
        outcome
    }

    fn finish(&self, exit_code: i64) {
        let mut runtime = self.runtime();
        runtime.state = InstanceState::Exited;
        runtime.exit_code = Some(exit_code);
        runtime.finished_at = Some(Utc::now());
        runtime.bound = None;
        runtime.shutdown = None;
        runtime.task = None;
        drop(runtime);
        self.exited.send_replace(true);
    }
}

/// Compiled modules by digest, oldest first.
#[derive(Default)]
struct ModuleCache {
    modules: HashMap<String, Module>,
    order: VecDeque<String>,
}

impl ModuleCache {
    fn get(&self, digest: &str) -> Option<Module> {
        self.modules.get(digest).cloned()
    }

    fn insert(&mut self, digest: String, module: Module, max: usize) {
        if self.modules.insert(digest.clone(), module).is_none() {
            self.order.push_back(digest);
        }
        while self.order.len() > max.max(1) {
            if let Some(oldest) = self.order.pop_front() {
                self.modules.remove(&oldest);
            }
        }
    }
}

/// The WASM [`RuntimeDriver`].
pub struct WasmDriver {
    executor: Arc<Executor>,
    capabilities: DriverCapabilities,
    modules: Mutex<ModuleCache>,
    instances: Mutex<HashMap<String, Arc<WasmInstance>>>,
}

impl std::fmt::Debug for WasmDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmDriver")
            .field("config", &self.executor.config)
            .finish()
    }
}

/// Why the driver could not be built.
#[derive(Debug, thiserror::Error)]
pub enum WasmDriverError {
    #[error("the wasmtime engine could not be created for the WASM driver: {reason}")]
    Engine { reason: String },
    #[error("the WASI imports could not be linked for the WASM driver: {reason}")]
    Linker { reason: String },
}

fn unavailable(reason: impl Into<String>) -> DriverError {
    DriverError::Unavailable {
        driver: DRIVER_NAME.to_string(),
        reason: reason.into(),
    }
}

fn not_found(instance: &str) -> DriverError {
    DriverError::NotFound {
        driver: DRIVER_NAME.to_string(),
        instance: instance.to_string(),
    }
}

/// The digest of `bytes` as `sha256:<hex>`.
pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn normalise_digest(digest: &str) -> String {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    format!("sha256:{}", hex.to_ascii_lowercase())
}

/// A core module (`\0asm` version 1), as opposed to a component.
fn is_core_module(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[..4] == b"\0asm" && bytes[4..8] == [1, 0, 0, 0]
}

impl WasmDriver {
    pub fn new(config: WasmDriverConfig) -> Result<Self, WasmDriverError> {
        let mut engine_config = Config::new();
        // Compilation stays on the calling (blocking) thread: the
        // `parallel-compilation` feature is not enabled, so compiling a
        // module never fans out over every core of a small host.
        engine_config.consume_fuel(true).max_wasm_stack(512 * 1024);
        let engine = Engine::new(&engine_config).map_err(|e| WasmDriverError::Engine {
            reason: e.to_string(),
        })?;
        let mut linker: Linker<RunState> = Linker::new(&engine);
        wasmtime_wasi::p1::add_to_linker_async(&mut linker, |state: &mut RunState| &mut state.wasi)
            .map_err(|e| WasmDriverError::Linker {
                reason: e.to_string(),
            })?;
        let network_attachment = AttachmentKind::InHost;
        Ok(Self {
            executor: Arc::new(Executor {
                engine,
                linker,
                config,
            }),
            capabilities: DriverCapabilities {
                name: DRIVER_NAME.to_string(),
                artifacts: ArtifactKinds {
                    oci: false,
                    wasm: true,
                    rootfs: false,
                },
                isolation: Isolation::InHost,
                network_attachment,
                volumes: VolumeKinds::default(),
                exec: false,
                snapshot: false,
                resize_disk: false,
                // Memory through the store limiter. CPU is a per-run fuel
                // budget, not a rate, so a cpu limit is reported as not
                // enforced rather than silently mapped.
                // A guest has no processes and no filesystem, so a pids
                // or disk limit holds by construction.
                limits: LimitSupport {
                    cpu: false,
                    memory: true,
                    pids: true,
                    disk: true,
                },
            },
            modules: Mutex::new(ModuleCache::default()),
            instances: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &WasmDriverConfig {
        &self.executor.config
    }

    fn instance(&self, handle: &InstanceHandle) -> Option<Arc<WasmInstance>> {
        self.instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&handle.id)
            .cloned()
    }

    fn require(&self, handle: &InstanceHandle) -> Result<Arc<WasmInstance>, DriverError> {
        self.instance(handle).ok_or_else(|| not_found(&handle.name))
    }

    fn memory_bytes(&self, spec: &InstanceSpec) -> u64 {
        let config = &self.executor.config;
        spec.limits
            .memory_bytes
            .and_then(|bytes| u64::try_from(bytes).ok())
            .filter(|bytes| *bytes > 0)
            .unwrap_or(config.default_memory_bytes)
            .min(config.max_memory_bytes)
    }

    async fn serve(
        &self,
        instance: &Arc<WasmInstance>,
        port: &PortBinding,
    ) -> Result<(SocketAddr, JoinHandle<()>, watch::Sender<bool>), DriverError> {
        let ip: IpAddr = port.host_ip.parse().map_err(|e| DriverError::Runtime {
            driver: DRIVER_NAME.to_string(),
            operation: "start",
            instance: instance.handle.name.clone(),
            reason: format!("host interface '{}' is not an address: {e}", port.host_ip),
        })?;
        if ip.is_unspecified() {
            return Err(DriverError::Runtime {
                driver: DRIVER_NAME.to_string(),
                operation: "start",
                instance: instance.handle.name.clone(),
                reason: "refusing to listen on every interface; the plane binds a loopback or \
                         private address"
                    .to_string(),
            });
        }
        let listener = tokio::net::TcpListener::bind((ip, port.host_port.unwrap_or(0)))
            .await
            .map_err(|e| DriverError::Runtime {
                driver: DRIVER_NAME.to_string(),
                operation: "start",
                instance: instance.handle.name.clone(),
                reason: format!(
                    "binding {}:{}: {e}",
                    port.host_ip,
                    port.host_port.unwrap_or(0)
                ),
            })?;
        let bound = listener.local_addr().map_err(|e| DriverError::Runtime {
            driver: DRIVER_NAME.to_string(),
            operation: "start",
            instance: instance.handle.name.clone(),
            reason: format!("reading the bound address: {e}"),
        })?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(http::accept_loop(
            listener,
            Arc::clone(instance),
            Arc::clone(&self.executor),
            shutdown_rx,
        ));
        Ok((bound, task, shutdown_tx))
    }
}

#[async_trait]
impl RuntimeDriver for WasmDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    /// Read the blob, check its digest and compile it (once per digest).
    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        let Artifact::WasmComponent { blob, digest } = artifact else {
            return Err(DriverError::Artifact {
                driver: DRIVER_NAME.to_string(),
                artifact: format!("{artifact:?}"),
                reason: "the WASM driver only runs WASM artifacts".to_string(),
            });
        };
        let artifact_error = |reason: String| DriverError::Artifact {
            driver: DRIVER_NAME.to_string(),
            artifact: blob.clone(),
            reason,
        };
        let digest = normalise_digest(digest);
        let cached = self
            .modules
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&digest);
        if cached.is_some() {
            return Ok(PreparedArtifact {
                artifact: artifact.clone(),
                local_ref: digest,
            });
        }
        let max = self.executor.config.max_module_bytes;
        let size = tokio::fs::metadata(blob)
            .await
            .map_err(|e| artifact_error(format!("reading the blob's size: {e}")))?
            .len();
        if size > max as u64 {
            return Err(artifact_error(format!(
                "the blob is {size} bytes; the limit is {max}"
            )));
        }
        let bytes = tokio::fs::read(blob)
            .await
            .map_err(|e| artifact_error(format!("reading the blob: {e}")))?;
        let actual = digest_of(&bytes);
        if actual != digest {
            return Err(artifact_error(format!(
                "the blob's digest is {actual}, the artifact names {digest}"
            )));
        }
        if !is_core_module(&bytes) {
            return Err(artifact_error(
                "the blob is not a WASI preview 1 core module (a component or another format); \
                 this driver runs core modules"
                    .to_string(),
            ));
        }
        let engine = self.executor.engine.clone();
        // Compiling is CPU-bound and can take a while for a large module.
        let module = tokio::task::spawn_blocking(move || Module::new(&engine, &bytes))
            .await
            .map_err(|e| artifact_error(format!("the compile task failed: {e}")))?
            .map_err(|e| artifact_error(format!("compiling the module: {e}")))?;
        self.modules
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                digest.clone(),
                module,
                self.executor.config.max_cached_modules,
            );
        tracing::info!(digest = %digest, size, "compiled a WASM module");
        Ok(PreparedArtifact {
            artifact: artifact.clone(),
            local_ref: digest,
        })
    }

    async fn create(
        &self,
        spec: &InstanceSpec,
        _attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError> {
        let module = self
            .modules
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&normalise_digest(&spec.image))
            .ok_or_else(|| DriverError::Runtime {
                driver: DRIVER_NAME.to_string(),
                operation: "create",
                instance: spec.name.clone(),
                reason: format!(
                    "module {} is not prepared on this node; prepare it first",
                    spec.image
                ),
            })?;
        if spec.ports.len() > 1 {
            return Err(DriverError::Runtime {
                driver: DRIVER_NAME.to_string(),
                operation: "create",
                instance: spec.name.clone(),
                reason: format!(
                    "{} ports requested; a WASM instance serves one HTTP trigger",
                    spec.ports.len()
                ),
            });
        }
        let mut env = spec.env.clone();
        // A WASM guest has no filesystem: secret files are delivered as
        // variables, which only this run's memory ever holds.
        for secret in &spec.secret_files {
            env.push((secret.name.clone(), secret.value.clone()));
        }
        let mut args = vec![spec.name.clone()];
        if let Some(entrypoint) = &spec.entrypoint {
            args.extend(entrypoint.iter().cloned());
        }
        if let Some(command) = &spec.command {
            args.extend(command.iter().cloned());
        }
        let handle = InstanceHandle {
            driver: DRIVER_NAME.to_string(),
            id: spec.name.clone(),
            name: spec.name.clone(),
        };
        let instance = Arc::new(WasmInstance {
            handle: handle.clone(),
            labels: spec.labels.clone(),
            module,
            env,
            args,
            port: spec.ports.first().cloned(),
            memory_bytes: self.memory_bytes(spec),
            runtime: Mutex::new(Runtime {
                state: InstanceState::Created,
                exit_code: None,
                started_at: None,
                finished_at: None,
                bound: None,
                task: None,
                shutdown: None,
            }),
            exited: watch::channel(false).0,
            runs: Arc::new(Semaphore::new(self.executor.config.max_concurrent_runs)),
            logs: Mutex::new(LogRing::new(self.executor.config.log_ring_bytes)),
            requests: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            fuel_used: AtomicU64::new(0),
        });
        let mut instances = self
            .instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if instances.contains_key(&handle.id) {
            return Err(DriverError::Runtime {
                driver: DRIVER_NAME.to_string(),
                operation: "create",
                instance: spec.name.clone(),
                reason: "an instance of this name already exists".to_string(),
            });
        }
        instances.insert(handle.id.clone(), instance);
        Ok(handle)
    }

    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        let instance = self.require(handle)?;
        if instance.runtime().state == InstanceState::Running {
            return Ok(());
        }
        instance.exited.send_replace(false);
        match instance.port.clone() {
            Some(port) => {
                let (bound, task, shutdown) = self.serve(&instance, &port).await?;
                let mut runtime = instance.runtime();
                runtime.state = InstanceState::Running;
                runtime.exit_code = None;
                runtime.started_at = Some(Utc::now());
                runtime.finished_at = None;
                runtime.bound = Some(bound);
                runtime.task = Some(task);
                runtime.shutdown = Some(shutdown);
                drop(runtime);
                instance.log(false, format!("listening on {bound}"));
                tracing::info!(instance = %handle.name, address = %bound, "WASM HTTP trigger started");
            }
            None => {
                let executor = Arc::clone(&self.executor);
                let runner = Arc::clone(&instance);
                let task = tokio::spawn(async move {
                    let outcome = runner.run(&executor, Bytes::new(), Vec::new()).await;
                    runner.log_output(false, &outcome.stdout);
                    if !matches!(outcome.result, RunResult::Exited(_)) {
                        runner.log(true, format!("the run {}", outcome.result.describe()));
                    }
                    runner.finish(outcome.result.exit_code());
                });
                let mut runtime = instance.runtime();
                runtime.state = InstanceState::Running;
                runtime.exit_code = None;
                runtime.started_at = Some(Utc::now());
                runtime.finished_at = None;
                runtime.task = Some(task);
            }
        }
        Ok(())
    }

    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        let Some(instance) = self.instance(handle) else {
            // Stopping what is gone is done.
            return Ok(());
        };
        let (task, shutdown, running) = {
            let mut runtime = instance.runtime();
            (
                runtime.task.take(),
                runtime.shutdown.take(),
                runtime.state == InstanceState::Running,
            )
        };
        if let Some(shutdown) = shutdown {
            shutdown.send_replace(true);
        }
        if let Some(task) = task {
            task.abort();
        }
        if running {
            // A stopped command never reports its own exit: it was killed.
            let code = if instance.port.is_some() { 0 } else { 137 };
            instance.finish(code);
        }
        Ok(())
    }

    async fn remove(&self, handle: &InstanceHandle, _purge: bool) -> Result<(), DriverError> {
        self.stop(handle).await?;
        self.instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&handle.id);
        Ok(())
    }

    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        let Some(instance) = self.instance(handle) else {
            return Ok(InstanceStatus {
                state: InstanceState::Missing,
                exit_code: None,
                started_at: None,
                finished_at: None,
                address: None,
                published_ports: Vec::new(),
            });
        };
        let runtime = instance.runtime();
        let published_ports = match (&instance.port, runtime.bound) {
            (Some(port), Some(bound)) => vec![PortBinding {
                container_port: port.container_port,
                protocol: port.protocol,
                host_ip: bound.ip().to_string(),
                host_port: Some(bound.port()),
            }],
            _ => Vec::new(),
        };
        Ok(InstanceStatus {
            state: runtime.state,
            exit_code: runtime.exit_code,
            started_at: runtime.started_at,
            finished_at: runtime.finished_at,
            address: runtime.bound.map(|bound| bound.ip().to_string()),
            published_ports,
        })
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        let instance = self.require(handle)?;
        let lines: Vec<Result<LogLine, DriverError>> = instance
            .logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tail(query.tail)
            .into_iter()
            .map(Ok)
            .collect();
        Ok(Box::pin(futures::stream::iter(lines)))
    }

    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        let instance = self.require(handle)?;
        tracing::debug!(
            instance = %handle.name,
            requests = instance.requests.load(Ordering::Relaxed),
            rejected = instance.rejected.load(Ordering::Relaxed),
            fuel_used = instance.fuel_used.load(Ordering::Relaxed),
            "WASM instance stats"
        );
        Ok(InstanceStats {
            cpu_nanos: None,
            memory_bytes: None,
            // A guest is no process: there are none to count.
            pids: Some(0),
        })
    }

    async fn list(
        &self,
        labels: &BTreeMap<String, String>,
    ) -> Result<Vec<ListedInstance>, DriverError> {
        Ok(self
            .instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|instance| {
                labels
                    .iter()
                    .all(|(key, value)| instance.labels.get(key) == Some(value))
            })
            .map(|instance| ListedInstance {
                handle: instance.handle.clone(),
                labels: instance.labels.clone(),
            })
            .collect())
    }

    async fn wait(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        let Some(instance) = self.instance(handle) else {
            return Ok(());
        };
        let mut exited = instance.exited.subscribe();
        if instance.runtime().state != InstanceState::Running {
            return Ok(());
        }
        loop {
            if *exited.borrow_and_update() {
                return Ok(());
            }
            if exited.changed().await.is_err() {
                return Err(unavailable(format!(
                    "instance '{}' was dropped while waited on",
                    handle.name
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use temps_workload::{
        InstanceLimits, PortProtocol, RestartPolicy, SecurityProfile, WorkloadKind, WorkloadRef,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const HEADER: &str = "Status: 201\nContent-Type: text/plain\n\nhello ";

    /// Writes a CGI header block and "hello ", then echoes stdin.
    fn echo_module() -> Vec<u8> {
        let wat = format!(
            r#"(module
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{header}")
  (func (export "_start")
    (i32.store (i32.const 0) (i32.const 1024))
    (i32.store (i32.const 4) (i32.const {len}))
    (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))
    (i32.store (i32.const 16) (i32.const 2048))
    (i32.store (i32.const 20) (i32.const 1024))
    (drop (call $fd_read (i32.const 0) (i32.const 16) (i32.const 1) (i32.const 24)))
    (i32.store (i32.const 32) (i32.const 2048))
    (i32.store (i32.const 36) (i32.load (i32.const 24)))
    (drop (call $fd_write (i32.const 1) (i32.const 32) (i32.const 1) (i32.const 8)))))"#,
            header = HEADER.replace('\n', "\\n"),
            len = HEADER.len()
        );
        wat::parse_str(wat).expect("the echo module assembles")
    }

    fn spinning_module() -> Vec<u8> {
        wat::parse_str(r#"(module (func (export "_start") (loop $spin (br $spin))))"#)
            .expect("the spinning module assembles")
    }

    fn exiting_module(code: i32) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start") (call $proc_exit (i32.const {code}))))"#
        ))
        .expect("the exiting module assembles")
    }

    /// Two pages of initial memory: 128 KiB.
    fn hungry_module() -> Vec<u8> {
        wat::parse_str(r#"(module (memory (export "memory") 2) (func (export "_start")))"#)
            .expect("the hungry module assembles")
    }

    fn spec(name: &str, digest: &str, port: bool, memory_bytes: Option<i64>) -> InstanceSpec {
        InstanceSpec {
            owner: WorkloadRef::new(WorkloadKind::Function, 7),
            name: name.to_string(),
            image: digest.to_string(),
            platform: None,
            env: vec![("GREETING".into(), "hi".into())],
            secret_files: vec![],
            command: None,
            entrypoint: None,
            workdir: None,
            user: None,
            ports: if port {
                vec![PortBinding {
                    container_port: 8080,
                    protocol: PortProtocol::Tcp,
                    host_ip: Ipv4Addr::LOCALHOST.to_string(),
                    host_port: None,
                }]
            } else {
                vec![]
            },
            restart: RestartPolicy::Never,
            limits: InstanceLimits {
                memory_bytes,
                ..Default::default()
            },
            log_rotation: None,
            labels: BTreeMap::from([("sh.temps.kind".to_string(), "function".to_string())]),
            security: SecurityProfile::Untrusted,
        }
    }

    async fn prepared(driver: &WasmDriver, bytes: &[u8]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("module.wasm");
        std::fs::write(&path, bytes).expect("the module is written");
        let digest = digest_of(bytes);
        let prepared = driver
            .prepare(&Artifact::WasmComponent {
                blob: path.display().to_string(),
                digest: digest.clone(),
            })
            .await
            .expect("the module prepares");
        assert_eq!(prepared.local_ref, digest);
        (dir, digest)
    }

    async fn request(address: SocketAddr, method: &str, body: &str) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("the trigger accepts a connection");
        let request = format!(
            "{method} /greet?x=1 HTTP/1.1\r\nHost: fn.test\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("the request is sent");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("the response is read");
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }

    async fn started(driver: &WasmDriver, spec: &InstanceSpec) -> (InstanceHandle, SocketAddr) {
        let handle = driver
            .create(spec, &Attachments::default())
            .await
            .expect("the instance is created");
        driver.start(&handle).await.expect("the instance starts");
        let status = driver.inspect(&handle).await.expect("inspect");
        assert_eq!(status.state, InstanceState::Running);
        let port = status.published_ports[0]
            .host_port
            .expect("a bound host port");
        (handle, SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }

    #[test]
    fn the_driver_advertises_wasm_in_host() {
        let driver = WasmDriver::new(WasmDriverConfig::default()).expect("the driver builds");
        let capabilities = driver.capabilities();
        assert!(capabilities.artifacts.wasm);
        assert!(!capabilities.artifacts.oci);
        assert_eq!(capabilities.isolation, Isolation::InHost);
        assert_eq!(capabilities.network_attachment, AttachmentKind::InHost);
        assert!(capabilities.limits.memory);
        assert!(!capabilities.limits.cpu, "fuel is a budget, not a rate");
    }

    #[test]
    fn the_socket_allow_list_denies_by_default() {
        let empty = SocketAllowList::default();
        let target: SocketAddr = "10.0.0.5:5432".parse().unwrap();
        assert!(!empty.allows(&target));
        let list = SocketAllowList {
            entries: vec![
                AllowedSocket {
                    ip: "10.0.0.5".parse().unwrap(),
                    port: Some(5432),
                },
                AllowedSocket {
                    ip: "10.0.0.6".parse().unwrap(),
                    port: None,
                },
            ],
        };
        assert!(list.allows(&target));
        assert!(!list.allows(&"10.0.0.5:22".parse().unwrap()));
        assert!(list.allows(&"10.0.0.6:22".parse().unwrap()));
        assert!(!list.allows(&"10.0.0.7:5432".parse().unwrap()));
    }

    #[tokio::test]
    async fn an_http_trigger_runs_the_module_once_per_request() {
        let driver = WasmDriver::new(WasmDriverConfig::default()).expect("the driver builds");
        let (_dir, digest) = prepared(&driver, &echo_module()).await;
        let (handle, address) = started(&driver, &spec("fn-7-a1", &digest, true, None)).await;

        let (status, body) = request(address, "POST", "ada").await;
        assert_eq!(status, 201, "the guest's Status header sets the code");
        assert_eq!(body, "hello ada");
        let (status, body) = request(address, "POST", "grace").await;
        assert_eq!(status, 201);
        assert_eq!(body, "hello grace", "every request gets a fresh instance");

        driver.stop(&handle).await.expect("stop");
        let status = driver.inspect(&handle).await.expect("inspect");
        assert_eq!(status.state, InstanceState::Exited);
        assert!(
            tokio::net::TcpStream::connect(address).await.is_err(),
            "a stopped trigger no longer listens"
        );
        driver
            .start(&handle)
            .await
            .expect("a stopped instance starts again");
        let status = driver.inspect(&handle).await.expect("inspect");
        assert_eq!(status.state, InstanceState::Running);
        driver.remove(&handle, true).await.expect("remove");
        assert_eq!(
            driver.inspect(&handle).await.expect("inspect").state,
            InstanceState::Missing
        );
    }

    #[tokio::test]
    async fn a_guest_that_never_returns_runs_out_of_fuel() {
        let driver = WasmDriver::new(WasmDriverConfig {
            fuel_per_run: 1_000_000,
            ..Default::default()
        })
        .expect("the driver builds");
        let (_dir, digest) = prepared(&driver, &spinning_module()).await;
        let (handle, address) = started(&driver, &spec("fn-7-a2", &digest, true, None)).await;
        let (status, body) = request(address, "GET", "").await;
        assert_eq!(status, 504);
        assert!(body.contains("ran out of fuel"), "{body}");
        let logs: Vec<LogLine> = futures::StreamExt::collect::<Vec<_>>(
            driver
                .logs(&handle, LogQuery::default())
                .await
                .expect("logs"),
        )
        .await
        .into_iter()
        .filter_map(Result::ok)
        .collect();
        assert!(
            logs.iter()
                .any(|line| String::from_utf8_lossy(&line.line).contains("ran out of fuel")),
            "the failure is in the instance's logs"
        );
        driver.remove(&handle, true).await.expect("remove");
    }

    #[tokio::test]
    async fn the_run_timeout_interrupts_a_guest_and_runs_are_bounded() {
        let driver = WasmDriver::new(WasmDriverConfig {
            fuel_per_run: u64::MAX / 2,
            run_timeout: Duration::from_millis(800),
            max_concurrent_runs: 1,
            ..Default::default()
        })
        .expect("the driver builds");
        let (_dir, digest) = prepared(&driver, &spinning_module()).await;
        let (handle, address) = started(&driver, &spec("fn-7-a3", &digest, true, None)).await;
        let first = tokio::spawn(async move { request(address, "GET", "").await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (status, _) = request(address, "GET", "").await;
        assert_eq!(status, 503, "a run over the bound is refused, not queued");
        let (status, body) = first.await.expect("the first request completes");
        assert_eq!(status, 504);
        assert!(body.contains("run timeout"), "{body}");
        driver.remove(&handle, true).await.expect("remove");
    }

    #[tokio::test]
    async fn the_memory_limit_holds() {
        let driver = WasmDriver::new(WasmDriverConfig::default()).expect("the driver builds");
        let (_dir, digest) = prepared(&driver, &hungry_module()).await;
        // 64 KiB: one page, and the module asks for two.
        let (handle, address) =
            started(&driver, &spec("fn-7-a4", &digest, true, Some(64 * 1024))).await;
        let (status, body) = request(address, "GET", "").await;
        assert_eq!(status, 502, "{body}");
        driver.remove(&handle, true).await.expect("remove");

        let (handle, address) =
            started(&driver, &spec("fn-7-a5", &digest, true, Some(256 * 1024))).await;
        let (status, _) = request(address, "GET", "").await;
        assert_eq!(status, 200, "within the limit the module runs");
        driver.remove(&handle, true).await.expect("remove");
    }

    #[tokio::test]
    async fn a_command_exits_with_the_guests_code() {
        let driver = WasmDriver::new(WasmDriverConfig::default()).expect("the driver builds");
        let (_dir, digest) = prepared(&driver, &exiting_module(3)).await;
        let handle = driver
            .create(
                &spec("job-7-a1", &digest, false, None),
                &Attachments::default(),
            )
            .await
            .expect("create");
        driver.start(&handle).await.expect("start");
        tokio::time::timeout(Duration::from_secs(10), driver.wait(&handle))
            .await
            .expect("the command finishes")
            .expect("wait");
        let status = driver.inspect(&handle).await.expect("inspect");
        assert_eq!(status.state, InstanceState::Exited);
        assert_eq!(status.exit_code, Some(3));
        let listed = driver
            .list(&BTreeMap::from([(
                "sh.temps.kind".to_string(),
                "function".to_string(),
            )]))
            .await
            .expect("list");
        assert_eq!(listed.len(), 1);
        driver.remove(&handle, true).await.expect("remove");
    }

    #[tokio::test]
    async fn prepare_refuses_a_wrong_digest_a_component_and_a_missing_blob() {
        let driver = WasmDriver::new(WasmDriverConfig::default()).expect("the driver builds");
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("module.wasm");
        std::fs::write(&path, echo_module()).expect("write");
        let error = driver
            .prepare(&Artifact::WasmComponent {
                blob: path.display().to_string(),
                digest: digest_of(b"something else"),
            })
            .await
            .expect_err("a wrong digest is refused");
        assert!(error.to_string().contains("digest"), "{error}");

        // A component's preamble: `\0asm`, version 0x0d, layer 1.
        let component = b"\0asm\x0d\x00\x01\x00";
        let component_path = dir.path().join("component.wasm");
        std::fs::write(&component_path, component).expect("write");
        let error = driver
            .prepare(&Artifact::WasmComponent {
                blob: component_path.display().to_string(),
                digest: digest_of(component),
            })
            .await
            .expect_err("a component is refused");
        assert!(error.to_string().contains("core module"), "{error}");

        let error = driver
            .prepare(&Artifact::WasmComponent {
                blob: dir.path().join("absent.wasm").display().to_string(),
                digest: digest_of(b""),
            })
            .await
            .expect_err("a missing blob is refused");
        assert!(matches!(error, DriverError::Artifact { .. }));

        let error = driver
            .prepare(&Artifact::oci("nginx:1"))
            .await
            .expect_err("an image is not a WASM artifact");
        assert!(matches!(error, DriverError::Artifact { .. }));
    }

    #[tokio::test]
    async fn an_unprepared_module_and_a_wildcard_interface_are_refused() {
        let driver = WasmDriver::new(WasmDriverConfig::default()).expect("the driver builds");
        let error = driver
            .create(
                &spec("fn-7-a6", &digest_of(b"never prepared"), true, None),
                &Attachments::default(),
            )
            .await
            .expect_err("create needs a prepared module");
        assert!(error.to_string().contains("not prepared"), "{error}");

        let (_dir, digest) = prepared(&driver, &echo_module()).await;
        let mut wildcard = spec("fn-7-a7", &digest, true, None);
        wildcard.ports[0].host_ip = "0.0.0.0".to_string();
        let handle = driver
            .create(&wildcard, &Attachments::default())
            .await
            .expect("create");
        let error = driver
            .start(&handle)
            .await
            .expect_err("every interface is refused");
        assert!(error.to_string().contains("every interface"), "{error}");
    }
}
