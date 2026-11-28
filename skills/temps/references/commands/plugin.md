<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `plugin` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`plugin add`](#plugin-add)
- [`plugin grants`](#plugin-grants)
- [`plugin services`](#plugin-services)
- [`plugin add-service`](#plugin-add-service)
- [`plugin remove`](#plugin-remove)
- [`plugin update`](#plugin-update)
- [`plugin approve-update`](#plugin-approve-update)
- [`plugin disable`](#plugin-disable)
- [`plugin enable`](#plugin-enable)
- [`plugin limits`](#plugin-limits)
- [`plugin dev`](#plugin-dev)
- [`plugin init`](#plugin-init)
- [`plugin build`](#plugin-build)
- [`plugin publish`](#plugin-publish)
- [`plugin api`](#plugin-api)

## `plugin`

Add a plugin from its image, approve its updates and manage its permissions; create TypeScript plugins

**Subcommands:**

- `add` - Deploy a plugin's image as a service and add it as a plugin, in one step; for a plugin that is already added, deploy the image as an update that waits for approval. The image comes from a registry (--image), from your Docker (--local-image) or from a Dockerfile built here (--build)
- `grants` - Inspect or replace a plugin's host API permissions and AI limits
- `services` - List the plugins, the service each runs as and whether it is running; also lists plugins that no longer run, with the reason
- `add-service` - Make a deployed service a plugin; it starts with no permissions
- `remove` - Remove a plugin and revoke its permissions; its service and the files it stored are kept
- `update` - Show the build a plugin is pinned to and the update that waits for approval, with what it asks for
- `approve-update` - Approve the update of a plugin that waits; an update is never applied without this, and it allows no new permissions
- `disable` - Cut a plugin off from Temps at once: no requests, no events, no calls from it; its service, permissions and approved build are kept
- `enable` - Let a disabled plugin connect again, with the permissions it had
- `limits` - Show the memory and CPU a plugin's container is limited to: the limits of its service
- `dev` - Run a plugin with a local simulated host, UI preview, and events (no Temps server)
- `init` - Create a TypeScript plugin project
- `build` - Build every platform selected in package.json; --all selects all six supported targets
- `publish` - Build, publish native npm packages, verify ownership and submit for review; resumes interrupted releases
- `api` - Api commands

### `plugin add`

Deploy a plugin's image as a service and add it as a plugin, in one step; for a plugin that is already added, deploy the image as an update that waits for approval. The image comes from a registry (--image), from your Docker (--local-image) or from a Dockerfile built here (--build)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | Image of the plugin in a registry the server pulls from, by digest or tag (e.g. registry.example.com/acme/reports:1.2.0) | - | No |
| `--local-image <image>` | Image of the plugin in your Docker; it is uploaded to the server, no registry needed | - | No |
| `--build` | Build the plugin's Dockerfile here, then upload the image to the server | - | No |
| `--dockerfile <path>` | Dockerfile that --build builds (default: Dockerfile in the context) | - | No |
| `--context <path>` | Directory that --build builds from (default: the current directory) | - | No |
| `-p, --project <project>` | Slug or id of an existing service to deploy the image to; without it a new service is created | - | No |
| `--name <name>` | Name of the service to create (default: taken from the image, or from the directory that is built) | - | No |
| `-e, --environment <env>` | Environment name | `production` | No |
| `--port <port>` | Port the plugin listens on, for a service that is created | `8080` | No |
| `--health-check-path <path>` | Path the deployment is checked on; a plugin answers /health unless its manifest says otherwise | `/health` | No |
| `--timeout <seconds>` | Seconds to wait for the deployment to be live | `300` | No |

### `plugin grants`

Inspect or replace a plugin's host API permissions and AI limits

**Subcommands:**

- `get` - Show what a plugin asks for, what it is allowed, and what it can reach
- `set` - Replace all host grants; --clear revokes all permissions immediately

#### `plugin grants get`

Show what a plugin asks for, what it is allowed, and what it can reach

**Usage:** `plugin grants get <name>`

#### `plugin grants set`

Replace all host grants; --clear revokes all permissions immediately

**Usage:** `plugin grants set <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--grant <permissions...>` | Complete list of permissions to grant | - | No |
| `--ai-daily-calls <count>` | Daily AI call limit (0–10000) | - | No |
| `--ai-max-tokens <count>` | Maximum AI output tokens per call (1–4096) | - | No |
| `--write <areas...>` | With api_write: what may change (projects, environments, deployments). Omitted: nothing | - | No |
| `--write-project <ids...>` | With api_write: the only services that may change. Omitted: every service | - | No |
| `--clear` | Revoke all host permissions | - | No |

### `plugin services`

List the plugins, the service each runs as and whether it is running; also lists plugins that no longer run, with the reason

### `plugin add-service`

Make a deployed service a plugin; it starts with no permissions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <id>` | Id of the service the plugin runs as | - | Yes |
| `--environment <id>` | Id of the environment whose deployment is the plugin | - | Yes |

### `plugin remove`

Remove a plugin and revoke its permissions; its service and the files it stored are kept

**Usage:** `plugin remove <name>`

### `plugin update`

Show the build a plugin is pinned to and the update that waits for approval, with what it asks for

**Usage:** `plugin update <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

### `plugin approve-update`

Approve the update of a plugin that waits; an update is never applied without this, and it allows no new permissions

**Usage:** `plugin approve-update <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--deployment <id>` | Id of the deployment you reviewed, as shown by temps plugin update | - | Yes |

### `plugin disable`

Cut a plugin off from Temps at once: no requests, no events, no calls from it; its service, permissions and approved build are kept

**Usage:** `plugin disable <name>`

### `plugin enable`

Let a disabled plugin connect again, with the permissions it had

**Usage:** `plugin enable <name>`

### `plugin limits`

Show the memory and CPU a plugin's container is limited to: the limits of its service

**Usage:** `plugin limits <name>`

### `plugin dev`

Run a plugin with a local simulated host, UI preview, and events (no Temps server)

**Usage:** `plugin dev [binary] [args...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Local session name | `default` | No |
| `--exec <command>` | Run a command with arguments after -- | - | No |
| `--port <port>` | Loopback port (0 selects a free port) | `0` | No |
| `--data-dir <path>` | Persistent plugin data directory | - | No |
| `--grant <permission...>` | Explicit host grants; defaults to none or saved session grants | - | No |
| `--fixtures <file>` | Version-1 JSON host fixtures and mock AI settings | - | No |
| `--role <role>` | Synthetic preview role: admin or reader | `admin` | No |
| `--startup-timeout <ms>` | Handshake deadline | `10000` | No |

**Subcommands:**

- `events` - List host event fixtures and example payloads
- `status` - Show local plugin state
- `logs` - Show the last 200 redacted simulator records
- `emit` - Deliver an event; sent does not mean handler completed
- `grants` - Change simulated host permissions immediately

#### `plugin dev events`

List host event fixtures and example payloads

#### `plugin dev status`

Show local plugin state

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--json` | Machine-readable output | - | No |

#### `plugin dev logs`

Show the last 200 redacted simulator records

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--json` | Machine-readable output | - | No |

#### `plugin dev emit`

Deliver an event; sent does not mean handler completed

**Usage:** `plugin dev emit [type] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--file <path>` | Full JSON event envelope with a stable ID | - | No |
| `--project-id <id>` | Project ID | `1` | No |
| `--environment-id <id>` | Environment ID | `1` | No |
| `--deployment-id <id>` | Deployment ID | `42` | No |
| `--environment <name>` | Environment | `production` | No |
| `--url <url>` | Deployment URL | - | No |
| `--repeat <n>` | Repeat the same event ID, up to 1000 | - | No |
| `--count <n>` | Deliver distinct IDs, up to 1000 | - | No |
| `--transport <transport>` | auto or http | `auto` | No |
| `--json` | Machine-readable receipts and envelope | - | No |

#### `plugin dev grants`

Change simulated host permissions immediately

**Subcommands:**

- `set` - Replace all grants or revoke all with --clear

##### `plugin dev grants set`

Replace all grants or revoke all with --clear

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--grant <permission...>` | Complete replacement grant set | - | No |
| `--clear` | Revoke every grant | - | No |

### `plugin init`

Create a TypeScript plugin project

**Usage:** `plugin init <directory> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Scoped npm name, e.g. @your-scope/my-plugin | - | Yes |

### `plugin build`

Build every platform selected in package.json; --all selects all six supported targets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--all` | Build all supported targets | - | No |

### `plugin publish`

Build, publish native npm packages, verify ownership and submit for review; resumes interrupted releases

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Confirm public npm publication non-interactively | - | No |

### `plugin api`

Api commands

**Subcommands:**

- `list-chat-watches` - List chat plugin watches
- `change-chat-watch` - Change chat plugin watch
- `get-ai-tools` - Get plugin ai tools
- `put-ai-tools` - Put plugin ai tools

#### `plugin api list-chat-watches`

List chat plugin watches

**Usage:** `plugin api list-chat-watches <chat> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `plugin api change-chat-watch`

Change chat plugin watch

**Usage:** `plugin api change-chat-watch <chat> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--action <value>` | Body field action (one of: create, pause, resume, remove) [required] | - | No |
| `--digest <value>` | Body field digest | - | No |
| `--mode <value>` | Body field mode (one of: notify, wake) | - | No |
| `--plugin <value>` | Body field plugin | - | No |
| `--source <value>` | Body field source | - | No |
| `--subject <value>` | Body field subject | - | No |
| `--where <json>` | Body field where (JSON) | - | No |
| `--id <value>` | Body field id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `plugin api get-ai-tools`

Get plugin ai tools

**Usage:** `plugin api get-ai-tools <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `plugin api put-ai-tools`

Put plugin ai tools

**Usage:** `plugin api put-ai-tools <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--actor-id <value>` | Current actor_id returned by GET; replacing the installation requires fresh consent [required] | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--installation-revision <value>` | Binary revision returned by GET; a changed binary requires fresh consent [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
