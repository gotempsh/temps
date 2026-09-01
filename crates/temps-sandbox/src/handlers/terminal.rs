// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Interactive terminal for a sandbox: `GET /v1/sandboxes/{id}/terminal`.
//!
//! Bridges a browser/CLI WebSocket to the in-sandbox `temps-pty-agent`
//! (ADR-008). The agent owns the PTY and keeps it alive with zero
//! subscribers, so dropping this connection does **not** kill whatever is
//! running in the tab — closing your laptop mid-`claude` and reattaching
//! later resumes the same session, with a replay of recent scrollback.
//!
//! # Protocol on the wire
//!
//! Deliberately the same shape `temps-deployments`' container terminal
//! already speaks, so one xterm.js client works against both:
//!
//! - **Binary frames** — raw PTY bytes, both directions.
//! - **Text frames (client → server)** — JSON control:
//!   `{"type":"resize","cols":N,"rows":N}`
//! - **Text frames (server → client)** — JSON events:
//!   `{"type":"ready","tab_id":"…","existed":bool}` once the tab is open,
//!   `{"type":"exit","code":N}` when the program exits,
//!   `{"type":"error","message":"…"}` for anything that went wrong.
//!
//! The agent's own framed protocol is terminated here rather than passed
//! through, so clients don't need to implement it.
//!
//! # Why the heartbeat exists
//!
//! The expiration sweeper suspends a sandbox that has been idle for
//! `timeout_secs`, and "activity" is otherwise only recorded by exec and
//! filesystem calls. Someone sitting in a terminal talking to an AI CLI
//! makes neither — so without the periodic `touch` below, the sweeper would
//! stop the container out from under a live session, killing the agent and
//! every PTY with it (`/run/temps-pty` is tmpfs). An attached terminal is
//! activity; this is what makes that true.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use temps_auth::{permissions::Permission, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_pty_agent::protocol as pty;
use tokio::io::AsyncWriteExt;
use tokio_util::codec::FramedRead;
use tokio_util::io::StreamReader;
use utoipa::ToSchema;

use super::sandboxes::sandbox_permission_guard;
use super::SandboxAppState;

/// How often an attached terminal reports activity. Comfortably inside the
/// 60s minimum `timeout_secs` so even the shortest-lived sandbox can't be
/// swept while someone is typing in it.
const ACTIVITY_HEARTBEAT: Duration = Duration::from_secs(20);

/// Scrollback replayed on attach. Matches the agent's default ring size —
/// asking for more just gets clamped agent-side.
const REPLAY_BYTES: u32 = 64 * 1024;

/// Concurrent terminal attachments allowed across the whole process.
///
/// Every attach holds a `docker exec` and a bollard connection for the life
/// of the session, and writes an activity row every [`ACTIVITY_HEARTBEAT`].
/// Unbounded, that is a self-service denial of service from any account with
/// `sandboxes:write` on the 3 vCPU / 4 GB reference host. A global cap is
/// crude but bounds the daemon pressure; callers past it get a 429 telling
/// them to close a session rather than a hang.
const MAX_CONCURRENT_TERMINALS: usize = 32;

/// Permits for [`MAX_CONCURRENT_TERMINALS`]. A process-wide static rather
/// than state on `SandboxAppState` so the bound holds even if a future
/// refactor builds more than one router.
static TERMINAL_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_TERMINALS);

/// Concurrent terminals a *single* account may hold.
///
/// [`MAX_CONCURRENT_TERMINALS`] alone bounds the host, but it is global: one
/// account opening 32 sessions locks every other user out of the feature with
/// a 429. Nobody drives more than a handful of terminals by hand, so a small
/// per-user sub-cap turns an instance-wide denial of service into a limit the
/// offending account hits by itself.
const MAX_TERMINALS_PER_USER: usize = 4;

/// Live attachment count per user id, guarding [`MAX_TERMINALS_PER_USER`].
/// A plain `Mutex` is fine here: it is taken twice per *session*, never on a
/// data path.
static TERMINALS_PER_USER: std::sync::Mutex<Option<HashMap<i32, usize>>> =
    std::sync::Mutex::new(None);

/// RAII counter for [`TERMINALS_PER_USER`]. Decrements on drop, so every exit
/// path — clean close, error, panic in the session future — returns the slot.
struct UserSlot(i32);

impl UserSlot {
    /// Returns `None` when this user is already at their cap.
    fn acquire(user_id: i32) -> Option<Self> {
        // A poisoned `HashMap<i32, usize>` is not logically corrupt — the
        // critical section is one entry update — so recover rather than turn
        // every future attach into a permanent instance-wide 429 that blames
        // the user's own session count.
        let mut guard = TERMINALS_PER_USER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let counts = guard.get_or_insert_with(HashMap::new);
        let entry = counts.entry(user_id).or_insert(0);
        if *entry >= MAX_TERMINALS_PER_USER {
            return None;
        }
        *entry += 1;
        Some(UserSlot(user_id))
    }
}

impl Drop for UserSlot {
    fn drop(&mut self) {
        // Recover from poisoning here too: bailing out would leak the slot
        // permanently, which is the one outcome worse than a poisoned map.
        let mut guard = TERMINALS_PER_USER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(counts) = guard.as_mut() {
            if let Some(entry) = counts.get_mut(&self.0) {
                *entry = entry.saturating_sub(1);
                // Don't let the map grow one entry per user id forever.
                if *entry == 0 {
                    counts.remove(&self.0);
                }
            }
        }
    }
}

/// Largest WebSocket message accepted from a terminal client.
///
/// A terminal client sends keystrokes and small JSON control frames; 64 KiB is
/// already generous for a paste. axum's default is 64 MiB, which times
/// [`MAX_CONCURRENT_TERMINALS`] is ~2 GiB of attacker-controlled buffering on
/// the 3 vCPU / 4 GB reference host — and the agent-side
/// `MAX_FRAME_BYTES` check only runs *after* the whole message is buffered,
/// so it does not bound this.
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024;

/// How long a session may sit with no traffic from the client before we close
/// it, and how many unanswered pings that allows.
///
/// Without this a half-open connection (closed laptop, NAT drop, dead Wi-Fi)
/// leaves the heartbeat as the only firing branch, so the session keeps
/// calling `touch` forever: the sandbox is never swept, the `docker exec`
/// stays open, and a global slot is never returned. A live terminal answers
/// pings; a dead one does not.
const MISSED_PONGS_BEFORE_CLOSE: u32 = 3;

/// How long a single write to the agent may block before the session gives up.
///
/// Writes happen in a `select!` handler body, which tokio does not cancel, so
/// an agent that has stopped draining its socket blocks every other branch
/// with it. Bounding the write is what keeps the session ceiling and the ping
/// timeout meaningful.
const AGENT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard ceiling on a single attachment.
///
/// Authorization is evaluated once, at upgrade, and never revalidated — so a
/// logout, an account deactivation, a password change with
/// `revoke_other_sessions`, or a deleted API key leave this shell working.
/// (The container-exec terminal has the same shape.) Bounding the session
/// bounds that window: the client reconnects transparently and is
/// re-authorized when it does.
const MAX_SESSION_DURATION: Duration = Duration::from_secs(12 * 60 * 60);

/// Longest accepted `tab` identifier, and the character set.
///
/// The agent keys tabs by this string and allocates a PTY plus a 64 KiB
/// scrollback ring per distinct value, so an unbounded one is a memory lever
/// inside the container. It is also interpolated into operator logs, where a
/// newline would forge log lines.
fn validate_tab(tab: &str) -> Result<(), Problem> {
    let ok = !tab.is_empty()
        && tab.len() <= 64
        && tab
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        return Ok(());
    }
    Err(problemdetails::new(StatusCode::BAD_REQUEST)
        .with_title("Invalid Tab Name")
        .with_detail(
            "tab must be 1-64 characters of ASCII letters, digits, '-' or '_'".to_string(),
        ))
}

/// Query parameters for the terminal upgrade.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TerminalParams {
    /// Which tab to attach to. Re-using a tab id reattaches to the running
    /// program instead of starting a second one — that is how a `claude`
    /// session survives a disconnect. Defaults to `"main"`.
    #[serde(default)]
    pub tab: Option<String>,
    /// Program to run when the tab is created. Ignored when the tab already
    /// exists. Defaults to an interactive login shell.
    #[serde(default)]
    pub cmd: Option<String>,
    /// Initial terminal width in columns. Defaults to 80.
    #[serde(default)]
    pub cols: Option<u16>,
    /// Initial terminal height in rows. Defaults to 24.
    #[serde(default)]
    pub rows: Option<u16>,
}

/// Client → server control message on a text frame.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientControl {
    Resize { cols: u16, rows: u16 },
}

/// Server → client event on a text frame.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ServerEvent<'a> {
    Ready {
        tab_id: &'a str,
        existed: bool,
    },
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Error {
        message: String,
    },
}

fn default_shell_cmd() -> String {
    // Login shell so ~/.bashrc runs and the AI CLIs are on PATH. The image
    // seeds PATH there precisely because non-login shells miss them.
    "/bin/bash -l".to_string()
}

/// Wrap the requested program so the PTY is in a sane line discipline before
/// it starts.
///
/// The in-sandbox agent used to call `cfmakeraw()` when creating the PTY,
/// which clears `ECHO`. Bash's readline assumes something else is echoing, so
/// the user typed completely blind: commands ran, output appeared, keystrokes
/// were invisible.
///
/// That is fixed in the agent, **but the agent binary is compiled into the
/// sandbox image**. Every sandbox built from an image that predates the fix
/// carries the old behaviour, and rebuilding the image is an operator action
/// the person trying to use a terminal should not have to perform first. So
/// the host normalises the terminal itself, which costs one `stty` per tab and
/// works on every image.
///
/// `stty sane` is safe in front of anything: full-screen programs (claude,
/// vim) set their own termios on startup regardless, and starting them from a
/// sane state is what a normal terminal would give them anyway. Failures are
/// swallowed — a sandbox image without `stty` should still get a shell.
///
/// Once no supported image ships the old agent this wrapper can go; until
/// then, removing it silently reintroduces a shell you cannot type into.
///
/// # Why the command is nested rather than spliced
///
/// The obvious form — `stty sane; exec {cmd}` — splices at *statement* level,
/// so any shell operator inside `cmd` binds to the wrapper instead of to the
/// user's program. `--cmd "cd /app && npm start"` became
/// `exec cd /app && npm start`: `exec cd` fails (`cd` is a builtin, not a
/// program), and `npm start` never ran. Nesting in an inner `sh -c` hands the
/// original string to a shell verbatim, so it parses exactly as the caller
/// wrote it.
fn wrap_with_sane_termios(cmd: &str) -> String {
    // Single-quote the whole command and escape any embedded single quote by
    // the standard `'\''` dance: close, emit an escaped quote, reopen.
    let quoted = cmd.replace('\'', r"'\''");
    format!("stty sane 2>/dev/null; exec /bin/sh -c '{quoted}'")
}

/// Attach an interactive terminal to a sandbox.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/terminal",
    params(
        ("id" = String, Path, description = "Sandbox public ID"),
        ("tab" = Option<String>, Query, description = "Tab to attach to (default \"main\"); reusing an id reattaches to the running program"),
        ("cmd" = Option<String>, Query, description = "Program to launch when the tab is created (default: login shell)"),
        ("cols" = Option<u16>, Query, description = "Initial columns (default 80)"),
        ("rows" = Option<u16>, Query, description = "Initial rows (default 24)"),
    ),
    responses(
        (status = 101, description = "WebSocket established; PTY attached"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Sandbox not found"),
        (status = 409, description = "Sandbox is not in a state that can be attached to"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn terminal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<TerminalParams>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    // This endpoint launches a PTY and forwards `?cmd=` to the agent, which
    // spawns it via `/bin/sh -c` — it is arbitrary code execution inside the
    // sandbox, exactly like the /exec routes. It must therefore require
    // `SandboxesExec`, not the lifecycle/file-write `SandboxesWrite` scope,
    // or a token deliberately granted `sandboxes:write` without
    // `sandboxes:exec` gets execution anyway and the split is meaningless.
    sandbox_permission_guard(&auth, Permission::SandboxesExec, Permission::ProjectsWrite)?;

    let tab = params.tab.clone().unwrap_or_else(|| "main".to_string());
    validate_tab(&tab)?;

    // Take a slot before doing any work. Held for the whole session by moving
    // the permit into the upgrade closure; dropped when the session ends.
    let permit = TERMINAL_SLOTS.try_acquire().map_err(|_| {
        problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
            .with_title("Too Many Terminals")
            .with_detail(format!(
                "This instance already has {} terminal sessions open. \
                 Close one and retry.",
                MAX_CONCURRENT_TERMINALS
            ))
    })?;

    // Per-user sub-cap, so one account cannot take every global slot and lock
    // the whole instance out of the feature.
    let user_slot = UserSlot::acquire(auth.user_id()).ok_or_else(|| {
        problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
            .with_title("Too Many Terminals")
            .with_detail(format!(
                "You already have {} terminal sessions open. \
                 Close one and retry.",
                MAX_TERMINALS_PER_USER
            ))
    })?;

    // Resolve *before* the upgrade so ownership failures, a missing sandbox,
    // or a stopped ephemeral one surface as a real HTTP status instead of a
    // WebSocket that opens and immediately dies. This also wakes a suspended
    // workspace (ADR-036), which is exactly what should happen when someone
    // opens a terminal on it.
    let (row, internal_id) = state
        .sandbox_service
        .resolve_id(&id, auth.user_id())
        .await?;

    let handle = state
        .sandbox_service
        .registry()
        .get(internal_id, &id)
        .await
        .map_err(|e| {
            let err = crate::services::sandbox_service::SandboxService::provider_err(&id, e);
            Problem::from(err)
        })?;

    let attachment = state
        .sandbox_service
        .registry()
        .provider()
        .attach_pty(&handle)
        .await
        .map_err(|e| {
            // Most likely cause on a real deployment: a sandbox created from
            // a custom image that doesn't carry the PTY agent. Say so, rather
            // than leaving the user with a terminal that never echoes.
            problemdetails::new(StatusCode::CONFLICT)
                .with_title("Terminal Unavailable")
                .with_detail(format!(
                    "Could not attach a terminal to sandbox {}: {}. \
                     Interactive terminals need the temps PTY agent, which \
                     ships in the default sandbox image.",
                    id, e
                ))
        })?;

    let service = state.sandbox_service.clone();
    let timeout_secs = row.timeout_secs;
    let cmd = wrap_with_sane_termios(&params.cmd.unwrap_or_else(default_shell_cmd));
    let cols = params.cols.unwrap_or(80);
    let rows = params.rows.unwrap_or(24);
    // Take the working directory from the provider handle, not the DB row.
    // The handle is what the backend actually created; rows written before
    // the work_dir fix carry a "/workspace" that does not exist in the
    // image, and spawning a shell there fails with a bare
    // "spawn_failed: No such file or directory".
    let work_dir = handle.work_dir.to_string_lossy().to_string();

    Ok(ws
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            // Held for the session's lifetime; released when this future ends,
            // however it ends.
            let _permit = permit;
            let _user_slot = user_slot;
            let session = TerminalSession {
                socket,
                attachment,
                service,
                sandbox_id: id,
                internal_id,
                timeout_secs,
                tab,
                cmd,
                cols,
                rows,
                work_dir,
            };
            session.run().await;
        }))
}

struct TerminalSession {
    socket: WebSocket,
    attachment: temps_agents::sandbox::PtyAttachment,
    service: Arc<crate::services::sandbox_service::SandboxService>,
    sandbox_id: String,
    internal_id: i32,
    timeout_secs: i32,
    tab: String,
    cmd: String,
    cols: u16,
    rows: u16,
    work_dir: String,
}

impl TerminalSession {
    async fn run(self) {
        let TerminalSession {
            socket,
            attachment,
            service,
            sandbox_id,
            internal_id,
            timeout_secs,
            tab,
            cmd,
            cols,
            rows,
            work_dir,
        } = self;

        let (mut ws_tx, mut ws_rx) = socket.split();
        let mut agent_in = attachment.input;

        // Map the provider's typed errors into io::Error so the frame reader
        // can consume the stream. The message is preserved — it's what the
        // user sees if the relay dies.
        let agent_bytes = attachment
            .output
            .map(|chunk| chunk.map_err(|e| std::io::Error::other(e.to_string())));
        // `FramedRead` + `FrameCodec`, never `read_frame`: this stream is
        // multiplexed in the `select!` below against client input and the
        // activity heartbeat, so the read is cancelled constantly. The codec
        // buffers across polls; `read_frame`'s `read_exact` pair would drop
        // whatever it had already consumed and desynchronise the stream on
        // the first keystroke that landed mid-frame.
        let mut agent_out = FramedRead::new(StreamReader::new(agent_bytes), pty::FrameCodec);

        // OPEN the tab. Reattaches when it already exists, which is what
        // makes a long-running CLI survive disconnection.
        let open = pty::OpenRequest {
            tab_id: tab.clone(),
            kind: "shell".to_string(),
            cmd,
            cols,
            rows,
            replay_bytes: REPLAY_BYTES,
            label: Some(tab.clone()),
            cwd: Some(work_dir),
            env: None,
        };
        if let Err(e) = pty::write_json_frame(&mut agent_in, pty::OP_OPEN, &open).await {
            send_error(&mut ws_tx, format!("failed to open terminal tab: {}", e)).await;
            return;
        }

        tracing::info!(
            "Terminal attached to sandbox {} (internal {}), tab '{}'",
            sandbox_id,
            internal_id,
            tab
        );

        let mut heartbeat = tokio::time::interval(ACTIVITY_HEARTBEAT);
        // The first tick fires immediately: attaching is itself activity, and
        // it means a sandbox seconds from expiry is rescued on connect.
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Liveness. `touch` on a tick keeps the sweeper off a sandbox someone
        // is using — but only a *live* client should get that. Each tick pings
        // and counts; any frame from the client (including the pong) clears
        // the count. After MISSED_PONGS_BEFORE_CLOSE unanswered pings we treat
        // the peer as gone, stop touching, and let the sandbox suspend
        // normally. Without this a half-open socket pins the container
        // forever, which is exactly the idle timeout this PR set out to fix.
        let mut unanswered_pings: u32 = 0;
        let deadline = tokio::time::sleep(MAX_SESSION_DURATION);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                // Bounded session: the client reconnects and is re-authorized.
                _ = &mut deadline => {
                    send_error(
                        &mut ws_tx,
                        "terminal session reached its maximum duration; reconnect to continue"
                            .to_string(),
                    )
                    .await;
                    break;
                }

                // Keep the sweeper honest while someone is attached.
                _ = heartbeat.tick() => {
                    if unanswered_pings >= MISSED_PONGS_BEFORE_CLOSE {
                        tracing::debug!(
                            "Terminal on sandbox {} tab '{}' stopped answering pings; closing",
                            sandbox_id,
                            tab
                        );
                        break;
                    }
                    service.touch(internal_id, timeout_secs).await;
                    // A closed receive window must not wedge the whole select!
                    // loop (no more heartbeats, no more reads). Time it out
                    // and drop the session instead.
                    match tokio::time::timeout(
                        ACTIVITY_HEARTBEAT,
                        ws_tx.send(Message::Ping(Vec::new().into())),
                    )
                    .await
                    {
                        Ok(Ok(())) => unanswered_pings += 1,
                        _ => break,
                    }
                }

                // Agent → client. Cancellation-safe: see the codec above.
                frame = agent_out.next() => {
                    match frame {
                        Some(Ok((op, payload))) => {
                            if !forward_agent_frame(&mut ws_tx, op, payload).await {
                                break;
                            }
                        }
                        // Clean EOF: the relay exited (container stopped, or
                        // the agent went away). Nothing more to read.
                        None => break,
                        Some(Err(e)) => {
                            send_error(&mut ws_tx, format!("terminal stream failed: {}", e)).await;
                            break;
                        }
                    }
                }

                // Client → agent.
                msg = ws_rx.next() => {
                    // Anything at all from the peer proves it is alive.
                    unanswered_pings = 0;
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            // Bounded: this write is in a handler body, which
                            // `select!` does not cancel, so an agent that has
                            // stopped draining its socket would otherwise wedge
                            // the whole loop — the deadline and heartbeat
                            // branches stop being polled, defeating both
                            // MAX_SESSION_DURATION and the ping timeout while
                            // still holding a global permit and a docker exec.
                            // A tab whose program never reads stdin (say
                            // `?cmd=sleep 3600`) fills the PTY buffer and does
                            // exactly that.
                            match tokio::time::timeout(
                                AGENT_WRITE_TIMEOUT,
                                pty::write_frame(&mut agent_in, pty::OP_INPUT, &data),
                            )
                            .await
                            {
                                Ok(Ok(())) => {}
                                Ok(Err(e)) => {
                                    send_error(
                                        &mut ws_tx,
                                        format!("could not forward input: {}", e),
                                    )
                                    .await;
                                    break;
                                }
                                Err(_) => {
                                    send_error(
                                        &mut ws_tx,
                                        "the program in this tab stopped reading input; \
                                         detaching. Reattach to try again."
                                            .to_string(),
                                    )
                                    .await;
                                    break;
                                }
                            }
                        }
                        Some(Ok(Message::Text(text))) => {
                            if !handle_client_control(&mut agent_in, &text).await {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                        Some(Err(e)) => {
                            // Where an oversized paste actually lands: the
                            // WebSocket cap (MAX_WS_MESSAGE_BYTES) is enforced
                            // by the *reader*, so the message never reaches the
                            // frame writer's size check. Without this the
                            // terminal simply vanished, which is the behaviour
                            // the message cap was supposed to replace.
                            send_error(
                                &mut ws_tx,
                                format!(
                                    "connection closed: {} (input is limited to {} KiB per message)",
                                    e,
                                    MAX_WS_MESSAGE_BYTES / 1024
                                ),
                            )
                            .await;
                            break;
                        }
                    }
                }
            }
        }

        // DETACH rather than KILL: the tab and its program stay alive for the
        // next attach. This is the whole point — a dropped connection must
        // not cost the user their session.
        let _ = pty::write_frame(&mut agent_in, pty::OP_DETACH, &[]).await;
        let _ = agent_in.shutdown().await;
        let _ = ws_tx.close().await;

        // One last activity record so a sandbox isn't swept moments after a
        // long session ends.
        service.touch(internal_id, timeout_secs).await;

        tracing::debug!(
            "Terminal detached from sandbox {}, tab '{}'",
            sandbox_id,
            tab
        );
    }
}

/// Translate one agent frame onto the WebSocket. Returns false when the
/// session should end.
async fn forward_agent_frame(
    ws_tx: &mut futures::stream::SplitSink<WebSocket, Message>,
    op: u8,
    payload: Vec<u8>,
) -> bool {
    match op {
        pty::OP_OUTPUT => ws_tx.send(Message::Binary(payload.into())).await.is_ok(),
        pty::OP_OPENED => {
            let opened: Option<pty::OpenedResponse> = serde_json::from_slice(&payload)
                .inspect_err(|e| {
                    // Falling back to a default silently makes a
                    // protocol-version mismatch look like normal
                    // operation. Say which frame failed.
                    tracing::warn!(
                        "terminal: could not decode OPENED frame from the pty agent: {}",
                        e
                    );
                })
                .ok();
            let (tab_id, existed) = opened
                .as_ref()
                .map(|o| (o.tab_id.as_str(), o.existed))
                .unwrap_or(("", false));
            send_event(ws_tx, ServerEvent::Ready { tab_id, existed }).await
        }
        pty::OP_EXIT => {
            let exit: Option<pty::ExitEvent> = serde_json::from_slice(&payload)
                .inspect_err(|e| {
                    // Falling back to a default silently makes a
                    // protocol-version mismatch look like normal
                    // operation. Say which frame failed.
                    tracing::warn!(
                        "terminal: could not decode EXIT frame from the pty agent: {}",
                        e
                    );
                })
                .ok();
            let (code, signal) = exit.map(|e| (e.code, e.signal)).unwrap_or((None, None));
            send_event(ws_tx, ServerEvent::Exit { code, signal }).await;
            // The program is gone; nothing left to relay.
            false
        }
        pty::OP_ERROR => {
            let err: Option<pty::ErrorPayload> = serde_json::from_slice(&payload)
                .inspect_err(|e| {
                    // Falling back to a default silently makes a
                    // protocol-version mismatch look like normal
                    // operation. Say which frame failed.
                    tracing::warn!(
                        "terminal: could not decode ERROR frame from the pty agent: {}",
                        e
                    );
                })
                .ok();
            let message = err
                .map(|e| format!("{}: {}", e.code, e.message))
                .unwrap_or_else(|| "terminal agent reported an error".to_string());
            send_event(ws_tx, ServerEvent::Error { message }).await;
            false
        }
        // TABS/PONG are for clients that enumerate or ping; this endpoint
        // drives a single tab, so they're not interesting. Ignore rather
        // than error — a newer agent may send frames we don't know yet.
        _ => true,
    }
}

/// Apply a client control message. Returns false when the session should end.
async fn handle_client_control(
    agent_in: &mut std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>,
    text: &str,
) -> bool {
    match serde_json::from_str::<ClientControl>(text) {
        Ok(ClientControl::Resize { cols, rows }) => {
            let payload = pty::encode_resize(cols, rows);
            pty::write_frame(agent_in, pty::OP_RESIZE, &payload)
                .await
                .is_ok()
        }
        // An unparsable control message is a client bug, not a reason to
        // drop someone's shell. Ignore it and keep going.
        Err(e) => {
            tracing::debug!("Ignoring unparsable terminal control message: {}", e);
            true
        }
    }
}

async fn send_event(
    ws_tx: &mut futures::stream::SplitSink<WebSocket, Message>,
    event: ServerEvent<'_>,
) -> bool {
    match serde_json::to_string(&event) {
        Ok(text) => ws_tx.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => true,
    }
}

async fn send_error(ws_tx: &mut futures::stream::SplitSink<WebSocket, Message>, message: String) {
    tracing::warn!("Sandbox terminal error: {}", message);
    send_event(ws_tx, ServerEvent::Error { message }).await;
    let _ = ws_tx.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_is_a_login_shell() {
        // Non-login shells don't source ~/.bashrc, and that's where the image
        // puts claude/codex/opencode on PATH. Losing the `-l` makes every AI
        // CLI mysteriously "not found" inside the terminal.
        assert!(default_shell_cmd().contains("-l"));
    }

    #[test]
    fn heartbeat_stays_inside_the_shortest_idle_window() {
        // MIN_TIMEOUT_SECS is 60. A heartbeat at or above that would let the
        // sweeper stop a sandbox between two beats of a live session.
        assert!(ACTIVITY_HEARTBEAT.as_secs() < 60);
        // ...and not so chatty that an idle terminal hammers the database.
        assert!(ACTIVITY_HEARTBEAT.as_secs() >= 5);
    }

    #[test]
    fn client_resize_control_parses() {
        let parsed: ClientControl =
            serde_json::from_str(r#"{"type":"resize","cols":120,"rows":40}"#).expect("resize");
        match parsed {
            ClientControl::Resize { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
        }
    }

    #[test]
    fn server_events_are_tagged_for_xterm_clients() {
        let ready = serde_json::to_string(&ServerEvent::Ready {
            tab_id: "main",
            existed: true,
        })
        .expect("serialize");
        assert!(ready.contains(r#""type":"ready""#), "{}", ready);
        assert!(ready.contains(r#""existed":true"#), "{}", ready);

        let exit = serde_json::to_string(&ServerEvent::Exit {
            code: Some(0),
            signal: None,
        })
        .expect("serialize");
        assert!(exit.contains(r#""type":"exit""#), "{}", exit);
    }

    #[test]
    fn shell_command_is_wrapped_so_the_pty_echoes() {
        // The regression guard for a terminal you cannot type into. Images
        // predating the agent's `cfmakeraw` removal start the PTY with ECHO
        // off; without this wrapper the user types blind on every one of
        // them, and rebuilding the image is not something they can do from
        // inside a shell that does not work.
        let wrapped = wrap_with_sane_termios(&default_shell_cmd());
        assert!(wrapped.starts_with("stty sane"), "{wrapped}");
        assert!(wrapped.contains("'/bin/bash -l'"), "{wrapped}");
    }

    #[test]
    fn wrapping_preserves_shell_operators_in_a_custom_command() {
        // The regression this nesting exists to prevent. Splicing at
        // statement level bound the caller's operators to the *wrapper*:
        // `exec cd /app && npm start` ran `exec cd` (which fails — `cd` is a
        // builtin, not a program) and never reached `npm start`.
        let wrapped = wrap_with_sane_termios("cd /app && npm start");
        assert!(
            wrapped.contains("'cd /app && npm start'"),
            "the command must reach an inner shell intact: {wrapped}"
        );
        assert!(
            !wrapped.contains("exec cd /app"),
            "must not splice the command at statement level: {wrapped}"
        );
    }

    #[test]
    fn wrapping_escapes_embedded_single_quotes() {
        // Otherwise a quote in the command closes our quoting early and the
        // rest is reinterpreted by the outer shell.
        let wrapped = wrap_with_sane_termios("echo 'hi there'");
        assert!(wrapped.contains(r"'\''hi there'\''"), "{wrapped}");
        // The wrapper's own quoting must still be balanced.
        assert_eq!(
            wrapped.matches('\'').count() % 2,
            0,
            "unbalanced quoting: {wrapped}"
        );
    }

    #[test]
    fn wrapping_preserves_a_custom_command_verbatim() {
        // `--cmd claude` must still start claude, arguments intact.
        let wrapped = wrap_with_sane_termios("claude --resume");
        assert!(wrapped.ends_with("'claude --resume'"), "{wrapped}");
        // `exec` matters: the wrapper shell must be replaced, not left as a
        // parent, or signals and exit codes get an extra hop.
        assert!(wrapped.contains("; exec "), "{wrapped}");
    }

    #[test]
    fn wrapping_tolerates_images_without_stty() {
        // A minimal image with no `stty` must still get a shell, not a tab
        // that dies on the first command.
        assert!(
            wrap_with_sane_termios("sh").contains("2>/dev/null"),
            "stty failure must be swallowed"
        );
    }

    #[test]
    fn tab_accepts_ordinary_names() {
        for ok in ["main", "claude", "tab-2", "my_work_1", &"a".repeat(64)] {
            assert!(validate_tab(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn tab_rejects_log_forging_and_unbounded_names() {
        // A newline would forge lines in the operator's structured log, and
        // each distinct tab costs a PTY plus a 64 KiB ring in the agent.
        for bad in [
            "",
            "has space",
            "new\nline",
            "semi;colon",
            "../escape",
            "unicodé",
        ] {
            assert!(validate_tab(bad).is_err(), "should reject {bad:?}");
        }
        assert!(
            validate_tab(&"a".repeat(65)).is_err(),
            "should reject over-long tab names"
        );
    }

    #[test]
    fn terminal_slot_cap_is_bounded_and_nonzero() {
        // Zero would make every attach 429; unbounded would let one account
        // exhaust dockerd on the reference host.
        const _: () = assert!(MAX_CONCURRENT_TERMINALS > 0);
        const _: () = assert!(MAX_CONCURRENT_TERMINALS <= 128);
        assert_eq!(TERMINAL_SLOTS.available_permits(), MAX_CONCURRENT_TERMINALS);
    }

    #[test]
    fn replay_request_is_bounded() {
        // The agent clamps this, but sending an absurd value would be a
        // pointless round trip. 64 KiB matches the agent's ring size.
        assert_eq!(REPLAY_BYTES, 64 * 1024);
    }
}
