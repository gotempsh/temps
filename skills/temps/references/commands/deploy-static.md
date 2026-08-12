<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `deploy:static` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `deploy:static` (alias: `deploy-static`)

Deploy static files (tar.gz, zip, or directory)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Path to static files archive or directory | - | Yes |
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | `production` | No |
| `--environment-id <id>` | Target environment ID | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--metadata <json>` | Additional metadata (JSON format) | - | No |
| `--health-check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Overrides .temps.yaml; also updates the uptime monitor. | - | No |
| `--timeout <seconds>` | Timeout in seconds for --wait | `300` | No |
