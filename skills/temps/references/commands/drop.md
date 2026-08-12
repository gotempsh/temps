<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `drop` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `drop`

Detect and deploy a local source directory or ZIP without Git

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Project name (slugified automatically) | - | No |
| `--preset <preset>` | Select a detected preset | - | No |
| `--directory <directory>` | Select a detected project root | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `--timeout <seconds>` | Deployment timeout | `600` | No |
