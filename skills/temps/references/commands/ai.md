<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `ai` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `ai`

AI assistant status for a project, and AI Gateway governance

**Subcommands:**

- `readiness` - Show which AI prerequisites this project meets, and how to fix the rest
- `governance` - Manage AI gateway governance policies (model allowlists, rate limits, spend caps) per scope

### `ai readiness`

Show which AI prerequisites this project meets, and how to fix the rest

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

### `ai governance`

Manage AI gateway governance policies — per-scope model allowlists, request-rate limits, and monthly spend caps for the AI Gateway. Served by the `temps-ai-gateway` plugin; 404s with a clear message if that plugin isn't installed on the target server. Every subcommand, including `list`, requires the `AiGatewayWrite` permission on the calling credential, since the response exposes budget and allowlist details.

Scope is always `"instance"`, `"project:<id>"`, `"environment:<id>"`, or `"token:<id>"`.

**Subcommands:**

- `list` (`ls`) - List all AI gateway governance policies
- `set <scope>` - Create or update a governance policy for a scope
- `unset <scope>` - Remove the governance policy for a scope, lifting its spend/rate limits

#### `ai governance list` (alias: `ls`)

List all AI gateway governance policies

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai governance set <scope>`

Create or update a governance policy for a scope. Only the fields you pass are changed — omitted flags leave that field untouched on an existing policy.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--models <comma-separated-list>` | Comma-separated model ids to allow. Use `none` to block all models. Omit to leave the allowlist unset (all models allowed). | - | No |
| `--rpm <number>` | Max requests per minute | - | No |
| `--monthly-budget <dollars>` | Max spend per calendar month, in dollars (e.g. `50.00`) | - | No |
| `--json` | Output in JSON format | - | No |

Example: `bunx @temps-sdk/cli@0.1.33 ai governance set instance --rpm 100 --monthly-budget 50.00`

#### `ai governance unset <scope>`

Remove the governance policy for a scope, lifting its model allowlist, rate limit, and spend cap back to unlimited.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |
