<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `workflows` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

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

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |

### `workflows start`

Start a run with an explicit deduplication key

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

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows resume`

resume a run

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows retry`

retry a run

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

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows send-event`

Send an event to matching waits

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

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--timeout <seconds>` | Overall timeout (max 3600 seconds) | `300` | No |

### `workflows init`

Create a Node workflow worker in a new directory

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
