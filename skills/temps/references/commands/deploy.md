<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `deploy` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `deploy`

Deploy a project from git

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | - | No |
| `--environment-id <id>` | Target environment ID | - | No |
| `-b, --branch <branch>` | Git branch to deploy | - | No |
| `-c, --commit <sha>` | Specific commit SHA to deploy | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
