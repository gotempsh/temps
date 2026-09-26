<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `plugin` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `plugin`

Create, install, update and build TypeScript plugins

**Subcommands:**

- `install` - Install a GitHub TypeScript plugin on the configured Temps server; the server uses its host Git credentials and Docker
- `update` - Rebuild an installed GitHub plugin from its stored source; keep the current plugin if the update fails
- `grants` - Inspect or replace a plugin's host API permissions and AI limits
- `dev` - Run a plugin with a local simulated host, UI preview, and events (no Temps server)
- `init` - Create a TypeScript plugin project
- `build` - Build every platform selected in package.json; --all selects all six supported targets
- `publish` - Build, publish native npm packages, verify ownership and submit for review; resumes interrupted releases

### `plugin install`

Install a GitHub TypeScript plugin on the configured Temps server; the server uses its host Git credentials and Docker

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Plugin subdirectory containing package.json and bun.lock (default: repository root) | - | No |
| `--name <name>` | Advanced: require this plugin name (otherwise auto-detected) | - | No |
| `--ref <ref>` | Advanced: branch, tag, or commit (otherwise repository default branch) | - | No |
| `-y, --yes` | Trust the repository and allow installation without prompting | - | No |
| `--grant <permissions...>` | Explicitly approve declared host permissions (default: none); e.g. ai_generate projects_read | - | No |
| `--ai-daily-calls <count>` | Maximum AI attempts per day (0 pauses usage; default: 100) | - | No |
| `--ai-max-tokens <count>` | Maximum output tokens per AI call (1–4096; default: 1024) | - | No |

### `plugin update`

Rebuild an installed GitHub plugin from its stored source; keep the current plugin if the update fails

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ref <ref>` | Use a different branch, tag, or commit | - | No |
| `-y, --yes` | Trust the update without prompting | - | No |

### `plugin grants`

Inspect or replace a plugin's host API permissions and AI limits

**Subcommands:**

- `get` - Show current grants, actor identity, and AI availability
- `set` - Replace all host grants; --clear revokes all permissions immediately

#### `plugin grants get`

Show current grants, actor identity, and AI availability

#### `plugin grants set`

Replace all host grants; --clear revokes all permissions immediately

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--grant <permissions...>` | Complete list of permissions to grant | - | No |
| `--ai-daily-calls <count>` | Daily AI call limit (0–10000) | - | No |
| `--ai-max-tokens <count>` | Maximum AI output tokens per call (1–4096) | - | No |
| `--clear` | Revoke all host permissions | - | No |

### `plugin dev`

Run a plugin with a local simulated host, UI preview, and events (no Temps server)

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
