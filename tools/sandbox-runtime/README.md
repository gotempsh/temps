# Sandbox runtime daemon (local development)

These images run the Temps daemon under `tini`, as the unprivileged sandbox
user. The daemon and the Temps bridge pin `temps-agent-runtime` to commit
`91ef59ba85140bafba6fcd50d91114e7edee99e8` of the public
[agent-runtime-sdk repository](https://github.com/gotempsh/agent-runtime-sdk).
Cargo fetches that exact revision during builds; no sibling checkout or GitHub
credentials are required. No reusable provider credentials are baked into the image.

## Build and verify

From the repository root:

```sh
tools/sandbox-runtime/build-local.sh temps-sandbox-runtime:nodejs-dev-4 nodejs
tools/sandbox-runtime/build-local.sh temps-sandbox-runtime:python-dev-4 python
tools/sandbox-runtime/build-local.sh temps-sandbox-runtime:all-dev-4 all
tools/sandbox-runtime/smoke.sh temps-sandbox-runtime:nodejs-dev-4
cargo test -p temps-agents --test runtime_daemon -- --nocapture
cargo test --locked --manifest-path tools/sandbox-runtime/Cargo.toml
python3 tools/sandbox-runtime/test_build_inputs.py
```

Node.js includes Node, Bun and the Claude/Codex/OpenCode CLIs. Python adds
Python, pip and venv. All adds build-essential and Debian's Go/Rust toolchains.
The latter are distribution versions, not promises of the latest toolchains.

Application workspaces use the managed `ghcr.io/gotempsh/temps-sandbox-{nodejs,python,all}:0.3.2`
pins. For local application-workspace testing, build the corresponding managed
tag with `build-local.sh`; arbitrary custom images are not accepted by this
surface. These tags must be published before distributing a release that uses
them. Building locally does not publish an image.

## GitHub Actions publication

`daemon-images-check.yml` builds all three flavors for Linux amd64 and arm64
on pull requests without registry credentials or pushes. `sandbox-images-beta.yml`
calls `daemon-images.yml` on image-related main changes; `release.yml` calls
the same workflow after its release validation. Both honor their dry-run input.

The build reads the image version from the actual managed-workspace image
mapping and fetches the SDK commit pinned above. Stable release tags publish
the canonical version; main and prerelease tags publish only `<version>-beta`.
Each publication also has a `daemon-<commit>` tag. No `latest`, `stable`, `beta`,
or bare commit alias is changed, avoiding collisions with legacy Python images.
Beta images are verification artifacts; they do not silently upgrade workspace
image settings. Check all image jobs succeed before rolling out the release.

## Updating an existing application workspace

Open **AI workspace → Workspace → Update runtime**. Select a different runtime
flavor in Desired resources first if needed. Confirm the interruption warning:
files and saved settings remain, but running processes stop and in-memory
agent sessions are renewed. Finish or stop active threads before updating.
Restart keeps the same image and cannot fix a protocol incompatibility.

Temps checks the actual SDK protocol using `temps-sandbox-runtime check`, not
just Docker's running state. It validates the candidate in an isolated
container, then replaces compute using the candidate's immutable image ID.
Failed replacement attempts restore the old immutable image and data-network
connections. Settings are committed together only after verification. The
operation survives browser disconnection, but is not a crash-recovery journal
for a control-plane shutdown. Workspace files are never deleted by this action.

Updates and chat claims are serialized per application in the single Temps
server process; unrelated applications continue independently. Multiple
control-plane replicas would require a shared durable lock before enabling
this flow. Existing containers are not silently upgraded.

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
restoration is not implemented. Retained harness sessions use the SDK transport
and can reconnect while the daemon remains alive; they do not survive an image
replacement as live processes.

The smoke test verifies service lifecycle, daemon command parentage, and volume
preservation across replacement. The provider test creates all three image
flavors through the actual Temps provider, and skips explicitly when Docker or
the locally built images are unavailable.
