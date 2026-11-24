// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keeps `<environment>.<project>.temps.local` served on this worker
//! (ADR-098), and says how in the heartbeat.
//!
//! Containers call port 80 of the overlay bridge's address. The worker must
//! not depend on owning that port, so each pass of [`supervise`]:
//!
//! 1. binds the internal proxy on [`INTERNAL_PROXY_PORT`] of the bridge
//!    address, once; while it cannot, it removes the redirect, because a
//!    redirect to a port this agent does not hold would hand every internal
//!    request to whoever does;
//! 2. replaces the nftables redirect of the bridge's port 80 to that port
//!    ([`crate::internal_redirect`]), which also renews its liveness;
//! 3. when either failed, binds the bridge's port 80 directly, as agents did
//!    before ADR-098, and releases it again once the redirect is installed.
//!
//! A listener that exits on its own (it is not expected to) wakes the
//! supervisor at once and is started again; while the proxy's cannot be,
//! the redirect is removed as in step 1.
//!
//! Every pass's outcome goes to the heartbeat ([`heartbeat_value`]). Only
//! changes are logged.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::RwLock;
use serde::Serialize;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::internal_redirect::{
    removal_ruleset, NftApply, NftCommand, Redirect, RedirectError, CONTAINER_HTTP_PORT,
};
use crate::route_store::SharedRouteStore;

/// Key of the internal routing state in the heartbeat's `capacity`.
///
/// No column: `capacity` is agent-derived and wholly replaced on every beat,
/// which is exactly the lifecycle of "are internal names served right now".
pub const HEARTBEAT_KEY: &str = "internal_proxy";

/// The internal proxy's own port on the bridge address (ADR-098 D1). Below
/// 1024 so only root can hold it: the redirect outlives an agent restart, and
/// whoever holds this port receives every internal request.
pub const INTERNAL_PROXY_PORT: u16 = 591;

/// How often a working redirect is replaced (restoring it after a ruleset
/// flush and renewing its liveness), and the longest wait between attempts
/// while something is not working.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// First wait after a failed pass.
const RETRY_FIRST: Duration = Duration::from_secs(1);

/// How the bridge's port 80 is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    /// The proxy on its own port, and the redirect in place.
    Redirect,
    /// The fallback: the proxy bound on the bridge's port 80 itself.
    Direct,
    /// Nothing serves the calls.
    Down,
}

/// Whether the redirect is in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectState {
    Installed,
    Missing,
    /// Not on Linux: the agent only ever binds port 80 directly.
    Unsupported,
}

/// What the heartbeat says about internal routing. `address`, `listening`,
/// `error` and `attempts` keep the meaning they had before ADR-098, so an
/// older control plane reads a newer agent correctly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InternalRoutingStatus {
    /// What containers call: the bridge address, port 80.
    pub address: SocketAddr,
    /// Whether those calls are served, by either mode.
    pub listening: bool,
    /// Why they are not, written for the operator. `None` while served.
    pub error: Option<String>,
    /// Passes so far.
    pub attempts: u32,
    pub mode: RoutingMode,
    /// The proxy's own listener, while this agent holds it.
    pub proxy_address: Option<SocketAddr>,
    pub redirect: RedirectState,
    /// Why the redirect is missing; also set while `direct` serves.
    pub redirect_error: Option<String>,
}

impl InternalRoutingStatus {
    pub fn heartbeat_value(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }

    /// The listener that is the internal proxy, while one serves: its own
    /// port whenever it holds it, otherwise the bridge's port 80 of the
    /// fallback. A sandbox's egress proxy connects here directly (ADR 105),
    /// so it needs no redirect, and never reaches port 80 while that may be
    /// another program's.
    pub fn serving_address(&self) -> Option<SocketAddr> {
        match self.mode {
            _ if !self.listening => None,
            RoutingMode::Down => None,
            RoutingMode::Redirect => self.proxy_address,
            RoutingMode::Direct => Some(self.proxy_address.unwrap_or(self.address)),
        }
    }
}

/// One routing supervisor per agent process, so its state is too.
static STATUS: RwLock<Option<InternalRoutingStatus>> = RwLock::new(None);

/// The state for the heartbeat; `None` until the first pass (the overlay
/// bridge has no address yet).
pub fn heartbeat_value() -> Option<serde_json::Value> {
    STATUS
        .read()
        .as_ref()
        .and_then(InternalRoutingStatus::heartbeat_value)
}

/// The last pass's outcome, for a process that reports it some other way
/// than the heartbeat: the control plane stores it with node 0's DNS report
/// (ADR-104 D4). `None` until the first pass.
pub fn current_status() -> Option<InternalRoutingStatus> {
    STATUS.read().clone()
}

/// Forget the last outcome, when the process stops supervising (the control
/// plane, once cluster DNS is turned off), so nothing reports a listener that
/// no longer exists.
pub fn clear_status() {
    *STATUS.write() = None;
    temps_sandbox_providers::docker::publish_internal_proxy_address(None);
}

fn record_status(status: InternalRoutingStatus) {
    temps_sandbox_providers::docker::publish_internal_proxy_address(status.serving_address());
    *STATUS.write() = Some(status);
}

/// Which listener a bind error is about; the advice differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerRole {
    /// The proxy's own port.
    Proxy,
    /// Port 80, as the fallback when the redirect is missing.
    Fallback,
    /// Port 80 on a host without the redirect (not Linux).
    DirectOnly,
}

/// Why a listener could not start on `addr`, and what to do about it.
pub fn explain_bind_error(error: &std::io::Error, addr: SocketAddr, role: ListenerRole) -> String {
    let ip = addr.ip();
    let port = addr.port();
    match (error.kind(), role) {
        (std::io::ErrorKind::AddrInUse, ListenerRole::Proxy) => format!(
            "another program already listens on {addr}, or on port {port} of every address of \
             this host. Temps reserves port {port} of the bridge address for this node's \
             internal proxy. Find the program with `sudo ss -ltnp 'sport = :{port}'` and stop \
             it or move it to another port; the agent keeps retrying"
        ),
        (std::io::ErrorKind::AddrInUse, ListenerRole::Fallback) => format!(
            "port {port} of {ip} is held by another program (one listening on 0.0.0.0:{port}, \
             such as a web server or another Temps install). That program may keep it: the \
             fix is the redirect, not the port"
        ),
        (std::io::ErrorKind::AddrInUse, ListenerRole::DirectOnly) => format!(
            "another program already listens on {addr}, or on port {port} of every address of \
             this host (0.0.0.0:{port}), so calls to <environment>.<project>.temps.local names \
             reach that program instead of this node's internal proxy. On this operating system \
             the agent cannot redirect them to a port of its own; it keeps retrying"
        ),
        (std::io::ErrorKind::AddrNotAvailable, _) => format!(
            "{ip} is not an address of this host yet (the overlay bridge is not up); the agent \
             keeps retrying"
        ),
        (std::io::ErrorKind::PermissionDenied, _) => format!(
            "the agent may not listen on port {port} of {ip}: ports below 1024 need root or \
             CAP_NET_BIND_SERVICE. Run the agent service as root; it keeps retrying"
        ),
        // Also the failures after the bind (the first read of workload
        // identities from Docker), which release the port again.
        _ => format!("could not start on {addr}: {error}; the agent keeps retrying"),
    }
}

/// Wait before the next pass after `failures` failed ones in a row: 1 s,
/// doubling, at most [`REFRESH_INTERVAL`].
pub fn retry_delay(failures: u32) -> Duration {
    let factor = 1u32 << failures.saturating_sub(1).min(6);
    (RETRY_FIRST * factor).min(REFRESH_INTERVAL)
}

/// Starts the proxy on an address; a trait so passes can be tested without
/// Docker or real ports.
pub(crate) trait ListenerSpawner: Send + Sync + 'static {
    /// Bind `addr` and serve until `stop` turns true or its sender is
    /// dropped. Returns once bound, with the task serving it: it ends when
    /// the listener stops, asked to or not.
    fn spawn(
        &self,
        addr: SocketAddr,
        stop: watch::Receiver<bool>,
    ) -> BoxFuture<'static, std::io::Result<JoinHandle<()>>>;
}

/// A listener this supervisor started.
struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Running {
    /// Ended on its own: a panic, or the server failed. A listener told to
    /// stop is never kept, so this is only ever asked of a running one.
    fn exited(&self) -> bool {
        self.task.is_finished()
    }

    /// Graceful: calls already accepted finish.
    fn stop(self) {
        let _ = self.stop.send(true);
    }
}

/// The real proxy ([`crate::internal_proxy::spawn`]).
struct ProxyListeners {
    store: SharedRouteStore,
    docker: bollard::Docker,
}

impl ListenerSpawner for ProxyListeners {
    fn spawn(
        &self,
        addr: SocketAddr,
        stop: watch::Receiver<bool>,
    ) -> BoxFuture<'static, std::io::Result<JoinHandle<()>>> {
        Box::pin(crate::internal_proxy::spawn(
            addr,
            self.store.clone(),
            self.docker.clone(),
            stop,
        ))
    }
}

/// The routing state between passes.
pub(crate) struct Routing<L, N> {
    bridge_ip: IpAddr,
    /// `Err` when the redirect cannot exist here: not Linux (empty), or the
    /// bridge address cannot go into a rule (says why).
    redirect: Result<Redirect, String>,
    listeners: L,
    nft: N,
    proxy: Option<Running>,
    direct: Option<Running>,
    redirect_installed: bool,
    /// A table an earlier agent left behind was removed (or there was none).
    stale_removed: bool,
    proxy_error: Option<String>,
    redirect_error: Option<String>,
    direct_error: Option<String>,
    attempts: u32,
    failures: u32,
    last_logged: Option<(RoutingMode, Option<String>)>,
}

impl<L: ListenerSpawner, N: NftApply> Routing<L, N> {
    /// `redirect_supported` is false off Linux.
    pub(crate) fn new(bridge_ip: IpAddr, redirect_supported: bool, listeners: L, nft: N) -> Self {
        let redirect = if redirect_supported {
            Redirect::new(bridge_ip, INTERNAL_PROXY_PORT)
                .map_err(|error| redirect_missing(bridge_ip, &error))
        } else {
            Err(String::new())
        };
        Self::with_redirect(bridge_ip, redirect, listeners, nft)
    }

    fn with_redirect(
        bridge_ip: IpAddr,
        redirect: Result<Redirect, String>,
        listeners: L,
        nft: N,
    ) -> Self {
        Self {
            bridge_ip,
            redirect,
            listeners,
            nft,
            proxy: None,
            direct: None,
            redirect_installed: false,
            stale_removed: false,
            proxy_error: None,
            redirect_error: None,
            direct_error: None,
            attempts: 0,
            failures: 0,
            last_logged: None,
        }
    }

    fn supported(&self) -> bool {
        !matches!(&self.redirect, Err(reason) if reason.is_empty())
    }

    fn direct_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bridge_ip, CONTAINER_HTTP_PORT)
    }

    fn proxy_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bridge_ip, INTERNAL_PROXY_PORT)
    }

    /// One pass; returns what the heartbeat should say.
    pub(crate) async fn pass(&mut self) -> InternalRoutingStatus {
        self.attempts = self.attempts.saturating_add(1);
        self.reap_exited();
        match self.redirect.clone() {
            Ok(redirect) => self.ensure_redirect(&redirect).await,
            Err(reason) => {
                self.redirect_installed = false;
                if !reason.is_empty() && !self.stale_removed {
                    // No redirect can be built for this address, but one an
                    // earlier agent installed may still point somewhere.
                    self.stale_removed = self.remove_redirect().await;
                }
                self.redirect_error = (!reason.is_empty()).then_some(reason);
            }
        }

        if self.proxy.is_some() && self.redirect_installed {
            if let Some(direct) = self.direct.take() {
                // Graceful: calls already accepted on port 80 finish there.
                direct.stop();
                info!(
                    address = %self.direct_addr(),
                    "internal routing redirect installed; released port 80 of the bridge address"
                );
            }
            self.direct_error = None;
        } else if self.direct.is_none() {
            let addr = self.direct_addr();
            let role = if self.supported() {
                ListenerRole::Fallback
            } else {
                ListenerRole::DirectOnly
            };
            let (stop, stopped) = watch::channel(false);
            match self.listeners.spawn(addr, stopped).await {
                Ok(task) => {
                    self.direct = Some(Running { stop, task });
                    self.direct_error = None;
                }
                Err(error) => self.direct_error = Some(explain_bind_error(&error, addr, role)),
            }
        }

        let status = self.status();
        // Healthy: redirected, or direct where no redirect can exist.
        let healthy = status.listening
            && (status.mode == RoutingMode::Redirect
                || status.redirect == RedirectState::Unsupported);
        if healthy {
            self.failures = 0;
        } else {
            self.failures = self.failures.saturating_add(1);
        }
        self.log_change(&status);
        status
    }

    async fn ensure_redirect(&mut self, redirect: &Redirect) {
        if self.proxy.is_none() {
            let addr = self.proxy_addr();
            let (stop, stopped) = watch::channel(false);
            match self.listeners.spawn(addr, stopped).await {
                Ok(task) => {
                    self.proxy = Some(Running { stop, task });
                    self.proxy_error = None;
                }
                Err(error) => {
                    self.proxy_error = Some(explain_bind_error(&error, addr, ListenerRole::Proxy));
                }
            }
        }
        if self.proxy.is_some() {
            match self
                .nft
                .apply("install the internal proxy's redirect", &redirect.ruleset())
                .await
            {
                Ok(()) => {
                    self.redirect_installed = true;
                    self.redirect_error = None;
                }
                Err(error) => {
                    self.redirect_installed = false;
                    self.redirect_error = Some(redirect_missing(self.bridge_ip, &error));
                }
            }
            return;
        }
        // Never point the bridge's port 80 at a port this agent does not
        // hold, including a table an earlier run of the agent left behind.
        self.remove_redirect().await;
        self.redirect_installed = false;
        self.redirect_error = Some(format!(
            "containers' calls to port {CONTAINER_HTTP_PORT} of {} are not redirected: the \
             internal proxy cannot listen on {}",
            self.bridge_ip,
            self.proxy_addr()
        ));
    }

    /// Delete the redirect table; true once it is gone. Without nft there is
    /// no table to remove.
    async fn remove_redirect(&self) -> bool {
        match self
            .nft
            .apply("remove the internal proxy's redirect", &removal_ruleset())
            .await
        {
            Ok(()) | Err(RedirectError::NftMissing) => true,
            Err(error) => {
                warn!(%error, bridge_ip = %self.bridge_ip, "could not remove the internal proxy's redirect while the proxy cannot serve it");
                false
            }
        }
    }

    /// Forget listeners that ended on their own, so this pass starts them
    /// again (and removes the redirect if the proxy's cannot be).
    fn reap_exited(&mut self) {
        if self.proxy.as_ref().is_some_and(Running::exited) {
            self.proxy = None;
            let addr = self.proxy_addr();
            warn!(%addr, "the internal proxy stopped on its own; starting it again");
            self.proxy_error = Some(format!(
                "the internal proxy on {addr} stopped unexpectedly (see the agent's log); the \
                 agent keeps restarting it"
            ));
        }
        if self.direct.as_ref().is_some_and(Running::exited) {
            self.direct = None;
            let addr = self.direct_addr();
            warn!(%addr, "the internal proxy's port-80 fallback stopped on its own; starting it again");
            self.direct_error = Some(format!(
                "the internal proxy on {addr} stopped unexpectedly (see the agent's log); the \
                 agent keeps restarting it"
            ));
        }
    }

    /// What the heartbeat says now.
    pub(crate) fn status(&self) -> InternalRoutingStatus {
        let redirecting = self.proxy.is_some() && self.redirect_installed;
        let mode = if redirecting {
            RoutingMode::Redirect
        } else if self.direct.is_some() {
            RoutingMode::Direct
        } else {
            RoutingMode::Down
        };
        let redirect = if !self.supported() {
            RedirectState::Unsupported
        } else if redirecting {
            RedirectState::Installed
        } else {
            RedirectState::Missing
        };
        let error = (mode == RoutingMode::Down).then(|| self.down_reason());
        InternalRoutingStatus {
            address: self.direct_addr(),
            listening: mode != RoutingMode::Down,
            error,
            attempts: self.attempts,
            mode,
            proxy_address: self.proxy.is_some().then(|| self.proxy_addr()),
            redirect,
            redirect_error: (redirect == RedirectState::Missing)
                .then(|| self.redirect_reason())
                .flatten(),
        }
    }

    /// The reason the redirect is missing, the proxy port's first: it is
    /// what keeps the redirect from being installed at all.
    fn redirect_reason(&self) -> Option<String> {
        match (&self.proxy_error, &self.redirect_error) {
            (Some(proxy), _) => Some(format!(
                "containers' calls to port {CONTAINER_HTTP_PORT} of {} are not redirected: {proxy}",
                self.bridge_ip
            )),
            (None, Some(redirect)) => Some(redirect.clone()),
            (None, None) => None,
        }
    }

    /// Why nothing serves the calls: what to fix first, then what the
    /// fallback ran into.
    fn down_reason(&self) -> String {
        let direct = self
            .direct_error
            .clone()
            .unwrap_or_else(|| "no error reported".to_string());
        if !self.supported() {
            return direct;
        }
        let first = self
            .redirect_reason()
            .unwrap_or_else(|| "the redirect is not installed".to_string());
        format!(
            "{first}. Until it is, the agent tried to listen on {} directly: {direct}",
            self.direct_addr()
        )
    }

    fn log_change(&mut self, status: &InternalRoutingStatus) {
        let reason = status
            .error
            .clone()
            .or_else(|| status.redirect_error.clone());
        let current = (status.mode, reason);
        if self.last_logged.as_ref() == Some(&current) {
            return;
        }
        let address = status.address;
        let proxy = status.proxy_address;
        match (&current.0, &current.1) {
            (RoutingMode::Redirect, _) => info!(
                %address,
                proxy = ?proxy,
                attempts = status.attempts,
                "internal routing: containers' port-80 calls are redirected to the internal proxy"
            ),
            (RoutingMode::Direct, reason) => warn!(
                %address,
                attempts = status.attempts,
                reason = reason.as_deref().unwrap_or(""),
                "internal routing: serving port 80 of the bridge address directly"
            ),
            (RoutingMode::Down, reason) => warn!(
                %address,
                attempts = status.attempts,
                "internal routing is down: {}",
                reason.as_deref().unwrap_or("no error reported")
            ),
        }
        self.last_logged = Some(current);
    }

    /// Resolves when a running listener ends on its own, logging why; never
    /// while none runs. Each task is awaited at most once: the pass that
    /// follows drops it.
    async fn listener_exited(&mut self) {
        async fn ended(running: Option<&mut Running>, addr: SocketAddr) {
            let Some(running) = running else {
                return std::future::pending().await;
            };
            match (&mut running.task).await {
                Ok(()) => warn!(%addr, "internal proxy listener ended"),
                Err(error) => warn!(%addr, %error, "internal proxy listener panicked"),
            }
        }
        let (proxy_addr, direct_addr) = (self.proxy_addr(), self.direct_addr());
        tokio::select! {
            _ = ended(self.proxy.as_mut(), proxy_addr) => {}
            _ = ended(self.direct.as_mut(), direct_addr) => {}
        }
    }

    /// Stop every listener this supervisor started.
    fn stop(&mut self) {
        for running in [self.proxy.take(), self.direct.take()]
            .into_iter()
            .flatten()
        {
            running.stop();
        }
    }

    /// The wait before the next pass.
    fn next_delay(&self) -> Duration {
        if self.failures == 0 {
            REFRESH_INTERVAL
        } else {
            retry_delay(self.failures)
        }
    }
}

fn redirect_missing(bridge_ip: IpAddr, error: &RedirectError) -> String {
    format!(
        "containers' calls to port {CONTAINER_HTTP_PORT} of {bridge_ip} are not redirected to \
         the internal proxy on {}: {error}",
        SocketAddr::new(bridge_ip, INTERNAL_PROXY_PORT)
    )
}

/// Serve internal routing on `bridge_ip` until `shutdown` is notified.
///
/// The redirect is left in place on shutdown: its liveness element expires
/// on its own (ADR-098 D3), and until then calls are refused rather than
/// delivered to whatever holds port 80.
pub async fn supervise(
    bridge_ip: IpAddr,
    store: SharedRouteStore,
    docker: bollard::Docker,
    shutdown: Arc<Notify>,
) {
    let routing = Routing::new(
        bridge_ip,
        cfg!(target_os = "linux"),
        ProxyListeners { store, docker },
        NftCommand::default(),
    );
    run(routing, shutdown).await;
}

async fn run<L: ListenerSpawner, N: NftApply>(mut routing: Routing<L, N>, shutdown: Arc<Notify>) {
    loop {
        let status = routing.pass().await;
        record_status(status);
        let delay = routing.next_delay();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            // A dead listener is replaced now, not at the next refresh: the
            // redirect must not point at a port nobody holds for long.
            _ = routing.listener_exited() => {}
            _ = shutdown.notified() => {
                routing.stop();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn ip() -> IpAddr {
        "10.205.0.1".parse().unwrap()
    }

    /// Held by tests that run the supervisor: it records into the one
    /// process-wide status.
    static SUPERVISOR: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A listener that bound, what tells it to stop, and its task.
    type Bound = (SocketAddr, watch::Receiver<bool>, tokio::task::AbortHandle);

    /// Ports that fail to bind, and with what; the listeners that bound.
    #[derive(Default, Clone)]
    struct FakeListeners {
        refuse: Arc<Mutex<HashMap<u16, std::io::ErrorKind>>>,
        bound: Arc<Mutex<Vec<Bound>>>,
    }

    impl FakeListeners {
        fn refuse(&self, port: u16, kind: std::io::ErrorKind) {
            self.refuse.lock().unwrap().insert(port, kind);
        }
        fn allow(&self, port: u16) {
            self.refuse.lock().unwrap().remove(&port);
        }
        /// Ports bound, not told to stop, and still running.
        fn serving(&self) -> Vec<u16> {
            self.bound
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, stop, task)| {
                    !*stop.borrow() && stop.has_changed().is_ok() && !task.is_finished()
                })
                .map(|(addr, _, _)| addr.port())
                .collect()
        }
        /// End a listener as a crash would, and wait until it has.
        async fn crash(&self, port: u16) {
            let tasks: Vec<_> = self
                .bound
                .lock()
                .unwrap()
                .iter()
                .filter(|(addr, _, _)| addr.port() == port)
                .map(|(_, _, task)| task.clone())
                .collect();
            for task in &tasks {
                task.abort();
            }
            while !tasks.iter().all(tokio::task::AbortHandle::is_finished) {
                tokio::task::yield_now().await;
            }
        }
    }

    impl ListenerSpawner for FakeListeners {
        fn spawn(
            &self,
            addr: SocketAddr,
            stop: watch::Receiver<bool>,
        ) -> BoxFuture<'static, std::io::Result<JoinHandle<()>>> {
            let refused = self.refuse.lock().unwrap().get(&addr.port()).copied();
            let bound = Arc::clone(&self.bound);
            Box::pin(async move {
                if let Some(kind) = refused {
                    return Err(std::io::Error::from(kind));
                }
                let mut serving = stop.clone();
                let task = tokio::spawn(async move {
                    let _ = serving.wait_for(|stop| *stop).await;
                });
                bound
                    .lock()
                    .unwrap()
                    .push((addr, stop, task.abort_handle()));
                Ok(task)
            })
        }
    }

    /// Records every script; fails with `fail` while set.
    #[derive(Default, Clone)]
    struct FakeNft {
        fail: Arc<Mutex<Option<RedirectError>>>,
        applied: Arc<Mutex<Vec<String>>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeNft {
        fn fail_with(&self, error: Option<RedirectError>) {
            *self.fail.lock().unwrap() = error;
        }
        fn last(&self) -> String {
            self.applied
                .lock()
                .unwrap()
                .last()
                .cloned()
                .unwrap_or_default()
        }
    }

    impl NftApply for FakeNft {
        fn apply<'a>(
            &'a self,
            _operation: &'static str,
            ruleset: &'a str,
        ) -> BoxFuture<'a, Result<(), RedirectError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if let Some(error) = self.fail.lock().unwrap().clone() {
                    return Err(error);
                }
                self.applied.lock().unwrap().push(ruleset.to_string());
                Ok(())
            })
        }
    }

    fn routing(listeners: &FakeListeners, nft: &FakeNft) -> Routing<FakeListeners, FakeNft> {
        Routing::new(ip(), true, listeners.clone(), nft.clone())
    }

    #[tokio::test]
    async fn the_proxy_binds_its_own_port_then_installs_the_redirect_and_leaves_port_80_alone() {
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        let mut routing = routing(&listeners, &nft);

        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Redirect);
        assert!(status.listening);
        assert_eq!(status.error, None);
        assert_eq!(status.redirect, RedirectState::Installed);
        assert_eq!(status.redirect_error, None);
        assert_eq!(status.address, "10.205.0.1:80".parse().unwrap());
        assert_eq!(
            status.proxy_address,
            Some("10.205.0.1:591".parse().unwrap())
        );
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);
        assert!(
            nft.last().contains("dnat to 10.205.0.1:591"),
            "{}",
            nft.last()
        );
        assert!(!nft.last().contains("iifname"), "{}", nft.last());
        assert_eq!(routing.next_delay(), REFRESH_INTERVAL);

        // Each pass replaces the table, which renews the liveness element.
        routing.pass().await;
        assert_eq!(nft.applied.lock().unwrap().len(), 2);
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);
    }

    #[tokio::test]
    async fn without_nft_the_agent_falls_back_to_port_80_and_says_why() {
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        nft.fail_with(Some(RedirectError::NftMissing));
        let mut routing = routing(&listeners, &nft);

        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Direct);
        assert!(status.listening, "the fallback serves the calls");
        assert_eq!(status.error, None);
        assert_eq!(status.redirect, RedirectState::Missing);
        let reason = status.redirect_error.expect("why the redirect is missing");
        assert!(
            reason.contains("nft (nftables) is not installed"),
            "{reason}"
        );
        assert!(reason.contains("apt install nftables"), "{reason}");
        let mut serving = listeners.serving();
        serving.sort_unstable();
        assert_eq!(serving, vec![80, INTERNAL_PROXY_PORT]);
        assert!(routing.next_delay() < REFRESH_INTERVAL);

        // nft installed later: the redirect goes in and port 80 is released.
        nft.fail_with(None);
        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Redirect);
        assert_eq!(status.redirect_error, None);
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);
    }

    #[tokio::test]
    async fn both_failing_reports_down_with_the_redirect_fix_first_and_never_says_free_port_80() {
        let listeners = FakeListeners::default();
        listeners.refuse(80, std::io::ErrorKind::AddrInUse);
        let nft = FakeNft::default();
        nft.fail_with(Some(RedirectError::Rejected {
            operation: "install the internal proxy's redirect",
            exit_code: 1,
            reason: "Error: Could not process rule: No such file or directory".into(),
        }));
        let mut routing = routing(&listeners, &nft);

        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Down);
        assert!(!status.listening);
        let error = status.error.expect("why routing is down");
        assert!(
            error.starts_with("containers' calls to port 80 of 10.205.0.1 are not redirected"),
            "{error}"
        );
        assert!(
            error.contains("nft refused to install the internal proxy's redirect"),
            "{error}"
        );
        assert!(error.contains("10.205.0.1:80 directly"), "{error}");
        assert!(
            error.contains("the fix is the redirect, not the port"),
            "{error}"
        );
        assert!(!error.to_lowercase().contains("free port"), "{error}");
        // The proxy port stays bound for the next attempt at the redirect.
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);
        assert_eq!(
            status.proxy_address,
            Some("10.205.0.1:591".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn a_held_proxy_port_removes_any_redirect_and_falls_back() {
        let listeners = FakeListeners::default();
        listeners.refuse(INTERNAL_PROXY_PORT, std::io::ErrorKind::AddrInUse);
        let nft = FakeNft::default();
        let mut routing = routing(&listeners, &nft);

        let status = routing.pass().await;
        // A redirect to a port this agent does not hold would deliver every
        // internal request to whoever holds it.
        assert_eq!(nft.last(), removal_ruleset());
        assert!(!nft
            .applied
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("dnat")));
        assert_eq!(status.mode, RoutingMode::Direct);
        assert_eq!(status.proxy_address, None);
        let reason = status.redirect_error.expect("why");
        assert!(reason.contains("sudo ss -ltnp 'sport = :591'"), "{reason}");
        assert_eq!(listeners.serving(), vec![80]);

        // Freed: the proxy binds, the redirect goes in, port 80 is released.
        listeners.allow(INTERNAL_PROXY_PORT);
        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Redirect);
        assert!(nft.last().contains("dnat to 10.205.0.1:591"));
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);
    }

    #[tokio::test]
    async fn a_held_proxy_port_and_port_80_name_the_proxy_port_first() {
        let listeners = FakeListeners::default();
        listeners.refuse(INTERNAL_PROXY_PORT, std::io::ErrorKind::AddrInUse);
        listeners.refuse(80, std::io::ErrorKind::AddrInUse);
        let nft = FakeNft::default();
        let mut routing = routing(&listeners, &nft);

        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Down);
        let error = status.error.unwrap();
        let proxy = error.find(":591").expect("names the proxy port");
        let fallback = error
            .find("10.205.0.1:80 directly")
            .expect("then the fallback");
        assert!(proxy < fallback, "{error}");
        assert!(!error.to_lowercase().contains("free port"), "{error}");
    }

    #[tokio::test]
    async fn a_failing_refresh_keeps_serving_through_the_fallback() {
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        let mut routing = routing(&listeners, &nft);
        assert_eq!(routing.pass().await.mode, RoutingMode::Redirect);

        nft.fail_with(Some(RedirectError::Timeout {
            operation: "install the internal proxy's redirect",
            timeout_secs: 10,
        }));
        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Direct);
        assert!(status.listening);
        assert!(status.redirect_error.unwrap().contains("did not install"));
    }

    #[tokio::test]
    async fn off_linux_only_port_80_is_used_and_the_redirect_is_unsupported() {
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        let mut routing = Routing::new(ip(), false, listeners.clone(), nft.clone());

        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Direct);
        assert_eq!(status.redirect, RedirectState::Unsupported);
        assert_eq!(status.redirect_error, None);
        assert_eq!(nft.calls.load(Ordering::SeqCst), 0, "nft is never run");
        assert_eq!(listeners.serving(), vec![80]);
        // Direct-only is the healthy state here: no fast retries.
        let _ = routing.pass().await;
        assert_eq!(listeners.serving(), vec![80], "bound once");

        let held = FakeListeners::default();
        held.refuse(80, std::io::ErrorKind::AddrInUse);
        let mut routing = Routing::new(ip(), false, held, FakeNft::default());
        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Down);
        let error = status.error.unwrap();
        assert!(
            error.contains("cannot redirect them to a port of its own"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_ipv6_bridge_address_removes_a_stale_redirect_once_and_falls_back() {
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        let v6: IpAddr = "fd00::1".parse().unwrap();
        let mut routing = Routing::new(v6, true, listeners.clone(), nft.clone());

        let status = routing.pass().await;
        // Whatever an earlier agent installed is gone; nothing is installed.
        assert_eq!(*nft.applied.lock().unwrap(), vec![removal_ruleset()]);
        assert_eq!(status.mode, RoutingMode::Direct);
        assert_eq!(status.redirect, RedirectState::Missing);
        assert!(status.redirect_error.unwrap().contains("is not IPv4"));
        assert_eq!(listeners.serving(), vec![80]);

        routing.pass().await;
        assert_eq!(nft.calls.load(Ordering::SeqCst), 1, "removed once");

        // A failed removal is tried again on the next pass.
        let nft = FakeNft::default();
        nft.fail_with(Some(RedirectError::Timeout {
            operation: "remove the internal proxy's redirect",
            timeout_secs: 10,
        }));
        let mut routing = Routing::new(v6, true, FakeListeners::default(), nft.clone());
        routing.pass().await;
        routing.pass().await;
        assert_eq!(nft.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_proxy_that_dies_is_restarted_or_its_redirect_removed() {
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        let mut routing = routing(&listeners, &nft);
        assert_eq!(routing.pass().await.mode, RoutingMode::Redirect);

        // It dies and its port is free again: started again, redirect kept.
        listeners.crash(INTERNAL_PROXY_PORT).await;
        assert!(listeners.serving().is_empty());
        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Redirect);
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);
        assert!(nft.last().contains("dnat to 10.205.0.1:591"));

        // It dies and something else takes its port: the redirect goes, and
        // port 80 is served directly.
        listeners.crash(INTERNAL_PROXY_PORT).await;
        listeners.refuse(INTERNAL_PROXY_PORT, std::io::ErrorKind::AddrInUse);
        let status = routing.pass().await;
        assert_eq!(nft.last(), removal_ruleset());
        assert_eq!(status.mode, RoutingMode::Direct);
        assert_eq!(status.proxy_address, None);
        assert_eq!(listeners.serving(), vec![80]);

        // The fallback dies too: started again on the next pass.
        listeners.crash(80).await;
        assert!(listeners.serving().is_empty());
        let status = routing.pass().await;
        assert_eq!(status.mode, RoutingMode::Direct);
        assert_eq!(listeners.serving(), vec![80]);
    }

    #[tokio::test]
    async fn stopping_releases_every_listener_and_keeps_the_redirect() {
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        nft.fail_with(Some(RedirectError::NftMissing));
        let mut routing = routing(&listeners, &nft);
        routing.pass().await;
        assert_eq!(listeners.serving().len(), 2);
        let scripts = nft.calls.load(Ordering::SeqCst);
        routing.stop();
        assert!(listeners.serving().is_empty());
        assert_eq!(
            nft.calls.load(Ordering::SeqCst),
            scripts,
            "no removal on stop"
        );
    }

    #[test]
    fn a_sandbox_egress_proxy_is_sent_to_the_proxy_listener_itself() {
        let status = |mode, listening, proxy: Option<&str>| InternalRoutingStatus {
            address: "10.205.0.1:80".parse().unwrap(),
            listening,
            error: None,
            attempts: 1,
            mode,
            proxy_address: proxy.map(|proxy| proxy.parse().unwrap()),
            redirect: RedirectState::Installed,
            redirect_error: None,
        };
        let own: SocketAddr = "10.205.0.1:591".parse().unwrap();
        let port80: SocketAddr = "10.205.0.1:80".parse().unwrap();
        assert_eq!(
            status(RoutingMode::Redirect, true, Some("10.205.0.1:591")).serving_address(),
            Some(own)
        );
        // The fallback serves port 80; its own port, when held, is still
        // preferred.
        assert_eq!(
            status(RoutingMode::Direct, true, Some("10.205.0.1:591")).serving_address(),
            Some(own)
        );
        assert_eq!(
            status(RoutingMode::Direct, true, None).serving_address(),
            Some(port80)
        );
        assert_eq!(
            status(RoutingMode::Down, false, None).serving_address(),
            None
        );
        assert_eq!(
            status(RoutingMode::Redirect, false, Some("10.205.0.1:591")).serving_address(),
            None
        );
    }

    #[tokio::test]
    async fn the_supervisor_records_each_pass_and_stops_on_shutdown() {
        let _serial = SUPERVISOR.lock().await;
        *STATUS.write() = None;
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        let shutdown = Arc::new(Notify::new());
        let task = tokio::spawn(run(routing(&listeners, &nft), Arc::clone(&shutdown)));
        for _ in 0..2000 {
            if heartbeat_value().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let value = heartbeat_value().expect("recorded");
        assert_eq!(
            value,
            serde_json::json!({
                "address": "10.205.0.1:80",
                "listening": true,
                "error": null,
                "attempts": 1,
                "mode": "redirect",
                "proxy_address": "10.205.0.1:591",
                "redirect": "installed",
                "redirect_error": null,
            })
        );
        // `notify_waiters` only reaches a waiting supervisor: repeat until it
        // has stopped (it may be between a pass and its wait).
        for _ in 0..2000 {
            shutdown.notify_waiters();
            if task.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(task.is_finished(), "the supervisor stops on shutdown");
        task.await.unwrap();
        assert!(listeners.serving().is_empty());
    }

    #[tokio::test]
    async fn a_dead_proxy_is_replaced_at_once_not_at_the_next_refresh() {
        let _serial = SUPERVISOR.lock().await;
        *STATUS.write() = None;
        let listeners = FakeListeners::default();
        let nft = FakeNft::default();
        let shutdown = Arc::new(Notify::new());
        let task = tokio::spawn(run(routing(&listeners, &nft), Arc::clone(&shutdown)));
        let bound = || listeners.bound.lock().unwrap().len();
        for _ in 0..2000 {
            if bound() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);

        // Healthy, so the next refresh is 30 s away; the crash must not wait
        // for it.
        listeners.crash(INTERNAL_PROXY_PORT).await;
        for _ in 0..2000 {
            if bound() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(bound(), 2, "restarted well before REFRESH_INTERVAL");
        assert_eq!(listeners.serving(), vec![INTERNAL_PROXY_PORT]);
        assert_eq!(nft.applied.lock().unwrap().len(), 2, "redirect renewed");

        for _ in 0..2000 {
            shutdown.notify_waiters();
            if task.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        task.await.unwrap();
    }

    #[test]
    fn bind_errors_are_explained_per_listener() {
        let proxy: SocketAddr = "10.205.0.1:591".parse().unwrap();
        let direct: SocketAddr = "10.205.0.1:80".parse().unwrap();
        let in_use = std::io::Error::from(std::io::ErrorKind::AddrInUse);

        let held = explain_bind_error(&in_use, proxy, ListenerRole::Proxy);
        assert!(held.contains("Temps reserves port 591"), "{held}");
        assert!(held.contains("sudo ss -ltnp 'sport = :591'"), "{held}");

        let fallback = explain_bind_error(&in_use, direct, ListenerRole::Fallback);
        assert!(fallback.contains("0.0.0.0:80"), "{fallback}");
        assert!(fallback.contains("may keep it"), "{fallback}");

        for role in [
            ListenerRole::Proxy,
            ListenerRole::Fallback,
            ListenerRole::DirectOnly,
        ] {
            for addr in [proxy, direct] {
                let text = explain_bind_error(&in_use, addr, role).to_lowercase();
                assert!(!text.contains("free port"), "{text}");
            }
        }

        let missing = explain_bind_error(
            &std::io::Error::from(std::io::ErrorKind::AddrNotAvailable),
            proxy,
            ListenerRole::Proxy,
        );
        assert!(missing.contains("overlay bridge is not up"), "{missing}");
        let denied = explain_bind_error(
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            proxy,
            ListenerRole::Proxy,
        );
        assert!(denied.contains("CAP_NET_BIND_SERVICE"), "{denied}");
    }

    #[test]
    fn retries_back_off_from_one_second_to_the_refresh_interval() {
        let delays: Vec<u64> = (1..=8).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_delay(u32::MAX), REFRESH_INTERVAL);
    }
}
