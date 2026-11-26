<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `agents` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`agents create`](#agents-create)
- [`agents get-cli-status`](#agents-get-cli-status)
- [`agents list-all-runs`](#agents-list-all-runs)
- [`agents latest-run-for-source`](#agents-latest-run-for-source)
- [`agents cancel-run`](#agents-cancel-run)
- [`agents retry-run`](#agents-retry-run)
- [`agents get-sandbox-status`](#agents-get-sandbox-status)
- [`agents smoke-test`](#agents-smoke-test)
- [`agents delete`](#agents-delete)
- [`agents get`](#agents-get)
- [`agents update`](#agents-update)
- [`agents list-runs`](#agents-list-runs)
- [`agents stream-events`](#agents-stream-events)
- [`agents list-mcps`](#agents-list-mcps)
- [`agents create-mcp`](#agents-create-mcp)
- [`agents delete-mcp`](#agents-delete-mcp)
- [`agents get-mcp`](#agents-get-mcp)
- [`agents update-mcp`](#agents-update-mcp)
- [`agents reveal-mcp-config`](#agents-reveal-mcp-config)
- [`agents get-skill`](#agents-get-skill)
- [`agents download-skill-archive`](#agents-download-skill-archive)
- [`agents save-token`](#agents-save-token)
- [`agents list-ai-providers`](#agents-list-ai-providers)
- [`agents update-ai-provider`](#agents-update-ai-provider)
- [`agents activate-ai-provider`](#agents-activate-ai-provider)
- [`agents save-ai-provider-credential`](#agents-save-ai-provider-credential)
- [`agents import-local-ai-provider-credential`](#agents-import-local-ai-provider-credential)
- [`agents verify-saved-ai-provider-credential`](#agents-verify-saved-ai-provider-credential)
- [`agents get-ai-provider-harness-update`](#agents-get-ai-provider-harness-update)
- [`agents update-ai-provider-harness`](#agents-update-ai-provider-harness)
- [`agents refresh-ai-provider-models`](#agents-refresh-ai-provider-models)
- [`agents run-ai-provider-preflight`](#agents-run-ai-provider-preflight)
- [`agents run-ai-provider-smoke`](#agents-run-ai-provider-smoke)
- [`agents list-global-mcps`](#agents-list-global-mcps)
- [`agents create-global-mcp`](#agents-create-global-mcp)
- [`agents delete-global-mcp`](#agents-delete-global-mcp)
- [`agents get-global-mcp`](#agents-get-global-mcp)
- [`agents update-global-mcp`](#agents-update-global-mcp)
- [`agents reveal-global-mcp-config`](#agents-reveal-global-mcp-config)
- [`agents rebuild-sandbox-image`](#agents-rebuild-sandbox-image)
- [`agents get-global-sandbox-status`](#agents-get-global-sandbox-status)
- [`agents get-global-skill`](#agents-get-global-skill)
- [`agents download-global-skill-archive`](#agents-download-global-skill-archive)

## `agents`

Project agents, their MCP servers, skills and AI providers

**Subcommands:**

- `create` - Create agent
- `get-cli-status` - Get cli status
- `list-all-runs` - List all runs
- `latest-run-for-source` - Latest run for source
- `cancel-run` - Cancel run
- `retry-run` - Retry a completed, failed, cancelled, or no_fix run with the same trigger context
- `get-sandbox-status` - Get sandbox status
- `smoke-test` - Run a smoke test to verify the selected AI CLI works in the environment where agents will actually execute..
- `delete` - Delete agent
- `get` - Get agent
- `update` - Update agent
- `list-runs` - List agent runs
- `stream-events` - SSE endpoint: streams run log events in real-time
- `list-mcps` - List mcps
- `create-mcp` - Create mcp
- `delete-mcp` - Delete mcp
- `get-mcp` - Get mcp
- `update-mcp` - Update mcp
- `reveal-mcp-config` - Reveal mcp config
- `get-skill` - Get skill
- `download-skill-archive` - Download a skill's archive (tar.gz) — project-scoped
- `save-token` - Save an encrypted AI provider token for use in sandbox containers
- `list-ai-providers` - List the AI provider catalog
- `update-ai-provider` - Update provider-scoped settings without touching the saved credential
- `activate-ai-provider` - Activate a provider as the platform-wide default
- `save-ai-provider-credential` - Save (or replace) a provider's credential
- `import-local-ai-provider-credential` - Import the credential already used by this provider's CLI on the Temps host
- `verify-saved-ai-provider-credential` - Verify a previously imported OpenCode credential against a user-selected model without asking the browser..
- `get-ai-provider-harness-update` - Installed and latest version of the provider's harness CLI in your persistent workspace, and whether an..
- `update-ai-provider-harness` - Install the latest release of the provider's harness CLI in your persistent workspace so newly released..
- `refresh-ai-provider-models` - Refresh exactly one provider's account-aware model inventory
- `run-ai-provider-preflight` - Run ai provider preflight
- `run-ai-provider-smoke` - Run ai provider smoke
- `list-global-mcps` - List global mcps
- `create-global-mcp` - Create global mcp
- `delete-global-mcp` - Delete global mcp
- `get-global-mcp` - Get global mcp
- `update-global-mcp` - Update global mcp
- `reveal-global-mcp-config` - Reveal global mcp config
- `rebuild-sandbox-image` - Rebuild sandbox image
- `get-global-sandbox-status` - Get global sandbox status
- `get-global-skill` - Get global skill
- `download-global-skill-archive` - Download a skill's archive (tar.gz) — global

### `agents create`

Create agent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Preferred model identifier for the CLI. 'Some("")' clears the stored value | - | No |
| `--ai-provider <value>` | Body field ai_provider | - | No |
| `--ai-provider-key-id <n>` | Body field ai_provider_key_id | - | No |
| `--api-key-stdin` | Plain-text API key — will be encrypted before storage (read from stdin) | - | No |
| `--api-key-file <path>` | Plain-text API key — will be encrypted before storage (read from a file) | - | No |
| `--branch-prefix <value>` | Body field branch_prefix | - | No |
| `--config-repo-branch <value>` | Branch of the config repo to use (default: "main") | - | No |
| `--config-repo-url <value>` | Private config repo containing .claude/ directory (skills, MCP, plugins) | - | No |
| `--cooldown-minutes <n>` | Body field cooldown_minutes | - | No |
| `--daily-budget-cents <n>` | Body field daily_budget_cents | - | No |
| `--deliverable <value>` | Body field deliverable | - | No |
| `--description <value>` | Body field description | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--max-turns <n>` | Body field max_turns | - | No |
| `--mcp-servers-config <json>` | MCP servers config (Claude Code settings.json mcpServers format) (JSON) | - | No |
| `--name <value>` | Body field name | - | No |
| `--prompt <value>` | Body field prompt | - | No |
| `--sandbox-enabled` | Body field sandbox_enabled | - | No |
| `--no-sandbox-enabled` | Set sandbox_enabled to false | - | No |
| `--skills-config <json>` | Skills config as JSON array (JSON) | - | No |
| `--slug <value>` | Body field slug | - | No |
| `--timeout-seconds <n>` | Body field timeout_seconds | - | No |
| `--tools-config <json>` | Tools config as JSON array (JSON) | - | No |
| `--trigger-config <json>` | Trigger configuration JSON: { "error": { "new_issue": true, "regression": true }, "manual": true } (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-cli-status`

Get cli status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--provider <value>` | AI provider: claude_cli or codex_cli | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-all-runs`

List all runs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (1-based) | - | No |
| `--page-size <n>` | Page size (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents latest-run-for-source`

Latest run for source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--trigger-source-type <value>` | Trigger source type, e.g. 'error_group' [required] | - | No |
| `--trigger-source-id <n>` | Trigger source ID [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `agents cancel-run`

Cancel run

**Usage:** `agents cancel-run <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents retry-run`

Retry a completed, failed, cancelled, or no_fix run with the same trigger context

**Usage:** `agents retry-run <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-sandbox-status`

Get sandbox status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents smoke-test`

Run a smoke test to verify the selected AI CLI works in the environment where agents will actually execute..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--provider-id <value>` | Provider id to test; defaults to the globally active provider | - | No |
| `--json` | Output in JSON format | - | No |

### `agents delete`

Delete agent

**Usage:** `agents delete <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get`

Get agent

**Usage:** `agents get <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update`

Update agent

**Usage:** `agents update <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Preferred model identifier for the CLI. 'Some("")' clears the stored value | - | No |
| `--ai-provider <value>` | Body field ai_provider | - | No |
| `--ai-provider-key-id <n>` | Body field ai_provider_key_id | - | No |
| `--api-key-stdin` | Plain-text API key — will be encrypted before storage (read from stdin) | - | No |
| `--api-key-file <path>` | Plain-text API key — will be encrypted before storage (read from a file) | - | No |
| `--branch-prefix <value>` | Body field branch_prefix | - | No |
| `--config-repo-branch <value>` | Branch of the config repo to use (default: "main") | - | No |
| `--config-repo-url <value>` | Private config repo containing .claude/ directory (skills, MCP, plugins) | - | No |
| `--cooldown-minutes <n>` | Body field cooldown_minutes | - | No |
| `--daily-budget-cents <n>` | Body field daily_budget_cents | - | No |
| `--deliverable <value>` | Body field deliverable | - | No |
| `--description <value>` | Body field description | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--max-turns <n>` | Body field max_turns | - | No |
| `--mcp-servers-config <json>` | MCP servers config (Claude Code settings.json mcpServers format) (JSON) | - | No |
| `--name <value>` | Body field name | - | No |
| `--prompt <value>` | Body field prompt | - | No |
| `--sandbox-enabled` | Body field sandbox_enabled | - | No |
| `--no-sandbox-enabled` | Set sandbox_enabled to false | - | No |
| `--skills-config <json>` | Skills config as JSON array (JSON) | - | No |
| `--slug <value>` | Body field slug | - | No |
| `--timeout-seconds <n>` | Body field timeout_seconds | - | No |
| `--tools-config <json>` | Tools config as JSON array (JSON) | - | No |
| `--trigger-config <json>` | Trigger configuration JSON: { "error": { "new_issue": true, "regression": true }, "manual": true } (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-runs`

List agent runs

**Usage:** `agents list-runs <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (1-based) | - | No |
| `--page-size <n>` | Page size (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents stream-events`

SSE endpoint: streams run log events in real-time

**Usage:** `agents stream-events <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

### `agents list-mcps`

List mcps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents create-mcp`

Create mcp

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--slug <value>` | Body field slug [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents delete-mcp`

Delete mcp

**Usage:** `agents delete-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-mcp`

Get mcp

**Usage:** `agents get-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update-mcp`

Update mcp

**Usage:** `agents update-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents reveal-mcp-config`

Reveal mcp config

**Usage:** `agents reveal-mcp-config <slug> <field> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-skill`

Get skill

**Usage:** `agents get-skill <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents download-skill-archive`

Download a skill's archive (tar.gz) — project-scoped

**Usage:** `agents download-skill-archive <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `agents save-token`

Save an encrypted AI provider token for use in sandbox containers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--token-stdin` | The OAuth token from 'claude setup-token' or an API key [required] (read from stdin) | - | No |
| `--token-file <path>` | The OAuth token from 'claude setup-token' or an API key [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-ai-providers`

List the AI provider catalog

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--catalog-only` | Return only static/cached catalog metadata | - | No |
| `--no-catalog-only` | Set catalog_only to false | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update-ai-provider`

Update provider-scoped settings without touching the saved credential

**Usage:** `agents update-ai-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--default-model <value>` | New default model id. 'None' or an empty string clears the stored value so the CLI falls back to its own... | - | No |
| `--max-turns-analysis <n>` | Default max turns for the autofixer analysis phase (1–200). '0' clears the stored value (built-in default... | - | No |
| `--max-turns-feedback <n>` | Default max turns for autofixer feedback rounds (1–200). '0' clears; omitted leaves unchanged | - | No |
| `--max-turns-fix <n>` | Default max turns for the autofixer fix phase (1–200). '0' clears; omitted leaves unchanged | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents activate-ai-provider`

Activate a provider as the platform-wide default

**Usage:** `agents activate-ai-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents save-ai-provider-credential`

Save (or replace) a provider's credential

**Usage:** `agents save-ai-provider-credential <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auth-type <value>` | Auth flavor id (must match one of the provider's catalog entries) [required] | - | No |
| `--credential-stdin` | Plaintext credential body (API key, OAuth token, or full config file contents) [required] (read from stdin) | - | No |
| `--credential-file <path>` | Plaintext credential body (API key, OAuth token, or full config file contents) [required] (read from a file) | - | No |
| `--verification-model <value>` | OpenCode model to test, in 'provider/model' form | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents import-local-ai-provider-credential`

Import the credential already used by this provider's CLI on the Temps host

**Usage:** `agents import-local-ai-provider-credential <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--verification-model <value>` | OpenCode model to use for the verification request, in 'provider/model' form | - | No |
| `--json` | Output in JSON format | - | No |

### `agents verify-saved-ai-provider-credential`

Verify a previously imported OpenCode credential against a user-selected model without asking the browser..

**Usage:** `agents verify-saved-ai-provider-credential <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--verification-model <value>` | OpenCode model in 'provider/model' form, used to verify the saved secret [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-ai-provider-harness-update`

Installed and latest version of the provider's harness CLI in your persistent workspace, and whether an..

**Usage:** `agents get-ai-provider-harness-update <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--refresh` | Look inside the workspace again instead of returning the cached check | - | No |
| `--no-refresh` | Set refresh to false | - | No |
| `--cached-only` | Return only what was last read, never looking inside the workspace (which could create or wake it) | - | No |
| `--no-cached-only` | Set cached_only to false | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update-ai-provider-harness`

Install the latest release of the provider's harness CLI in your persistent workspace so newly released..

**Usage:** `agents update-ai-provider-harness <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents refresh-ai-provider-models`

Refresh exactly one provider's account-aware model inventory

**Usage:** `agents refresh-ai-provider-models <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents run-ai-provider-preflight`

Run ai provider preflight

**Usage:** `agents run-ai-provider-preflight <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents run-ai-provider-smoke`

Run ai provider smoke

**Usage:** `agents run-ai-provider-smoke <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--consent` | Explicit acknowledgement that this check makes a billable model request [required] | - | No |
| `--no-consent` | Set consent to false | - | No |
| `--model <value>` | Optional catalog model identifier | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-global-mcps`

List global mcps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents create-global-mcp`

Create global mcp

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--slug <value>` | Body field slug [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents delete-global-mcp`

Delete global mcp

**Usage:** `agents delete-global-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-global-mcp`

Get global mcp

**Usage:** `agents get-global-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents update-global-mcp`

Update global mcp

**Usage:** `agents update-global-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents reveal-global-mcp-config`

Reveal global mcp config

**Usage:** `agents reveal-global-mcp-config <slug> <field> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents rebuild-sandbox-image`

Rebuild sandbox image

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

### `agents get-global-sandbox-status`

Get global sandbox status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents get-global-skill`

Get global skill

**Usage:** `agents get-global-skill <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents download-global-skill-archive`

Download a skill's archive (tar.gz) — global

**Usage:** `agents download-global-skill-archive <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |
