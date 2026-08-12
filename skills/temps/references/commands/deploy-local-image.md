<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `deploy:local-image` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `deploy:local-image` (alias: `deploy-local-image`)

Build and deploy a local Docker image (or deploy existing image with --image)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | Use existing local image instead of building (skips build) | - | No |
| `-f, --dockerfile <path>` | Path to Dockerfile | `Dockerfile` | No |
| `-c, --context <path>` | Build context directory | `.` | No |
| `--build-arg <arg...>` | Build arguments (can be specified multiple times) | - | No |
| `--no-build` | Skip building, requires --image | - | No |
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | `production` | No |
| `--environment-id <id>` | Target environment ID | - | No |
| `-t, --tag <tag>` | Tag for the built/uploaded image | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--metadata <json>` | Additional metadata (JSON format) | - | No |
| `--health-check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Overrides .temps.yaml; also updates the uptime monitor. | - | No |
| `--timeout <seconds>` | Timeout in seconds for --wait | `600` | No |
