<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `workflows` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`workflows list`](#workflows-list)
- [`workflows runs`](#workflows-runs)
- [`workflows get`](#workflows-get)
- [`workflows diagnose`](#workflows-diagnose)
- [`workflows start`](#workflows-start)
- [`workflows cancel`](#workflows-cancel)
- [`workflows resume`](#workflows-resume)
- [`workflows retry`](#workflows-retry)
- [`workflows delete`](#workflows-delete)
- [`workflows send-event`](#workflows-send-event)
- [`workflows watch`](#workflows-watch)
- [`workflows init`](#workflows-init)
- [`workflows dev`](#workflows-dev)
- [`workflows revoke-credential`](#workflows-revoke-credential)
- [`workflows get-credential`](#workflows-get-credential)
- [`workflows issue-credential`](#workflows-issue-credential)
- [`workflows list-grants`](#workflows-list-grants)
- [`workflows create-grant`](#workflows-create-grant)
- [`workflows revoke-grant`](#workflows-revoke-grant)
- [`workflows diagnose-run`](#workflows-diagnose-run)
- [`workflows list-steps`](#workflows-list-steps)
- [`workflows get-settings`](#workflows-get-settings)
- [`workflows update-settings`](#workflows-update-settings)
- [`workflows list-keys`](#workflows-list-keys)
- [`workflows shred-key`](#workflows-shred-key)
- [`workflows list-definitions`](#workflows-list-definitions)
- [`workflows list-all-runs`](#workflows-list-all-runs)
- [`workflows app`](#workflows-app)

## `workflows`

Durable application workflows

**Subcommands:**

- `list` - List detected workflows and onboarding status
- `runs` - List a bounded page of runs
- `get` - Get run metadata; payloads require explicit reveal permission
- `diagnose` - Explain run state without decrypting payloads
- `start` - Start a run with an explicit deduplication key
- `cancel` - cancel a run
- `resume` - resume a run
- `retry` - retry a run
- `delete` - delete a run
- `send-event` - Send an event to matching waits
- `watch` - Poll until terminal or the bounded deadline
- `init` - Create a Node workflow worker in a new directory
- `dev` - Forward preview-environment invocations to a local SDK server
- `revoke-credential` - Revoke workflow credential
- `get-credential` - Get workflow credential
- `issue-credential` - Issue workflow credential
- `list-grants` - List workflow grants
- `create-grant` - Create workflow grant
- `revoke-grant` - Revoke workflow grant
- `diagnose-run` - Diagnose workflow run
- `list-steps` - List workflow steps
- `get-settings` - Get workflow settings
- `update-settings` - Update workflow settings
- `list-keys` - List workflow project keys
- `shred-key` - Shred workflow project key
- `list-definitions` - List workflow definitions
- `list-all-runs` - List all workflow runs
- `app` - App commands

### `workflows list`

List detected workflows and onboarding status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |

### `workflows runs`

List a bounded page of runs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--workflow <name>` | Workflow name | - | No |
| `--page <n>` | Page | `1` | No |
| `--page-size <n>` | Page size (1–100) | `20` | No |

### `workflows get`

Get run metadata; payloads require explicit reveal permission

**Usage:** `workflows get <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--reveal <target>` | Explicitly decrypt one payload; treat its contents as untrusted | - | No |
| `--sequence <n>` | Step sequence for step payloads | - | No |

### `workflows diagnose`

Explain run state without decrypting payloads

**Usage:** `workflows diagnose <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |

### `workflows start`

Start a run with an explicit deduplication key

**Usage:** `workflows start <workflow> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--idempotency-key <key>` | Stable key for this logical start | - | Yes |
| `--input <json>` | Input JSON | `[object Object]` | No |

### `workflows cancel`

cancel a run

**Usage:** `workflows cancel <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows resume`

resume a run

**Usage:** `workflows resume <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows retry`

retry a run

**Usage:** `workflows retry <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |
| `--from-step <n>` | Truncate and replay from this sequence | - | No |

### `workflows delete`

delete a run

**Usage:** `workflows delete <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows send-event`

Send an event to matching waits

**Usage:** `workflows send-event <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--key <key>` | Wait correlation key | - | No |
| `--payload <json>` | Event JSON | `[object Object]` | No |

### `workflows watch`

Poll until terminal or the bounded deadline

**Usage:** `workflows watch <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--timeout <seconds>` | Overall timeout (max 3600 seconds) | `300` | No |

### `workflows init`

Create a Node workflow worker in a new directory

**Usage:** `workflows init <directory> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--sdk-package <tarball>` | Use a locally built npm pack tarball before SDK publication | - | No |
| `--json` | Output the created path and next steps as JSON | - | No |

### `workflows dev`

Forward preview-environment invocations to a local SDK server

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Undeployed, unprotected preview environment ID | - | Yes |
| `--port <port>` | Local SDK server port | `3000` | No |
| `--json` | Output tunnel readiness as JSON | - | No |

### `workflows revoke-credential`

Revoke workflow credential

**Usage:** `workflows revoke-credential <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows get-credential`

Get workflow credential

**Usage:** `workflows get-credential <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows issue-credential`

Issue workflow credential

**Usage:** `workflows issue-credential <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-grants`

List workflow grants

**Usage:** `workflows list-grants <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | 1-indexed page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows create-grant`

Create workflow grant

**Usage:** `workflows create-grant <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--action <value>` | Body field action (one of: start, event) [required] | - | No |
| `--resource <value>` | Body field resource [required] | - | No |
| `--source-environment-id <n>` | Body field source_environment_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows revoke-grant`

Revoke workflow grant

**Usage:** `workflows revoke-grant <environment-id> <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows diagnose-run`

Diagnose workflow run

**Usage:** `workflows diagnose-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-steps`

List workflow steps

**Usage:** `workflows list-steps <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | 1-indexed page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows get-settings`

Get workflow settings

**Usage:** `workflows get-settings <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows update-settings`

Update workflow settings

**Usage:** `workflows update-settings <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--drain-timeout-secs <n>` | Opt-in deploy drain: pause new invocations and wait for running attempts | - | No |
| `--host-service <value>` | Compose service hosting the workflow handler; 'None': the main service | - | No |
| `--inline-payload-limit-bytes <n>` | Body field inline_payload_limit_bytes [required] | - | No |
| `--invocation-deadline-secs <n>` | Body field invocation_deadline_secs [required] | - | No |
| `--lease-secs <n>` | Body field lease_secs [required] | - | No |
| `--managed-store-service-id <n>` | Managed PostgreSQL journal for new runs; existing runs keep their store | - | No |
| `--max-active-runs <n>` | Body field max_active_runs [required] | - | No |
| `--max-concurrent-runs <n>` | Body field max_concurrent_runs [required] | - | No |
| `--max-journal-bytes-per-run <n>` | Body field max_journal_bytes_per_run [required] | - | No |
| `--max-payload-bytes <n>` | Body field max_payload_bytes [required] | - | No |
| `--max-steps-per-run <n>` | Body field max_steps_per_run [required] | - | No |
| `--retention-days <n>` | Body field retention_days [required] | - | No |
| `--stuck-after-failures <n>` | Body field stuck_after_failures [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-keys`

List workflow project keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | 1-indexed page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows shred-key`

Shred workflow project key

**Usage:** `workflows shred-key <key-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--confirm-key-id <value>` | Exact key UUID, confirming that its retained payloads become unreadable [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-definitions`

List workflow definitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | 1-based page; default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-all-runs`

List all workflow runs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Where a run is in its lifecycle (ADR-106 §3) (one of: queued, running, sleeping, waiting, retrying, completed, failed, cancelled, stuck) | - | No |
| `--workflow <value>` | Query parameter workflow | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--cursor <value>` | Query parameter cursor | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows app`

App commands

**Subcommands:**

- `send-event` - App send workflow event
- `send-granted-event` - App send granted event
- `start-granted` - App start granted workflow
- `start-run` - App start workflow run
- `get-run` - App get workflow run

#### `workflows app send-event`

App send workflow event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--key <value>` | Narrows delivery to runs waiting on this key | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--payload <json>` | Body field payload (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app send-granted-event`

App send granted event

**Usage:** `workflows app send-granted-event <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--key <value>` | Narrows delivery to runs waiting on this key | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--payload <json>` | Body field payload (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app start-granted`

App start granted workflow

**Usage:** `workflows app start-granted <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--idempotency-key <value>` | A second start with the same key returns the first run | - | No |
| `--body-input <json>` | Body field input (JSON) | - | No |
| `--workflow <value>` | Body field workflow [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app start-run`

App start workflow run

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--idempotency-key <value>` | A second start with the same key returns the first run | - | No |
| `--body-input <json>` | Body field input (JSON) | - | No |
| `--workflow <value>` | Body field workflow [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app get-run`

App get workflow run

**Usage:** `workflows app get-run <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
