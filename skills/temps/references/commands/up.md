<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `up` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `up`

Deploy the current project (runs setup wizard if not linked)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | - | No |
| `-b, --branch <branch>` | Git branch to deploy (auto-detected from cwd) | - | No |
| `-n, --name <name>` | Project name (for new projects) | - | No |
| `--preset <preset>` | Framework preset slug (skip auto-detection) | - | No |
| `--manual` | Use manual deployment mode (no git) | - | No |
| `--static` | Deploy a pre-built static folder (no Docker, no git) | - | No |
| `--static-dir <dir>` | Folder to upload for static deploys (auto-detected by default) | - | No |
| `--no-services` | Skip external service setup | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |
