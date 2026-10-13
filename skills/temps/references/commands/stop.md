<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `stop` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `stop`

Stop the Temps server and database started by `temps start`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--delete-data` | Also delete everything in your local Temps: its apps, services, sandboxes, database and admin password (asks first) | - | No |
| `-y, --yes` | Do not ask before deleting data | - | No |
