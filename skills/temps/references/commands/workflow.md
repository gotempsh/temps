<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `workflow` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `workflow` (alias: `wf`)

Trigger and inspect agent/workflow runs

**Subcommands:**

- `list` (`ls`) - List workflows/agents available on this project
- `run` - Trigger a workflow and stream its output

### `workflow list` (alias: `ls`)

List workflows/agents available on this project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detect from .temps/config.json) | - | No |
| `--json` | Output as JSON | - | No |

### `workflow run`

Trigger a workflow and stream its output

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detect from .temps/config.json) | - | No |
| `-c, --context <text>` | Free-form user context passed to the workflow (e.g. a bug description) | - | No |
| `-f, --from-file <path>` | Run an ephemeral workflow from a local YAML file (no server-side persistence). Mutually exclusive with <slug>. | - | No |
| `-e, --error-group <id>` | Link this run to an error group id. The workflow will see the error type, message, and stack trace via the usual {{error_type}} / {{error_message}} template fields. Works with both committed slugs and --from-file. | - | No |
| `--cpu <cores>` | CPU cores for the ephemeral sandbox (0.1–4.0). Overrides the YAML value. Only applies with --from-file. | - | No |
| `--memory <mb>` | Memory limit in MB for the ephemeral sandbox (128–8192). Overrides the YAML value. Only applies with --from-file. | - | No |
| `--no-follow` | Return immediately after queueing instead of streaming logs | - | No |
| `--json` | Print the run record as JSON when it terminates | - | No |
