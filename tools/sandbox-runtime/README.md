# Sandbox runtime daemon (local development)

These images run the Temps daemon under `tini`, as the unprivileged sandbox
user. The daemon uses `temps-agent-runtime` through a temporary sibling path
dependency. No reusable provider credentials are baked into the image.

## Build and verify

From the repository root, with the runtime checkout beside this checkout:

```sh
tools/sandbox-runtime/build-local.sh ../temps-agent-runtime temps-sandbox-runtime:nodejs-dev-4 nodejs
tools/sandbox-runtime/build-local.sh ../temps-agent-runtime temps-sandbox-runtime:python-dev-4 python
tools/sandbox-runtime/build-local.sh ../temps-agent-runtime temps-sandbox-runtime:all-dev-4 all
tools/sandbox-runtime/smoke.sh temps-sandbox-runtime:nodejs-dev-4
cargo test -p temps-agents --test runtime_daemon -- --nocapture
cargo test --manifest-path tools/sandbox-runtime/Cargo.toml
```

Node.js includes Node, Bun and the Claude/Codex/OpenCode CLIs. Python adds
Python, pip and venv. All adds build-essential and Debian's Go/Rust toolchains.
The latter are distribution versions, not promises of the latest toolchains.

Select the built image using the sandbox custom-image setting or creation
image override. Published defaults are deliberately unchanged until these
images are published. Existing containers are not silently replaced.

## Execution and persistent services

Temps recognizes image label `sh.temps.runtime.protocol=1`, starts `serve`,
checks health, and routes ordinary sandbox execution through the daemon's
private Unix socket. Stdout, stderr and exit status are streamed separately.
Root maintenance remains direct Docker execution. There is no public daemon
port and no Docker socket inside the sandbox.

Within the sandbox, a supervised development server can be launched with:

```sh
temps-sandbox-runtime request /run/temps-runtime/control.sock '{"version":1,"operation":{"type":"start","name":"web","program":"npm","args":["run","dev","--","--hostname","0.0.0.0"],"directory":"/home/temps/workspace/projects/app","restart":true}}'
temps-sandbox-runtime request /run/temps-runtime/control.sock '{"version":1,"operation":{"type":"list"}}'
```

Use the returned process ID with `logs`, `stop`, `restart` or `delete`.
Supervised services survive client disconnects and agent turns. Foreground
exec disconnects cancel that command. Merely starting an ordinary harness
background command does not override that harness's own cleanup policy.

Workspace files are stored separately from the image and survive replacement
when the same volume is mounted. Running processes do not survive container
shutdown, and service definitions are currently in-memory. Automatic service
restoration and retained warm Claude/Codex sessions are not implemented by this
first integration. Do not advertise either as available.

The smoke test verifies service lifecycle, daemon command parentage, and volume
preservation across replacement. The provider test creates all three image
flavors through the actual Temps provider, and skips explicitly when Docker or
the locally built images are unavailable.
