<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `access` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `access`

Manage which teams can reach a project

**Subcommands:**

- `list` (`ls`) - List the teams granted access to a project
- `grant` - Grant a team access to a project
- `revoke` - Revoke a team's access to a project

### `access list` (alias: `ls`)

List the teams granted access to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `access grant`

Grant a team access to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-r, --role <role>` | Role the team holds on the project (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

### `access revoke`

Revoke a team's access to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-y, --yes` | Skip confirmation | - | No |
