# Temps CLI Reference

> Auto-generated documentation for the Temps CLI.
>
> Generated on: 2026-01-02

## Installation

```bash
# Install globally
npm install -g @temps/cli

# Or use with npx
npx @temps/cli [command]
```

## Authentication

Before using most commands, you need to authenticate:

```bash
# Login with API key
temps login

# Or configure with wizard
temps configure
```

## Global Options

| Flag | Description |
|------|-------------|
| `-v, --version` | Display version number |
| `--no-color` | Disable colored output |
| `--debug` | Enable debug output |
| `-h, --help` | Display help for command |

## Commands

## `login`

Authenticate with Temps using an API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-k, --api-key <key>` | API key (will prompt if not provided) | - | Yes |

## `logout`

Log out and clear credentials

## `whoami`

Display current authenticated user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

## `configure`

Configure CLI settings (AWS-style wizard)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-url <url>` | API URL | - | Yes |
| `--api-token <token>` | API token for authentication | - | Yes |
| `--output-format <format>` | Output format (table, json, minimal) | - | Yes |
| `--enable-colors` | Enable colored output in config | - | No |
| `--disable-colors` | Disable colored output in config | - | No |
| `-i, --interactive` | Force interactive mode even in non-TTY | - | No |
| `-y, --no-interactive` | Non-interactive mode (uses defaults for unspecified options) | - | No |

**Subcommands:**

- `get` - Get a configuration value
- `set` - Set a configuration value
- `list` - List all configuration values
- `show` - Show current configuration and authentication status
- `reset` - Reset configuration to defaults

### `configure get`

Get a configuration value

### `configure set`

Set a configuration value

### `configure list`

List all configuration values

### `configure show`

Show current configuration and authentication status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `configure reset`

Reset configuration to defaults

## `projects` (alias: `project`, `p`)

Manage projects

**Subcommands:**

- `list` (`ls`) - List all projects
- `create` (`new`) - Create a new project
- `show` (`get`) - Show project details
- `update` (`edit`) - Update project name and description
- `settings` - Update project settings (slug, attack mode, preview environments)
- `git` - Update git repository settings
- `config` - Update deployment configuration (resources, replicas)
- `delete` (`rm`) - Delete a project

### `projects list` (alias: `ls`)

List all projects

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `projects create` (alias: `new`)

Create a new project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Project name | - | Yes |
| `-d, --description <description>` | Project description | - | Yes |
| `--repo <repository>` | Git repository URL | - | Yes |

### `projects show` (alias: `get`)

Show project details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `projects update` (alias: `edit`)

Update project name and description

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `-n, --name <name>` | New project name | - | Yes |
| `-d, --description <description>` | New project description | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts, use provided values (for automation) | - | No |

### `projects settings`

Update project settings (slug, attack mode, preview environments)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `--slug <slug>` | Project URL slug | - | Yes |
| `--attack-mode` | Enable attack mode (CAPTCHA protection) | - | No |
| `--no-attack-mode` | Disable attack mode | - | No |
| `--preview-envs` | Enable preview environments | - | No |
| `--no-preview-envs` | Disable preview environments | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts (for automation) | - | No |

### `projects git`

Update git repository settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `--owner <owner>` | Repository owner | - | Yes |
| `--repo <repo>` | Repository name | - | Yes |
| `--branch <branch>` | Main branch | - | Yes |
| `--directory <directory>` | App directory path | - | Yes |
| `--preset <preset>` | Build preset (auto, nextjs, nodejs, static, docker, rust, go, python) | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts, use provided/existing values (for automation) | - | No |

### `projects config`

Update deployment configuration (resources, replicas)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `--replicas <n>` | Number of container replicas | - | Yes |
| `--cpu-limit <limit>` | CPU limit in cores (e.g., 0.5, 1, 2) | - | Yes |
| `--memory-limit <limit>` | Memory limit in MB | - | Yes |
| `--auto-deploy` | Enable automatic deployments | - | No |
| `--no-auto-deploy` | Disable automatic deployments | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts (for automation) | - | No |

### `projects delete` (alias: `rm`)

Delete a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `deploy`

Deploy a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `-e, --environment <env>` | Target environment name | - | Yes |
| `--environment-id <id>` | Target environment ID | - | Yes |
| `-b, --branch <branch>` | Git branch to deploy | - | Yes |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

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

### `deployments list` (alias: `ls`)

List deployments

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `-e, --environment <env>` | Filter by environment | - | Yes |
| `-n, --limit <number>` | Limit results | `10` | Yes |
| `--json` | Output in JSON format | - | No |

### `deployments status`

Show deployment status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID (required) | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID (required) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `deployments rollback`

Rollback to previous deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID (required) | - | Yes |
| `-e, --environment <env>` | Target environment | `production` | Yes |
| `--to <deployment>` | Rollback to specific deployment ID | - | Yes |

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

## `logs`

Stream deployment logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `-e, --environment <env>` | Environment | `production` | Yes |
| `-f, --follow` | Follow log output | - | No |
| `-n, --lines <number>` | Number of lines to show | `100` | Yes |
| `-d, --deployment <id>` | Specific deployment ID | - | Yes |

## `domains` (alias: `domain`)

Manage custom domains

**Subcommands:**

- `list` (`ls`) - List domains
- `add` - Add a custom domain
- `verify` - Verify domain and provision SSL certificate
- `remove` (`rm`) - Remove a domain
- `ssl` - Manage SSL certificate
- `status` - Check domain status

### `domains list` (alias: `ls`)

List domains

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains add`

Add a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-c, --challenge <type>` | Challenge type (http-01 or dns-01) | `http-01` | Yes |

### `domains verify`

Verify domain and provision SSL certificate

### `domains remove` (alias: `rm`)

Remove a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `domains ssl`

Manage SSL certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--renew` | Force certificate renewal | - | No |

### `domains status`

Check domain status

## `environments` (alias: `envs`, `env`)

Manage environments and environment variables

**Subcommands:**

- `list` (`ls`) - List environments for a project
- `create` - Create a new environment
- `delete` (`rm`) - Delete an environment
- `vars` - Manage environment variables
- `resources` - View or set CPU/memory resources for an environment
- `scale` - View or set the number of replicas for an environment

### `environments list` (alias: `ls`)

List environments for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `environments create`

Create a new environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Environment name | - | Yes |
| `-b, --branch <branch>` | Git branch | - | Yes |
| `--preview` | Set as preview environment | - | No |

### `environments delete` (alias: `rm`)

Delete an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `environments vars`

Manage environment variables

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
| `-e, --environment <name>` | Filter by environment name | - | Yes |
| `--show-values` | Show actual values (hidden by default) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments vars get`

Get a specific environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Specify environment (if variable exists in multiple) | - | Yes |

#### `environments vars set`

Set an environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names (interactive if not provided) | - | Yes |
| `--no-preview` | Exclude from preview environments | - | No |
| `--update` | Update existing variable instead of creating new | - | No |

#### `environments vars delete` (alias: `rm`, `unset`)

Delete an environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Delete only from specific environment | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

#### `environments vars import`

Import environment variables from a .env file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names | - | Yes |
| `--overwrite` | Overwrite existing variables | - | No |

#### `environments vars export`

Export environment variables to .env format

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Export from specific environment | - | Yes |
| `-o, --output <file>` | Write to file instead of stdout | - | Yes |

### `environments resources`

View or set CPU/memory resources for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cpu <millicores>` | CPU limit in millicores (e.g., 500 = 0.5 CPU) | - | Yes |
| `--memory <mb>` | Memory limit in MB (e.g., 512) | - | Yes |
| `--cpu-request <millicores>` | CPU request in millicores (guaranteed minimum) | - | Yes |
| `--memory-request <mb>` | Memory request in MB (guaranteed minimum) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `environments scale`

View or set the number of replicas for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `providers` (alias: `provider`)

Manage Git providers

**Subcommands:**

- `list` (`ls`) - List configured Git providers
- `add` - Add a new Git provider (interactive)
- `remove` (`rm`) - Remove a Git provider
- `show` - Show Git provider details
- `git` - Manage Git providers

### `providers list` (alias: `ls`)

List configured Git providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `providers add`

Add a new Git provider (interactive)

### `providers remove` (alias: `rm`)

Remove a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `providers show`

Show Git provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `providers git`

Manage Git providers

**Subcommands:**

- `connect` - Connect a Git provider (github, gitlab)
- `repos` - List available repositories

#### `providers git connect`

Connect a Git provider (github, gitlab)

#### `providers git repos`

List available repositories

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `backups` (alias: `backup`)

Manage backup schedules and backups

**Subcommands:**

- `schedules` (`schedule`) - Manage backup schedules
- `list` (`ls`) - List backups for a schedule
- `show` - Show backup details

### `backups schedules` (alias: `schedule`)

Manage backup schedules

**Subcommands:**

- `list` (`ls`) - List backup schedules
- `create` - Create a backup schedule
- `show` - Show backup schedule details
- `enable` - Enable a backup schedule
- `disable` - Disable a backup schedule
- `delete` (`rm`) - Delete a backup schedule

#### `backups schedules list` (alias: `ls`)

List backup schedules

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `backups schedules create`

Create a backup schedule

#### `backups schedules show`

Show backup schedule details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `backups schedules enable`

Enable a backup schedule

#### `backups schedules disable`

Disable a backup schedule

#### `backups schedules delete` (alias: `rm`)

Delete a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `backups list` (alias: `ls`)

List backups for a schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups show`

Show backup details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `runtime-logs` (alias: `rlogs`)

Stream runtime container logs (not build logs)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | Yes |
| `-e, --environment <env>` | Environment name | `production` | Yes |
| `-c, --container <id>` | Container ID (partial match supported) | - | Yes |
| `-n, --tail <lines>` | Number of lines to tail | `1000` | Yes |
| `-t, --timestamps` | Show timestamps | - | No |

## `notifications` (alias: `notify`)

Manage notification providers (Slack, Email, etc.)

**Subcommands:**

- `list` (`ls`) - List configured notification providers
- `add` - Add a new notification provider (interactive)
- `show` - Show notification provider details
- `remove` (`rm`) - Remove a notification provider
- `test` - Send a test notification

### `notifications list` (alias: `ls`)

List configured notification providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notifications add`

Add a new notification provider (interactive)

### `notifications show`

Show notification provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notifications remove` (alias: `rm`)

Remove a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `notifications test`

Send a test notification

## `dns` (alias: `dns-providers`)

Manage DNS providers for automated domain verification

**Subcommands:**

- `list` (`ls`) - List configured DNS providers
- `add` - Add a new DNS provider (interactive)
- `show` - Show DNS provider details
- `remove` (`rm`) - Remove a DNS provider
- `test` - Test DNS provider connection
- `zones` - List available zones in a DNS provider

### `dns list` (alias: `ls`)

List configured DNS providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `dns add`

Add a new DNS provider (interactive)

### `dns show`

Show DNS provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `dns remove` (alias: `rm`)

Remove a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `dns test`

Test DNS provider connection

### `dns zones`

List available zones in a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `services` (alias: `svc`)

Manage external services (databases, caches, storage)

**Subcommands:**

- `list` (`ls`) - List all external services
- `create` (`add`) - Create a new external service (interactive)
- `show` - Show service details
- `remove` (`rm`) - Remove a service
- `start` - Start a stopped service
- `stop` - Stop a running service
- `types` - List available service types
- `projects` - List projects linked to a service

### `services list` (alias: `ls`)

List all external services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services create` (alias: `add`)

Create a new external service (interactive)

### `services show`

Show service details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services remove` (alias: `rm`)

Remove a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `services start`

Start a stopped service

### `services stop`

Stop a running service

### `services types`

List available service types

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services projects`

List projects linked to a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `settings`

Manage platform settings

**Subcommands:**

- `show` (`get`) - Show current platform settings
- `update` (`set`) - Update platform settings (interactive)
- `set-external-url` - Set the external URL for the platform
- `set-preview-domain` - Set the preview domain pattern

### `settings show` (alias: `get`)

Show current platform settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings update` (alias: `set`)

Update platform settings (interactive)

### `settings set-external-url`

Set the external URL for the platform

### `settings set-preview-domain`

Set the preview domain pattern

## `users`

Manage platform users

**Subcommands:**

- `list` (`ls`) - List all users
- `create` (`add`) - Create a new user
- `me` - Show current user info
- `remove` (`rm`) - Remove a user
- `restore` - Restore a deleted user
- `role` - Manage user roles

### `users list` (alias: `ls`)

List all users

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `users create` (alias: `add`)

Create a new user

### `users me`

Show current user info

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `users remove` (alias: `rm`)

Remove a user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `users restore`

Restore a deleted user

### `users role`

Manage user roles

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--add <role>` | Add a role to user | - | Yes |
| `--remove <role>` | Remove a role from user | - | Yes |

## `apikeys` (alias: `keys`)

Manage API keys for programmatic access

**Subcommands:**

- `list` (`ls`) - List all API keys
- `create` (`add`) - Create a new API key
- `show` - Show API key details
- `remove` (`rm`) - Delete an API key
- `activate` - Activate a deactivated API key
- `deactivate` - Deactivate an API key
- `permissions` - List available API key permissions

### `apikeys list` (alias: `ls`)

List all API keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `apikeys create` (alias: `add`)

Create a new API key

### `apikeys show`

Show API key details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `apikeys remove` (alias: `rm`)

Delete an API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `apikeys activate`

Activate a deactivated API key

### `apikeys deactivate`

Deactivate an API key

### `apikeys permissions`

List available API key permissions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `monitors`

Manage uptime monitors for status pages

**Subcommands:**

- `list` (`ls`) - List all monitors for a project
- `create` (`add`) - Create a new monitor for a project
- `show` - Show monitor details and current status
- `remove` (`rm`) - Delete a monitor
- `status` - Get current monitor status
- `history` - Get monitor uptime history

### `monitors list` (alias: `ls`)

List all monitors for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `monitors create` (alias: `add`)

Create a new monitor for a project

### `monitors show`

Show monitor details and current status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `monitors remove` (alias: `rm`)

Delete a monitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `monitors status`

Get current monitor status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `monitors history`

Get monitor uptime history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--days <days>` | Number of days to show | `7` | Yes |

## `webhooks` (alias: `hooks`)

Manage webhooks for project events

**Subcommands:**

- `list` (`ls`) - List all webhooks for a project
- `create` (`add`) - Create a new webhook for a project
- `show` - Show webhook details
- `remove` (`rm`) - Delete a webhook
- `enable` - Enable a webhook
- `disable` - Disable a webhook
- `events` - List available webhook event types

### `webhooks list` (alias: `ls`)

List all webhooks for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `webhooks create` (alias: `add`)

Create a new webhook for a project

### `webhooks show`

Show webhook details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `webhooks remove` (alias: `rm`)

Delete a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |

### `webhooks enable`

Enable a webhook

### `webhooks disable`

Disable a webhook

### `webhooks events`

List available webhook event types

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `containers` (alias: `cts`)

Manage project containers in environments

**Subcommands:**

- `list` (`ls`) - List all containers in an environment
- `show` - Show container details
- `start` - Start a stopped container
- `stop` - Stop a running container
- `restart` - Restart a container
- `metrics` - Get container resource metrics

### `containers list` (alias: `ls`)

List all containers in an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `containers show`

Show container details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `containers start`

Start a stopped container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |

### `containers stop`

Stop a running container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

### `containers restart`

Restart a container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |

### `containers metrics`

Get container resource metrics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |
| `--json` | Output in JSON format | - | No |


---

## Examples

### Basic Workflow

```bash
# Login to Temps
temps login

# Create a new project
temps projects create --name my-app

# Deploy to production
temps deploy --project my-app --environment production

# View deployment logs
temps logs --project my-app --follow

# Stream runtime container logs
temps runtime-logs --project my-app

# List containers
temps containers list --project-id 1 --environment-id 1
```

### Managing Environments

```bash
# List environments
temps environments list --project my-app

# Set environment variables
temps environments vars set --project my-app --key DATABASE_URL --value "postgres://..."

# View environment variables
temps environments vars list --project my-app
```

### Managing Domains

```bash
# Add a custom domain
temps domains add --project my-app --domain app.example.com

# List domains
temps domains list --project my-app

# Remove a domain
temps domains remove --project my-app --domain app.example.com
```

## Environment Variables

The CLI respects the following environment variables:

| Variable | Description |
|----------|-------------|
| `TEMPS_API_URL` | API endpoint URL |
| `TEMPS_API_TOKEN` | API authentication token |
| `TEMPS_API_KEY` | API key (alternative to token) |
| `NO_COLOR` | Disable colored output |

## Configuration

Configuration is stored in:
- **Config file**: `~/.temps/config.json`
- **Credentials**: `~/.temps/.secrets`

Use `temps configure show` to view current configuration.

## Support

- Documentation: https://temps.dev/docs
- Issues: https://github.com/kfs/temps/issues
