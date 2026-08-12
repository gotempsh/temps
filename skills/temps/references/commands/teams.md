<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `teams` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `teams`

Manage teams and project access

**Subcommands:**

- `list` (`ls`) - List all teams
- `create` (`add`) - Create a new team
- `show` - Show a team with its members and projects
- `update` - Update a team name or description
- `delete` (`rm`) - Delete a team (removes its members and project grants)
- `members` - Manage team membership

### `teams list` (alias: `ls`)

List all teams

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `teams create` (alias: `add`)

Create a new team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Team name | - | No |
| `-s, --slug <slug>` | URL-safe slug ([a-z0-9-]+) | - | No |
| `-d, --description <description>` | Team description | - | No |
| `--json` | Output in JSON format | - | No |

### `teams show`

Show a team with its members and projects

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `teams update`

Update a team name or description

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New team name | - | No |
| `-d, --description <description>` | New description | - | No |
| `--json` | Output in JSON format | - | No |

### `teams delete` (alias: `rm`)

Delete a team (removes its members and project grants)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `teams members`

Manage team membership

**Subcommands:**

- `list` (`ls`) - List a team's members
- `add` - Add a user to a team
- `set-role` - Change a member's role in the team
- `remove` (`rm`) - Remove a user from a team

#### `teams members list` (alias: `ls`)

List a team's members

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `teams members add`

Add a user to a team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-r, --role <role>` | Team role (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

#### `teams members set-role`

Change a member's role in the team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-r, --role <role>` | Team role (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

#### `teams members remove` (alias: `rm`)

Remove a user from a team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-y, --yes` | Skip confirmation | - | No |
