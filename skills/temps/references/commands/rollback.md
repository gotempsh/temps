<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `rollback` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `rollback`

Rollback to a previous deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Target environment | `production` | No |
| `--to <id>` | Rollback to specific deployment ID | - | No |
| `-y, --yes` | Skip confirmation | - | No |
