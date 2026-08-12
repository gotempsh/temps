<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `deployments` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `deployments` (alias: `deploys`)

Manage deployments

**Subcommands:**

- `list` (`ls`) - List deployments
- `status` - Show deployment status
- `rollback` - Rollback to previous deployment
- `cancel` - Cancel a running deployment
- `pause` - Pause a deployment
- `resume` - Resume a paused deployment
- `teardown` - Teardown a deployment and remove all resources
- `logs` - Show deployment build logs

### `deployments list` (alias: `ls`)

List deployments

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Filter by environment name (client-side) | - | No |
| `--environment-id <id>` | Filter by environment ID (server-side) | - | No |
| `-n, --limit <number>` | Limit results | `10` | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments status`

Show deployment status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID (required) | - | No |
| `-d, --deployment-id <id>` | Deployment ID (required) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments rollback`

Rollback to previous deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID (required) | - | No |
| `-e, --environment <env>` | Target environment | `production` | No |
| `--to <deployment>` | Rollback to specific deployment ID | - | No |

### `deployments cancel`

Cancel a running deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

### `deployments pause`

Pause a deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |

### `deployments resume`

Resume a paused deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |

### `deployments teardown`

Teardown a deployment and remove all resources

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

### `deployments logs`

Show deployment build logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment | `production` | No |
| `-f, --follow` | Follow log output | - | No |
| `-n, --lines <number>` | Number of lines to show | `100` | No |
| `-d, --deployment <id>` | Specific deployment ID | - | No |
