<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `environments` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `environments` (alias: `envs`, `env`)

Manage environments and environment variables

**Subcommands:**

- `list` (`ls`) - List environments for a project
- `create` - Create a new environment
- `delete` (`rm`) - Delete an environment
- `vars` - Manage environment variables
- `resources` - View or set CPU/memory resources for an environment
- `force-https` - View or set the HTTP to HTTPS redirect override for an environment
- `scale` - View or set the number of replicas for an environment
- `crons` - Manage cron jobs

### `environments list` (alias: `ls`)

List environments for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `environments create`

Create a new environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Environment name | - | No |
| `-b, --branch <branch>` | Git branch | - | No |
| `--preview` | Set as preview environment | - | No |

### `environments delete` (alias: `rm`)

Delete an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |

### `environments vars`

Manage environment variables

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

**Subcommands:**

- `list` (`ls`) - List environment variables
- `get` - Get a specific environment variable
- `set` - Set an environment variable
- `delete` (`rm`, `unset`) - Delete an environment variable
- `import` - Import environment variables from a .env file
- `export` - Export environment variables to .env format

#### `environments vars list` (alias: `ls`)

List environment variables

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Filter by environment name | - | No |
| `--show-values` | Show actual values (hidden by default) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments vars get`

Get a specific environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Specify environment (if variable exists in multiple) | - | No |

#### `environments vars set`

Set an environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names (interactive if not provided) | - | No |
| `--no-preview` | Exclude from preview environments | - | No |
| `--update` | Update existing variable instead of creating new | - | No |
| `--secret` | Store as a secret: the value is masked in the UI and never returned by the API. One-way — to make a secret readable again you must delete the variable and create it anew | - | No |

#### `environments vars delete` (alias: `rm`, `unset`)

Delete an environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Delete only from specific environment | - | No |
| `-f, --force` | Skip confirmation | - | No |

#### `environments vars import`

Import environment variables from a .env file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names | - | No |
| `--overwrite` | Overwrite existing variables | - | No |

#### `environments vars export`

Export environment variables to .env format

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Export from specific environment | - | No |
| `-o, --output <file>` | Write to file instead of stdout | - | No |

### `environments resources`

View or set CPU/memory resources for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--cpu <millicores>` | CPU limit in millicores (e.g., 500 = 0.5 CPU) | - | No |
| `--memory <mb>` | Memory limit in MB (e.g., 512) | - | No |
| `--cpu-request <millicores>` | CPU request in millicores (guaranteed minimum) | - | No |
| `--memory-request <mb>` | Memory request in MB (guaranteed minimum) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments force-https`

View or set the HTTP to HTTPS redirect override for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--enable` | Always redirect plain HTTP to HTTPS, even without a local certificate | - | No |
| `--disable` | Never redirect: keep serving this environment over plain HTTP | - | No |
| `--inherit` | Clear the override and follow the proxy default | - | No |
| `--json` | Output in JSON format | - | No |

### `environments scale`

View or set the number of replicas for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name or slug | `production` | No |
| `-r, --replicas <count>` | Number of replicas to set | - | No |
| `--json` | Output in JSON format | - | No |

### `environments crons`

Manage cron jobs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name or slug | - | Yes |

**Subcommands:**

- `list` (`ls`) - List cron jobs for an environment
- `show` - Show cron job details
- `executions` (`execs`) - Show cron job execution history

#### `environments crons list` (alias: `ls`)

List cron jobs for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments crons show`

Show cron job details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Cron job ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `environments crons executions` (alias: `execs`)

Show cron job execution history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Cron job ID | - | Yes |
| `--page <page>` | Page number | `1` | No |
| `--per-page <count>` | Items per page | `20` | No |
| `--json` | Output in JSON format | - | No |
