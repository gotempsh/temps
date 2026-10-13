<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `project-groups` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `project-groups` (alias: `pg`)

Manage Projects (what the console calls a Project): groups of services that ship together and share a repository, variables and path routes. A service is what the `projects` command manages; the `services` command manages databases and caches, not these.

**Subcommands:**

- `list` (`ls`) - List projects (groups) and their services
- `show` (`get`) - Show a project (group), by slug or id
- `create` (`new`) - Create a project (group), optionally with services
- `update` (`edit`) - Rename a project (group) or change its description
- `delete` (`rm`) - Delete a project (group); its services keep running as standalone services
- `add` - Add services to a project (group), by slug or id as listed by `projects list`; moves them out of any other project
- `remove` - Remove a service (slug or id) from a project (group); it becomes a standalone service
- `repo` - The repository a project deploys its services from (monorepos)
- `import` - Scan the project's repository and add its apps as services that follow it
- `follow` - Make a service follow the project's repository and branch (keeps its directory)
- `unfollow` - Stop a service following the project's repository; it keeps its current settings
- `routes` - Serve a path of one service's domains from another service (e.g. /api → backend)
- `vars` - Variables shared by every service in a project (group)

### `project-groups list` (alias: `ls`)

List projects (groups) and their services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default 1) | - | No |
| `--per-page <n>` | How many per page (default 100, at most 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups show` (alias: `get`)

Show a project (group), by slug or id

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `project-groups create` (alias: `new`)

Create a project (group), optionally with services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Project name | - | No |
| `-s, --slug <slug>` | URL slug (derived from the name when omitted) | - | No |
| `-d, --description <text>` | What the services do together | - | No |
| `--service <service...>` | Services to put in the project, by slug or id as listed by `projects list` (apps, not the databases of the `services` command) | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups update` (alias: `edit`)

Rename a project (group) or change its description

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New name | - | No |
| `-s, --slug <slug>` | New slug | - | No |
| `-d, --description <text>` | New description ("" clears it) | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups delete` (alias: `rm`)

Delete a project (group); its services keep running as standalone services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `project-groups add`

Add services to a project (group), by slug or id as listed by `projects list`; moves them out of any other project

### `project-groups remove`

Remove a service (slug or id) from a project (group); it becomes a standalone service

### `project-groups repo`

The repository a project deploys its services from (monorepos)

**Subcommands:**

- `show` - Show the project's repository and which services follow it
- `set` - Point the project at a repository and branch; services that follow it are updated to match
- `unset` - Remove the repository; services keep their current settings and stop following

#### `project-groups repo show`

Show the project's repository and which services follow it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups repo set`

Point the project at a repository and branch; services that follow it are updated to match

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-c, --connection <id>` | Git connection id (see: git-providers / connections) | - | No |
| `--public <provider>` | Use a public repository without a connection: github or gitlab | - | No |
| `-r, --repo <owner/name|url>` | Repository, e.g. acme/shop; a github.com/gitlab.com URL implies --public | - | Yes |
| `-b, --branch <branch>` | Branch to deploy from, e.g. main | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `project-groups repo unset`

Remove the repository; services keep their current settings and stop following

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `project-groups import`

Scan the project's repository and add its apps as services that follow it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --path <dir...>` | Only these app directories (default: every app without a service) | - | No |
| `--dry-run` | List what would be created without creating anything | - | No |
| `--timeout <minutes>` | Stop waiting after this long (default 30); the import keeps running on the server | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups follow`

Make a service follow the project's repository and branch (keeps its directory)

### `project-groups unfollow`

Stop a service following the project's repository; it keeps its current settings

### `project-groups routes`

Serve a path of one service's domains from another service (e.g. /api → backend)

**Subcommands:**

- `list` (`ls`) - List path routes and how each applies per environment
- `add` - Send requests under <prefix> on <source>'s domains to <target>, e.g. `routes add shop web /api api`
- `remove` (`rm`) - Stop routing <prefix>; <source> serves it itself again

#### `project-groups routes list` (alias: `ls`)

List path routes and how each applies per environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups routes add`

Send requests under <prefix> on <source>'s domains to <target>, e.g. `routes add shop web /api api`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--strip-prefix` | Forward /api/users as /users (default: keep the full path) | - | No |
| `--json` | Output in JSON format | - | No |

#### `project-groups routes remove` (alias: `rm`)

Stop routing <prefix>; <source> serves it itself again

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `project-groups vars`

Variables shared by every service in a project (group)

**Subcommands:**

- `list` (`ls`) - List shared variables (secret values are never shown)
- `set` - Create or update a shared variable; a service's own variable still wins. Omit the value to type it at a hidden prompt, or pass --value-stdin to pipe it (keeps secrets out of shell history)
- `inherited` - Show the shared variables a service (slug or id) receives from its project, and which of them the service's own variables override
- `unset` - Remove a shared variable

#### `project-groups vars list` (alias: `ls`)

List shared variables (secret values are never shown)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups vars set`

Create or update a shared variable; a service's own variable still wins. Omit the value to type it at a hidden prompt, or pass --value-stdin to pipe it (keeps secrets out of shell history)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <slug>` | Only this environment (e.g. production); default: all | - | No |
| `--secret` | Write-only: the value can never be read back | - | No |
| `--value-stdin` | Read the value from stdin, e.g. `cat key.pem \| ... vars set shop TLS_KEY --secret --value-stdin` | - | No |
| `--preview` | Also send an all-environments variable to preview deployments (off by default) | - | No |
| `--no-preview` | Do not send it to preview deployments | - | No |
| `--json` | Output in JSON format | - | No |

#### `project-groups vars inherited`

Show the shared variables a service (slug or id) receives from its project, and which of them the service's own variables override

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups vars unset`

Remove a shared variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <slug>` | The environment it was scoped to; default: all | - | No |
| `-y, --yes` | Skip confirmation | - | No |
