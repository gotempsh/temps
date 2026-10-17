<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `ai` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `ai`

AI assistant status for a project

**Subcommands:**

- `provider` - Manage the AI CLIs (Claude Code, Codex) Temps runs on this server
- `readiness` - Show which AI prerequisites this project meets, and how to fix the rest

### `ai provider`

Manage the AI CLIs (Claude Code, Codex) Temps runs on this server

**Subcommands:**

- `login` - Sign an AI CLI in to your subscription. Prints a link to open, then asks for the code (Claude) or shows a device code (Codex)

#### `ai provider login`

Sign an AI CLI in to your subscription. Prints a link to open, then asks for the code (Claude) or shows a device code (Codex)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Print the final sign-in state as JSON | - | No |

### `ai readiness`

Show which AI prerequisites this project meets, and how to fix the rest

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |
